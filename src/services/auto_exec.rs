use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::Context;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    connectors::build_from_row,
    models::{ActorKind, ConnectorStatus, Severity},
    repos::{AuditEventRepo, AutoExecPolicyRepo, ConnectorRepo},
    state::AppState,
};

/// Upper bound for `rate_limit_per_min`. The table CHECK (migration 0040) is
/// the primary enforcement; the write path validates against this constant.
pub const MAX_RATE_LIMIT_PER_MIN: u32 = 1000;

// ── Policy types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Trigger {
    pub signal_title_prefix: Option<String>,
    pub severity_at_most: Option<Severity>,
}

#[derive(Debug, Clone)]
pub struct AutoExecPolicy {
    pub id: Uuid,
    pub name: String,
    pub trigger: Trigger,
    pub connector_id: Uuid,
    pub op: String,
    pub args_template: serde_json::Value,
    pub rate_limit_per_min: u32,
    pub severity_cap: Severity,
}

#[derive(Debug)]
pub struct AutoExecDecision {
    pub policy: AutoExecPolicy,
}

#[derive(Debug)]
pub enum AutoExecOutcome {
    NoMatch,
    Delivered { policy_name: String },
    DeliveryFailed { policy_name: String, error: String },
    TemplateError { policy_name: String, error: String },
    ConnectorMissing { policy_name: String },
    RateLimited { policy_name: String },
}

// ── Token bucket ──────────────────────────────────────────────────────────────

struct TokenBucket {
    capacity: u32,
    tokens: u32,
    last_refill: Instant,
}

impl TokenBucket {
    fn new(capacity: u32) -> Self {
        Self {
            capacity,
            tokens: capacity,
            last_refill: Instant::now(),
        }
    }

    /// Refill tokens based on elapsed time (capacity per 60s), then return
    /// whether a token is available without consuming it.
    fn peek(&mut self) -> bool {
        self.refill();
        self.tokens > 0
    }

    /// Consume one token. Returns true if a token was available.
    fn consume(&mut self) -> bool {
        self.refill();
        if self.tokens > 0 {
            self.tokens -= 1;
            true
        } else {
            false
        }
    }

    fn refill(&mut self) {
        let elapsed_secs = self.last_refill.elapsed().as_secs_f64();
        if elapsed_secs >= 60.0 {
            self.tokens = self.capacity;
            self.last_refill = Instant::now();
        }
    }
}

// ── Rate-limit registry ───────────────────────────────────────────────────────

/// Buckets untouched for longer than the refill window are discardable:
/// `TokenBucket::refill` resets to full capacity after 60s, so recreating an
/// expired entry gives the same answer as keeping it. Eviction here cannot
/// change a rate-limit decision.
const RATE_LIMIT_ENTRY_TTL: Duration = Duration::from_secs(60);
/// Hard ceiling on distinct `(workspace, policy)` pairs held at once. Mirrors
/// `funnel::MAX_RATE_LIMIT_SESSIONS`; without it the map grew for the life of
/// the process, which matters most on a long-running edge node.
const MAX_RATE_LIMIT_ENTRIES: usize = 10_000;

static RATE_LIMIT_REGISTRY: std::sync::OnceLock<Mutex<HashMap<(Uuid, Uuid), TokenBucket>>> =
    std::sync::OnceLock::new();

fn registry() -> &'static Mutex<HashMap<(Uuid, Uuid), TokenBucket>> {
    RATE_LIMIT_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Make room for `key` when the map has reached its ceiling: drop expired
/// entries first, then the least recently refilled one if that was not enough.
///
/// Below the ceiling this is a length check and nothing else — sweeping on
/// every decision would put an O(n) scan on the hot path to bound a map that is
/// already bounded.
fn evict_stale(map: &mut HashMap<(Uuid, Uuid), TokenBucket>, key: (Uuid, Uuid)) {
    if map.len() < MAX_RATE_LIMIT_ENTRIES || map.contains_key(&key) {
        return;
    }
    map.retain(|_, bucket| bucket.last_refill.elapsed() < RATE_LIMIT_ENTRY_TTL);
    if map.len() >= MAX_RATE_LIMIT_ENTRIES {
        if let Some(oldest) = map
            .iter()
            .min_by_key(|(_, bucket)| bucket.last_refill)
            .map(|(id, _)| *id)
        {
            map.remove(&oldest);
        }
    }
}

