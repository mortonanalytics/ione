use std::{net::IpAddr, str::FromStr, time::Duration};

use anyhow::{anyhow, bail, Context};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION},
    Method, StatusCode, Url,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::models::ConnectorKind;

use super::{ConnectorImpl, PollResult, StreamDescriptor, StreamEventInput};

const DEFAULT_TIMEOUT_MS: u64 = 15_000;
const MAX_TIMEOUT_MS: u64 = 30_000;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug)]
pub struct OpenApiConnector {
    config: OpenApiConfig,
    http: reqwest::Client,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenApiConfig {
    #[serde(default)]
    spec_url: Option<String>,
    #[serde(default)]
    spec_inline: Option<Value>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    auth: AuthConfig,
    #[serde(default)]
    defaults: DefaultsConfig,
    streams: Vec<StreamConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AuthConfig {
    #[default]
    None,
    Bearer {
        token_env: String,
        #[serde(default)]
        token: Option<String>,
    },
    ApiKey {
        r#in: ApiKeyLocation,
        name: String,
        value_env: String,
        #[serde(default)]
        value: Option<String>,
    },
    Basic {
        username_env: String,
        password_env: String,
        #[serde(default)]
        username: Option<String>,
        #[serde(default)]
        password: Option<String>,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ApiKeyLocation {
    Header,
    Query,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct DefaultsConfig {
    #[serde(default)]
    headers: Map<String, Value>,
    #[serde(default)]
    query: Map<String, Value>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct StreamConfig {
    name: String,
    method: String,
    path: String,
    #[serde(default)]
    operation_id: Option<String>,
    #[serde(default)]
    path_params: Map<String, Value>,
    #[serde(default)]
    query: Map<String, Value>,
    #[serde(default)]
    headers: Map<String, Value>,
    #[serde(default)]
    body: Option<Value>,
    items_json_pointer: String,
    observed_at_json_pointer: String,
    #[serde(default)]
    event_id_json_pointer: Option<String>,
    #[serde(default)]
    cursor: CursorConfig,
    #[serde(default)]
    schema: Option<Value>,
    #[serde(default)]
    max_items: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CursorConfig {
    #[serde(rename = "type", default)]
    kind: Option<String>,
}

struct LoadedSpec {
    doc: Value,
    base_url: Url,
}

impl OpenApiConnector {
    pub fn from_config(config: &Value) -> anyhow::Result<Self> {
        let parsed: OpenApiConfig =
            serde_json::from_value(config.clone()).context("invalid openapi connector config")?;
        parsed.validate()?;

        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("failed to build openapi connector http client")?;

        Ok(Self {
            config: parsed,
            http,
        })
    }

    async fn load_spec(&self) -> anyhow::Result<LoadedSpec> {
        let doc = match (&self.config.spec_url, &self.config.spec_inline) {
            (Some(url), None) => self.fetch_spec(url).await?,
            (None, Some(doc)) => doc.clone(),
            _ => bail!("openapi config must set exactly one of 'spec_url' or 'spec_inline'"),
        };

        if !doc.is_object() {
            bail!("openapi spec must be a JSON object");
        }

        let base_url = if let Some(explicit) = &self.config.base_url {
            parse_and_validate_url(explicit, "base_url")?
        } else {
            resolve_base_url_from_spec(&doc)?
        };

        for stream in &self.config.streams {
            validate_operation(&doc, stream)?;
        }

        Ok(LoadedSpec { doc, base_url })
    }

    async fn fetch_spec(&self, spec_url: &str) -> anyhow::Result<Value> {
        let url = parse_and_validate_url(spec_url, "spec_url")?;
        let timeout_ms = self
            .config
            .defaults
            .timeout_ms
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .min(MAX_TIMEOUT_MS);

        let resp = self
            .http
            .get(url.clone())
            .header(ACCEPT, "application/json")
            .timeout(Duration::from_millis(timeout_ms))
            .send()
            .await
            .with_context(|| format!("openapi spec fetch failed for {}", url))?;

        if resp.status() == StatusCode::UNAUTHORIZED || resp.status() == StatusCode::FORBIDDEN {
            bail!("openapi spec fetch auth failed: HTTP {}", resp.status().as_u16());
        }
        if !resp.status().is_success() {
            bail!("openapi spec fetch failed: HTTP {}", resp.status().as_u16());
        }

        let bytes = resp.bytes().await.context("failed to read openapi spec response")?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            bail!("openapi spec fetch failed: response too large");
        }

        serde_json::from_slice(&bytes).map_err(|e| {
            let body = std::str::from_utf8(&bytes).unwrap_or_default();
            if body.trim_start().starts_with("openapi:") || body.trim_start().starts_with("swagger:") {
                anyhow!("openapi YAML specs are not supported; provide JSON spec_url or spec_inline: {}", e)
            } else {
                anyhow!("openapi spec parse failure: {}", e)
            }
        })
    }

    fn stream_by_name(&self, stream_name: &str) -> anyhow::Result<&StreamConfig> {
        self.config
            .streams
            .iter()
            .find(|s| s.name == stream_name)
            .ok_or_else(|| anyhow!("openapi stream '{}' not found in config", stream_name))
    }

    async fn execute_stream(
        &self,
        stream: &StreamConfig,
        cursor: Option<Value>,
    ) -> anyhow::Result<PollResult> {
        let spec = self.load_spec().await?;
        let _ = find_operation(&spec.doc, stream)?;

        let cursor_ctx = CursorContext::from_value(cursor.as_ref());
        let path = render_path(&stream.path, &stream.path_params, &cursor_ctx)?;
        let mut url = spec
            .base_url
            .join(path.trim_start_matches('/'))
            .with_context(|| format!("failed to resolve openapi path '{}'", path))?;
        ensure_safe_url(&url, "resolved request url")?;

        let merged_query = merge_maps(&self.config.defaults.query, &stream.query);
        if !merged_query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in &merged_query {
                let rendered = render_scalar(value, &cursor_ctx)
                    .with_context(|| format!("failed to render query '{}'", key))?;
                pairs.append_pair(key, &rendered);
            }
        }
        drop(merged_query);

        let method = parse_method(&stream.method)?;
        let timeout_ms = self
            .config
            .defaults
            .timeout_ms
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .min(MAX_TIMEOUT_MS);

        let mut req = self.http.request(method.clone(), url.clone());
        req = req.timeout(Duration::from_millis(timeout_ms));
        req = req.header(ACCEPT, "application/json");

        let headers = self.render_headers(stream, &cursor_ctx)?;
        req = req.headers(headers);
        req = self.apply_auth(req, &cursor_ctx)?;

        if method == Method::POST {
            let body = stream
                .body
                .as_ref()
                .cloned()
                .unwrap_or_else(|| Value::Object(Map::new()));
            let rendered_body = render_value(&body, &cursor_ctx)?;
            req = req.json(&rendered_body);
        }

        let resp = req
            .send()
            .await
            .with_context(|| format!("openapi request failed for {}", url))?;

        if resp.status() == StatusCode::UNAUTHORIZED || resp.status() == StatusCode::FORBIDDEN {
            bail!("openapi auth failed: HTTP {}", resp.status().as_u16());
        }
        if !resp.status().is_success() {
            bail!("openapi request failed: HTTP {}", resp.status().as_u16());
        }

        let bytes = resp.bytes().await.context("failed to read openapi response body")?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            bail!("openapi response too large");
        }

        let body: Value = serde_json::from_slice(&bytes)
            .context("openapi response is not valid JSON")?;
        extract_events_from_response(stream, &body)
    }

    fn render_headers(
        &self,
        stream: &StreamConfig,
        cursor_ctx: &CursorContext,
    ) -> anyhow::Result<HeaderMap> {
        let merged = merge_maps(&self.config.defaults.headers, &stream.headers);
        let mut headers = HeaderMap::new();

        for (key, value) in merged {
            let header_name =
                HeaderName::from_str(&key).with_context(|| format!("invalid header name '{}'", key))?;
            let rendered = render_scalar(&value, cursor_ctx)
                .with_context(|| format!("failed to render header '{}'", key))?;
            let header_value = HeaderValue::from_str(&rendered)
                .with_context(|| format!("invalid header value for '{}'", key))?;
            headers.insert(header_name, header_value);
        }

        Ok(headers)
    }

    fn apply_auth(
        &self,
        mut req: reqwest::RequestBuilder,
        cursor_ctx: &CursorContext,
    ) -> anyhow::Result<reqwest::RequestBuilder> {
        match &self.config.auth {
            AuthConfig::None => Ok(req),
            AuthConfig::Bearer { token_env, .. } => {
                let token = env_value(token_env)?;
                let rendered = render_template_string(&token, cursor_ctx)?;
                req = req.header(AUTHORIZATION, format!("Bearer {}", rendered));
                Ok(req)
            }
            AuthConfig::ApiKey {
                r#in,
                name,
                value_env,
                ..
            } => {
                let value = render_template_string(&env_value(value_env)?, cursor_ctx)?;
                match r#in {
                    ApiKeyLocation::Header => {
                        let header_name = HeaderName::from_str(name)
                            .with_context(|| format!("invalid api_key header '{}'", name))?;
                        let header_value = HeaderValue::from_str(&value)
                            .with_context(|| format!("invalid api_key header value for '{}'", name))?;
                        req = req.header(header_name, header_value);
                    }
                    ApiKeyLocation::Query => {
                        req = req.query(&[(name, value.as_str())]);
                    }
                }
                Ok(req)
            }
            AuthConfig::Basic {
                username_env,
                password_env,
                ..
            } => {
                let username = render_template_string(&env_value(username_env)?, cursor_ctx)?;
                let password = render_template_string(&env_value(password_env)?, cursor_ctx)?;
                let encoded = base64::engine::general_purpose::STANDARD
                    .encode(format!("{}:{}", username, password));
                req = req.header(AUTHORIZATION, format!("Basic {}", encoded));
                Ok(req)
            }
        }
    }
}

#[async_trait::async_trait]
impl ConnectorImpl for OpenApiConnector {
    fn kind(&self) -> ConnectorKind {
        ConnectorKind::Openapi
    }

    async fn default_streams(&self) -> anyhow::Result<Vec<StreamDescriptor>> {
        let spec = self.load_spec().await?;

        self.config
            .streams
            .iter()
            .map(|stream| {
                let op = find_operation(&spec.doc, stream)?;
                let schema = stream
                    .schema
                    .clone()
                    .unwrap_or_else(|| default_stream_schema(stream, op));
                Ok(StreamDescriptor {
                    name: stream.name.clone(),
                    schema,
                })
            })
            .collect()
    }

    async fn poll(&self, stream_name: &str, cursor: Option<Value>) -> anyhow::Result<PollResult> {
        let stream = self.stream_by_name(stream_name)?;
        self.execute_stream(stream, cursor).await
    }
}

impl OpenApiConfig {
    fn validate(&self) -> anyhow::Result<()> {
        match (&self.spec_url, &self.spec_inline) {
            (Some(_), Some(_)) => bail!("openapi config must not set both 'spec_url' and 'spec_inline'"),
            (None, None) => bail!("openapi config must set one of 'spec_url' or 'spec_inline'"),
            _ => {}
        }

        if let Some(url) = &self.spec_url {
            parse_and_validate_url(url, "spec_url")?;
        }
        if let Some(url) = &self.base_url {
            parse_and_validate_url(url, "base_url")?;
        }

        self.auth.validate()?;

        if let Some(timeout_ms) = self.defaults.timeout_ms {
            if timeout_ms == 0 {
                bail!("openapi defaults.timeout_ms must be > 0");
            }
        }

        if self.streams.is_empty() {
            bail!("openapi config requires at least one stream");
        }

        for stream in &self.streams {
            stream.validate()?;
        }

        Ok(())
    }
}

impl AuthConfig {
    fn validate(&self) -> anyhow::Result<()> {
        match self {
            AuthConfig::None => Ok(()),
            AuthConfig::Bearer {
                token_env, token, ..
            } => {
                if token.is_some() {
                    bail!("openapi auth.bearer requires 'token_env'; literal 'token' is not allowed");
                }
                let _ = env_value(token_env)?;
                Ok(())
            }
            AuthConfig::ApiKey {
                name,
                value_env,
                value,
                ..
            } => {
                if value.is_some() {
                    bail!("openapi auth.api_key requires 'value_env'; literal 'value' is not allowed");
                }
                if name.trim().is_empty() {
                    bail!("openapi auth.api_key requires non-empty 'name'");
                }
                let _ = env_value(value_env)?;
                Ok(())
            }
            AuthConfig::Basic {
                username_env,
                password_env,
                username,
                password,
            } => {
                if username.is_some() || password.is_some() {
                    bail!("openapi auth.basic requires 'username_env' and 'password_env'; literal credentials are not allowed");
                }
                let _ = env_value(username_env)?;
                let _ = env_value(password_env)?;
                Ok(())
            }
        }
    }
}

impl StreamConfig {
    fn validate(&self) -> anyhow::Result<()> {
        if self.name.trim().is_empty() {
            bail!("openapi stream requires non-empty 'name'");
        }

        parse_method(&self.method)?;

        if !self.path.starts_with('/') {
            bail!("openapi stream '{}' path must start with '/'", self.name);
        }

        validate_json_pointer(&self.items_json_pointer)
            .with_context(|| format!("stream '{}' has invalid items_json_pointer", self.name))?;
        validate_json_pointer(&self.observed_at_json_pointer)
            .with_context(|| format!("stream '{}' has invalid observed_at_json_pointer", self.name))?;

        if let Some(ptr) = &self.event_id_json_pointer {
            validate_json_pointer(ptr)
                .with_context(|| format!("stream '{}' has invalid event_id_json_pointer", self.name))?;
        }

        if let Some(kind) = &self.cursor.kind {
            if kind != "none" && kind != "max_observed_at" && kind != "static_window" {
                bail!(
                    "openapi stream '{}' cursor.type must be one of none, max_observed_at, static_window",
                    self.name
                );
            }
        }

        Ok(())
    }
}

fn validate_json_pointer(ptr: &str) -> anyhow::Result<()> {
    if ptr.is_empty() {
        return Ok(());
    }
    if !ptr.starts_with('/') {
        bail!("JSON Pointer must be empty or start with '/'");
    }
    let mut chars = ptr.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '~' {
            match chars.next() {
                Some('0') | Some('1') => {}
                _ => bail!("JSON Pointer contains invalid '~' escape"),
            }
        }
    }
    Ok(())
}

fn parse_method(method: &str) -> anyhow::Result<Method> {
    match method.to_ascii_uppercase().as_str() {
        "GET" => Ok(Method::GET),
        "POST" => Ok(Method::POST),
        other => bail!("unsupported openapi method '{}'; expected GET or POST", other),
    }
}

fn resolve_base_url_from_spec(doc: &Value) -> anyhow::Result<Url> {
    let server_url = doc["servers"]
        .as_array()
        .and_then(|servers| servers.first())
        .and_then(|server| server["url"].as_str())
        .ok_or_else(|| anyhow!("openapi spec missing servers[0].url; set connector base_url explicitly"))?;

    parse_and_validate_url(server_url, "spec.servers[0].url")
}

fn validate_operation(doc: &Value, stream: &StreamConfig) -> anyhow::Result<()> {
    let _ = find_operation(doc, stream)?;
    Ok(())
}

fn find_operation<'a>(doc: &'a Value, stream: &StreamConfig) -> anyhow::Result<&'a Value> {
    let method = stream.method.to_ascii_lowercase();
    let path_item = doc["paths"]
        .get(&stream.path)
        .ok_or_else(|| anyhow!("openapi operation mismatch: path '{}' not found for stream '{}'", stream.path, stream.name))?;

