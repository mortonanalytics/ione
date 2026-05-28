//! Pure projection of geo-mapped `stream_events` into GeoJSON point layers.
//!
//! Input comes from two repo queries (catalog of geo-mapped streams + their
//! events); the projection here is I/O-free so it is unit-testable without a DB.
//! `view_config` is validated before projection so bad stream config is reported
//! through `streamsFailed` without failing the whole endpoint.

use std::collections::{HashMap, HashSet};
use std::fmt;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::util::json_pointer::validate_json_pointer;

/// Catalog row (repo Q1): every geo-mapped stream in the workspace, whether or
/// not it has events in the window. Drives which layers appear in the response.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct GeoStreamRow {
    pub stream_id: Uuid,
    pub stream_name: String,
    pub view_config: Value,
}

/// Event row (repo Q2): a single stream event for a geo-mapped stream.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct GeoEventRow {
    pub event_id: Uuid,
    pub stream_id: Uuid,
    pub payload: Value,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventLayersResponse {
    pub layers: Vec<EventLayer>,
    pub streams_ok: Vec<Uuid>,
    pub streams_failed: Vec<StreamProjectionError>,
    pub truncated: bool,
    pub queried_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventLayer {
    pub stream_id: Uuid,
    pub stream_name: String,
    pub attribution: Option<String>,
    pub features_skipped: i64,
    /// GeoJSON `FeatureCollection`; geometry is always `Point`.
    pub collection: Value,
    pub style: Option<LayerStyle>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerStyle {
    pub size_field: Option<String>,
    pub size_domain: Option<Value>,
    pub size_range: Option<Value>,
    pub color_field: Option<String>,
    pub color_domain: Option<Value>,
    pub color_range: Option<Value>,
    pub label_field: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamProjectionError {
    pub stream_id: Uuid,
    pub stream_name: String,
    pub error: String,
}

struct PropertyField {
    pointer: String,
    name: String,
}

struct CompiledConfig {
    lon_pointer: String,
    lat_pointer: String,
    property_fields: Vec<PropertyField>,
    attribution: Option<String>,
    style: Option<LayerStyle>,
}

impl CompiledConfig {
    fn parse(vc: &Value) -> Result<CompiledConfig, String> {
        let lon_pointer = optional_string(vc, "lon_pointer")
            .ok_or(ViewConfigError::MissingLonPointer)
            .map_err(|e| e.to_string())?;
        validate_pointer_field("lon_pointer", lon_pointer).map_err(|e| e.to_string())?;
        if lon_pointer.is_empty() {
            return Err(ViewConfigError::MissingLonPointer.to_string());
        }

        let lat_pointer = optional_string(vc, "lat_pointer")
            .ok_or(ViewConfigError::MissingLatPointer)
            .map_err(|e| e.to_string())?;
        validate_pointer_field("lat_pointer", lat_pointer).map_err(|e| e.to_string())?;
        if lat_pointer.is_empty() {
            return Err(ViewConfigError::MissingLatPointer.to_string());
        }

        let property_fields = match vc.get("property_fields") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .enumerate()
                .map(|(idx, item)| {
                    let pointer = item.get("pointer").and_then(Value::as_str).ok_or_else(|| {
                        ViewConfigError::PropertyPointerMissing { index: idx }.to_string()
                    })?;
                    validate_pointer_field("property_fields.pointer", pointer)
                        .map_err(|e| e.to_string())?;
                    let name = item
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "view_config.property_fields[].name missing".to_string())?
                        .to_string();
                    Ok(PropertyField {
                        pointer: pointer.to_string(),
                        name,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?,
            Some(_) => return Err("view_config.property_fields must be an array".to_string()),
        };
        validate_property_names(&property_fields).map_err(|e| e.to_string())?;

        let attribution = vc
            .get("attribution")
            .and_then(Value::as_str)
            .map(str::to_string);

        let style = match vc.get("style") {
            None | Some(Value::Null) => None,
            Some(style) => Some(parse_style(style, &property_fields).map_err(|e| e.to_string())?),
        };

        Ok(CompiledConfig {
            lon_pointer: lon_pointer.to_string(),
            lat_pointer: lat_pointer.to_string(),
            property_fields,
            attribution,
            style,
        })
    }
}

#[derive(Debug)]
enum ViewConfigError {
    InvalidPointer { field: String, reason: String },
    MissingLonPointer,
    MissingLatPointer,
    PropertyPointerMissing { index: usize },
    InvalidPropertyName { name: String },
    DuplicatePropertyName { name: String },
    ReservedPropertyName { name: String },
    PartialStyleTriple { which: &'static str },
    InvalidStyleShape { field: &'static str },
    UnknownStyleFieldReference { field: &'static str, name: String },
}

impl fmt::Display for ViewConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ViewConfigError::InvalidPointer { field, reason } => {
                write!(f, "view_config.{field} invalid JSON Pointer: {reason}")
            }
            ViewConfigError::MissingLonPointer => write!(f, "view_config.lon_pointer missing"),
            ViewConfigError::MissingLatPointer => write!(f, "view_config.lat_pointer missing"),
            ViewConfigError::PropertyPointerMissing { index } => write!(
                f,
                "view_config.property_fields[{index}].pointer missing"
            ),
            ViewConfigError::InvalidPropertyName { name } => write!(
                f,
                "view_config.property_fields[].name invalid: {name}"
            ),
            ViewConfigError::DuplicatePropertyName { name } => write!(
                f,
                "view_config.property_fields[].name duplicate: {name}"
            ),
            ViewConfigError::ReservedPropertyName { name } => write!(
                f,
                "view_config.property_fields[].name collides with reserved key: {name}"
            ),
            ViewConfigError::PartialStyleTriple { which } => write!(
                f,
                "view_config.style {which} style triple must include field, domain, and range together"
            ),
            ViewConfigError::InvalidStyleShape { field } => {
                write!(f, "view_config.style.{field} has invalid shape")
            }
            ViewConfigError::UnknownStyleFieldReference { field, name } => write!(
                f,
                "view_config.style.{field} references unknown property field: {name}"
            ),
        }
    }
}

fn optional_string<'a>(vc: &'a Value, key: &str) -> Option<&'a str> {
    vc.get(key).and_then(Value::as_str)
}

fn validate_pointer_field(field: &str, ptr: &str) -> Result<(), ViewConfigError> {
    validate_json_pointer(ptr).map_err(|err| ViewConfigError::InvalidPointer {
        field: field.to_string(),
        reason: err.to_string(),
    })
}

fn validate_property_names(fields: &[PropertyField]) -> Result<(), ViewConfigError> {
    let mut seen = HashSet::new();
    for field in fields {
        if field.name == "_event_id" || field.name == "_observed_at" {
            return Err(ViewConfigError::ReservedPropertyName {
                name: field.name.clone(),
            });
        }
        if !is_valid_property_name(&field.name) {
            return Err(ViewConfigError::InvalidPropertyName {
                name: field.name.clone(),
            });
        }
        if !seen.insert(field.name.clone()) {
            return Err(ViewConfigError::DuplicatePropertyName {
                name: field.name.clone(),
            });
        }
    }
    Ok(())
}

fn is_valid_property_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first == '_' || first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn parse_style(
    style: &Value,
    property_fields: &[PropertyField],
) -> Result<LayerStyle, ViewConfigError> {
    let obj = style
        .as_object()
        .ok_or(ViewConfigError::InvalidStyleShape { field: "style" })?;
    validate_style_triple(obj, "size", "size_field", "size_domain", "size_range")?;
    validate_style_triple(obj, "color", "color_field", "color_domain", "color_range")?;

    let size_field = style_string(obj, "size_field")?;
    let color_field = style_string(obj, "color_field")?;
    let label_field = style_string(obj, "label_field")?;
    let size_domain = style_value(obj, "size_domain");
    let size_range = style_value(obj, "size_range");
    let color_domain = style_value(obj, "color_domain");
    let color_range = style_value(obj, "color_range");

    if size_domain.is_some() && !is_number_pair(size_domain.as_ref().unwrap()) {
        return Err(ViewConfigError::InvalidStyleShape {
            field: "size_domain",
        });
    }
    if size_range.is_some() && !is_number_pair(size_range.as_ref().unwrap()) {
        return Err(ViewConfigError::InvalidStyleShape {
            field: "size_range",
        });
    }
    if let (Some(domain), Some(range)) = (&color_domain, &color_range) {
        let Some(domain) = domain.as_array() else {
            return Err(ViewConfigError::InvalidStyleShape {
                field: "color_domain",
            });
        };
        let Some(range) = range.as_array() else {
            return Err(ViewConfigError::InvalidStyleShape {
                field: "color_range",
            });
        };
        if domain.len() < 2 || domain.len() != range.len() || !domain.iter().all(Value::is_number) {
            return Err(ViewConfigError::InvalidStyleShape {
                field: "color_domain",
            });
        }
        if !range.iter().all(Value::is_string) {
            return Err(ViewConfigError::InvalidStyleShape {
                field: "color_range",
            });
        }
    }

    validate_style_field_reference("size_field", size_field.as_deref(), property_fields)?;
    validate_style_field_reference("color_field", color_field.as_deref(), property_fields)?;
    validate_style_field_reference("label_field", label_field.as_deref(), property_fields)?;

    Ok(LayerStyle {
        size_field,
        size_domain,
        size_range,
        color_field,
        color_domain,
        color_range,
        label_field,
    })
}

fn validate_style_triple(
    obj: &Map<String, Value>,
    which: &'static str,
    field_key: &'static str,
    domain_key: &'static str,
    range_key: &'static str,
) -> Result<(), ViewConfigError> {
    let present = [
        obj.get(field_key).is_some_and(|v| !v.is_null()),
        obj.get(domain_key).is_some_and(|v| !v.is_null()),
        obj.get(range_key).is_some_and(|v| !v.is_null()),
    ]
    .into_iter()
    .filter(|v| *v)
    .count();
    if present == 0 || present == 3 {
        Ok(())
    } else {
        Err(ViewConfigError::PartialStyleTriple { which })
    }
}

fn style_string(
    obj: &Map<String, Value>,
    key: &'static str,
) -> Result<Option<String>, ViewConfigError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(ViewConfigError::InvalidStyleShape { field: key }),
    }
}

fn style_value(obj: &Map<String, Value>, key: &str) -> Option<Value> {
    match obj.get(key) {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.clone()),
    }
}

