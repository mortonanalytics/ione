use axum::{
    extract::{Path, Query, State},
    response::Json,
    Extension,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    auth::{ensure_workspace_in_org, AuthContext},
    error::AppError,
    repos::{SortTarget, StreamEventRepo, TableQuery},
    services::event_layers::table_property_columns,
    state::AppState,
};

const OBSERVED_AT_COL: &str = "_observed_at";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventTableParams {
    #[serde(alias = "stream_id")]
    stream_id: Uuid,
    page: Option<i64>,
    #[serde(alias = "per_page")]
    per_page: Option<i64>,
    #[serde(alias = "sort_by")]
    sort_by: Option<String>,
    #[serde(alias = "sort_dir")]
    sort_dir: Option<String>,
    #[serde(alias = "filter_col")]
    filter_col: Option<String>,
    #[serde(alias = "filter_val")]
    filter_val: Option<String>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableColumn {
    pub name: String,
    pub label: String,
    #[serde(rename = "type")]
    pub column_type: String,
    pub pointer: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventTableResponse {
    pub stream_id: Uuid,
    pub columns: Vec<TableColumn>,
    pub rows: Vec<Value>,
    pub total_count: i64,
    pub page: i64,
    pub per_page: i64,
    pub truncated: bool,
}

pub async fn get_event_table(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
    Query(params): Query<EventTableParams>,
) -> Result<Json<EventTableResponse>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;

    let view_config: Option<Value> = sqlx::query(
        "SELECT s.view_config
         FROM streams s
         JOIN connectors c ON c.id = s.connector_id
         JOIN workspaces w ON w.id = c.workspace_id
         WHERE s.id = $1 AND c.workspace_id = $2 AND w.org_id = $3",
    )
    .bind(params.stream_id)
    .bind(workspace_id)
    .bind(ctx.org_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|err| AppError::Internal(anyhow::Error::new(err)))?
    // view_config is a nullable JSONB column — decode as Option so a stream that
    // exists with a NULL config doesn't panic (Value has no Decode for SQL NULL).
    .and_then(|row| row.get::<Option<Value>, _>("view_config"));

    let view_config =
        view_config.ok_or_else(|| AppError::NotFound("stream not found in workspace".into()))?;
    let property_columns = table_property_columns(&view_config)
        .map_err(|err| AppError::BadRequest(format!("invalid table view_config: {err}")))?;
    if property_columns.is_empty() {
        return Err(AppError::NotFound("table stream not found".into()));
    }

    let page = params.page.unwrap_or(1);
    let per_page = params.per_page.unwrap_or(25);
    if page < 1 {
        return Err(AppError::BadRequest("page must be >= 1".into()));
    }
    if !(1..=200).contains(&per_page) {
        return Err(AppError::BadRequest(
            "per_page must be between 1 and 200".into(),
        ));
    }
    let offset = (page - 1)
        .checked_mul(per_page)
        .ok_or_else(|| AppError::BadRequest("page offset is too large".into()))?;
    if offset > 10_000 {
        return Err(AppError::BadRequest(
            "page offset is too large; narrow with since/until".into(),
        ));
    }

    let until = params.until.unwrap_or_else(Utc::now);
    let since = params.since.unwrap_or(until - Duration::days(30));
    if since > until {
        return Err(AppError::BadRequest("since must be <= until".into()));
    }
    if until - since > Duration::days(90) {
        return Err(AppError::BadRequest("window must be <= 90 days".into()));
    }

    let sort_desc = match params.sort_dir.as_deref().unwrap_or("desc") {
        "desc" => true,
        "asc" => false,
        _ => return Err(AppError::BadRequest("sort_dir must be asc or desc".into())),
    };
    let sort = resolve_target(
        params.sort_by.as_deref().unwrap_or(OBSERVED_AT_COL),
        &property_columns,
    )?;

    let filter = match (params.filter_col.as_deref(), params.filter_val.as_deref()) {
        (Some(_), None) | (None, Some(_)) => {
            return Err(AppError::BadRequest(
                "filter_col and filter_val must be provided together".into(),
            ))
        }
        (Some(col), Some(value)) => {
            let target = resolve_target(col, &property_columns)?;
            if matches!(target, SortTarget::ObservedAt)
                && DateTime::parse_from_rfc3339(value).is_err()
            {
                return Err(AppError::BadRequest(
                    "filter_val must be RFC3339 for _observed_at".into(),
                ));
            }
            Some((target, value.to_string()))
        }
        (None, None) => None,
    };

    let query = TableQuery {
        page,
        per_page,
        sort,
        sort_desc,
        filter,
        since,
        until,
    };
    let repo = StreamEventRepo::new(state.pool.clone());
    let (raw_rows, total_count) = repo
        .fetch_table_rows(workspace_id, ctx.org_id, params.stream_id, &query)
        .await
        .map_err(AppError::Internal)?;

    let columns = response_columns(&property_columns);
    let rows = raw_rows
        .into_iter()
        .map(|(payload, observed_at)| project_row(&property_columns, &payload, observed_at))
        .collect();

    Ok(Json(EventTableResponse {
        stream_id: params.stream_id,
        columns,
        rows,
        total_count,
        page,
        per_page,
        truncated: page * per_page < total_count,
    }))
}

fn resolve_target(name: &str, columns: &[(String, Vec<String>)]) -> Result<SortTarget, AppError> {
    if name == OBSERVED_AT_COL {
        return Ok(SortTarget::ObservedAt);
    }
    columns
        .iter()
        .find(|(column_name, _)| column_name == name)
        .map(|(_, path)| SortTarget::Field(path.clone()))
        .ok_or_else(|| AppError::BadRequest(format!("unknown table column '{name}'")))
}

fn response_columns(columns: &[(String, Vec<String>)]) -> Vec<TableColumn> {
    let mut out = vec![TableColumn {
        name: OBSERVED_AT_COL.to_string(),
        label: "Observed At".to_string(),
        column_type: "datetime".to_string(),
        pointer: None,
    }];
    out.extend(columns.iter().map(|(name, path)| TableColumn {
        name: name.clone(),
        label: name.clone(),
        column_type: "string".to_string(),
        pointer: Some(path_to_pointer(path)),
    }));
    out
}

fn project_row(
    columns: &[(String, Vec<String>)],
    payload: &Value,
    observed_at: DateTime<Utc>,
) -> Value {
    let mut row = Map::new();
    row.insert(OBSERVED_AT_COL.to_string(), json!(observed_at.to_rfc3339()));
    for (name, path) in columns {
        let value = value_at_path(payload, path)
            .and_then(value_to_cell)
            .map(Value::String)
            .unwrap_or(Value::Null);
        row.insert(name.clone(), value);
    }
    Value::Object(row)
}

fn value_at_path<'a>(value: &'a Value, path: &[String]) -> Option<&'a Value> {
    let mut current = value;
    for segment in path {
        match current {
            Value::Object(map) => current = map.get(segment)?,
            Value::Array(items) => {
                let index = segment.parse::<usize>().ok()?;
                current = items.get(index)?;
            }
            _ => return None,
        }
    }
    Some(current)
}

fn value_to_cell(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        other => Some(other.to_string()),
    }
}

fn path_to_pointer(path: &[String]) -> String {
    if path.is_empty() {
        return String::new();
    }
    let mut pointer = String::new();
    for segment in path {
        pointer.push('/');
        pointer.push_str(&segment.replace('~', "~0").replace('/', "~1"));
    }
    pointer
}