    let op = path_item
        .get(&method)
        .ok_or_else(|| anyhow!(
            "openapi operation mismatch: method '{}' not found at path '{}' for stream '{}'",
            stream.method,
            stream.path,
            stream.name
        ))?;

    if let Some(expected) = &stream.operation_id {
        let actual = op["operationId"]
            .as_str()
            .ok_or_else(|| anyhow!(
                "openapi operation mismatch: operation at {} {} has no operationId; expected '{}'",
                stream.method,
                stream.path,
                expected
            ))?;
        if actual != expected {
            bail!(
                "openapi operation mismatch: {} {} has operationId '{}', expected '{}'",
                stream.method,
                stream.path,
                actual,
                expected
            );
        }
    }

    Ok(op)
}

fn default_stream_schema(stream: &StreamConfig, operation: &Value) -> Value {
    let description = operation["summary"]
        .as_str()
        .or_else(|| operation["description"].as_str())
        .unwrap_or("OpenAPI stream records");
    json!({
        "type": "object",
        "description": format!("{} ({})", description, stream.name)
    })
}

fn parse_and_validate_url(raw: &str, label: &str) -> anyhow::Result<Url> {
    let url = Url::parse(raw).with_context(|| format!("invalid {} URL '{}'", label, raw))?;
    ensure_safe_url(&url, label)?;
    Ok(url)
}

