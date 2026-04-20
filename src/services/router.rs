use anyhow::Context;
use serde::Deserialize;
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    models::{RoutingDecision, RoutingTarget},
    repos::RoutingDecisionRepo,
    services::ollama::OllamaClient,
    state::AppState,
};

// ── Internal JSON shape from classifier model ────────────────────────────────

#[derive(Debug, Deserialize)]
struct RouterTarget {
    kind: String,
    #[serde(default)]
    role_id: Option<String>,
    #[serde(default)]
    peer_id: Option<String>,
    #[serde(default)]
    rationale: String,
}

#[derive(Debug, Deserialize)]
struct RouterOutput {
    targets: Vec<RouterTarget>,
}

// ── Public Decision type (used by parse_response callers) ────────────────────

pub struct Decision {
    pub target_kind: RoutingTarget,
    pub target_ref: serde_json::Value,
    pub rationale: String,
}

// ── Map severity string to a fallback RoutingTarget ─────────────────────────

fn severity_fallback(severity: &str) -> RoutingTarget {
    match severity {
        "flagged" => RoutingTarget::Notification,
        "command" => RoutingTarget::Draft,
        _ => RoutingTarget::Feed,
    }
}

// ── Parse a routing_target kind string ──────────────────────────────────────

fn parse_kind(s: &str) -> Option<RoutingTarget> {
    match s.to_lowercase().as_str() {
        "feed" => Some(RoutingTarget::Feed),
        "notification" => Some(RoutingTarget::Notification),
        "draft" => Some(RoutingTarget::Draft),
        "peer" => Some(RoutingTarget::Peer),
        _ => None,
    }
}

// ── Public test-hook: parse raw model output into Decisions ─────────────────

/// Extract routing decisions from a raw model response string.
///
/// Uses the same brace-count extractor as `services::critic`.
/// On any parse or validation failure returns a single fallback Decision
/// based on the provided `severity` string:
///   - `routine`  → `feed`
///   - `flagged`  → `notification`
///   - `command`  → `draft`
pub fn parse_response(raw: &str, severity: &str) -> Vec<Decision> {
    let fallback = |reason: &str| -> Vec<Decision> {
        warn!(reason = reason, "router parse_response fallback");
        vec![Decision {
            target_kind: severity_fallback(severity),
            target_ref: serde_json::json!({}),
            rationale: format!("fallback: {}", reason),
        }]
    };

    // Extract the first complete {...} JSON object via brace counting.
    let start = match raw.find('{') {
        Some(i) => i,
        None => return fallback("no JSON object found"),
    };
    let substr = &raw[start..];

    let mut depth: i32 = 0;
    let mut end_idx: Option<usize> = None;

    for (i, ch) in substr.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end_idx = Some(i + 1);
                    break;
                }
            }
            _ => {}
        }
    }

    let end = match end_idx {
        Some(e) => e,
        None => return fallback("unbalanced braces in JSON"),
    };

    let json_str = &substr[..end];

    let output: RouterOutput = match serde_json::from_str(json_str) {
        Ok(o) => o,
        Err(e) => return fallback(&format!("JSON deserialize error: {}", e)),
    };

    if output.targets.is_empty() {
        return fallback("targets array is empty");
    }

    let mut decisions = Vec::with_capacity(output.targets.len());
    for t in output.targets {
        let kind = match parse_kind(&t.kind) {
            Some(k) => k,
            None => {
                warn!(kind = %t.kind, "router: unknown target kind, skipping");
                continue;
            }
        };

        let target_ref = match &kind {
            RoutingTarget::Feed | RoutingTarget::Notification => {
                if let Some(rid) = &t.role_id {
                    serde_json::json!({ "role_id": rid })
                } else {
                    serde_json::json!({})
                }
            }
            RoutingTarget::Peer => {
                if let Some(pid) = &t.peer_id {
                    serde_json::json!({ "peer_id": pid })
                } else {
                    serde_json::json!({})
                }
            }
            RoutingTarget::Draft => serde_json::json!({}),
        };

        let rationale = if t.rationale.is_empty() {
            "no rationale provided".to_string()
        } else {
            t.rationale
        };

        decisions.push(Decision {
            target_kind: kind,
            target_ref,
            rationale,
        });
    }

    if decisions.is_empty() {
        return fallback("no valid targets after parsing");
    }

    decisions
}

// ── Test hook: insert decisions from a canned raw response ──────────────────

/// Parse `raw` for routing decisions and insert them, bypassing Ollama.
/// `severity` is used as the fallback key when the model response is unparseable.
/// The model string stored on each row is `OLLAMA_ROUTER_MODEL` (or default).
pub async fn classify_with_response(
    pool: &PgPool,
    survivor_id: Uuid,
    raw: &str,
    severity: &str,
) -> anyhow::Result<Vec<RoutingDecision>> {
    let model = std::env::var("OLLAMA_ROUTER_MODEL").unwrap_or_else(|_| "qwen3:8b".to_string());

    let decisions = parse_response(raw, severity);
    let repo = RoutingDecisionRepo::new(pool.clone());

    let mut rows = Vec::with_capacity(decisions.len());
    for d in decisions {
        let row = repo
            .insert(
                survivor_id,
                d.target_kind,
                d.target_ref,
                &model,
                &d.rationale,
            )
            .await
            .context("failed to insert routing_decision via test hook")?;
        rows.push(row);
    }

    Ok(rows)
}