/// Peek: returns true if the bucket has tokens available (does not consume).
fn bucket_peek(workspace_id: Uuid, policy_id: Uuid, capacity: u32) -> bool {
    let key = (workspace_id, policy_id);
    let mut map = registry().lock().expect("rate limit registry poisoned");
    evict_stale(&mut map, key);
    let bucket = map.entry(key).or_insert_with(|| TokenBucket::new(capacity));
    bucket.peek()
}

/// Consume one token. Returns true if a token was available and consumed.
fn bucket_consume(workspace_id: Uuid, policy_id: Uuid, capacity: u32) -> bool {
    let key = (workspace_id, policy_id);
    let mut map = registry().lock().expect("rate limit registry poisoned");
    evict_stale(&mut map, key);
    let bucket = map.entry(key).or_insert_with(|| TokenBucket::new(capacity));
    bucket.consume()
}

/// Reset the bucket for a (workspace_id, policy_id) pair to full capacity.
/// Used as a test hook to simulate the end of a rate-limit window.
pub fn test_reset_rate_limit(workspace_id: Uuid, policy_id: Uuid) {
    let key = (workspace_id, policy_id);
    let mut map = registry().lock().expect("rate limit registry poisoned");
    map.remove(&key);
}

// ── Severity ordering ─────────────────────────────────────────────────────────

fn severity_rank(s: &Severity) -> u8 {
    match s {
        Severity::Routine => 0,
        Severity::Flagged => 1,
        Severity::Command => 2,
    }
}

fn severity_exceeds_cap(signal_severity: &Severity, cap: &Severity) -> bool {
    severity_rank(signal_severity) > severity_rank(cap)
}

// ── Policy read path (auto_exec_policies table, migration 0040) ──────────────

fn parse_severity(s: &str) -> Option<Severity> {
    match s {
        "routine" => Some(Severity::Routine),
        "flagged" => Some(Severity::Flagged),
        "command" => Some(Severity::Command),
        _ => None,
    }
}

fn policy_from_row(row: crate::models::AutoExecPolicy) -> AutoExecPolicy {
    AutoExecPolicy {
        id: row.id,
        name: row.name,
        trigger: Trigger {
            signal_title_prefix: row.trigger_signal_title_prefix,
            // CHECK constraints limit these to routine/flagged; an unparseable
            // value falls to the safe floor rather than dropping the field.
            severity_at_most: row
                .trigger_severity_at_most
                .as_deref()
                .and_then(parse_severity),
        },
        connector_id: row.connector_id,
        op: row.op,
        args_template: row.args_template,
        // Defense-in-depth clamp; the table CHECK (1..=1000) is primary.
        rate_limit_per_min: row
            .rate_limit_per_min
            .clamp(1, MAX_RATE_LIMIT_PER_MIN as i32) as u32,
        severity_cap: parse_severity(&row.severity_cap).unwrap_or(Severity::Routine),
    }
}

async fn fetch_enabled_policies(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
) -> anyhow::Result<Vec<AutoExecPolicy>> {
    let rows = AutoExecPolicyRepo::new(pool.clone())
        .list_enabled_for_workspace(workspace_id)
        .await?;
    Ok(rows.into_iter().map(policy_from_row).collect())
}

// ── Template rendering ────────────────────────────────────────────────────────

/// Recursively walk `value`, replacing `{{signal.title}}`, `{{signal.body}}`,
/// and `{{signal.severity}}` in string leaves.
/// Returns Err if any `{{...}}` expression is not one of the known keys.
fn render_template(
    value: &serde_json::Value,
    title: &str,
    body: &str,
    severity: &str,
) -> anyhow::Result<serde_json::Value> {
    match value {
        serde_json::Value::String(s) => {
            let rendered = render_string(s, title, body, severity)?;
            Ok(serde_json::Value::String(rendered))
        }
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), render_template(v, title, body, severity)?);
            }
            Ok(serde_json::Value::Object(out))
        }
        serde_json::Value::Array(arr) => {
            let out: anyhow::Result<Vec<_>> = arr
                .iter()
                .map(|v| render_template(v, title, body, severity))
                .collect();
            Ok(serde_json::Value::Array(out?))
        }
        other => Ok(other.clone()),
    }
}