fn ensure_safe_url(url: &Url, label: &str) -> anyhow::Result<()> {
    match url.scheme() {
        "https" => {}
        "http" => {
            let host = url
                .host_str()
                .ok_or_else(|| anyhow!("unsafe URL in {}: missing host", label))?;
            if !host_is_http_allowed(host) {
                bail!("unsafe URL in {}: http is only allowed for localhost or private IP hosts", label);
            }
        }
        other => bail!("unsafe URL in {}: unsupported scheme '{}'", label, other),
    }

    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("unsafe URL in {}: missing host", label))?;
    if host.eq_ignore_ascii_case("169.254.169.254") {
        bail!("unsafe URL in {}: blocked metadata IP", label);
    }

    Ok(())
}

fn host_is_http_allowed(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    match IpAddr::from_str(host) {
        Ok(IpAddr::V4(ip)) => {
            ip.is_private() || ip.is_loopback() || ip.is_link_local()
        }
        Ok(IpAddr::V6(ip)) => ip.is_loopback() || ip.is_unique_local(),
        Err(_) => false,
    }
}

fn merge_maps(base: &Map<String, Value>, overlay: &Map<String, Value>) -> Map<String, Value> {
    let mut merged = base.clone();
    for (k, v) in overlay {
        merged.insert(k.clone(), v.clone());
    }
    merged
}

