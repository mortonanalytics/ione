use anyhow::Context;
use serde::Deserialize;
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    models::{CriticVerdict, Survivor},
    repos::SurvivorRepo,
    services::ollama::OllamaClient,
    state::AppState,
};

// ── Internal shape of a well-formed critic JSON response ──────────────────────

#[derive(Debug, Deserialize)]
struct CriticOutput {
    verdict: String,
    confidence: serde_json::Value,
    rationale: String,
    #[serde(default)]
    steps: Vec<String>,
}

// ── Public test-hook: parse raw model output ──────────────────────────────────

/// Extract structured critic fields from a raw model response string.
///
/// Uses the same brace-count extractor as `services::generator`.
/// On any parse or validation failure returns:
///   `("defer", 0.0, "<non-empty rationale>", vec![])`
///
/// This function is `pub` so integration tests can call it directly without
/// spinning up Ollama.
pub fn parse_response(raw: &str) -> (String, f32, String, Vec<String>) {
    let failure = |reason: &str| -> (String, f32, String, Vec<String>) {
        (
            "defer".to_string(),
            0.0_f32,
            format!("critic response failed to parse: {}", reason),
            vec![],
        )
    };

    // Extract the first complete {...} JSON object via brace counting.
    let start = match raw.find('{') {
        Some(i) => i,
        None => return failure("no JSON object found"),
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
        None => return failure("unbalanced braces in JSON"),
    };

    let json_str = &substr[..end];

    let output: CriticOutput = match serde_json::from_str(json_str) {
        Ok(o) => o,
        Err(e) => return failure(&format!("JSON deserialize error: {}", e)),
    };

    // Validate verdict variant.
    let verdict = match output.verdict.to_lowercase().as_str() {
        "survive" => "survive".to_string(),
        "reject" => "reject".to_string(),
        "defer" => "defer".to_string(),
        other => return failure(&format!("unknown verdict variant: {}", other)),
    };

    // Parse confidence — accept either a float or a quoted-float-string.
    let confidence: f32 = match &output.confidence {
        serde_json::Value::Number(n) => match n.as_f64() {
            Some(f) => f as f32,
            None => return failure("confidence is not a valid number"),
        },
        serde_json::Value::String(s) => match s.parse::<f32>() {
            Ok(f) => f,
            Err(_) => return failure(&format!("confidence string '{}' is not a float", s)),
        },
        _ => return failure("confidence must be a number"),
    };

    if !(0.0..=1.0).contains(&confidence) {
        return failure(&format!(
            "confidence {} is out of range [0.0, 1.0]",
            confidence
        ));
    }

    (verdict, confidence, output.rationale, output.steps)
}

// ── Internal helper: map string verdict + float + steps → DB types ────────────

fn to_verdict(s: &str) -> CriticVerdict {
    match s {
        "survive" => CriticVerdict::Survive,
        "reject" => CriticVerdict::Reject,
        _ => CriticVerdict::Defer,
    }
}

// ── evaluate_signal: main scheduler entry point ───────────────────────────────

