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

/// Three-way outcome for a peer call. Client-side errors (4xx) do not increment
/// the circuit breaker failure counter — they reflect caller mistakes, not peer
/// instability. Only peer-side failures (5xx, timeouts, parse errors) count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallOutcome {
    /// The call succeeded; reset the breaker's consecutive-failure counter.
    Success,
    /// The peer returned 5xx, timed out, or produced a parse error.
    PeerFailure,
    /// The caller sent a bad request (4xx). Does not affect the breaker.
    ClientError,
}

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
    /// True once a half-open probe has been admitted and not yet resolved.
    /// Gates the breaker to a single in-flight probe while half-open.
    half_open_probe_in_flight: bool,
}

#[derive(Debug)]
pub struct PeerGovernor {
    rate_limiter: RateLimiter<NotKeyed, InMemoryState, DefaultClock>,
    breaker: Mutex<Breaker>,
    recent_protocol_notifications: Mutex<VecDeque<Instant>>,
    /// Cooldown before an open breaker is eligible to half-open. Field (not a
    /// hardcoded constant) so tests can drive the transition deterministically.
    cooldown: Duration,
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
        let cooldown = Duration::from_secs(
            std::env::var("IONE_PEER_BREAKER_COOLDOWN_SECS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(30),
        );
        Self::with_cooldown(rps, burst, cooldown)
    }

    /// Construct with an explicit half-open cooldown. Used by `new` (env-driven)
    /// and by tests that need a deterministic transition.
    pub fn with_cooldown(rps: u32, burst: u32, cooldown: Duration) -> Self {
        let quota = Quota::per_second(NonZeroU32::new(rps.max(1)).unwrap())
            .allow_burst(NonZeroU32::new(burst.max(1)).unwrap());
        Self {
            rate_limiter: RateLimiter::direct(quota),
            breaker: Mutex::new(Breaker {
                state: BreakerState::Closed,
                consecutive_failures: 0,
                opened_at: None,
                emitted_open_signal: false,
                half_open_probe_in_flight: false,
            }),
            recent_protocol_notifications: Mutex::new(VecDeque::new()),
            cooldown,
        }
    }

    pub async fn acquire(&self) -> anyhow::Result<()> {
        self.breaker_admit()?;
        self.rate_limiter.until_ready().await;
        Ok(())
    }

    /// Breaker admission gate. Open → reject. Half-open → admit exactly one probe
    /// and reject the rest until it resolves. Closed → admit.
    fn breaker_admit(&self) -> anyhow::Result<()> {
        let state = self.maybe_half_open(self.cooldown);
        match state {
            BreakerState::Open => anyhow::bail!("peer circuit breaker is open"),
            BreakerState::HalfOpen => {
                let mut breaker = self.breaker.lock().expect("peer breaker mutex");
                // Re-check under lock; another caller may have resolved the probe.
                if breaker.state == BreakerState::HalfOpen {
                    if breaker.half_open_probe_in_flight {
                        anyhow::bail!("peer circuit breaker half-open probe in flight");
                    }
                    breaker.half_open_probe_in_flight = true;
                } else if breaker.state == BreakerState::Open {
                    anyhow::bail!("peer circuit breaker is open");
                }
            }
            BreakerState::Closed => {}
        }
        Ok(())
    }

    pub fn record_success(&self) {
        let mut breaker = self.breaker.lock().expect("peer breaker mutex");
        breaker.state = BreakerState::Closed;
        breaker.consecutive_failures = 0;
        breaker.opened_at = None;
        breaker.emitted_open_signal = false;
        breaker.half_open_probe_in_flight = false;
    }

    pub fn record_peer_failure(&self) -> bool {
        let mut breaker = self.breaker.lock().expect("peer breaker mutex");
        breaker.consecutive_failures += 1;
        // A failed half-open probe re-opens immediately, regardless of count.
        if breaker.state == BreakerState::HalfOpen {
            breaker.state = BreakerState::Open;
            breaker.opened_at = Some(Instant::now());
            breaker.half_open_probe_in_flight = false;
            breaker.emitted_open_signal = true;
            return true;
        }
        if breaker.consecutive_failures >= 5 && breaker.state != BreakerState::Open {
            breaker.state = BreakerState::Open;
            breaker.opened_at = Some(Instant::now());
            breaker.emitted_open_signal = true;
            return true;
        }
        false
    }

    /// Dispatch a `CallOutcome` to the circuit breaker.
    /// Returns `true` if the breaker just transitioned to Open (caller should update DB).
    /// `ClientError` is a no-op: 4xx indicates caller mistake, not peer instability.
    pub fn record_outcome(&self, outcome: CallOutcome) -> bool {
        match outcome {
            CallOutcome::Success => {
                self.record_success();
                false
            }
            CallOutcome::PeerFailure => self.record_peer_failure(),
            CallOutcome::ClientError => {
                // A 4xx probe is inconclusive: release the half-open slot so the
                // next call can probe, but leave breaker state unchanged.
                let mut breaker = self.breaker.lock().expect("peer breaker mutex");
                breaker.half_open_probe_in_flight = false;
                false
            }
        }
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

}
