use axum::{
    extract::{Extension, Query, State},
    response::Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    auth::AuthContext,
    error::AppError,
    models::{ActivationStepKey, ActivationTrack},
    repos::ActivationRepo,
    state::AppState,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListQuery {
    pub workspace_id: Uuid,
    pub track: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListItem {
    pub step_key: String,
    pub label: String,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListResp {
    pub track: String,
    pub items: Vec<ListItem>,
    pub dismissed: bool,
}

pub(crate) async fn list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Query(q): Query<ListQuery>,
) -> Result<Json<ListResp>, AppError> {
    let track = parse_track(&q.track)?;
    let repo = ActivationRepo::new(state.pool.clone());
    let progress = repo
        .list(ctx.user_id, q.workspace_id, track)
        .await
        .map_err(AppError::Internal)?;
    let dismissed = repo
        .is_dismissed(ctx.user_id, q.workspace_id, track)
        .await
        .map_err(AppError::Internal)?;

    let expected_steps: Vec<(ActivationStepKey, &str)> = match track {
        ActivationTrack::DemoWalkthrough => vec![
            (
                ActivationStepKey::AskedDemoQuestion,
                "Ask your workspace a question",
            ),
            (ActivationStepKey::OpenedDemoSurvivor, "Open a survivor"),
            (
                ActivationStepKey::ReviewedDemoApproval,
                "Review an approval",
            ),
            (ActivationStepKey::ViewedDemoAudit, "View an audit trail"),
        ],
        ActivationTrack::RealActivation => vec![
            (
                ActivationStepKey::AddedConnector,
                "Add your first connector",
            ),
            (ActivationStepKey::FirstSignal, "See your first signal"),
            (
                ActivationStepKey::FirstApprovalDecided,
                "Decide one approval",
            ),
            (ActivationStepKey::FirstAuditViewed, "View one audit trail"),
        ],
    };

    let items = expected_steps
        .into_iter()
        .map(|(step, label)| {
            let completed_at = progress
                .iter()
                .find(|p| p.step_key == step)
                .map(|p| p.completed_at);
            ListItem {
                step_key: step_key_to_str(step).to_string(),
                label: label.to_string(),
                completed_at,
            }
        })
        .collect();

    Ok(Json(ListResp {
        track: q.track,
        items,
        dismissed,
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MarkBody {
    pub track: String,
    pub step_key: String,
    pub workspace_id: Uuid,
}

pub(crate) async fn mark(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(body): Json<MarkBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let track = parse_track(&body.track)?;
    let step = parse_step(&body.step_key)?;
    let repo = ActivationRepo::new(state.pool.clone());
    repo.mark(ctx.user_id, body.workspace_id, track, step)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DismissBody {
    pub workspace_id: Uuid,
    pub track: String,
}

pub(crate) async fn dismiss(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(body): Json<DismissBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let track = parse_track(&body.track)?;
    let repo = ActivationRepo::new(state.pool.clone());
    repo.dismiss(ctx.user_id, body.workspace_id, track)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

fn parse_track(s: &str) -> Result<ActivationTrack, AppError> {
    match s {
        "demo_walkthrough" => Ok(ActivationTrack::DemoWalkthrough),
        "real_activation" => Ok(ActivationTrack::RealActivation),
        _ => Err(AppError::BadRequest(format!(
            "unknown activation track: {s}"
        ))),
    }
}

fn parse_step(s: &str) -> Result<ActivationStepKey, AppError> {
    match s {
        "asked_demo_question" => Ok(ActivationStepKey::AskedDemoQuestion),
        "opened_demo_survivor" => Ok(ActivationStepKey::OpenedDemoSurvivor),
        "reviewed_demo_approval" => Ok(ActivationStepKey::ReviewedDemoApproval),
        "viewed_demo_audit" => Ok(ActivationStepKey::ViewedDemoAudit),
        "added_connector" => Ok(ActivationStepKey::AddedConnector),
        "first_signal" => Ok(ActivationStepKey::FirstSignal),
        "first_approval_decided" => Ok(ActivationStepKey::FirstApprovalDecided),
        "first_audit_viewed" => Ok(ActivationStepKey::FirstAuditViewed),
        _ => Err(AppError::BadRequest(format!(
            "unknown activation step: {s}"
        ))),
    }
}

fn step_key_to_str(s: ActivationStepKey) -> &'static str {
    match s {
        ActivationStepKey::AskedDemoQuestion => "asked_demo_question",
        ActivationStepKey::OpenedDemoSurvivor => "opened_demo_survivor",
        ActivationStepKey::ReviewedDemoApproval => "reviewed_demo_approval",
        ActivationStepKey::ViewedDemoAudit => "viewed_demo_audit",
        ActivationStepKey::AddedConnector => "added_connector",
        ActivationStepKey::FirstSignal => "first_signal",
        ActivationStepKey::FirstApprovalDecided => "first_approval_decided",
        ActivationStepKey::FirstAuditViewed => "first_audit_viewed",
    }
}