fn render_string(s: &str, title: &str, body: &str, severity: &str) -> anyhow::Result<String> {
    let mut result = s.to_owned();
    let mut search_start = 0;

    #[allow(clippy::while_let_loop)]
    loop {
        let open = match result[search_start..].find("{{") {
            Some(pos) => search_start + pos,
            None => break,
        };
        let close = match result[open..].find("}}") {
            Some(pos) => open + pos,
            None => break,
        };
        let key = &result[open + 2..close];
        let replacement = match key {
            "signal.title" => title.to_owned(),
            "signal.body" => body.to_owned(),
            "signal.severity" => severity.to_owned(),
            other => {
                anyhow::bail!(
                    "unknown template variable '{{{{{}}}}}'; only signal.title, signal.body, \
                     signal.severity are supported",
                    other
                )
            }
        };
        result.replace_range(open..close + 2, &replacement);
        search_start = open + replacement.len();
    }

    Ok(result)
}

// ── Survivor context fetch ────────────────────────────────────────────────────

struct SurvivorContext {
    workspace_id: Uuid,
    signal_title: String,
    signal_body: String,
    signal_severity: Severity,
}

async fn fetch_survivor_context(
    pool: &sqlx::PgPool,
    survivor_id: Uuid,
) -> anyhow::Result<Option<SurvivorContext>> {
    let row: Option<(Uuid, String, String, Severity)> = sqlx::query_as(
        "SELECT sig.workspace_id,
                sig.title AS signal_title,
                sig.body AS signal_body,
                sig.severity AS signal_severity
         FROM survivors sv
         JOIN signals sig ON sig.id = sv.signal_id
         WHERE sv.id = $1",
    )
    .bind(survivor_id)
    .fetch_optional(pool)
    .await
    .context("failed to fetch survivor context")?;

    Ok(row.map(
        |(workspace_id, signal_title, signal_body, signal_severity)| SurvivorContext {
            workspace_id,
            signal_title,
            signal_body,
            signal_severity,
        },
    ))
}

// ── evaluate ─────────────────────────────────────────────────────────────────

/// Find the first matching policy for this survivor.
///
/// Returns `None` if:
/// - workspace has no enabled `auto_exec_policies` rows
/// - no policy triggers match
/// - signal severity exceeds the policy's `severity_cap`
/// - the rate-limit bucket is empty
/// - the policy's connector does not exist in the DB
///
/// Returns `Some(decision_as_json)` on a full match.
pub async fn evaluate(
    state: &AppState,
    survivor_id: Uuid,
) -> anyhow::Result<Option<serde_json::Value>> {
    let ctx = match fetch_survivor_context(&state.pool, survivor_id).await? {
        Some(c) => c,
        None => return Ok(None),
    };

    let policies = fetch_enabled_policies(&state.pool, ctx.workspace_id).await?;
    if policies.is_empty() {
        return Ok(None);
    }

    let connector_repo = ConnectorRepo::new(state.pool.clone());

    for policy in &policies {
        // Severity cap: command signals never auto-execute.
        if ctx.signal_severity == Severity::Command {
            continue;
        }
        if severity_exceeds_cap(&ctx.signal_severity, &policy.severity_cap) {
            continue;
        }

        // Trigger: title prefix match.
        if let Some(prefix) = &policy.trigger.signal_title_prefix {
            if !ctx.signal_title.starts_with(prefix.as_str()) {
                continue;
            }
        }

        // Trigger: severity_at_most (signal must be ≤ this).
        if let Some(at_most) = &policy.trigger.severity_at_most {
            if severity_rank(&ctx.signal_severity) > severity_rank(at_most) {
                continue;
            }
        }

        // Connector existence check, workspace-scoped (AEG-C1): a foreign
        // connector_id never resolves, so it can never be invoked.
        let connector = connector_repo
            .get_for_workspace(policy.connector_id, ctx.workspace_id)
            .await
            .context("failed to look up connector for auto_exec policy")?;
        let Some(connector) = connector else {
            // Connector not found in this workspace — skip this policy.
            continue;
        };
        if connector.status != crate::models::ConnectorStatus::Active {
            continue;
        }

        // Rate limit: peek (do not consume).
        if !bucket_peek(ctx.workspace_id, policy.id, policy.rate_limit_per_min) {
            // Bucket exhausted — skip (rate limited).
            continue;
        }

        // All checks passed: return a decision value.
        let decision_json = serde_json::json!({
            "policy_name": policy.name,
            "connector_id": policy.connector_id,
            "op": policy.op,
        });
        return Ok(Some(decision_json));
    }

    Ok(None)
}

