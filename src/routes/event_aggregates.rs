use axum::{
    extract::{Path, Query, State},
    response::Json,
    Extension,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    auth::{ensure_workspace_in_org, AuthContext},
    error::AppError,
    repos::StreamEventAggregateRepo,
    state::AppState,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventAggregatesQuery {
    #[serde(alias = "stream_id")]
    stream_id: Uuid,
    op: String,
    bucket: Option<String>,
    #[serde(alias = "value_pointer")]
    value_pointer: Option<String>,
    percentile: Option<f64>,
    #[serde(alias = "group_by_pointer")]
    group_by_pointer: Option<String>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventAggregatesResponse {
    op: String,
    bucket: String,
    rows: Vec<Value>,
    truncated: bool,
}

pub async fn get_event_aggregates(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<EventAggregatesQuery>,
) -> Result<Json<EventAggregatesResponse>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;

    let repo = StreamEventAggregateRepo::new(state.pool.clone());
    if !repo
        .stream_in_workspace_org(workspace_id, ctx.org_id, query.stream_id)
        .await
        .map_err(AppError::Internal)?
    {
        return Err(AppError::NotFound("stream not found".into()));
    }

    let until = query.until.unwrap_or_else(Utc::now);
    let since = query.since.unwrap_or(until - Duration::days(30));
    if since > until {
        return Err(AppError::BadRequest("since must be <= until".into()));
    }
    if until - since > Duration::days(90) {
        return Err(AppError::BadRequest("window must be <= 90 days".into()));
    }

    let bucket = query.bucket.unwrap_or_else(|| "day".to_string());
    let bucket_duration = bucket_duration(&bucket)?;
    if query.op == "baseline" && bucket != "day" {
        return Err(AppError::BadRequest("baseline bucket must be day".into()));
    }
    // group_by is a flat breakdown (no time buckets), so the bucket-count cap does not apply.
    if query.op != "group_by" {
        let bucket_count = ((until - since).num_seconds() as f64
            / bucket_duration.num_seconds() as f64)
            .ceil() as i64;
        if bucket_count > 1000 {
            return Err(AppError::BadRequest(
                "reduce window or increase bucket".into(),
            ));
        }
    }

    let mut truncated = false;
    let rows = match query.op.as_str() {
        "count" => repo
            .count_by_bucket(
                workspace_id,
                ctx.org_id,
                query.stream_id,
                since,
                until,
                &bucket,
            )
            .await
            .map_err(AppError::Internal)?,
        "avg" | "min" | "max" | "sum" => {
            let value_path = required_pointer(query.value_pointer.as_deref(), "value_pointer")?;
            repo.numeric_agg_by_bucket(
                workspace_id,
                ctx.org_id,
                query.stream_id,
                since,
                until,
                &bucket,
                value_path,
            )
            .await
            .map_err(AppError::Internal)?
        }
        "percentile" => {
            let value_path = required_pointer(query.value_pointer.as_deref(), "value_pointer")?;
            let pct = query.percentile.unwrap_or(0.95);
            if pct <= 0.0 || pct > 1.0 {
                return Err(AppError::BadRequest(
                    "percentile must be greater than 0 and <= 1".into(),
                ));
            }
            repo.percentile_by_bucket(
                workspace_id,
                ctx.org_id,
                query.stream_id,
                since,
                until,
                &bucket,
                value_path,
                pct,
            )
            .await
            .map_err(AppError::Internal)?
        }
        "group_by" => {
            let group_path =
                required_pointer(query.group_by_pointer.as_deref(), "group_by_pointer")?;
            let (rows, is_truncated) = repo
                .count_by_group(
                    workspace_id,
                    ctx.org_id,
                    query.stream_id,
                    since,
                    until,
                    group_path,
                )
                .await
                .map_err(AppError::Internal)?;
            truncated = is_truncated;
            rows
        }
        "baseline" => repo
            .rolling_baseline(workspace_id, ctx.org_id, query.stream_id, since, until)
            .await
            .map_err(AppError::Internal)?,
        _ => {
            return Err(AppError::BadRequest(format!(
                "unsupported aggregate op '{}'",
                query.op
            )));
        }
    };

    Ok(Json(EventAggregatesResponse {
        op: query.op,
        bucket,
        rows,
        truncated,
    }))
}

fn bucket_duration(bucket: &str) -> Result<Duration, AppError> {
    match bucket {
        "hour" => Ok(Duration::hours(1)),
        "day" => Ok(Duration::days(1)),
        "week" => Ok(Duration::weeks(1)),
        _ => Err(AppError::BadRequest(
            "bucket must be one of hour, day, week".into(),
        )),
    }
}

fn required_pointer(value: Option<&str>, field: &str) -> Result<Vec<String>, AppError> {
    let raw = value.ok_or_else(|| AppError::BadRequest(format!("{field} is required")))?;
    parse_json_pointer(raw).map_err(|err| AppError::BadRequest(format!("{field}: {err}")))
}

fn parse_json_pointer(raw: &str) -> Result<Vec<String>, &'static str> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    if !raw.starts_with('/') {
        return Err("must be a JSON Pointer");
    }
    raw.split('/')
        .skip(1)
        .map(|part| {
            let mut out = String::new();
            let mut chars = part.chars();
            while let Some(ch) = chars.next() {
                if ch == '~' {
                    match chars.next() {
                        Some('0') => out.push('~'),
                        Some('1') => out.push('/'),
                        _ => return Err("invalid escape"),
                    }
                } else {
                    out.push(ch);
                }
            }
            Ok(out)
        })
        .collect()
}