fn render_path(path: &str, path_params: &Map<String, Value>, cursor_ctx: &CursorContext) -> anyhow::Result<String> {
    let mut rendered = path.to_string();
    for (key, value) in path_params {
        let placeholder = format!("{{{}}}", key);
        let replacement = render_scalar(value, cursor_ctx)?;
        rendered = rendered.replace(&placeholder, &replacement);
    }
    Ok(rendered)
}

#[derive(Default)]
struct CursorContext {
    observed_at: Option<String>,
}

impl CursorContext {
    fn from_value(value: Option<&Value>) -> Self {
        let observed_at = value
            .and_then(|v| v.get("observed_at"))
            .and_then(Value::as_str)
            .map(str::to_string);
        Self { observed_at }
    }
}

fn render_value(value: &Value, cursor_ctx: &CursorContext) -> anyhow::Result<Value> {
    match value {
        Value::String(s) => Ok(Value::String(render_template_string(s, cursor_ctx)?)),
        Value::Array(items) => {
            let rendered = items
                .iter()
                .map(|item| render_value(item, cursor_ctx))
                .collect::<anyhow::Result<Vec<_>>>()?;
            Ok(Value::Array(rendered))
        }
        Value::Object(map) => {
            let mut rendered = Map::new();
            for (k, v) in map {
                rendered.insert(k.clone(), render_value(v, cursor_ctx)?);
            }
            Ok(Value::Object(rendered))
        }
        _ => Ok(value.clone()),
    }
}