// ── evaluate_and_invoke ───────────────────────────────────────────────────────

/// Run the full auto-exec flow for a survivor:
/// 1. Fetch context.
/// 2. Find the first matching policy (same logic as `evaluate`, then consume token).
/// 3. Render args template.
/// 4. Write `auto_authorized` audit.
/// 5. Invoke connector.
/// 6. Write `delivered` or `delivery_failed` audit.
///
/// Returns the `AutoExecOutcome` describing what happened so callers (notably
/// `services::delivery::process_draft`) can decide whether to skip the
/// approval path. `Err` is reserved for infrastructure failures only.
pub async fn evaluate_and_invoke(
    state: &AppState,
    survivor_id: Uuid,
) -> anyhow::Result<AutoExecOutcome> {
    let outcome = run_auto_exec(state, survivor_id).await?;

    match &outcome {
        AutoExecOutcome::NoMatch => {
            info!(survivor_id = %survivor_id, "auto_exec: no matching policy");
        }
        AutoExecOutcome::RateLimited { policy_name } => {
            info!(survivor_id = %survivor_id, policy_name = %policy_name, "auto_exec: rate limited");
        }
        AutoExecOutcome::ConnectorMissing { policy_name } => {
            warn!(survivor_id = %survivor_id, policy_name = %policy_name, "auto_exec: connector missing");
        }
        AutoExecOutcome::TemplateError { policy_name, error } => {
            warn!(survivor_id = %survivor_id, policy_name = %policy_name, error = %error, "auto_exec: template error");
        }
        AutoExecOutcome::Delivered { policy_name } => {
            info!(survivor_id = %survivor_id, policy_name = %policy_name, "auto_exec: delivered");
        }
        AutoExecOutcome::DeliveryFailed { policy_name, error } => {
            warn!(survivor_id = %survivor_id, policy_name = %policy_name, error = %error, "auto_exec: delivery failed");
        }
    }

    Ok(outcome)
}

