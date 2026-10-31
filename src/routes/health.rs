use std::time::Duration;

use axum::{extract::State, http::StatusCode, response::Json};
use serde::Serialize;
use serde_json::{json, Value};

use crate::state::AppState;

pub(crate) async fn health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

/// Readiness: 200 only when Postgres answers `SELECT 1` within 2 s. The pool has
/// no `acquire_timeout`, so without the outer bound a dead DB would hold the
/// probe for sqlx's 30 s default. The failure body names the reason class only,
/// never the URL, host, or driver error text.
pub(crate) async fn health_ready(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let check = tokio::time::timeout(
        Duration::from_secs(2),
        sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&state.pool),
    )
    .await;
    let reason = match check {
        Ok(Ok(_)) => {
            return (
                StatusCode::OK,
                Json(json!({
                    "status": "ready",
                    "version": env!("CARGO_PKG_VERSION")
                })),
            )
        }
        Ok(Err(_)) => "error",
        Err(_) => "timeout",
    };
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "status": "unavailable",
            "checks": { "database": reason }
        })),
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OllamaHealth {
    pub ok: bool,
    pub base_url: String,
    pub models: ModelStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelStatus {
    pub required: Vec<String>,
    pub available: Vec<String>,
    pub missing: Vec<String>,
}

pub(crate) async fn health_ollama(State(state): State<AppState>) -> Json<OllamaHealth> {
    let base_url = state.config.ollama_base_url.clone();
    let required = vec![state.config.ollama_model.clone()];

    match state.ollama.list_models().await {
        Ok(available) => {
            let missing: Vec<String> = required
                .iter()
                .filter(|model| {
                    !available
                        .iter()
                        .any(|available_model| available_model == *model)
                })
                .cloned()
                .collect();

            Json(OllamaHealth {
                ok: missing.is_empty(),
                base_url,
                models: ModelStatus {
                    required,
                    available,
                    missing,
                },
                error: None,
            })
        }
        Err(error) => Json(OllamaHealth {
            ok: false,
            base_url,
            models: ModelStatus {
                required: required.clone(),
                available: vec![],
                missing: required,
            },
            error: Some(error.to_string()),
        }),
    }
}
