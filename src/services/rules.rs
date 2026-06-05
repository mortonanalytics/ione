use std::collections::BTreeMap;

use anyhow::Context;
use evalexpr::{
    build_operator_tree, ContextWithMutableVariables, EvalexprError, HashMapContext,
    Value as EvalValue,
};
use serde::Deserialize;
use sqlx::PgPool;
use tracing::warn;
use uuid::Uuid;

use crate::{
    connectors::geojson_poll::FieldType,
    models::{DiagStatus, RuleDiagnostic, RuleEvalReport, Severity, SignalSource, SkipReason},
    repos::{ConnectorRepo, SignalRepo, StreamEventRepo, StreamRepo},
};

const MAX_SKIP_REASONS: usize = 5;

#[derive(Debug, Deserialize)]
pub(crate) struct Rule {
    stream: String,
    when: String,
    severity: String,
    title: String,
}

/// Evaluate all rules defined on `workspace.metadata.rules` against recent
/// stream events. Inserts a rule signal and deterministic survivor for each
/// matching rule/event pair that has not already produced a signal.
pub async fn evaluate_workspace(
    pool: &PgPool,
    workspace_id: Uuid,
) -> anyhow::Result<RuleEvalReport> {
    let metadata: serde_json::Value =
        sqlx::query_scalar("SELECT metadata FROM workspaces WHERE id = $1")
            .bind(workspace_id)
            .fetch_one(pool)
            .await
            .context("failed to fetch workspace metadata")?;

    let Some(rules_json) = metadata.get("rules").cloned() else {
        return Ok(RuleEvalReport {
            inserted: 0,
            diagnostics: vec![],
        });
    };

    let rules: Vec<Rule> = match serde_json::from_value(rules_json) {
        Ok(rules) => rules,
        Err(err) => {
            warn!(workspace_id = %workspace_id, error = %err, "failed to parse workspace rules");
            return Ok(RuleEvalReport {
                inserted: 0,
                diagnostics: vec![RuleDiagnostic {
                    rule_index: -1,
                    rule_title: "rules".to_string(),
                    stream: String::new(),
                    status: DiagStatus::RulesUnparseable,
                    events_evaluated: 0,
                    match_count: 0,
                    skip_reasons: vec![SkipReason {
                        code: "rules_unparseable".to_string(),
                        detail: err.to_string(),
                        count: 1,
                    }],
                }],
            });
        }
    };

    if rules.is_empty() {
        return Ok(RuleEvalReport {
            inserted: 0,
            diagnostics: vec![],
        });
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
    let mut diagnostics = Vec::with_capacity(rules.len());

    for (rule_index, rule) in rules.iter().enumerate() {
        let mut diagnostic = RuleDiagnostic {
            rule_index: rule_index as i64,
            rule_title: rule.title.clone(),
            stream: rule.stream.clone(),
            status: DiagStatus::Ok,
            events_evaluated: 0,
            match_count: 0,
            skip_reasons: vec![],
        };
        let mut skips = SkipAccumulator::default();

        let expression = match build_operator_tree(&rule.when) {
            Ok(expression) => expression,
            Err(err) => {
                diagnostic.status = DiagStatus::ParseError;
                skips.add("parse_error", err.to_string());
                diagnostic.skip_reasons = skips.into_reasons();
                diagnostics.push(diagnostic);
                continue;
            }
        };

        let mut target: Option<(Uuid, Vec<(String, FieldType)>)> = None;
        'outer: for connector in &connectors {
            let streams = stream_repo
                .list(connector.id)
                .await
                .context("failed to list streams")?;
            for stream in streams {
                if stream.name == rule.stream {
                    target = Some((stream.id, declared_field_types(&connector.config)));
                    break 'outer;
                }
            }
        }

        let Some((stream_id, field_types)) = target else {
            warn!(
                workspace_id = %workspace_id,
                stream = %rule.stream,
                "rule references unknown stream - skipping"
            );
            diagnostic.status = DiagStatus::StreamNotFound;
            skips.add(
                "stream_not_found",
                format!("stream '{}' was not found", rule.stream),
            );
            diagnostic.skip_reasons = skips.into_reasons();
            diagnostics.push(diagnostic);
            continue;
        };

        let events = event_repo
            .list_recent(stream_id, 100)
            .await
            .context("failed to list recent events")?;
        let rule_field_types = field_types
            .iter()
            .filter(|(pointer, _)| rule.when.contains(&pointer_to_context_key(pointer)))
            .cloned()
            .collect::<Vec<_>>();

        if events.is_empty() {
            diagnostic.status = DiagStatus::NoEvents;
            skips.add(
                "no_events",
                format!("stream '{}' has no events", rule.stream),
            );
            diagnostic.skip_reasons = skips.into_reasons();
            diagnostics.push(diagnostic);
            continue;
        }

        let severity = parse_severity(&rule.severity);
        let mut saw_type_mismatch = false;

        for event in &events {
            diagnostic.events_evaluated += 1;
            let evidence = serde_json::json!([event.id.to_string()]);

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

            let mut ctx = HashMapContext::new();
            populate_context(&mut ctx, "payload", "", &event.payload, &rule_field_types);

            let mut skip_event = false;
            for (pointer, field_type) in &rule_field_types {
                match event.payload.pointer(pointer) {
                    Some(value) => match normalize_value(*field_type, value) {
                        Ok(value) => {
                            let _ = ctx.set_value(pointer_to_context_key(pointer), value);
                        }
                        Err(()) => {
                            saw_type_mismatch = true;
                            skip_event = true;
                            skips.add("type_mismatch", pointer.clone());
                        }
                    },
                    None => {
                        skip_event = true;
                        skips.add("field_absent", pointer.clone());
                    }
                }
            }
            if skip_event {
                continue;
            }

            let matched = match expression.eval_boolean_with_context(&ctx) {
                Ok(matched) => matched,
                Err(err) => {
                    let (code, detail) = classify_eval_error(&err);
                    if code == "type_mismatch" {
                        saw_type_mismatch = true;
                    }
                    warn!(
                        workspace_id = %workspace_id,
                        rule_title = %rule.title,
                        expression = %rule.when,
                        error = %err,
                        "rule expression evaluation failed - skipping event"
                    );
                    skips.add(code, detail);
                    continue;
                }
            };

            if matched {
                insert_rule_signal_with_survivor(
                    pool,
                    workspace_id,
                    rule,
                    &evidence,
                    severity.clone(),
                )
                .await?;
                diagnostic.match_count += 1;
                inserted += 1;
            }
        }

        diagnostic.status = if saw_type_mismatch {
            DiagStatus::TypeMismatch
        } else {
            DiagStatus::Ok
        };
        diagnostic.skip_reasons = skips.into_reasons();
        diagnostics.push(diagnostic);
    }

    Ok(RuleEvalReport {
        inserted,
        diagnostics,
    })
}

