use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Extension, Path, RawQuery, State},
    http::header,
    response::Response,
};
use chrono::{Duration, Utc};
use futures_util::StreamExt;
use uuid::Uuid;

use crate::{
    auth::{ensure_workspace_in_org, require_permission, AuthContext},
    error::AppError,
    repos::AuditEventRepo,
    routes::audit_events::{encode_cursor, parse_audit_query},
    state::AppState,
    util::redact::scrub_error_fields,
};

/// Hard ceiling of rows per export response (no client-settable limit).
const EXPORT_PAGE_ROWS: usize = 10_000;

/// Per-org single-flight permit. The entry is removed on Drop, so a dropped
/// connection (client gone mid-stream) frees the slot.
struct ExportPermit {
    locks: Arc<dashmap::DashMap<Uuid, ()>>,
    org_id: Uuid,
}

impl ExportPermit {
    fn acquire(locks: Arc<dashmap::DashMap<Uuid, ()>>, org_id: Uuid) -> Result<Self, AppError> {
        let acquired = match locks.entry(org_id) {
            dashmap::mapref::entry::Entry::Occupied(_) => false,
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                vacant.insert(());
                true
            }
        };
        if acquired {
            Ok(Self { locks, org_id })
        } else {
            Err(AppError::TooManyRequests(
                "an export is already running for this org".into(),
            ))
        }
    }
}

impl Drop for ExportPermit {
    fn drop(&mut self) {
        self.locks.remove(&self.org_id);
    }
}

/// GET /api/v1/workspaces/:id/audit-export
///
/// Deterministic two-query NDJSON export: a cheap key query decides both
/// truncation and the `X-Next-Cursor` value before headers are sent; the row
/// stream is bounded by those keys, so both queries see identical data
/// (`audit_events` is append-only and `until` is clamped to now).
pub async fn get_audit_export(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
    RawQuery(raw): RawQuery,
) -> Result<Response, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "audit:read").await?;

    let mut params = parse_audit_query(raw.as_deref())?;
    if params.limit.is_some() {
        return Err(AppError::BadRequest(
            "limit is not supported on audit-export".into(),
        ));
    }
    let since = params
        .filter
        .since
        .ok_or_else(|| AppError::BadRequest("since is required".into()))?;
    let until = params
        .filter
        .until
        .ok_or_else(|| AppError::BadRequest("until is required".into()))?;
    if since > until {
        return Err(AppError::BadRequest("since must be <= until".into()));
    }
    if until - since > Duration::days(90) {
        return Err(AppError::BadRequest("window must be <= 90 days".into()));
    }
    params.filter.until = Some(until.min(Utc::now()));

    let permit = ExportPermit::acquire(state.export_locks.clone(), ctx.org_id)?;

    let repo = AuditEventRepo::new(state.pool.clone());
    let keys = repo
        .keyset_page(
            workspace_id,
            ctx.org_id,
            &params.filter,
            params.cursor,
            EXPORT_PAGE_ROWS as i64 + 1,
        )
        .await
        .map_err(AppError::Internal)?;

    let truncated = keys.len() > EXPORT_PAGE_ROWS;
    let included = if truncated {
        &keys[..EXPORT_PAGE_ROWS]
    } else {
        &keys[..]
    };

    let mut builder = Response::builder().header(header::CONTENT_TYPE, "application/x-ndjson");
    if truncated {
        let last = included[EXPORT_PAGE_ROWS - 1];
        builder = builder.header("x-next-cursor", encode_cursor(last.0, last.1));
    }

    let body = match (included.first(), included.last()) {
        (Some(&first), Some(&last)) => {
            let rows =
                repo.stream_between_keys(workspace_id, ctx.org_id, &params.filter, first, last);
            let lines = rows.map(move |row| {
                let _permit = &permit; // freed when the stream is dropped
                row.map_err(axum::BoxError::from).and_then(|mut event| {
                    scrub_error_fields(&mut event.payload);
                    serde_json::to_string(&event)
                        .map(|mut line| {
                            line.push('\n');
                            line
                        })
                        .map_err(axum::BoxError::from)
                })
            });
            Body::from_stream(lines)
        }
        _ => Body::empty(),
    };

    builder.body(body).map_err(|e| AppError::Internal(e.into()))
}
