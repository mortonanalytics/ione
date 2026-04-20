use axum::{extract::State, response::Json};
use serde_json::{json, Value};

use crate::{
    auth::AuthContext,
    error::AppError,
    repos::{MembershipRepo, UserRepo},
    state::AppState,
};

/// GET /api/v1/me
///
/// Returns `{ user: User, memberships: Membership[], activeRoleId: Uuid|null }`.
/// Derives the user from the `AuthContext` injected by the auth middleware.
pub async fn me(
    State(state): State<AppState>,
    axum::Extension(auth): axum::Extension<AuthContext>,
) -> Result<Json<Value>, AppError> {
    let user_repo = UserRepo::new(state.pool.clone());
    let user = user_repo
        .get(auth.user_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("auth user not found")))?;

    let membership_repo = MembershipRepo::new(state.pool.clone());
    let memberships = membership_repo
        .list_for_user(auth.user_id)
        .await
        .map_err(AppError::Internal)?;

    let active_role_id = memberships.first().map(|m| m.role_id);

    Ok(Json(json!({
        "user": user,
        "memberships": memberships,
        "activeRoleId": active_role_id,
    })))
}
