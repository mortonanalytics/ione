//! Demo-only mock MCP endpoint.
//!
//! Map and Document panels are federation-only: IONe has no native table for
//! them — it lists them by calling MCP `resources/list` on bound peers and
//! filtering by `metadata.ione_view`. To make those panels render in the
//! seeded demo workspace (the sales front door) without a real external peer,
//! the demo seeder registers a loopback peer pointing here, and this handler
//! returns canned map + document resources.
//!
//! Mounted only when `IONE_SEED_DEMO=1` (see `routes::router`). It is
//! unauthenticated by design — it exposes nothing real, only static fixture
//! resources, and the outbound peer call attaches a demo bearer the handler
//! ignores.

use axum::Json;
use serde_json::{json, Value};

/// JSON-RPC handler for the demo loopback peer. Only `resources/list` is
/// exercised by the map/document panel services; any other method returns an
/// empty result so the envelope stays well-formed.
pub async fn demo_mcp(Json(body): Json<Value>) -> Json<Value> {
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let method = body.get("method").and_then(Value::as_str).unwrap_or("");

    let result = match method {
        "resources/list" => json!({ "resources": demo_resources() }),
        _ => json!({}),
    };

    Json(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }))
}

/// One map resource (OSM raster tiles framed over western Montana) and one
/// document resource (a stable public sample PDF; `download_url` must be HTTPS
/// to satisfy the document panel's SSRF guard — it is opened by the browser,
/// not fetched server-side).
fn demo_resources() -> Vec<Value> {
    vec![
        json!({
            "uri": "ione-demo://map/fire-detections",
            "name": "Active Fire Detections — Lolo NF",
            "metadata": {
                "ione_view": "map",
                "tile_url": "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
                "bounds": [-114.6, 46.2, -113.3, 47.1],
                "attribution": "© OpenStreetMap contributors",
                "layer_name": "Fire detections"
            }
        }),
        json!({
            "uri": "ione-demo://document/ic-briefing",
            "name": "Lolo SE Fire — IC Briefing (sample)",
            "mimeType": "application/pdf",
            "metadata": {
                "ione_view": "document",
                "download_url": "https://www.w3.org/WAI/ER/tests/xhtml/testfiles/resources/pdf/dummy.pdf",
                "mime_type": "application/pdf",
                "file_size_bytes": 13264,
                "last_modified": "2026-05-29T18:00:00Z"
            }
        }),
    ]
}
