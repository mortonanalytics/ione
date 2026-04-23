use axum::{
    extract::{Extension, Path, State},
    response::Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::AuthContext,
    error::AppError,
    middleware::session_cookie::SessionId,
    models::{ActivationStepKey, ActivationTrack, Message, MessageRole},
    repos::{ConversationRepo, MessageRepo, WorkspaceRepo},
    state::AppState,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateConversationRequest {
    pub title: Option<String>,
    pub workspace_id: Option<Uuid>,
}

pub async fn list_conversations(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let repo = ConversationRepo::new(state.pool.clone());
    let items = repo
        .list(state.default_user_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}

pub async fn create_conversation(
    State(state): State<AppState>,
    Json(req): Json<CreateConversationRequest>,
) -> Result<Json<Value>, AppError> {
    // Default to the Operations workspace when none is supplied.
    let workspace_id = match req.workspace_id {
        Some(id) => {
            // Validate the workspace exists; return 400 if not.
            let ws_repo = WorkspaceRepo::new(state.pool.clone());
            ws_repo
                .get(id)
                .await
                .map_err(AppError::Internal)?
                .ok_or_else(|| AppError::BadRequest(format!("workspace {} does not exist", id)))?;
            id
        }
        None => state.default_workspace_id,
    };

    let title = req.title.as_deref().unwrap_or("Untitled").to_string();
    let repo = ConversationRepo::new(state.pool.clone());
    let conv = repo
        .create(state.default_user_id, &title, Some(workspace_id))
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(
        serde_json::to_value(conv).map_err(|e| AppError::Internal(e.into()))?,
    ))
}

pub async fn get_conversation(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let conv_repo = ConversationRepo::new(state.pool.clone());
    let msg_repo = MessageRepo::new(state.pool.clone());

    let conv = conv_repo
        .get(id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::BadRequest(format!("conversation {} not found", id)))?;

    let messages = msg_repo.list(id).await.map_err(AppError::Internal)?;

    Ok(Json(json!({
        "conversation": conv,
        "messages": messages,
    })))
}

#[derive(Deserialize, Serialize)]
pub struct PostMessageRequest {
    pub content: String,
    pub model: Option<String>,
}

pub(crate) async fn post_message(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Extension(ctx): Extension<AuthContext>,
    Extension(session): Extension<SessionId>,
    Json(req): Json<PostMessageRequest>,
) -> Result<Json<Message>, AppError> {
    let content = req.content.trim().to_string();
    if content.is_empty() {
        return Err(AppError::BadRequest(
            "content must not be empty".to_string(),
        ));
    }

    let conv_repo = ConversationRepo::new(state.pool.clone());
    let conv = conv_repo
        .get(id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::BadRequest(format!("conversation {} not found", id)))?;

    let msg_repo = MessageRepo::new(state.pool.clone());

    msg_repo
        .append(id, MessageRole::User, &content, None)
        .await
        .map_err(AppError::Internal)?;

    let history = msg_repo.list(id).await.map_err(AppError::Internal)?;

    if conv.workspace_id == crate::demo::DEMO_WORKSPACE_ID {
        let reply = crate::demo::canned_chat::canned_response(&content);
        let assistant_msg = msg_repo
            .append(id, MessageRole::Assistant, reply, Some("canned"))
            .await
            .map_err(AppError::Internal)?;
        let activation_repo = crate::repos::ActivationRepo::new(state.pool.clone());
        let _ = activation_repo
            .mark(
                state.default_user_id,
                conv.workspace_id,
                ActivationTrack::DemoWalkthrough,
                ActivationStepKey::AskedDemoQuestion,
            )
            .await;
        return Ok(Json(assistant_msg));
    }

    let model = req
        .model
        .as_deref()
        .unwrap_or(&state.config.ollama_model)
        .to_string();
    let prompt = build_prompt(&history, &model);

    let reply = match state.ollama.generate(&model, &prompt).await {
        Ok(reply) => reply,
        Err(AppError::OllamaUnreachable { base_url, error }) => {
            crate::services::funnel::track(
                &state,
                session.0,
                Some(ctx.user_id),
                Some(conv.workspace_id),
                "ollama_unreachable_seen",
                Some(json!({ "baseUrl": base_url })),
            );
            return Err(AppError::OllamaUnreachable { base_url, error });
        }
        Err(err) => return Err(err),
    };

    let assistant_msg = msg_repo
        .append(id, MessageRole::Assistant, &reply, Some(&model))
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(assistant_msg))
}

fn build_prompt(history: &[Message], _model: &str) -> String {
    let mut parts = Vec::with_capacity(history.len() + 1);
    for msg in history {
        let role = match msg.role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::System => "system",
        };
        parts.push(format!("\n{}: {}", role, msg.content));
    }
    parts.push("\nassistant:".to_string());
    parts.join("\n")
}
