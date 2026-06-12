use axum::{
    extract::{Extension, Path, RawQuery, State},
    response::Json,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::{ensure_workspace_in_org, require_permission, AuthContext},
    error::AppError,
    repos::{AuditEventAggregateRepo, GroupCol, PipelineEventAggregateRepo},
    routes::audit_events::parse_audit_query,
    state::AppState,
};

fn query_param(raw: Option<&str>, name: &str) -> Option<String> {
    url::form_urlencoded::parse(raw?.as_bytes())
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
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

fn parse_group_col(value: &str) -> Result<GroupCol, AppError> {
    match value {
        "actor_kind" => Ok(GroupCol::ActorKind),
        "verb" => Ok(GroupCol::Verb),
        "actor_ref" => Ok(GroupCol::ActorRef),
        _ => Err(AppError::BadRequest(
            "group_by must be one of actor_kind, verb, actor_ref".into(),
        )),
    }
}

/// Resolve and validate the since/until window: defaults (until=now,
/// since=until-30d), ordering, and the 90-day cap shared by both endpoints.
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

/// GET /api/v1/workspaces/:id/audit-aggregates
pub async fn get_audit_aggregates(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "audit:read").await?;

    let raw = raw.as_deref();
    let op = query_param(raw, "op").ok_or_else(|| AppError::BadRequest("op is required".into()))?;
    let bucket = query_param(raw, "bucket");
    let group_by = query_param(raw, "group_by");

    let mut params = parse_audit_query(raw)?;
    let (since, until) = resolve_window(params.filter.since, params.filter.until)?;
    params.filter.since = Some(since);
    params.filter.until = Some(until);

    let repo = AuditEventAggregateRepo::new(state.pool.clone());
    match op.as_str() {
        "count_by_bucket" => {
            let bucket = bucket.ok_or_else(|| {
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
            let group_col = parse_group_col(&group_by.ok_or_else(|| {
                AppError::BadRequest("group_by is required for count_by_bucket".into())
            })?)?;
            let groups = repo
                .count_by_bucket(workspace_id, ctx.org_id, &bucket, group_col, &params.filter)
                .await
                .map_err(AppError::Internal)?;
            Ok(Json(json!({
                "op": "count_by_bucket",
                "bucket": bucket,
                "groups": groups,
            })))
        }
        "count_by_actor" => {
            if bucket.is_some() || group_by.is_some() {
                return Err(AppError::BadRequest(
                    "bucket and group_by are not valid for count_by_actor".into(),
                ));
            }
            let groups = repo
                .count_by_actor(workspace_id, ctx.org_id, &params.filter)
                .await
                .map_err(AppError::Internal)?;
            Ok(Json(json!({
                "op": "count_by_actor",
                "groups": groups,
            })))
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported aggregate op '{other}'"
        ))),
    }
}

/// GET /api/v1/workspaces/:id/pipeline-aggregates
pub async fn get_pipeline_aggregates(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "audit:read").await?;

    let raw = raw.as_deref();
    let op = query_param(raw, "op").ok_or_else(|| AppError::BadRequest("op is required".into()))?;
    if op != "recovery_gap" {
        return Err(AppError::BadRequest(format!(
            "unsupported aggregate op '{op}'"
        )));
    }
    let connector_id = match query_param(raw, "connector_id") {
        Some(v) => Some(
            Uuid::parse_str(&v)
                .map_err(|_| AppError::BadRequest("connector_id must be a UUID".into()))?,
        ),
        None => None,
    };
    let params = parse_audit_query(raw)?;
    let (since, until) = resolve_window(params.filter.since, params.filter.until)?;

    let gaps = PipelineEventAggregateRepo::new(state.pool.clone())
        .recovery_gaps(workspace_id, ctx.org_id, connector_id, since, until)
        .await
        .map_err(AppError::Internal)?;

    let mut sorted: Vec<f64> = gaps.iter().map(|g| g.gap_seconds).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("gap_seconds is never NaN"));
    let summary = json!({
        "count": sorted.len(),
        "p50": percentile(&sorted, 0.50),
        "p90": percentile(&sorted, 0.90),
        "max": sorted.last().copied(),
    });
    Ok(Json(json!({
        "op": "recovery_gap",
        "items": gaps,
        "summary": summary,
    })))
}

/// Linear-interpolation percentile (percentile_cont semantics) over a sorted
/// slice. Computed in Rust over the returned gaps (≤10k) — no second query.
fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = p * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    Some(sorted[lo] + (sorted[hi] - sorted[lo]) * (rank - lo as f64))
}
