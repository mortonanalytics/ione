use anyhow::Context;
use evalexpr::{
    eval_boolean_with_context, ContextWithMutableVariables, HashMapContext, Value as EvalValue,
};
use serde::Deserialize;
use sqlx::PgPool;
use tracing::warn;
use uuid::Uuid;

use crate::{
    models::{Severity, SignalSource},
    repos::{ConnectorRepo, SignalRepo, StreamEventRepo, StreamRepo},
};

#[derive(Debug, Deserialize)]
struct Rule {
    stream: String,
    when: String,
    severity: String,
    title: String,
}

/// Evaluate all rules defined on `workspace.metadata.rules` against the
/// workspace's recent stream events.  Inserts a signal for each rule/event pair
/// that matches and has not already produced a signal (idempotent).
/// Returns the number of signals inserted.
pub async fn evaluate_workspace(pool: &PgPool, workspace_id: Uuid) -> anyhow::Result<usize> {
    // Fetch workspace metadata
    let metadata: serde_json::Value =
        sqlx::query_scalar("SELECT metadata FROM workspaces WHERE id = $1")
            .bind(workspace_id)
            .fetch_one(pool)
            .await
            .context("failed to fetch workspace metadata")?;

    let rules_json = match metadata.get("rules") {
        Some(v) => v.clone(),
        None => return Ok(0),
    };

    let rules: Vec<Rule> = match serde_json::from_value(rules_json) {
        Ok(r) => r,
        Err(e) => {
            warn!(workspace_id = %workspace_id, error = %e, "failed to parse workspace rules");
            return Ok(0);
        }
    };

    if rules.is_empty() {
        return Ok(0);
    }

    let connector_repo = ConnectorRepo::new(pool.clone());
    let stream_repo = StreamRepo::new(pool.clone());
    let event_repo = StreamEventRepo::new(pool.clone());
    let signal_repo = SignalRepo::new(pool.clone());

    let connectors = connector_repo
        .list(workspace_id)
        .await
        .context("failed to list connectors")?;

    let mut inserted = 0usize;

    for rule in &rules {
        // Find the stream by name across all connectors in this workspace
        let mut target_stream_id: Option<Uuid> = None;
        'outer: for connector in &connectors {
            let streams = stream_repo
                .list(connector.id)
                .await
                .context("failed to list streams")?;
            for s in streams {
                if s.name == rule.stream {
                    target_stream_id = Some(s.id);
                    break 'outer;
                }
            }
        }

        let stream_id = match target_stream_id {
            Some(id) => id,
            None => {
                warn!(
                    workspace_id = %workspace_id,
                    stream = %rule.stream,
                    "rule references unknown stream — skipping"
                );
                continue;
            }
        };

        let events = event_repo
            .list_recent(stream_id, 100)
            .await
            .context("failed to list recent events")?;

        let severity = parse_severity(&rule.severity);

        for event in &events {
            let evidence = serde_json::json!([event.id.to_string()]);

            // Idempotency: skip if we already have a signal for this event/rule
            let already_exists = signal_repo
                .exists_by_title_for_events(
                    workspace_id,
                    SignalSource::Rule,
                    &rule.title,
                    &evidence,
                )
                .await
                .context("failed to check signal existence")?;

            if already_exists {
                continue;
            }

            // Build evalexpr context from payload leaves
            let mut ctx = HashMapContext::new();
            populate_context(&mut ctx, "payload", &event.payload);

            let matched = match eval_boolean_with_context(&rule.when, &ctx) {
                Ok(b) => b,
                Err(e) => {
                    warn!(
                        workspace_id = %workspace_id,
                        rule_title = %rule.title,
                        expression = %rule.when,
                        error = %e,
                        "rule expression evaluation failed — skipping event"
                    );
                    continue;
                }
            };

            if matched {
                signal_repo
                    .insert(
                        workspace_id,
                        SignalSource::Rule,
                        &rule.title,
                        &format!("Rule matched: {}", rule.when),
                        evidence,
                        severity.clone(),
                        None,
                    )
                    .await
                    .context("failed to insert rule signal")?;
                inserted += 1;
            }
        }
    }

    Ok(inserted)
}

/// Recursively populate an evalexpr `HashMapContext` from a JSON value,
/// using dotted-key paths (e.g. `payload.humidity`).
fn populate_context(ctx: &mut HashMapContext, prefix: &str, value: &serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let key = format!("{}.{}", prefix, k);
                populate_context(ctx, &key, v);
            }
        }
        serde_json::Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                let _ = ctx.set_value(prefix.to_string(), EvalValue::Float(f));
            }
        }
        serde_json::Value::String(s) => {
            let _ = ctx.set_value(prefix.to_string(), EvalValue::String(s.clone()));
        }
        serde_json::Value::Bool(b) => {
            let _ = ctx.set_value(prefix.to_string(), EvalValue::Boolean(*b));
        }
        // Null and arrays are not mapped to scalar variables
        _ => {}
    }
}

fn parse_severity(s: &str) -> Severity {
    match s {
        "flagged" => Severity::Flagged,
        "command" => Severity::Command,
        _ => Severity::Routine,
    }
}