fn render_scalar(value: &Value, cursor_ctx: &CursorContext) -> anyhow::Result<String> {
    match render_value(value, cursor_ctx)? {
        Value::String(s) => Ok(s),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(if b { "true" } else { "false" }.to_string()),
        Value::Null => Ok(String::new()),
        other => bail!("templated scalar value must resolve to string/number/bool, got {}", other),
    }
}

fn render_template_string(input: &str, cursor_ctx: &CursorContext) -> anyhow::Result<String> {
    if !input.contains("{{") {
        return Ok(input.to_string());
    }

    let mut out = input.to_string();
    while let Some(start) = out.find("{{") {
        let rest = &out[start + 2..];
        let end_rel = rest
            .find("}}")
            .ok_or_else(|| anyhow!("unterminated template expression in '{}'", input))?;
        let expr = rest[..end_rel].trim();
        let replacement = match expr {
            "cursor.observed_at" => cursor_ctx
                .observed_at
                .clone()
                .ok_or_else(|| anyhow!("template requires cursor.observed_at but no cursor was provided"))?,
            other => bail!("unsupported template expression '{}'", other),
        };
        let end = start + 2 + end_rel + 2;
        out.replace_range(start..end, &replacement);
    }
    Ok(out)
}

fn extract_events_from_response(stream: &StreamConfig, body: &Value) -> anyhow::Result<PollResult> {
    let selected = if stream.items_json_pointer.is_empty() {
        body
    } else {
        body.pointer(&stream.items_json_pointer).ok_or_else(|| {
            anyhow!(
                "openapi item extraction failure: pointer '{}' not found for stream '{}'",
                stream.items_json_pointer,
                stream.name
            )
        })?
    };

    let mut records: Vec<Value> = match selected {
        Value::Array(items) => items.clone(),
        Value::Object(_) => vec![selected.clone()],
        _ => bail!(
            "openapi item extraction failure: pointer '{}' did not resolve to an array or object for stream '{}'",
            stream.items_json_pointer,
            stream.name
        ),
    };

    if let Some(limit) = stream.max_items {
        records.truncate(limit);
    }

    let mut events = Vec::with_capacity(records.len());
    for record in records {
        let observed_at = extract_observed_at(&record, &stream.observed_at_json_pointer)
            .with_context(|| format!("stream '{}' timestamp parse failure", stream.name))?;
        let mut source = json!({
            "observed_at": observed_at.to_rfc3339(),
            "connector": "openapi",
            "stream": stream.name,
        });
        if let Some(operation_id) = &stream.operation_id {
            source["operation_id"] = Value::String(operation_id.clone());
        }
        if let Some(ptr) = &stream.event_id_json_pointer {
            let id = record.pointer(ptr).and_then(Value::as_str).ok_or_else(|| {
                anyhow!(
                    "openapi item extraction failure: event_id_json_pointer '{}' missing or not a string for stream '{}'",
                    ptr,
                    stream.name
                )
            })?;
            source["id"] = Value::String(id.to_string());
        }

        events.push(StreamEventInput {
            payload: json!({
                "source": source,
                "record": record,
            }),
            observed_at,
        });
    }

    Ok(PollResult {
        events,
        next_cursor: None,
    })
}

