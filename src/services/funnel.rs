//! Fire-and-forget funnel event emission.
//!
//! `track()` never blocks user actions: it spawns the DB insert and logs
//! failures. Emission is rate-limited at 10 events per second per session by a
//! process-local sliding window.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use serde_json::Value;
use uuid::Uuid;

use crate::state::AppState;

const MAX_EVENTS_PER_WINDOW: usize = 10;
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(1);

static RATE_LIMITS: OnceLock<Mutex<HashMap<Uuid, VecDeque<Instant>>>> = OnceLock::new();

pub fn track(
    state: &AppState,
    session_id: Uuid,
    user_id: Option<Uuid>,
    workspace_id: Option<Uuid>,
    event_kind: &str,
    detail: Option<Value>,
) {
    if !allow_event(session_id) {
        tracing::debug!(%session_id, event_kind, "funnel event rate-limited");
        return;
    }

    let repo = crate::repos::FunnelEventRepo::new(state.pool.clone());
    let event_kind = event_kind.to_string();
    tokio::spawn(async move {
        if let Err(e) = repo
            .append(crate::models::FunnelEventInput {
                user_id,
                session_id,
                workspace_id,
                event_kind,
                detail,
            })
            .await
        {
            tracing::warn!(error = %e, "funnel event append failed");
        }
    });
}

fn allow_event(session_id: Uuid) -> bool {
    let now = Instant::now();
    let limits = RATE_LIMITS.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut limits) = limits.lock() else {
        tracing::warn!("funnel event rate limiter lock poisoned");
        return false;
    };

    let events = limits.entry(session_id).or_default();
    while events
        .front()
        .is_some_and(|event_at| now.duration_since(*event_at) >= RATE_LIMIT_WINDOW)
    {
        events.pop_front();
    }

    if events.len() >= MAX_EVENTS_PER_WINDOW {
        return false;
    }

    events.push_back(now);
    true
}
