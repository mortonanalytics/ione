use anyhow::Context;
use serde::Deserialize;
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    models::{Severity, SignalSource},
    repos::{ConnectorRepo, SignalRepo, StreamEventRepo, StreamRepo},
    services::ollama::OllamaClient,
};

#[derive(Debug, Deserialize)]
struct GeneratorOutput {
    title: String,
    body: String,
    severity: String,
    evidence_event_ids: Vec<String>,
}

/// Run the generator LLM pass for a single workspace.
/// Picks the first active connector's first stream, fetches the last 10 events,
/// builds a prompt, calls Ollama, and inserts a signal on success.
/// Returns the number of signals inserted (0 or 1).
pub async fn run_for_workspace(pool: &PgPool, workspace_id: Uuid) -> anyhow::Result<usize> {
    let connector_repo = ConnectorRepo::new(pool.clone());
    let stream_repo = StreamRepo::new(pool.clone());
    let event_repo = StreamEventRepo::new(pool.clone());
    let signal_repo = SignalRepo::new(pool.clone());

    // Pick the first active connector
    let connectors = connector_repo
        .list(workspace_id)
        .await
        .context("failed to list connectors")?;

    let active_connector = connectors
        .iter()
        .find(|c| c.status == crate::models::ConnectorStatus::Active);

    let connector = match active_connector {
        Some(c) => c,
        None => return Ok(0),
    };

    // Pick the first stream
    let streams = stream_repo
        .list(connector.id)
        .await
        .context("failed to list streams")?;

    let stream = match streams.first() {
        Some(s) => s,
        None => return Ok(0),
    };

    // Fetch last 10 events
    let events = event_repo
        .list_recent(stream.id, 10)
        .await
        .context("failed to list recent events")?;

    if events.is_empty() {
        return Ok(0);
    }

    // Build observations array for prompt
    let observations: Vec<serde_json::Value> = events
        .iter()
        .map(|e| {
            serde_json::json!({
                "id": e.id.to_string(),
                "observed_at": e.observed_at.to_rfc3339(),
                "payload": e.payload
            })
        })
        .collect();

    let observations_json =
        serde_json::to_string(&observations).context("failed to serialize observations")?;

    let prompt = format!(
        r#"You are a situational-awareness generator. Given the following recent observations from stream "{stream_name}", identify ONE notable development that a duty officer should see. Respond with strict JSON, no preamble:
{{"title": "...", "body": "...", "severity": "routine"|"flagged"|"command", "evidence_event_ids": ["<uuid>", ...]}}
Observations:
{observations}"#,
        stream_name = stream.name,
        observations = observations_json
    );

    // Build Ollama client from env
    let ollama_base_url =
        std::env::var("OLLAMA_BASE_URL").unwrap_or_else(|_| "http://localhost:11434".to_string());
    let model = std::env::var("OLLAMA_GENERATOR_MODEL").unwrap_or_else(|_| "qwen3:14b".to_string());

    let ollama = OllamaClient::new(ollama_base_url);

    let start = std::time::Instant::now();
    let raw_response = match ollama.generate(&model, &prompt).await {
        Ok(r) => r,
        Err(e) => {
            warn!(workspace_id = %workspace_id, error = %e, "ollama generate failed");
            return Ok(0);
        }
    };
    info!(
        workspace_id = %workspace_id,
        model = %model,
        elapsed_ms = start.elapsed().as_millis(),
        "generator ollama call complete"
    );

    // Parse structured output: find the first {...} JSON object in the response
    let parsed = parse_generator_output(&raw_response);

    let output = match parsed {
        Some(o) => o,
        None => {
            warn!(
                workspace_id = %workspace_id,
                raw = %raw_response,
                "generator response could not be parsed as JSON — skipping"
            );
            return Ok(0);
        }
    };

    let severity = match output.severity.as_str() {
        "flagged" => Severity::Flagged,
        "command" => Severity::Command,
        _ => Severity::Routine,
    };

    let evidence = serde_json::Value::Array(
        output
            .evidence_event_ids
            .iter()
            .map(|id| serde_json::Value::String(id.clone()))
            .collect(),
    );

    signal_repo
        .insert(
            workspace_id,
            SignalSource::Generator,
            &output.title,
            &output.body,
            evidence,
            severity,
            Some(model.as_str()),
        )
        .await
        .context("failed to insert generator signal")?;

    Ok(1)
}

/// Find the first complete `{...}` JSON object in the response string and
/// attempt to deserialize it as `GeneratorOutput`.  Returns `None` on failure.
fn parse_generator_output(raw: &str) -> Option<GeneratorOutput> {
    // Find the first '{' and attempt to find a matching '}'
    let start = raw.find('{')?;
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

    let end = end_idx?;
    let json_str = &substr[..end];

    serde_json::from_str(json_str).ok()
}