fn extract_observed_at(record: &Value, ptr: &str) -> anyhow::Result<DateTime<Utc>> {
    let raw = record
        .pointer(ptr)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("observed_at_json_pointer '{}' missing or not a string", ptr))?;
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .with_context(|| format!("invalid RFC3339 timestamp '{}'", raw))
}

fn env_value(name: &str) -> anyhow::Result<String> {
    std::env::var(name).with_context(|| format!("missing required env var '{}'", name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        matchers::{header, method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };

    fn valid_config() -> Value {
        json!({
            "spec_inline": {
                "openapi": "3.0.0",
                "servers": [{ "url": "http://127.0.0.1:3000" }],
                "paths": {
                    "/incidents": {
                        "get": { "operationId": "listIncidents", "summary": "List incidents" }
                    },
                    "/search": {
                        "post": { "operationId": "searchIncidents" }
                    }
                }
            },
            "base_url": "http://127.0.0.1:3000",
            "auth": { "type": "none" },
            "streams": [
                {
                    "name": "incidents",
                    "method": "GET",
                    "path": "/incidents",
                    "operation_id": "listIncidents",
                    "items_json_pointer": "/items",
                    "observed_at_json_pointer": "/updated_at"
                },
                {
                    "name": "search",
                    "method": "POST",
                    "path": "/search",
                    "operation_id": "searchIncidents",
                    "items_json_pointer": "",
                    "observed_at_json_pointer": "/updated_at"
                }
            ]
        })
    }

    #[test]
    fn config_parsing_accepts_get_and_post() {
        let cfg = OpenApiConnector::from_config(&valid_config()).expect("valid openapi config");
        assert_eq!(cfg.config.streams.len(), 2);
    }

    #[test]
    fn config_rejects_literal_secrets() {
        std::env::set_var("OPENAPI_TOKEN_ENV", "secret");
        let mut cfg = valid_config();
        cfg["auth"] = json!({ "type": "bearer", "token_env": "OPENAPI_TOKEN_ENV", "token": "literal" });
        let err = OpenApiConnector::from_config(&cfg).expect_err("literal secret must fail");
        assert!(err.to_string().contains("literal 'token' is not allowed"));
    }

    #[test]
    fn config_rejects_missing_env_var() {
        let cfg = json!({
            "spec_inline": {
                "openapi": "3.0.0",
                "servers": [{ "url": "http://127.0.0.1:3000" }],
                "paths": { "/incidents": { "get": { "operationId": "listIncidents" } } }
            },
            "auth": { "type": "bearer", "token_env": "OPENAPI_MISSING_TOKEN" },
            "streams": [{
                "name": "incidents",
                "method": "GET",
                "path": "/incidents",
                "items_json_pointer": "/items",
                "observed_at_json_pointer": "/updated_at"
            }]
        });
        std::env::remove_var("OPENAPI_MISSING_TOKEN");
        let err = OpenApiConnector::from_config(&cfg).expect_err("missing env must fail");
        assert!(err.to_string().contains("missing required env var"));
    }

    #[test]
    fn json_pointer_extraction_supports_array_and_object() {
        let stream = StreamConfig {
            name: "incidents".to_string(),
            method: "GET".to_string(),
            path: "/incidents".to_string(),
            operation_id: None,
            path_params: Map::new(),
            query: Map::new(),
            headers: Map::new(),
            body: None,
            items_json_pointer: "/items".to_string(),
            observed_at_json_pointer: "/updated_at".to_string(),
            event_id_json_pointer: None,
            cursor: CursorConfig::default(),
            schema: None,
            max_items: None,
        };
        let body = json!({
            "items": [
                { "updated_at": "2026-04-23T12:00:00Z" }
            ]
        });
        let result = extract_events_from_response(&stream, &body).expect("array extraction");
        assert_eq!(result.events.len(), 1);

        let mut stream2 = stream;
        stream2.items_json_pointer = "".to_string();
        let body2 = json!({ "updated_at": "2026-04-23T13:00:00Z" });
        let result2 = extract_events_from_response(&stream2, &body2).expect("object extraction");
        assert_eq!(result2.events.len(), 1);
    }

    #[test]
    fn template_rendering_supports_cursor_observed_at() {
        let ctx = CursorContext {
            observed_at: Some("2026-04-23T12:00:00Z".to_string()),
        };
        let rendered = render_template_string("since={{cursor.observed_at}}", &ctx).expect("render");
        assert_eq!(rendered, "since=2026-04-23T12:00:00Z");
    }

    #[test]
    fn validate_operation_rejects_mismatched_operation_id() {
        let spec = valid_config()["spec_inline"].clone();
        let stream: StreamConfig = serde_json::from_value(json!({
            "name": "incidents",
            "method": "GET",
            "path": "/incidents",
            "operation_id": "wrong",
            "items_json_pointer": "/items",
            "observed_at_json_pointer": "/updated_at"
        }))
        .expect("stream");
        stream.validate().expect("stream syntax valid");
        let err = validate_operation(&spec, &stream).expect_err("operation id mismatch");
        assert!(err.to_string().contains("operation mismatch"));
    }

    #[test]
    fn url_safety_rejects_blocked_scheme() {
        let err = parse_and_validate_url("file:///tmp/spec.json", "spec_url")
            .expect_err("file scheme must fail");
        assert!(err.to_string().contains("unsafe URL"));
    }

    #[tokio::test]
    async fn request_builder_applies_bearer_auth_header() {
        std::env::set_var("OPENAPI_BEARER_TOKEN", "secret-token");
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/incidents"))
            .and(header("authorization", "Bearer secret-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [{ "updated_at": "2026-04-23T12:00:00Z" }]
            })))
            .mount(&server)
            .await;

        let cfg = json!({
            "spec_inline": {
                "openapi": "3.0.0",
                "servers": [{ "url": server.uri() }],
                "paths": { "/incidents": { "get": { "operationId": "listIncidents" } } }
            },
            "base_url": server.uri(),
            "auth": { "type": "bearer", "token_env": "OPENAPI_BEARER_TOKEN" },
            "streams": [{
                "name": "incidents",
                "method": "GET",
                "path": "/incidents",
                "items_json_pointer": "/items",
                "observed_at_json_pointer": "/updated_at"
            }]
        });

        let connector = OpenApiConnector::from_config(&cfg).expect("connector");
        let result = connector.poll("incidents", None).await.expect("poll");
        assert_eq!(result.events.len(), 1);
    }

    #[tokio::test]
    async fn request_builder_applies_api_key_header_and_query_and_basic() {
        std::env::set_var("OPENAPI_KEY", "key123");
        std::env::set_var("OPENAPI_USER", "user");
        std::env::set_var("OPENAPI_PASS", "pass");
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/header"))
            .and(header("x-api-key", "key123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [{ "updated_at": "2026-04-23T12:00:00Z" }]
            })))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/query"))
            .and(query_param("api_key", "key123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [{ "updated_at": "2026-04-23T12:00:00Z" }]
            })))
            .mount(&server)
            .await;

        let basic = base64::engine::general_purpose::STANDARD.encode("user:pass");
        Mock::given(method("GET"))
            .and(path("/basic"))
            .and(header("authorization", format!("Basic {}", basic)))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [{ "updated_at": "2026-04-23T12:00:00Z" }]
            })))
            .mount(&server)
            .await;

        for (auth, path_name) in [
            (json!({ "type": "api_key", "in": "header", "name": "X-API-Key", "value_env": "OPENAPI_KEY" }), "/header"),
            (json!({ "type": "api_key", "in": "query", "name": "api_key", "value_env": "OPENAPI_KEY" }), "/query"),
            (json!({ "type": "basic", "username_env": "OPENAPI_USER", "password_env": "OPENAPI_PASS" }), "/basic"),
        ] {
            let cfg = json!({
                "spec_inline": {
                    "openapi": "3.0.0",
                    "servers": [{ "url": server.uri() }],
                    "paths": { path_name: { "get": { "operationId": "op" } } }
                },
                "base_url": server.uri(),
                "auth": auth,
                "streams": [{
                    "name": "stream",
                    "method": "GET",
                    "path": path_name,
                    "items_json_pointer": "/items",
                    "observed_at_json_pointer": "/updated_at"
                }]
            });
            let connector = OpenApiConnector::from_config(&cfg).expect("connector");
            let result = connector.poll("stream", None).await.expect("poll");
            assert_eq!(result.events.len(), 1);
        }
    }
}
