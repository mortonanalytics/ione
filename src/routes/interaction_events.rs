use axum::{
    extract::{Extension, Path, RawQuery, State},
    response::Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::{ensure_workspace_in_org, require_permission, AuthContext},
    error::AppError,
    models::outcome,
    repos::{InteractionEventFilter, InteractionEventRepo},
    state::AppState,
};

#[derive(Debug, Default)]
struct InteractionQueryParams {
    filter: InteractionEventFilter,
    cursor: Option<(DateTime<Utc>, Uuid)>,
    limit: Option<i64>,
    op: Option<String>,
    bucket: Option<String>,
}

fn parse_query(raw: Option<&str>) -> Result<InteractionQueryParams, AppError> {
    let mut params = InteractionQueryParams::default();
    let Some(raw) = raw else {
        return Ok(params);
    };
    for (key, value) in url::form_urlencoded::parse(raw.as_bytes()) {
        let value = value.into_owned();
        match key.as_ref() {
            "peer_id" => params.filter.peer_id = Some(parse_uuid("peer_id", &value)?),
            "caller_user_id" => {
                params.filter.caller_user_id = Some(parse_uuid("caller_user_id", &value)?)
            }
            "caller_peer_id" => {
                params.filter.caller_peer_id = Some(parse_uuid("caller_peer_id", &value)?)
            }
            "caller_token_id" => {
                params.filter.caller_token_id = Some(parse_uuid("caller_token_id", &value)?)
            }
            "outcome" => {
                if !outcome::is_valid(&value) {
                    return Err(AppError::BadRequest(
                        "outcome must be one of allow, deny, pending, error".into(),
                    ));
                }
                params.filter.outcome = Some(value);
            }
            "session_id" => params.filter.session_id = Some(parse_uuid("session_id", &value)?),
            "since" => params.filter.since = Some(parse_timestamp("since", &value)?),
            "until" => params.filter.until = Some(parse_timestamp("until", &value)?),
            "cursor" => params.cursor = Some(decode_cursor(&value)?),
            "limit" => {
                params.limit = Some(
                    value
                        .parse::<i64>()
                        .map_err(|_| AppError::BadRequest("limit must be an integer".into()))?,
                )
            }
            "op" => params.op = Some(value),
            "bucket" => params.bucket = Some(value),
            _ => {}
        }
    }
    Ok(params)
}

fn parse_uuid(field: &str, value: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(value).map_err(|_| AppError::BadRequest(format!("{field} must be a UUID")))
}

fn parse_timestamp(field: &str, value: &str) -> Result<DateTime<Utc>, AppError> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| AppError::BadRequest(format!("{field} must be an RFC 3339 timestamp")))
}

fn encode_cursor(recorded_at: DateTime<Utc>, id: Uuid) -> String {
    URL_SAFE_NO_PAD.encode(format!(
        "{}|{}",
        recorded_at.to_rfc3339_opts(SecondsFormat::Micros, true),
        id
    ))
}

fn decode_cursor(raw: &str) -> Result<(DateTime<Utc>, Uuid), AppError> {
    let invalid = || AppError::BadRequest("invalid cursor".into());
    let bytes = URL_SAFE_NO_PAD.decode(raw).map_err(|_| invalid())?;
    let decoded = String::from_utf8(bytes).map_err(|_| invalid())?;
    let (ts, id) = decoded.split_once('|').ok_or_else(invalid)?;
    let recorded_at = DateTime::parse_from_rfc3339(ts)
        .map_err(|_| invalid())?
        .with_timezone(&Utc);
    let id = Uuid::parse_str(id).map_err(|_| invalid())?;
    Ok((recorded_at, id))
}

