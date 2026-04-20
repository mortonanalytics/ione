use std::{collections::HashMap, sync::Mutex, time::Instant};

use anyhow::Context;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    connectors::build_from_row,
    models::{ActorKind, ConnectorStatus, Severity},
    repos::{AuditEventRepo, ConnectorRepo},
    state::AppState,
};

// ── Policy types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Trigger {
    pub signal_title_prefix: Option<String>,
    pub severity_at_most: Option<Severity>,
}

#[derive(Debug, Clone)]
pub struct AutoExecPolicy {
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

static RATE_LIMIT_REGISTRY: std::sync::OnceLock<Mutex<HashMap<(Uuid, String), TokenBucket>>> =
    std::sync::OnceLock::new();

fn registry() -> &'static Mutex<HashMap<(Uuid, String), TokenBucket>> {
    RATE_LIMIT_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Peek: returns true if the bucket has tokens available (does not consume).
fn bucket_peek(workspace_id: Uuid, policy_name: &str, capacity: u32) -> bool {
    let key = (workspace_id, policy_name.to_owned());
    let mut map = registry().lock().expect("rate limit registry poisoned");
    let bucket = map.entry(key).or_insert_with(|| TokenBucket::new(capacity));
    bucket.peek()
}

/// Consume one token. Returns true if a token was available and consumed.
fn bucket_consume(workspace_id: Uuid, policy_name: &str, capacity: u32) -> bool {
    let key = (workspace_id, policy_name.to_owned());
    let mut map = registry().lock().expect("rate limit registry poisoned");
    let bucket = map.entry(key).or_insert_with(|| TokenBucket::new(capacity));
    bucket.consume()
}

/// Reset the bucket for a (workspace_id, policy_name) pair to full capacity.
/// Used as a test hook to simulate the end of a rate-limit window.
pub fn test_reset_rate_limit(workspace_id: Uuid, policy_name: &str) {
    let key = (workspace_id, policy_name.to_owned());
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

// ── Policy parsing ────────────────────────────────────────────────────────────

fn parse_severity(s: &str) -> Option<Severity> {
    match s {
        "routine" => Some(Severity::Routine),
        "flagged" => Some(Severity::Flagged),
        "command" => Some(Severity::Command),
        _ => None,
    }
}

fn parse_policies(metadata: &serde_json::Value) -> Vec<AutoExecPolicy> {
    let arr = match metadata
        .get("auto_exec_policies")
        .and_then(|v| v.as_array())
    {
        Some(a) => a,
        None => return vec![],
    };

    arr.iter().filter_map(parse_single_policy).collect()
}

fn parse_single_policy(p: &serde_json::Value) -> Option<AutoExecPolicy> {
    let name = p.get("name")?.as_str()?.to_owned();
    let connector_id = Uuid::parse_str(p.get("connector_id")?.as_str()?).ok()?;
    let op = p.get("op")?.as_str()?.to_owned();
    let args_template = p.get("args_template")?.clone();
    let rate_limit_per_min = p.get("rate_limit_per_min")?.as_u64()? as u32;

    let severity_cap_str = p
        .get("severity_cap")
        .and_then(|v| v.as_str())
        .unwrap_or("flagged");
    let severity_cap = parse_severity(severity_cap_str)?;

    let trigger_val = p.get("trigger")?;
    let signal_title_prefix = trigger_val
        .get("signal_title_prefix")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());
    let severity_at_most = trigger_val
        .get("severity_at_most")
        .and_then(|v| v.as_str())
        .and_then(parse_severity);

    Some(AutoExecPolicy {
        name,
        trigger: Trigger {
            signal_title_prefix,
            severity_at_most,
        },
        connector_id,
        op,
        args_template,
        rate_limit_per_min,
        severity_cap,
    })
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
    workspace_metadata: serde_json::Value,
}

async fn fetch_survivor_context(
    pool: &sqlx::PgPool,
    survivor_id: Uuid,
) -> anyhow::Result<Option<SurvivorContext>> {
    let row: Option<(Uuid, String, String, Severity, serde_json::Value)> = sqlx::query_as(
        "SELECT sig.workspace_id,
                sig.title AS signal_title,
                sig.body AS signal_body,
                sig.severity AS signal_severity,
                w.metadata AS workspace_metadata
         FROM survivors sv
         JOIN signals sig ON sig.id = sv.signal_id
         JOIN workspaces w ON w.id = sig.workspace_id
         WHERE sv.id = $1",
    )
    .bind(survivor_id)
    .fetch_optional(pool)
    .await
    .context("failed to fetch survivor context")?;

    Ok(row.map(
        |(workspace_id, signal_title, signal_body, signal_severity, workspace_metadata)| {
            SurvivorContext {
                workspace_id,
                signal_title,
                signal_body,
                signal_severity,
                workspace_metadata,
            }
        },
    ))
}

// ── evaluate ─────────────────────────────────────────────────────────────────

/// Find the first matching policy for this survivor.
///
/// Returns `None` if:
/// - workspace has no `auto_exec_policies` key
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

    let policies = parse_policies(&ctx.workspace_metadata);
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

        // Connector existence check.
        let connector = connector_repo
            .get(policy.connector_id)
            .await
            .context("failed to look up connector for auto_exec policy")?;
        if connector.is_none() {
            // Connector not found — skip this policy, fall through.
            continue;
        }
        let connector = connector.unwrap();
        if connector.status != crate::models::ConnectorStatus::Active {
            continue;
        }

        // Rate limit: peek (do not consume).
        if !bucket_peek(ctx.workspace_id, &policy.name, policy.rate_limit_per_min) {
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

    let policies = parse_policies(&ctx.workspace_metadata);
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

        // Connector existence.
        let connector = connector_repo
            .get(policy.connector_id)
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
        if !bucket_consume(ctx.workspace_id, &policy.name, policy.rate_limit_per_min) {
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