/// Evaluate a single signal through the adversarial critic.
///
/// - Skips if a survivor already exists for this signal (idempotent).
/// - On Ollama network error: inserts a `defer` survivor and returns `Ok(Some(...))`.
/// - Returns `Ok(None)` if the signal already has a survivor.
pub async fn evaluate_signal(
    state: &AppState,
    signal_id: Uuid,
) -> anyhow::Result<Option<Survivor>> {
    let repo = SurvivorRepo::new(state.pool.clone());

    // Idempotency: skip if survivor already exists.
    if repo
        .exists_for_signal(signal_id)
        .await
        .context("failed to check survivor existence")?
    {
        return Ok(None);
    }

    // Fetch signal to build the critic prompt.
    let signal: Option<(String, String, String, Option<String>, serde_json::Value)> =
        sqlx::query_as(
            "SELECT title, body, severity::TEXT, generator_model, evidence
             FROM signals WHERE id = $1",
        )
        .bind(signal_id)
        .fetch_optional(&state.pool)
        .await
        .context("failed to fetch signal")?;

    let (title, body, severity_str, generator_model, evidence) = match signal {
        Some(row) => row,
        None => {
            warn!(signal_id = %signal_id, "evaluate_signal: signal not found");
            return Ok(None);
        }
    };

    // Fetch evidence event payloads.
    let event_ids: Vec<String> = if let Some(arr) = evidence.as_array() {
        arr.iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect()
    } else {
        vec![]
    };

    let evidence_text = if event_ids.is_empty() {
        "(no evidence events referenced)".to_string()
    } else {
        let id_list = event_ids
            .iter()
            .map(|id| format!("'{}'", id))
            .collect::<Vec<_>>()
            .join(",");

        let sql = format!(
            "SELECT payload::TEXT FROM stream_events WHERE id IN ({})",
            id_list
        );
        let payloads: Vec<String> = sqlx::query_scalar(&sql)
            .fetch_all(&state.pool)
            .await
            .unwrap_or_default();

        if payloads.is_empty() {
            "(evidence event ids provided but payloads not found)".to_string()
        } else {
            payloads.join("\n")
        }
    };

    let model_note = generator_model
        .map(|m| format!("\nGenerator model: {}", m))
        .unwrap_or_default();

    let prompt = format!(
        r#"You are an adversarial critic for a situational-awareness system. \
Your task is to stress-test the following proposed insight before it reaches a duty officer.

Signal title: {title}
Signal body: {body}
Severity: {severity}{model_note}

Evidence from sensor stream:
{evidence_text}

Evaluate strictly:
1. Is the claim grounded in the evidence above?
2. Is it physically / logically plausible?
3. Is it strong enough to warrant notifying a duty officer?

Respond with strict JSON only, no preamble or postscript:
{{"verdict": "survive"|"reject"|"defer", "confidence": 0.0-1.0, "rationale": "...", "steps": ["...", "..."]}}"#,
        title = title,
        body = body,
        severity = severity_str,
        model_note = model_note,
        evidence_text = evidence_text,
    );

    let ollama_base_url =
        std::env::var("OLLAMA_BASE_URL").unwrap_or_else(|_| "http://localhost:11434".to_string());
    let model =
        std::env::var("OLLAMA_CRITIC_MODEL").unwrap_or_else(|_| "phi4-reasoning:14b".to_string());

    let ollama = OllamaClient::new(ollama_base_url);

    let start = std::time::Instant::now();
    let raw_response = match ollama.generate(&model, &prompt).await {
        Ok(r) => {
            info!(
                signal_id = %signal_id,
                model = %model,
                elapsed_ms = start.elapsed().as_millis(),
                "critic ollama call complete"
            );
            r
        }
        Err(e) => {
            warn!(signal_id = %signal_id, error = %e, "critic ollama call failed — recording defer");
            // Network error path: record defer so the signal is not retried forever.
            let (_, _, rationale, _) = parse_response("");
            let survivor = repo
                .insert(
                    signal_id,
                    &model,
                    CriticVerdict::Defer,
                    &rationale,
                    0.0,
                    serde_json::json!([]),
                )
                .await
                .context("failed to insert defer survivor on network error")?;
            return Ok(Some(survivor));
        }
    };

    let (verdict_str, confidence, rationale, steps) = parse_response(&raw_response);
    let verdict = to_verdict(&verdict_str);
    let chain =
        serde_json::Value::Array(steps.into_iter().map(serde_json::Value::String).collect());

    let survivor = repo
        .insert(signal_id, &model, verdict, &rationale, confidence, chain)
        .await
        .context("failed to insert survivor")?;

    Ok(Some(survivor))
}

// ── evaluate_signal_with_response: test hook ─────────────────────────────────

/// Test hook: parse `raw_response` and insert a survivor directly, bypassing Ollama.
/// The model string used is `OLLAMA_CRITIC_MODEL` env var (or the default).
pub async fn evaluate_signal_with_response(
    pool: &PgPool,
    signal_id: Uuid,
    raw_response: &str,
) -> anyhow::Result<Survivor> {
    let model =
        std::env::var("OLLAMA_CRITIC_MODEL").unwrap_or_else(|_| "phi4-reasoning:14b".to_string());

    let (verdict_str, confidence, rationale, steps) = parse_response(raw_response);
    let verdict = to_verdict(&verdict_str);
    let chain =
        serde_json::Value::Array(steps.into_iter().map(serde_json::Value::String).collect());

    let repo = SurvivorRepo::new(pool.clone());
    repo.insert(signal_id, &model, verdict, &rationale, confidence, chain)
        .await
        .context("failed to insert survivor via test hook")
}