fn resolve_window(
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> Result<(DateTime<Utc>, DateTime<Utc>), AppError> {
    let until = until.unwrap_or_else(Utc::now);
    let since = since.unwrap_or(until - Duration::days(30));
    if since > until {
        return Err(AppError::BadRequest("since must be <= until".into()));
    }
    if until - since > Duration::days(90) {
        return Err(AppError::BadRequest("window must be <= 90 days".into()));
    }
    Ok((since, until))
}

fn bucket_duration(bucket: &str) -> Result<Duration, AppError> {
    match bucket {
        "minute" => Ok(Duration::minutes(1)),
        "hour" => Ok(Duration::hours(1)),
        "day" => Ok(Duration::days(1)),
        "week" => Ok(Duration::weeks(1)),
        _ => Err(AppError::BadRequest(
            "bucket must be one of minute, hour, day, week".into(),
        )),
    }
}

async fn authorize_read(
    state: &AppState,
    ctx: &AuthContext,
    workspace_id: Uuid,
) -> Result<(), AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    require_permission(ctx, &state.pool, workspace_id, "audit:read").await
}

/// GET /api/v1/workspaces/:id/interaction-events
pub async fn list_interaction_events(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, AppError> {
    authorize_read(&state, &ctx, workspace_id).await?;
    let params = parse_query(raw.as_deref())?;
    let limit = params.limit.unwrap_or(200).clamp(1, 200);
    let repo = InteractionEventRepo::new(state.pool.clone());
    let items = repo
        .list_filtered(
            workspace_id,
            ctx.org_id,
            &params.filter,
            params.cursor,
            limit,
        )
        .await
        .map_err(AppError::Internal)?;
    let next_cursor = if items.len() as i64 == limit {
        items.last().map(|e| encode_cursor(e.recorded_at, e.id))
    } else {
        None
    };
    Ok(Json(json!({ "items": items, "next_cursor": next_cursor })))
}

/// GET /api/v1/workspaces/:id/interaction-aggregates
pub async fn get_interaction_aggregates(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, AppError> {
    authorize_read(&state, &ctx, workspace_id).await?;
    let mut params = parse_query(raw.as_deref())?;
    let op = params
        .op
        .clone()
        .ok_or_else(|| AppError::BadRequest("op is required".into()))?;
    let (since, until) = resolve_window(params.filter.since, params.filter.until)?;
    params.filter.since = Some(since);
    params.filter.until = Some(until);

    let repo = InteractionEventRepo::new(state.pool.clone());
    match op.as_str() {
        "outcome_summary" => {
            if params.bucket.is_some() {
                return Err(AppError::BadRequest(
                    "bucket is not valid for outcome_summary".into(),
                ));
            }
            let outcomes = repo
                .outcome_summary(workspace_id, ctx.org_id, &params.filter)
                .await
                .map_err(AppError::Internal)?;
            Ok(Json(json!({ "op": op, "outcomes": outcomes })))
        }
        "count_by_bucket" => {
            let bucket = params.bucket.clone().ok_or_else(|| {
                AppError::BadRequest("bucket is required for count_by_bucket".into())
            })?;
            let bucket_count = ((until - since).num_seconds() as f64
                / bucket_duration(&bucket)?.num_seconds() as f64)
                .ceil() as i64;
            if bucket_count > 1000 {
                return Err(AppError::BadRequest(
                    "reduce window or increase bucket".into(),
                ));
            }
            let groups = repo
                .count_by_bucket(workspace_id, ctx.org_id, &bucket, &params.filter)
                .await
                .map_err(AppError::Internal)?;
            Ok(Json(
                json!({ "op": op, "bucket": bucket, "groups": groups }),
            ))
        }
        "count_by_principal" => {
            if params.bucket.is_some() {
                return Err(AppError::BadRequest(
                    "bucket is not valid for count_by_principal".into(),
                ));
            }
            let groups = repo
                .count_by_principal(workspace_id, ctx.org_id, &params.filter)
                .await
                .map_err(AppError::Internal)?;
            Ok(Json(json!({ "op": op, "groups": groups })))
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported aggregate op '{other}'"
        ))),
    }
}

/// GET /api/v1/workspaces/:id/interaction-sessions/:session_id
pub async fn get_interaction_session(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, session_id)): Path<(Uuid, Uuid)>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, AppError> {
    authorize_read(&state, &ctx, workspace_id).await?;
    let params = parse_query(raw.as_deref())?;
    let limit = params.limit.unwrap_or(1000).clamp(1, 1000);
    let repo = InteractionEventRepo::new(state.pool.clone());
    let items = repo
        .list_session_steps(workspace_id, ctx.org_id, session_id, limit)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(json!({ "session_id": session_id, "items": items })))
}
