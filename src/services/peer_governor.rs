use std::{
    collections::VecDeque,
    sync::Mutex,
    time::{Duration, Instant},
};

use governor::{
    clock::DefaultClock,
    state::{InMemoryState, NotKeyed},
    Quota, RateLimiter,
};
use serde::Serialize;
use std::num::NonZeroU32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakerState {
    Closed,
    Open,
    HalfOpen,
}

#[derive(Debug)]
struct Breaker {
    state: BreakerState,
    consecutive_failures: u32,
    opened_at: Option<Instant>,
    emitted_open_signal: bool,
}

#[derive(Debug)]
pub struct PeerGovernor {
    rate_limiter: RateLimiter<NotKeyed, InMemoryState, DefaultClock>,
    breaker: Mutex<Breaker>,
    recent_protocol_notifications: Mutex<VecDeque<Instant>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerGovernorSnapshot {
    pub breaker_state: BreakerState,
    pub consecutive_failures: u32,
    pub open: bool,
}

impl PeerGovernor {
    pub fn new(rps: u32, burst: u32) -> Self {
        let quota = Quota::per_second(NonZeroU32::new(rps.max(1)).unwrap())
            .allow_burst(NonZeroU32::new(burst.max(1)).unwrap());
        Self {
            rate_limiter: RateLimiter::direct(quota),
            breaker: Mutex::new(Breaker {
                state: BreakerState::Closed,
                consecutive_failures: 0,
                opened_at: None,
                emitted_open_signal: false,
            }),
            recent_protocol_notifications: Mutex::new(VecDeque::new()),
        }
    }

    pub async fn acquire(&self) -> anyhow::Result<()> {
        self.maybe_half_open(Duration::from_secs(30));
        if self.is_open() {
            anyhow::bail!("peer circuit breaker is open");
        }
        self.rate_limiter.until_ready().await;
        Ok(())
    }

    pub fn record_success(&self) {
        let mut breaker = self.breaker.lock().expect("peer breaker mutex");
        breaker.state = BreakerState::Closed;
        breaker.consecutive_failures = 0;
        breaker.opened_at = None;
        breaker.emitted_open_signal = false;
    }

    pub fn record_peer_failure(&self) -> bool {
        let mut breaker = self.breaker.lock().expect("peer breaker mutex");
        breaker.consecutive_failures += 1;
        if breaker.consecutive_failures >= 5 && breaker.state != BreakerState::Open {
            breaker.state = BreakerState::Open;
            breaker.opened_at = Some(Instant::now());
            breaker.emitted_open_signal = true;
            return true;
        }
        false
    }

    pub fn maybe_half_open(&self, cooldown: Duration) -> BreakerState {
        let mut breaker = self.breaker.lock().expect("peer breaker mutex");
        if breaker.state == BreakerState::Open
            && breaker
                .opened_at
                .map(|opened| opened.elapsed() >= cooldown)
                .unwrap_or(false)
        {
            breaker.state = BreakerState::HalfOpen;
        }
        breaker.state
    }

    pub fn allow_protocol_notification(&self, max_per_minute: usize) -> bool {
        let now = Instant::now();
        let mut recent = self
            .recent_protocol_notifications
            .lock()
            .expect("peer notification mutex");
        while recent
            .front()
            .map(|then| now.duration_since(*then) > Duration::from_secs(60))
            .unwrap_or(false)
        {
            recent.pop_front();
        }
        if recent.len() >= max_per_minute {
            return false;
        }
        recent.push_back(now);
        true
    }

    pub fn snapshot(&self) -> PeerGovernorSnapshot {
        let breaker = self.breaker.lock().expect("peer breaker mutex");
        PeerGovernorSnapshot {
            breaker_state: breaker.state,
            consecutive_failures: breaker.consecutive_failures,
            open: breaker.state == BreakerState::Open,
        }
    }

    fn is_open(&self) -> bool {
        self.breaker.lock().expect("peer breaker mutex").state == BreakerState::Open
    }
}
