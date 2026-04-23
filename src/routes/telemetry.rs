use axum::{
    extract::{Extension, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    auth::AuthContext, error::AppError, middleware::session_cookie::SessionId, state::AppState,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TrackBody {
    pub event_kind: String,
    #[serde(default)]
    pub detail: Option<Value>,
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
}

pub(crate) async fn track_event(
    State(state): State<AppState>,
    Extension(session): Extension<SessionId>,
    auth: Option<Extension<AuthContext>>,
    Json(body): Json<TrackBody>,
) -> Json<Value> {
    let user_id = auth.and_then(|Extension(ctx)| {
        if ctx.is_oidc || ctx.user_id != state.default_user_id {
            Some(ctx.user_id)
        } else {
            // Anonymous / default-user fallback: record as null user for funnel integrity.
            None
        }
    });
    crate::services::funnel::track(
        &state,
        session.0,
        user_id,
        body.workspace_id,
        &body.event_kind,
        body.detail,
    );
    Json(serde_json::json!({"ok": true}))
}

#[derive(Deserialize)]
pub(crate) struct FunnelQuery {
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    pub to: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FunnelResp {
    pub counts: Vec<FunnelCount>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FunnelCount {
    pub event_kind: String,
    pub count: i64,
}

pub(crate) async fn admin_funnel(
    State(state): State<AppState>,
    Query(q): Query<FunnelQuery>,
) -> Result<Response, AppError> {
    if std::env::var("IONE_ADMIN_FUNNEL").as_deref() != Ok("1") {
        return Ok((StatusCode::NOT_FOUND, "").into_response());
    }
    let to = q.to.unwrap_or_else(chrono::Utc::now);
    let from = q.from.unwrap_or(to - chrono::Duration::days(7));
    let repo = crate::repos::FunnelEventRepo::new(state.pool.clone());
    let rows = repo
        .counts_between(from, to)
        .await
        .map_err(AppError::Internal)?;
    let counts = rows
        .into_iter()
        .map(|(event_kind, count)| FunnelCount { event_kind, count })
        .collect();
    Ok(Json(FunnelResp { counts }).into_response())
}