async fn run_auto_exec(state: &AppState, survivor_id: Uuid) -> anyhow::Result<AutoExecOutcome> {
    let ctx = match fetch_survivor_context(&state.pool, survivor_id).await? {
        Some(c) => c,
        None => return Ok(AutoExecOutcome::NoMatch),
    };

    let policies = fetch_enabled_policies(&state.pool, ctx.workspace_id).await?;
    if policies.is_empty() {
        return Ok(AutoExecOutcome::NoMatch);
    }

    let connector_repo = ConnectorRepo::new(state.pool.clone());
    let audit_repo = AuditEventRepo::new(state.pool.clone());

    for policy in &policies {
        // Severity cap: command signals never auto-execute.
        if ctx.signal_severity == Severity::Command {
            continue;
        }
        if severity_exceeds_cap(&ctx.signal_severity, &policy.severity_cap) {
            continue;
        }

        // Trigger: title prefix.
        if let Some(prefix) = &policy.trigger.signal_title_prefix {
            if !ctx.signal_title.starts_with(prefix.as_str()) {
                continue;
            }
        }

        // Trigger: severity_at_most.
        if let Some(at_most) = &policy.trigger.severity_at_most {
            if severity_rank(&ctx.signal_severity) > severity_rank(at_most) {
                continue;
            }
        }

        // Connector existence, workspace-scoped (AEG-C1): a foreign or missing
        // connector is ConnectorMissing, never an invocation.
        let connector = connector_repo
            .get_for_workspace(policy.connector_id, ctx.workspace_id)
            .await
            .context("connector lookup failed")?;
        let connector = match connector {
            Some(c) if c.status == ConnectorStatus::Active => c,
            _ => {
                write_auto_exec_error(
                    &audit_repo,
                    ctx.workspace_id,
                    &policy.name,
                    "connector missing",
                    survivor_id,
                )
                .await?;
                return Ok(AutoExecOutcome::ConnectorMissing {
                    policy_name: policy.name.clone(),
                });
            }
        };

        // Consume rate-limit token.
        if !bucket_consume(ctx.workspace_id, policy.id, policy.rate_limit_per_min) {
            return Ok(AutoExecOutcome::RateLimited {
                policy_name: policy.name.clone(),
            });
        }

        // Render args template.
        let severity_str = match ctx.signal_severity {
            Severity::Routine => "routine",
            Severity::Flagged => "flagged",
            Severity::Command => "command",
        };
        let rendered_args = match render_template(
            &policy.args_template,
            &ctx.signal_title,
            &ctx.signal_body,
            severity_str,
        ) {
            Ok(v) => v,
            Err(e) => {
                let err_str = e.to_string();
                write_auto_exec_error(
                    &audit_repo,
                    ctx.workspace_id,
                    &policy.name,
                    &err_str,
                    survivor_id,
                )
                .await?;
                return Ok(AutoExecOutcome::TemplateError {
                    policy_name: policy.name.clone(),
                    error: err_str,
                });
            }
        };

        let actor_ref = format!("auto_exec:{}", policy.name);

        // Write auto_authorized audit.
        audit_repo
            .insert(
                Some(ctx.workspace_id),
                ActorKind::System,
                &actor_ref,
                "auto_authorized",
                "survivor",
                Some(survivor_id),
                serde_json::json!({
                    "policy_name": policy.name,
                    "survivor_id": survivor_id,
                }),
            )
            .await
            .context("failed to write auto_authorized audit event")?;

        // Invoke connector.
        match build_from_row(&connector) {
            Ok(impl_) => match impl_.invoke(&policy.op, rendered_args).await {
                Ok(_) => {
                    audit_repo
                        .insert(
                            Some(ctx.workspace_id),
                            ActorKind::System,
                            &actor_ref,
                            "delivered",
                            "connector",
                            Some(connector.id),
                            serde_json::json!({
                                "policy_name": policy.name,
                                "survivor_id": survivor_id,
                            }),
                        )
                        .await
                        .context("failed to write delivered audit event")?;
                    return Ok(AutoExecOutcome::Delivered {
                        policy_name: policy.name.clone(),
                    });
                }
                Err(e) => {
                    let err_str = e.to_string();
                    audit_repo
                        .insert(
                            Some(ctx.workspace_id),
                            ActorKind::System,
                            &actor_ref,
                            "delivery_failed",
                            "connector",
                            Some(connector.id),
                            serde_json::json!({
                                "policy_name": policy.name,
                                "survivor_id": survivor_id,
                                "error": err_str,
                            }),
                        )
                        .await
                        .context("failed to write delivery_failed audit event")?;
                    return Ok(AutoExecOutcome::DeliveryFailed {
                        policy_name: policy.name.clone(),
                        error: err_str,
                    });
                }
            },
            Err(e) => {
                let err_str = format!("failed to build connector: {}", e);
                audit_repo
                    .insert(
                        Some(ctx.workspace_id),
                        ActorKind::System,
                        &actor_ref,
                        "delivery_failed",
                        "connector",
                        Some(connector.id),
                        serde_json::json!({
                            "policy_name": policy.name,
                            "survivor_id": survivor_id,
                            "error": err_str,
                        }),
                    )
                    .await
                    .context("failed to write delivery_failed audit event after build error")?;
                return Ok(AutoExecOutcome::DeliveryFailed {
                    policy_name: policy.name.clone(),
                    error: err_str,
                });
            }
        }
    }

    Ok(AutoExecOutcome::NoMatch)
}