fn is_number_pair(value: &Value) -> bool {
    matches!(value.as_array(), Some(items) if items.len() == 2 && items.iter().all(Value::is_number))
}

fn validate_style_field_reference(
    field: &'static str,
    name: Option<&str>,
    property_fields: &[PropertyField],
) -> Result<(), ViewConfigError> {
    if let Some(name) = name {
        if !property_fields
            .iter()
            .any(|candidate| candidate.name == name)
        {
            return Err(ViewConfigError::UnknownStyleFieldReference {
                field,
                name: name.to_string(),
            });
        }
    }
    Ok(())
}

/// Project catalog + events into the wire response. Pure (no I/O).
pub fn project_event_layers(
    catalog: Vec<GeoStreamRow>,
    mut events: Vec<GeoEventRow>,
    limit: i64,
    queried_at: DateTime<Utc>,
) -> EventLayersResponse {
    // LIMIT + 1 was requested server-side: more rows than `limit` means truncation.
    let truncated = events.len() as i64 > limit;
    if truncated {
        events.truncate(limit.max(0) as usize);
    }

    let mut by_stream: HashMap<Uuid, Vec<GeoEventRow>> = HashMap::new();
    for ev in events {
        by_stream.entry(ev.stream_id).or_default().push(ev);
    }

    let mut layers = Vec::new();
    let mut streams_ok = Vec::new();
    let mut streams_failed = Vec::new();

    for row in catalog {
        match CompiledConfig::parse(&row.view_config) {
            Err(error) => streams_failed.push(StreamProjectionError {
                stream_id: row.stream_id,
                stream_name: row.stream_name,
                error,
            }),
            Ok(cfg) => {
                let stream_events = by_stream.remove(&row.stream_id).unwrap_or_default();
                let mut features = Vec::with_capacity(stream_events.len());
                let mut features_skipped = 0i64;

                for ev in &stream_events {
                    let lon = ev.payload.pointer(&cfg.lon_pointer).and_then(Value::as_f64);
                    let lat = ev.payload.pointer(&cfg.lat_pointer).and_then(Value::as_f64);
                    let (lon, lat) = match (lon, lat) {
                        (Some(lon), Some(lat)) => (lon, lat),
                        _ => {
                            features_skipped += 1;
                            continue;
                        }
                    };

                    let mut properties = Map::new();
                    for field in &cfg.property_fields {
                        let resolved = ev
                            .payload
                            .pointer(&field.pointer)
                            .cloned()
                            .unwrap_or(Value::Null);
                        properties.insert(field.name.clone(), resolved);
                    }
                    // Always-injected keys, written last (field-leakage guard: only
                    // declared property_fields plus these two ever reach the wire).
                    properties.insert("_event_id".to_string(), json!(ev.event_id));
                    properties.insert("_observed_at".to_string(), json!(ev.observed_at));

                    features.push(json!({
                        "type": "Feature",
                        "geometry": { "type": "Point", "coordinates": [lon, lat] },
                        "properties": Value::Object(properties),
                    }));
                }

                streams_ok.push(row.stream_id);
                layers.push(EventLayer {
                    stream_id: row.stream_id,
                    stream_name: row.stream_name,
                    attribution: cfg.attribution,
                    features_skipped,
                    collection: json!({ "type": "FeatureCollection", "features": features }),
                    style: cfg.style,
                });
            }
        }
    }

    EventLayersResponse {
        layers,
        streams_ok,
        streams_failed,
        truncated,
        queried_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usgs_config() -> Value {
        json!({
            "lon_pointer": "/geometry/coordinates/0",
            "lat_pointer": "/geometry/coordinates/1",
            "property_fields": [{ "pointer": "/properties/mag", "name": "mag" }]
        })
    }

    fn usgs_event(id: Uuid, lon: f64, lat: f64, mag: f64) -> GeoEventRow {
        GeoEventRow {
            event_id: id,
            stream_id: Uuid::nil(),
            payload: json!({
                "type": "Feature",
                "geometry": { "type": "Point", "coordinates": [lon, lat] },
                "properties": { "mag": mag, "place": "somewhere", "internal_id": "SECRET" }
            }),
            observed_at: Utc::now(),
        }
    }

    #[test]
    fn happy_path_projects_features_without_payload_leakage() {
        let stream_id = Uuid::new_v4();
        let catalog = vec![GeoStreamRow {
            stream_id,
            stream_name: "quakes".to_string(),
            view_config: usgs_config(),
        }];
        let mut e1 = usgs_event(Uuid::new_v4(), -122.0, 37.0, 5.1);
        let mut e2 = usgs_event(Uuid::new_v4(), -120.0, 36.0, 4.2);
        e1.stream_id = stream_id;
        e2.stream_id = stream_id;

        let resp = project_event_layers(catalog, vec![e1, e2], 5000, Utc::now());

        assert_eq!(resp.layers.len(), 1);
        assert!(!resp.truncated);
        let features = resp.layers[0].collection["features"].as_array().unwrap();
        assert_eq!(features.len(), 2);
        let props = features[0]["properties"].as_object().unwrap();
        let mut keys: Vec<&String> = props.keys().collect();
        keys.sort();
        assert_eq!(keys, vec!["_event_id", "_observed_at", "mag"]);
        assert_eq!(features[0]["geometry"]["type"], "Point");
    }

    #[test]
    fn zero_event_stream_still_emits_a_layer() {
        let stream_id = Uuid::new_v4();
        let catalog = vec![GeoStreamRow {
            stream_id,
            stream_name: "quiet".to_string(),
            view_config: usgs_config(),
        }];

        let resp = project_event_layers(catalog, vec![], 5000, Utc::now());

        assert_eq!(resp.layers.len(), 1);
        assert_eq!(resp.streams_ok, vec![stream_id]);
        assert_eq!(
            resp.layers[0].collection["features"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn missing_lon_pointer_lands_in_streams_failed() {
        let stream_id = Uuid::new_v4();
        let catalog = vec![GeoStreamRow {
            stream_id,
            stream_name: "broken".to_string(),
            view_config: json!({ "lat_pointer": "/y" }),
        }];

        let resp = project_event_layers(catalog, vec![], 5000, Utc::now());

        assert!(resp.layers.is_empty());
        assert_eq!(resp.streams_failed.len(), 1);
        assert!(resp.streams_failed[0].error.contains("lon_pointer"));
    }

    #[test]
    fn truncation_uses_limit_plus_one_semantics() {
        let stream_id = Uuid::new_v4();
        let catalog = vec![GeoStreamRow {
            stream_id,
            stream_name: "busy".to_string(),
            view_config: usgs_config(),
        }];
        // limit = 2, repo returned 3 (limit + 1) → truncated, trimmed to 2.
        let events: Vec<GeoEventRow> = (0..3)
            .map(|i| {
                let mut e = usgs_event(Uuid::new_v4(), -100.0 + i as f64, 40.0, 3.0);
                e.stream_id = stream_id;
                e
            })
            .collect();

        let resp = project_event_layers(catalog, events, 2, Utc::now());

        assert!(resp.truncated);
        assert_eq!(
            resp.layers[0].collection["features"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn non_numeric_coordinates_increment_features_skipped() {
        let stream_id = Uuid::new_v4();
        let catalog = vec![GeoStreamRow {
            stream_id,
            stream_name: "mixed".to_string(),
            view_config: usgs_config(),
        }];
        let mut good = usgs_event(Uuid::new_v4(), -122.0, 37.0, 5.1);
        good.stream_id = stream_id;
        let mut bad = GeoEventRow {
            event_id: Uuid::new_v4(),
            stream_id,
            payload: json!({ "geometry": { "coordinates": ["nope", null] } }),
            observed_at: Utc::now(),
        };
        bad.stream_id = stream_id;

        let resp = project_event_layers(catalog, vec![good, bad], 5000, Utc::now());

        assert_eq!(resp.layers[0].features_skipped, 1);
        assert_eq!(
            resp.layers[0].collection["features"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}