#[derive(Default)]
struct SkipAccumulator {
    counts: BTreeMap<(String, String), i64>,
}

impl SkipAccumulator {
    fn add(&mut self, code: impl Into<String>, detail: impl Into<String>) {
        *self.counts.entry((code.into(), detail.into())).or_insert(0) += 1;
    }

    fn into_reasons(self) -> Vec<SkipReason> {
        self.counts
            .into_iter()
            .take(MAX_SKIP_REASONS)
            .map(|((code, detail), count)| SkipReason {
                code,
                detail,
                count,
            })
            .collect()
    }
}

fn declared_field_types(config: &serde_json::Value) -> Vec<(String, FieldType)> {
    serde_json::from_value::<BTreeMap<String, FieldType>>(
        config
            .get("field_types")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({})),
    )
    .unwrap_or_default()
    .into_iter()
    .collect()
}

fn normalize_value(declared: FieldType, value: &serde_json::Value) -> Result<EvalValue, ()> {
    match declared {
        FieldType::Number => match value {
            serde_json::Value::Number(n) => n.as_f64().map(EvalValue::Float).ok_or(()),
            serde_json::Value::String(s) => s
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .map(EvalValue::Float)
                .ok_or(()),
            _ => Err(()),
        },
        FieldType::String => Ok(EvalValue::String(match value {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        })),
        FieldType::Boolean => match value {
            serde_json::Value::Bool(b) => Ok(EvalValue::Boolean(*b)),
            serde_json::Value::String(s) if s == "true" || s == "false" => {
                Ok(EvalValue::Boolean(s == "true"))
            }
            _ => Err(()),
        },
    }
}