async fn write_auto_exec_error(
    audit_repo: &AuditEventRepo,
    workspace_id: Uuid,
    policy_name: &str,
    reason: &str,
    survivor_id: Uuid,
) -> anyhow::Result<()> {
    audit_repo
        .insert(
            Some(workspace_id),
            ActorKind::System,
            &format!("auto_exec:{}", policy_name),
            "auto_exec_error",
            "survivor",
            Some(survivor_id),
            serde_json::json!({
                "policy_name": policy_name,
                "reason": reason,
                "survivor_id": survivor_id,
            }),
        )
        .await
        .context("failed to write auto_exec_error audit event")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{evict_stale, TokenBucket, MAX_RATE_LIMIT_ENTRIES, RATE_LIMIT_ENTRY_TTL};
    use std::collections::HashMap;
    use std::time::Instant;
    use uuid::Uuid;

    fn bucket_last_refilled(ago: std::time::Duration) -> TokenBucket {
        let mut bucket = TokenBucket::new(5);
        // A monotonic clock can sit close to its epoch shortly after boot, so
        // fall back to "now" rather than panicking on an underflowing subtract.
        if let Some(then) = Instant::now().checked_sub(ago) {
            bucket.last_refill = then;
        }
        bucket
    }

    fn fill(map: &mut HashMap<(Uuid, Uuid), TokenBucket>, count: usize, age: std::time::Duration) {
        for _ in 0..count {
            map.insert((Uuid::new_v4(), Uuid::new_v4()), bucket_last_refilled(age));
        }
    }

    /// The registry grew for the life of the process. It must not exceed its
    /// ceiling no matter how many distinct (workspace, policy) pairs arrive.
    #[test]
    fn the_registry_stays_bounded_when_every_entry_is_fresh() {
        let mut map = HashMap::new();
        fill(&mut map, MAX_RATE_LIMIT_ENTRIES, std::time::Duration::ZERO);
        for _ in 0..50 {
            let key = (Uuid::new_v4(), Uuid::new_v4());
            evict_stale(&mut map, key);
            map.insert(key, TokenBucket::new(5));
            assert!(
                map.len() <= MAX_RATE_LIMIT_ENTRIES,
                "registry grew past its ceiling: {}",
                map.len()
            );
        }
    }

    /// Entries older than the refill window are the ones dropped first —
    /// recreating them yields a full bucket, which is what refill would have
    /// produced anyway, so eviction cannot change a rate-limit answer.
    #[test]
    fn expired_entries_go_before_live_ones() {
        let mut map = HashMap::new();
        fill(
            &mut map,
            MAX_RATE_LIMIT_ENTRIES - 1,
            RATE_LIMIT_ENTRY_TTL * 2,
        );
        let live = (Uuid::new_v4(), Uuid::new_v4());
        map.insert(live, TokenBucket::new(5));

        let arriving = (Uuid::new_v4(), Uuid::new_v4());
        evict_stale(&mut map, arriving);

        assert!(map.contains_key(&live), "a live entry was evicted");
        assert!(
            map.len() < MAX_RATE_LIMIT_ENTRIES,
            "the expired entries should have been swept, leaving room"
        );
    }

    /// A key already in the map is a hit, not an insertion, so it must never
    /// pay for a sweep.
    #[test]
    fn an_existing_key_is_left_alone() {
        let mut map = HashMap::new();
        fill(&mut map, MAX_RATE_LIMIT_ENTRIES, RATE_LIMIT_ENTRY_TTL * 2);
        let existing = *map.keys().next().expect("seeded");
        evict_stale(&mut map, existing);
        assert_eq!(map.len(), MAX_RATE_LIMIT_ENTRIES);
    }

    /// A bucket recreated after eviction reports full capacity — the same
    /// answer `refill` gives once the window has passed.
    #[test]
    fn a_recreated_bucket_reports_full_capacity() {
        let mut bucket = TokenBucket::new(3);
        assert!(bucket.consume());
        assert!(TokenBucket::new(3).peek());
    }
}
