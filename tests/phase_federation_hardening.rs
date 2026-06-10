//! Federation hardening: circuit-breaker half-open single-probe semantics and
//! server-side MCP session idle-expiry. Both run without a database.

use std::time::Duration;

use ione::services::peer_governor::{BreakerState, CallOutcome, PeerGovernor};

fn open_breaker(gov: &PeerGovernor) {
    for _ in 0..5 {
        gov.record_peer_failure();
    }
    assert!(gov.snapshot().open, "five peer failures should open the breaker");
}

#[tokio::test]
async fn half_open_admits_a_single_probe() {
    // Zero cooldown so the first acquire transitions Open -> HalfOpen immediately.
    let gov = PeerGovernor::with_cooldown(1000, 1000, Duration::ZERO);
    open_breaker(&gov);

    // First call is the half-open probe: admitted.
    assert!(gov.acquire().await.is_ok());
    assert_eq!(gov.snapshot().breaker_state, BreakerState::HalfOpen);

    // Second call while the probe is in flight: rejected (single-probe).
    assert!(
        gov.acquire().await.is_err(),
        "half-open must admit only one probe at a time"
    );

    // Probe succeeds -> breaker closes and traffic flows again.
    gov.record_success();
    assert!(!gov.snapshot().open);
    assert!(gov.acquire().await.is_ok());
}

#[tokio::test]
async fn half_open_probe_failure_reopens() {
    let gov = PeerGovernor::with_cooldown(1000, 1000, Duration::ZERO);
    open_breaker(&gov);

    assert!(gov.acquire().await.is_ok()); // probe admitted, now half-open
    assert_eq!(gov.snapshot().breaker_state, BreakerState::HalfOpen);

    // A failed probe re-opens immediately and reports a fresh transition.
    assert!(gov.record_peer_failure());
    assert_eq!(gov.snapshot().breaker_state, BreakerState::Open);
}

#[tokio::test]
async fn client_error_probe_releases_the_slot() {
    let gov = PeerGovernor::with_cooldown(1000, 1000, Duration::ZERO);
    open_breaker(&gov);

    assert!(gov.acquire().await.is_ok()); // probe admitted
    // A 4xx is inconclusive: it must free the probe slot without closing/opening.
    assert!(!gov.record_outcome(CallOutcome::ClientError));
    assert_eq!(gov.snapshot().breaker_state, BreakerState::HalfOpen);
    // ...so the next caller can probe again.
    assert!(gov.acquire().await.is_ok());
}

#[test]
fn mcp_session_expiry_uses_last_seen() {
    let now = chrono::Utc::now();
    let ttl = 3600;

    let fresh = serde_json::json!({ "last_seen": now.to_rfc3339() });
    assert!(!ione::mcp_server::mcp_session_expired(&fresh, now, ttl));

    let stale = serde_json::json!({
        "last_seen": (now - chrono::Duration::seconds(7200)).to_rfc3339()
    });
    assert!(ione::mcp_server::mcp_session_expired(&stale, now, ttl));

    // last_seen takes precedence over an old created_at.
    let touched = serde_json::json!({
        "created_at": (now - chrono::Duration::seconds(7200)).to_rfc3339(),
        "last_seen": now.to_rfc3339(),
    });
    assert!(!ione::mcp_server::mcp_session_expired(&touched, now, ttl));

    // Missing timestamps are treated as expired rather than leaking forever.
    let malformed = serde_json::json!({ "workspace_id": "x" });
    assert!(ione::mcp_server::mcp_session_expired(&malformed, now, ttl));
}