fn pointer_to_context_key(pointer: &str) -> String {
    if pointer.is_empty() {
        return "payload".to_string();
    }
    let path = pointer
        .trim_start_matches('/')
        .split('/')
        .map(|part| part.replace("~1", "/").replace("~0", "~"))
        .collect::<Vec<_>>()
        .join(".");
    format!("payload.{path}")
}

fn classify_eval_error(err: &EvalexprError) -> (&'static str, String) {
    match err {
        EvalexprError::VariableIdentifierNotFound(name) => ("field_absent", name.clone()),
        EvalexprError::TypeError { .. }
        | EvalexprError::WrongTypeCombination { .. }
        | EvalexprError::ExpectedString { .. }
        | EvalexprError::ExpectedInt { .. }
        | EvalexprError::ExpectedFloat { .. }
        | EvalexprError::ExpectedNumber { .. }
        | EvalexprError::ExpectedNumberOrString { .. }
        | EvalexprError::ExpectedBoolean { .. }
        | EvalexprError::ExpectedTuple { .. }
        | EvalexprError::ExpectedFixedLengthTuple { .. }
        | EvalexprError::ExpectedRangedLengthTuple { .. }
        | EvalexprError::ExpectedEmpty { .. } => ("type_mismatch", err.to_string()),
        _ => ("eval_error", err.to_string()),
    }
}

async fn insert_rule_signal_with_survivor(
    pool: &PgPool,
    workspace_id: Uuid,
    rule: &Rule,
    evidence: &serde_json::Value,
    severity: Severity,
) -> anyhow::Result<()> {
    let approval_required = matches!(severity, Severity::Flagged | Severity::Command);
    let mut tx = pool.begin().await.context("begin rule-signal txn")?;
    let signal_id: Uuid = sqlx::query_scalar(
        "INSERT INTO signals
           (workspace_id, source, title, body, evidence, severity, generator_model, approval_required)
         VALUES ($1, 'rule', $2, $3, $4, $5, NULL, $6)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(&rule.title)
    .bind(format!("Rule matched: {}", rule.when))
    .bind(evidence)
    .bind(severity)
    .bind(approval_required)
    .fetch_one(&mut *tx)
    .await
    .context("insert rule signal")?;

    sqlx::query(
        "INSERT INTO survivors
           (signal_id, critic_model, verdict, rationale, confidence, chain_of_reasoning)
         VALUES ($1, 'rule-engine', 'survive'::critic_verdict, $2, 1.0, '[]'::jsonb)",
    )
    .bind(signal_id)
    .bind(format!("rule matched: {}", rule.when))
    .execute(&mut *tx)
    .await
    .context("insert deterministic survivor")?;

    tx.commit().await.context("commit rule-signal txn")?;
    Ok(())
}

/// Recursively populate an evalexpr `HashMapContext` from a JSON value,
/// using dotted-key paths (e.g. `payload.humidity`).
fn populate_context(
    ctx: &mut HashMapContext,
    prefix: &str,
    pointer: &str,
    value: &serde_json::Value,
    declared: &[(String, FieldType)],
) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let key = format!("{}.{}", prefix, k);
                let child_pointer = format!("{}/{}", pointer, escape_pointer_part(k));
                populate_context(ctx, &key, &child_pointer, v, declared);
            }
        }
        serde_json::Value::Number(n) => {
            if !is_declared_pointer(pointer, declared) {
                if let Some(f) = n.as_f64() {
                    let _ = ctx.set_value(prefix.to_string(), EvalValue::Float(f));
                }
            }
        }
        serde_json::Value::String(s) => {
            if !is_declared_pointer(pointer, declared) {
                let _ = ctx.set_value(prefix.to_string(), EvalValue::String(s.clone()));
            }
        }
        serde_json::Value::Bool(b) => {
            if !is_declared_pointer(pointer, declared) {
                let _ = ctx.set_value(prefix.to_string(), EvalValue::Boolean(*b));
            }
        }
        _ => {}
    }
}

fn is_declared_pointer(pointer: &str, declared: &[(String, FieldType)]) -> bool {
    declared
        .iter()
        .any(|(declared_pointer, _)| declared_pointer == pointer)
}

fn escape_pointer_part(part: &str) -> String {
    part.replace('~', "~0").replace('/', "~1")
}

pub(crate) fn parse_severity(s: &str) -> Severity {
    match s {
        "flagged" => Severity::Flagged,
        "command" => Severity::Command,
        _ => Severity::Routine,
    }
}
