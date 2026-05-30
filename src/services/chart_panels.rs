use std::{collections::HashSet, time::Duration};

use anyhow::Context;
use futures_util::future::join_all;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{models::Peer, state::AppState};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartSpec {
    pub chart_type: String,
    pub x_axis: String,
    pub y_axis: String,
    pub series: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregateDescriptor {
    pub stream_id: Uuid,
    pub op: String,
    pub bucket: String,
    pub value_pointer: Option<String>,
    pub group_by_pointer: Option<String>,
    pub percentile: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartPanelItem {
    pub id: String,
    pub name: String,
    pub source: String,
    pub spec: ChartSpec,
    pub descriptor: Option<AggregateDescriptor>,
    pub peer_id: Option<Uuid>,
    pub peer_name: Option<String>,
    pub uri: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerFetchError {
    pub peer_id: Uuid,
    pub peer_name: String,
    pub error: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartPanelsResponse {
    pub ione_charts: Vec<ChartPanelItem>,
    pub peer_charts: Vec<ChartPanelItem>,
    pub peer_errors: Vec<PeerFetchError>,
}

pub async fn fetch_chart_panels(
    pool: &PgPool,
    state: &AppState,
    workspace_id: Uuid,
    org_id: Uuid,
    peers: Vec<Peer>,
) -> anyhow::Result<ChartPanelsResponse> {
    let ione_charts = fetch_ione_charts(pool, workspace_id, org_id).await?;
    let (peer_charts, peer_errors) = fetch_peer_charts(state, peers).await;

    Ok(ChartPanelsResponse {
        ione_charts,
        peer_charts,
        peer_errors,
    })
}

async fn fetch_ione_charts(
    pool: &PgPool,
    workspace_id: Uuid,
    org_id: Uuid,
) -> anyhow::Result<Vec<ChartPanelItem>> {
    let rows = sqlx::query(
        "SELECT s.id AS stream_id, s.name AS stream_name, s.view_config
         FROM streams s
         JOIN connectors c ON c.id = s.connector_id
         JOIN workspaces w ON w.id = c.workspace_id
         WHERE c.workspace_id = $1
           AND w.org_id = $2
           AND s.view_config IS NOT NULL
         ORDER BY s.name",
    )
    .bind(workspace_id)
    .bind(org_id)
    .fetch_all(pool)
    .await
    .context("failed to fetch chart stream catalog")?;

    let mut charts = Vec::new();
    for row in rows {
        let stream_id: Uuid = row.get("stream_id");
        let stream_name: String = row.get("stream_name");
        let view_config: Value = row.get("view_config");

        charts.push(ChartPanelItem {
            id: format!("ione:{stream_id}:count"),
            name: format!("{stream_name} frequency"),
            source: "ione".to_string(),
            spec: ChartSpec {
                chart_type: "line".to_string(),
                x_axis: "bucket_start".to_string(),
                y_axis: "Events".to_string(),
                series: vec!["value".to_string()],
            },
            descriptor: Some(AggregateDescriptor {
                stream_id,
                op: "count".to_string(),
                bucket: "day".to_string(),
                value_pointer: None,
                group_by_pointer: None,
                percentile: None,
            }),
            peer_id: None,
            peer_name: None,
            uri: None,
        });

        if let Some(fields) = view_config.get("property_fields").and_then(Value::as_array) {
            for field in fields {
                let Some(pointer) = field.get("pointer").and_then(Value::as_str) else {
                    continue;
                };
                let label = field
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("value")
                    .to_string();
                if !field_has_numeric_values(pool, stream_id, pointer).await? {
                    continue;
                }

                charts.push(numeric_chart(
                    stream_id,
                    &stream_name,
                    &label,
                    pointer,
                    "avg",
                    vec!["avg", "max"],
                    None,
                ));
                charts.push(numeric_chart(
                    stream_id,
                    &stream_name,
                    &label,
                    pointer,
                    "percentile",
                    vec!["percentileValue"],
                    Some(0.95),
                ));
            }
        }
    }

    Ok(charts)
}

async fn field_has_numeric_values(
    pool: &PgPool,
    stream_id: Uuid,
    pointer: &str,
) -> anyhow::Result<bool> {
    let path = parse_json_pointer(pointer)?;
    sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1
            FROM stream_events
            WHERE stream_id = $1
              AND jsonb_typeof(payload #> $2) = 'number'
            LIMIT 1
         )",
    )
    .bind(stream_id)
    .bind(path)
    .fetch_one(pool)
    .await
    .context("failed to check numeric chart field")
}

fn numeric_chart(
    stream_id: Uuid,
    stream_name: &str,
    label: &str,
    pointer: &str,
    op: &str,
    series: Vec<&str>,
    percentile: Option<f64>,
) -> ChartPanelItem {
    let series: Vec<String> = series.into_iter().map(str::to_string).collect();
    ChartPanelItem {
        id: format!("ione:{stream_id}:{op}:{label}"),
        name: match op {
            "percentile" => format!("{stream_name} {label} p95"),
            _ => format!("{stream_name} {label} trend"),
        },
        source: "ione".to_string(),
        spec: ChartSpec {
            chart_type: "line".to_string(),
            x_axis: "bucket_start".to_string(),
            y_axis: label.to_string(),
            series,
        },
        descriptor: Some(AggregateDescriptor {
            stream_id,
            op: op.to_string(),
            bucket: "day".to_string(),
            value_pointer: Some(pointer.to_string()),
            group_by_pointer: None,
            percentile,
        }),
        peer_id: None,
        peer_name: None,
        uri: None,
    }
}

async fn fetch_peer_charts(
    state: &AppState,
    peers: Vec<Peer>,
) -> (Vec<ChartPanelItem>, Vec<PeerFetchError>) {
    let outcomes = join_all(
        peers
            .into_iter()
            .map(|peer| fetch_charts_from_peer(state.clone(), peer)),
    )
    .await;
    let mut charts = Vec::new();
    let mut errors = Vec::new();
    let mut seen = HashSet::new();

    for outcome in outcomes {
        match outcome {
            Ok(items) => {
                for item in items {
                    if let (Some(peer_id), Some(uri)) = (item.peer_id, item.uri.clone()) {
                        if seen.insert((peer_id, uri)) {
                            charts.push(item);
                        }
                    }
                }
            }
            Err(err) => errors.push(err),
        }
    }

    (charts, errors)
}

async fn fetch_charts_from_peer(
    state: AppState,
    peer: Peer,
) -> Result<Vec<ChartPanelItem>, PeerFetchError> {
    let endpoint = peer.mcp_url.trim_end_matches('/').to_string();
    let resources = match tokio::time::timeout(
        Duration::from_secs(5),
        call_resources_list(&state, &peer, &endpoint),
    )
    .await
    {
        Err(_) => {
            return Err(PeerFetchError {
                peer_id: peer.id,
                peer_name: peer.name,
                error: "timeout".to_string(),
            })
        }
        Ok(Err(err)) => {
            return Err(PeerFetchError {
                peer_id: peer.id,
                peer_name: peer.name,
                error: err.to_string(),
            })
        }
        Ok(Ok(resources)) => resources,
    };

    Ok(resources
        .into_iter()
        .filter_map(|resource| extract_chart_panel(&peer, resource))
        .collect())
}

async fn call_resources_list(
    state: &AppState,
    peer: &Peer,
    endpoint: &str,
) -> anyhow::Result<Vec<Value>> {
    let resp: Value = crate::services::peer_tokens::send_mcp_request(
        &state.pool,
        &state.http,
        peer,
        endpoint,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "resources/list",
            "params": null
        }),
    )
    .await?
    .error_for_status()
    .context("peer returned error status")?
    .json()
    .await
    .context("failed to parse peer response")?;

    if let Some(err) = resp.get("error").filter(|v| !v.is_null()) {
        anyhow::bail!("peer MCP error: {err}");
    }

    Ok(resp["result"]["resources"]
        .as_array()
        .cloned()
        .unwrap_or_default())
}

fn extract_chart_panel(peer: &Peer, resource: Value) -> Option<ChartPanelItem> {
    let meta = resource.get("metadata")?;
    if meta.get("ione_view")?.as_str()? != "chart" {
        return None;
    }
    let uri = resource.get("uri")?.as_str()?.to_string();
    if uri.is_empty() {
        return None;
    }

    let spec_value = meta
        .get("spec")
        .or_else(|| meta.get("chart_spec"))
        .cloned()
        .unwrap_or_else(|| {
            json!({
                "chartType": meta.get("chart_type").and_then(Value::as_str).unwrap_or("line"),
                "xAxis": meta.get("x_axis").and_then(Value::as_str).unwrap_or("bucket_start"),
                "yAxis": meta.get("y_axis").and_then(Value::as_str).unwrap_or("value"),
                "series": meta.get("series").cloned().unwrap_or_else(|| json!(["value"]))
            })
        });
    let spec = parse_chart_spec(spec_value)?;
    let name = resource
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Peer chart")
        .to_string();

    Some(ChartPanelItem {
        id: format!("peer:{}:{uri}", peer.id),
        name,
        source: "peer".to_string(),
        spec,
        descriptor: None,
        peer_id: Some(peer.id),
        peer_name: Some(peer.name.clone()),
        uri: Some(uri),
    })
}

fn parse_chart_spec(value: Value) -> Option<ChartSpec> {
    let chart_type = value
        .get("chart_type")
        .or_else(|| value.get("chartType"))?
        .as_str()?
        .to_string();
    let x_axis = value
        .get("x_axis")
        .or_else(|| value.get("xAxis"))?
        .as_str()?
        .to_string();
    let y_axis = value
        .get("y_axis")
        .or_else(|| value.get("yAxis"))?
        .as_str()?
        .to_string();
    let series = value
        .get("series")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| vec![y_axis.clone()]);

    Some(ChartSpec {
        chart_type,
        x_axis,
        y_axis,
        series,
    })
}

fn parse_json_pointer(raw: &str) -> anyhow::Result<Vec<String>> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    if !raw.starts_with('/') {
        anyhow::bail!("must be a JSON Pointer");
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
                        _ => anyhow::bail!("invalid JSON Pointer escape"),
                    }
                } else {
                    out.push(ch);
                }
            }
            Ok(out)
        })
        .collect()
}
