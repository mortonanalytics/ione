use axum::{
    extract::{Extension, Path, RawQuery, State},
    response::Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::{ensure_workspace_in_org, AuthContext},
    error::AppError,
    models::ActorKind,
    repos::{AuditEventFilter, AuditEventRepo},
    state::AppState,
    util::redact::scrub_error_fields,
};

/// Parsed query params shared by the audit list and export endpoints.
/// Parsed by hand (not serde) because `verb` is repeatable and
/// serde_urlencoded cannot collect repeated keys into a Vec.
#[derive(Debug, Default)]
pub struct AuditQueryParams {
    pub filter: AuditEventFilter,
    pub cursor: Option<(DateTime<Utc>, Uuid)>,
    pub limit: Option<i64>,
}

pub fn parse_audit_query(raw: Option<&str>) -> Result<AuditQueryParams, AppError> {
    let mut params = AuditQueryParams::default();
    let Some(raw) = raw else {
        return Ok(params);
    };
    for (key, value) in url::form_urlencoded::parse(raw.as_bytes()) {
        let value = value.into_owned();
        match key.as_ref() {
            "actor_kind" => params.filter.actor_kind = Some(parse_actor_kind(&value)?),
            "actor_ref" => params.filter.actor_ref = Some(value),
            "verb" => params.filter.verbs.push(value),
            "object_kind" => params.filter.object_kind = Some(value),
            "object_id" => {
                params.filter.object_id = Some(
                    Uuid::parse_str(&value)
                        .map_err(|_| AppError::BadRequest("object_id must be a UUID".into()))?,
                )
            }
            "foreign_tenant_id" => params.filter.foreign_tenant_id = Some(value),
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
            _ => {}
        }
    }
    Ok(params)
}

pub fn parse_actor_kind(value: &str) -> Result<ActorKind, AppError> {
    match value {
        "user" => Ok(ActorKind::User),
        "system" => Ok(ActorKind::System),
        "peer" => Ok(ActorKind::Peer),
        _ => Err(AppError::BadRequest(
            "actor_kind must be one of user, system, peer".into(),
        )),
    }
}

fn parse_timestamp(field: &str, value: &str) -> Result<DateTime<Utc>, AppError> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| AppError::BadRequest(format!("{field} must be an RFC 3339 timestamp")))
}

pub fn encode_cursor(created_at: DateTime<Utc>, id: Uuid) -> String {
    URL_SAFE_NO_PAD.encode(format!(
        "{}|{}",
        created_at.to_rfc3339_opts(SecondsFormat::Micros, true),
        id
    ))
}

pub fn decode_cursor(raw: &str) -> Result<(DateTime<Utc>, Uuid), AppError> {
    let invalid = || AppError::BadRequest("invalid cursor".into());
    let bytes = URL_SAFE_NO_PAD.decode(raw).map_err(|_| invalid())?;
    let decoded = String::from_utf8(bytes).map_err(|_| invalid())?;
    let (ts, id) = decoded.split_once('|').ok_or_else(invalid)?;
    let created_at = DateTime::parse_from_rfc3339(ts)
        .map_err(|_| invalid())?
        .with_timezone(&Utc);
    let id = Uuid::parse_str(id).map_err(|_| invalid())?;
    Ok((created_at, id))
}

/// GET /api/v1/workspaces/:id/audit_events
///
/// Filterable, keyset-paged audit list. Backward compatible: with no params,
/// returns the first page of 200 plus a `next_cursor`.
pub async fn list_audit_events(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    let params = parse_audit_query(raw.as_deref())?;
    let limit = params.limit.unwrap_or(200).clamp(1, 200);

    let repo = AuditEventRepo::new(state.pool.clone());
    let mut items = repo
        .list_filtered(workspace_id, ctx.org_id, &params.filter, params.cursor, limit)
        .await
        .map_err(AppError::Internal)?;
    // Read-time backstop: rows written before the write-time scrub existed.
    for item in &mut items {
        scrub_error_fields(&mut item.payload);
    }
    let next_cursor = if items.len() as i64 == limit {
        items.last().map(|e| encode_cursor(e.created_at, e.id))
    } else {
        None
    };
    Ok(Json(json!({ "items": items, "next_cursor": next_cursor })))
}