// ── classify_survivor: main scheduler entry point ───────────────────────────

/// Classify a single survivor through the routing classifier.
///
/// Idempotent: if routing decisions already exist for this survivor, returns
/// the existing rows without calling Ollama.
///
/// On Ollama network error: inserts the severity-based fallback decision so the
/// audit trail records the miss, then returns Ok with those rows.
pub async fn classify_survivor(
    state: &AppState,
    survivor_id: Uuid,
) -> anyhow::Result<Vec<RoutingDecision>> {
    let repo = RoutingDecisionRepo::new(state.pool.clone());

    // Idempotency: return existing decisions if any.
    if repo
        .exists_for_survivor(survivor_id)
        .await
        .context("failed to check routing_decision existence")?
    {
        return repo
            .list_for_survivor(survivor_id)
            .await
            .context("failed to fetch existing routing_decisions");
    }

    // Fetch survivor + its signal + workspace info.
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT sig.title, sig.body, sig.severity::TEXT
         FROM survivors s
         JOIN signals sig ON sig.id = s.signal_id
         WHERE s.id = $1",
    )
    .bind(survivor_id)
    .fetch_optional(&state.pool)
    .await
    .context("failed to fetch survivor/signal for routing")?;

    let (title, body, severity_str) = match row {
        Some(r) => r,
        None => {
            warn!(survivor_id = %survivor_id, "classify_survivor: survivor not found");
            return Ok(vec![]);
        }
    };

    // Fetch workspace roles for CoC context.
    let workspace_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT sig.workspace_id FROM survivors s
         JOIN signals sig ON sig.id = s.signal_id
         WHERE s.id = $1",
    )
    .bind(survivor_id)
    .fetch_optional(&state.pool)
    .await
    .context("failed to fetch workspace_id for routing")?;

    let roles_text = if let Some(ws_id) = workspace_id {
        let roles: Vec<(String, i32)> = sqlx::query_as(
            "SELECT name, coc_level FROM roles WHERE workspace_id = $1 ORDER BY coc_level ASC",
        )
        .bind(ws_id)
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default();

        if roles.is_empty() {
            "(no roles defined in this workspace)".to_string()
        } else {
            roles
                .iter()
                .map(|(name, coc)| format!("  - {} (coc_level={})", name, coc))
                .collect::<Vec<_>>()
                .join("\n")
        }
    } else {
        "(workspace not found)".to_string()
    };

    let model = std::env::var("OLLAMA_ROUTER_MODEL").unwrap_or_else(|_| "qwen3:8b".to_string());
    let ollama_base_url =
        std::env::var("OLLAMA_BASE_URL").unwrap_or_else(|_| "http://localhost:11434".to_string());

    let prompt = format!(
        r#"You are a routing classifier for a situational-awareness system.
Given a survivor signal and the roles in this workspace, decide which roles
should receive it and via what channel.

Signal title: {title}
Signal body: {body}
Severity: {severity}

Workspace roles (name, chain-of-command level):
{roles}

Severity guidance:
  routine  → prefer feed targets for appropriate roles
  flagged  → prefer notification targets for appropriate roles
  command  → prefer draft (requires human approval before action)

Respond with strict JSON only, no preamble or postscript:
{{"targets": [{{"kind": "feed"|"notification"|"draft"|"peer", "role_id": "<uuid_or_omit>", "peer_id": "<uuid_or_omit>", "rationale": "..."}}]}}"#,
        title = title,
        body = body,
        severity = severity_str,
        roles = roles_text,
    );

    let ollama = OllamaClient::new(ollama_base_url);

    let start = std::time::Instant::now();
    let raw_response = match ollama.generate(&model, &prompt).await {
        Ok(r) => {
            info!(
                survivor_id = %survivor_id,
                model = %model,
                elapsed_ms = start.elapsed().as_millis(),
                "router ollama call complete"
            );
            r
        }
        Err(e) => {
            warn!(survivor_id = %survivor_id, error = %e, "router ollama call failed — recording fallback");
            let fallback_kind = severity_fallback(&severity_str);
            let fallback_rationale = format!("classifier unreachable: {}", e);
            let fallback_model = format!("{}:fallback", model);
            let row = repo
                .insert(
                    survivor_id,
                    fallback_kind,
                    serde_json::json!({}),
                    &fallback_model,
                    &fallback_rationale,
                )
                .await
                .context("failed to insert fallback routing_decision on network error")?;
            return Ok(vec![row]);
        }
    };

    let decisions = parse_response(&raw_response, &severity_str);
    let mut rows = Vec::with_capacity(decisions.len());
    for d in decisions {
        let row = repo
            .insert(
                survivor_id,
                d.target_kind,
                d.target_ref,
                &model,
                &d.rationale,
            )
            .await
            .context("failed to insert routing_decision")?;
        rows.push(row);
    }

    Ok(rows)
}
