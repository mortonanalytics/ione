//! IONe peer conformance kit — contract v1.
//!
//! A standalone pass/fail checker a candidate peer runs against **its own**
//! endpoint. It never talks to an IONe deployment: every rule it asserts is read
//! out of `md/design/app-integration-contract-v1.md` and re-implemented here, so
//! an app team can validate before IONe is anywhere near their loop.
//!
//! Usage:
//!   cargo run --bin ione-conformance -- --url https://app.example.com/mcp [options]
//!
//! This file deliberately imports nothing from the `ione` crate. It depends only
//! on axum, reqwest, serde_json, chrono, hmac, sha2, hex, url and tokio, so it
//! can be lifted verbatim into a standalone crate by anyone who does not want to
//! build IONe to run it.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::process::ExitCode;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use url::{Host, Url};

const USAGE: &str = r#"IONe peer conformance kit — contract v1

Checks a candidate peer's six surfaces and reports PASS/FAIL per surface.
Exits 0 when no surface failed, 1 when any surface failed, 2 on bad usage.

USAGE:
  ione-conformance --url <MCP_URL> [OPTIONS]

REQUIRED:
  --url <URL>                 The peer's MCP endpoint (contract §1), e.g.
                              https://app.example.com/mcp

OPTIONS:
  --token <BEARER>            Bearer token presented on every MCP request (§1).
                              Omit for an unauthenticated / loopback peer.
  --pre-brokered              Declare that this deployment uses pre-brokered
                              static credentials, so surface 2 (OAuth 2.1) does
                              not apply. Without it, the OAuth discovery
                              document is required.
  --oauth-issuer <URL>        Origin hosting the OAuth metadata when it is not
                              the origin of --url.
  --webhook-peer-id <UUID>    The peer_id IONe issued you (§3). Enables the
                              webhook surface.
  --webhook-secret <SECRET>   The signingSecret IONe provisioned for you (§3).
                              Enables the webhook surface.
  --webhook-trigger <URL>     Optional. Endpoint on YOUR app that the kit POSTs
                              {"ione_base_url": "<kit receiver>"} to, to make
                              your app emit one event. Without it the kit prints
                              its receiver URL and waits for you to fire one by
                              hand. Not part of contract v1 — a test affordance.
  --webhook-timeout <SECS>    How long to wait for the event. Default 30.
  --help                      Print this message.

Surface 3 is optional in contract v1 and reports SKIP when not configured.
Surface 4 reports SKIP for a tools-only peer that exposes no renderable
resource, and surface 5 reports SKIP for a peer that serves no slice:// —
both are conforming shapes, so neither fails the run.
A SKIP never fails the run; a FAIL always does."#;

// ─── Reporting ────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Clone, Copy)]
enum Status {
    Pass,
    Fail,
    Skip,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Fail => "FAIL",
            Status::Skip => "SKIP",
        }
    }
}

/// Accumulates the checks performed for one surface. Any recorded failure makes
/// the surface FAIL; an explicit skip only survives if nothing failed.
struct Surface {
    number: u8,
    name: &'static str,
    section: &'static str,
    lines: Vec<String>,
    failures: usize,
    skipped: Option<String>,
}

impl Surface {
    fn new(number: u8, name: &'static str, section: &'static str) -> Surface {
        Surface {
            number,
            name,
            section,
            lines: Vec::new(),
            failures: 0,
            skipped: None,
        }
    }

    fn ok(&mut self, message: impl Into<String>) {
        self.lines.push(format!("      ok    {}", message.into()));
    }

    fn fail(&mut self, message: impl Into<String>) {
        self.failures += 1;
        self.lines.push(format!("      FAIL  {}", message.into()));
    }

    /// Record `ok`/`fail` from a boolean, so a check reads as one line.
    fn check(&mut self, condition: bool, ok: impl Into<String>, fail: impl Into<String>) {
        if condition {
            self.ok(ok);
        } else {
            self.fail(fail);
        }
    }

    fn skip(&mut self, reason: impl Into<String>) {
        self.skipped = Some(reason.into());
    }

    fn status(&self) -> Status {
        if self.failures > 0 {
            Status::Fail
        } else if self.skipped.is_some() {
            Status::Skip
        } else {
            Status::Pass
        }
    }

    fn print(&self) {
        let status = self.status();
        println!(
            "[{}] {}. {}  ({})",
            status.label(),
            self.number,
            self.name,
            self.section
        );
        if let Some(reason) = &self.skipped {
            println!("      skip  {reason}");
        }
        for line in &self.lines {
            println!("{line}");
        }
        println!();
    }
}

// ─── Options ──────────────────────────────────────────────────────────────────

struct Options {
    url: String,
    token: Option<String>,
    pre_brokered: bool,
    oauth_issuer: Option<String>,
    webhook_peer_id: Option<String>,
    webhook_secret: Option<String>,
    webhook_trigger: Option<String>,
    webhook_timeout: u64,
}

fn parse_options(args: Vec<String>) -> Result<Options, String> {
    let mut url = None;
    let mut token = None;
    let mut pre_brokered = false;
    let mut oauth_issuer = None;
    let mut webhook_peer_id = None;
    let mut webhook_secret = None;
    let mut webhook_trigger = None;
    let mut webhook_timeout = 30u64;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        let mut value = || {
            iter.next()
                .ok_or_else(|| format!("option '{arg}' requires a value"))
        };
        match arg.as_str() {
            "--url" => url = Some(value()?),
            "--token" => token = Some(value()?),
            "--pre-brokered" => pre_brokered = true,
            "--oauth-issuer" => oauth_issuer = Some(value()?),
            "--webhook-peer-id" => webhook_peer_id = Some(value()?),
            "--webhook-secret" => webhook_secret = Some(value()?),
            "--webhook-trigger" => webhook_trigger = Some(value()?),
            "--webhook-timeout" => {
                let raw = value()?;
                webhook_timeout = raw
                    .parse()
                    .map_err(|_| format!("--webhook-timeout expects seconds, got '{raw}'"))?;
            }
            other => return Err(format!("unknown option '{other}'")),
        }
    }

    Ok(Options {
        url: url.ok_or("--url is required")?,
        token,
        pre_brokered,
        oauth_issuer,
        webhook_peer_id,
        webhook_secret,
        webhook_trigger,
        webhook_timeout,
    })
}

// ─── MCP client (§1) ──────────────────────────────────────────────────────────

struct RpcCall {
    http_status: u16,
    body: Value,
}

impl RpcCall {
    fn result(&self) -> Option<&Value> {
        self.body.get("result").filter(|v| !v.is_null())
    }

    fn error(&self) -> Option<&Value> {
        self.body.get("error").filter(|v| !v.is_null())
    }

    fn error_code(&self) -> Option<i64> {
        self.error()?.get("code")?.as_i64()
    }
}

struct McpClient {
    http: reqwest::Client,
    url: String,
    token: Option<String>,
    session: Option<String>,
}

impl McpClient {
    fn new(options: &Options) -> McpClient {
        McpClient {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("reqwest client"),
            url: options.url.trim_end_matches('/').to_string(),
            token: options.token.clone(),
            session: None,
        }
    }

    async fn call(&self, method: &str, params: Value) -> Result<RpcCall, String> {
        let mut request = self.http.post(&self.url).json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params
        }));
        // §1: credentials are always exactly `Authorization: Bearer <token>`.
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        if let Some(session) = &self.session {
            request = request.header("MCP-Session-Id", session);
        }
        let response = request
            .send()
            .await
            .map_err(|err| format!("{method} transport error: {err}"))?;
        let http_status = response.status().as_u16();
        let text = response
            .text()
            .await
            .map_err(|err| format!("{method} body read failed: {err}"))?;
        let body = serde_json::from_str(&text)
            .map_err(|err| format!("{method} response is not JSON ({err}): {text}"))?;
        Ok(RpcCall { http_status, body })
    }

    /// `initialize` and adopt the returned session id (§1, session recovery).
    async fn initialize(&mut self) -> Result<RpcCall, String> {
        let mut request = self.http.post(&self.url).json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-11-25", "capabilities": {} }
        }));
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .map_err(|err| format!("initialize transport error: {err}"))?;
        let http_status = response.status().as_u16();
        self.session = response
            .headers()
            .get("MCP-Session-Id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let text = response
            .text()
            .await
            .map_err(|err| format!("initialize body read failed: {err}"))?;
        let body = serde_json::from_str(&text)
            .map_err(|err| format!("initialize response is not JSON ({err}): {text}"))?;
        Ok(RpcCall { http_status, body })
    }

    /// Follow `nextCursor` per §8.1, capped at IONe's `MAX_PAGINATION_PAGES`.
    async fn paginated(&self, method: &str, field: &str) -> Result<(Vec<Value>, usize), String> {
        let mut cursor: Option<Value> = None;
        let mut items = Vec::new();
        for page in 1..=50 {
            let params = cursor
                .clone()
                .map(|c| json!({ "cursor": c }))
                .unwrap_or(Value::Null);
            let call = self.call(method, params).await?;
            if let Some(err) = call.error() {
                return Err(format!("{method} returned a JSON-RPC error: {err}"));
            }
            let result = call
                .result()
                .ok_or_else(|| format!("{method} response has no 'result' object"))?;
            match result.get(field).and_then(Value::as_array) {
                Some(page_items) => items.extend(page_items.iter().cloned()),
                None => return Err(format!("{method} result is missing the '{field}' array")),
            }
            cursor = result
                .get("nextCursor")
                .filter(|v| !v.is_null())
                .cloned()
                .or_else(|| result.get("cursor").filter(|v| !v.is_null()).cloned());
            if cursor.is_none() {
                return Ok((items, page));
            }
        }
        Err(format!(
            "{method} still returned a nextCursor after 50 pages; IONe caps pagination at 50 \
             pages (§8.1) and would silently truncate your listing"
        ))
    }

    /// `resources/read` returning `contents[0].text` parsed as JSON.
    async fn read_json(&self, uri: &str) -> Result<Value, String> {
        let call = self.call("resources/read", json!({ "uri": uri })).await?;
        if !(200..300).contains(&call.http_status) {
            return Err(format!(
                "resources/read {uri} answered HTTP {}; IONe calls error_for_status() before \
                 parsing, so a non-2xx status is reported as a peer fault (502) whatever the \
                 JSON-RPC body says (§7.3)",
                call.http_status
            ));
        }
        if let Some(err) = call.error() {
            return Err(format!(
                "resources/read {uri} returned a JSON-RPC error: {err}"
            ));
        }
        let text = call
            .body
            .pointer("/result/contents/0/text")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                format!("resources/read {uri} is missing result.contents[0].text (§1)")
            })?;
        serde_json::from_str(text)
            .map_err(|err| format!("resources/read {uri} text is not valid JSON: {err}"))
    }

    async fn read_mime(&self, uri: &str) -> Result<String, String> {
        let call = self.call("resources/read", json!({ "uri": uri })).await?;
        call.body
            .pointer("/result/contents/0/mimeType")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("resources/read {uri} is missing result.contents[0].mimeType"))
    }
}

// ─── Surface 1 — MCP server endpoint (§1) ─────────────────────────────────────

/// Returns the surface-1 result and the peer's resource listing. `None` means
/// `resources/list` produced no listing at all — an unreachable or non-MCP
/// endpoint — which surfaces 4 and 5 must treat as a hard failure rather than as
/// "this peer simply exposes nothing".
async fn check_mcp_endpoint(client: &mut McpClient) -> (Surface, Option<Vec<Value>>) {
    let mut surface = Surface::new(1, "MCP server endpoint", "§1");
    let mut resources = None;

    match client.initialize().await {
        Ok(call) => {
            surface.check(
                (200..300).contains(&call.http_status),
                format!("initialize answered HTTP {}", call.http_status),
                format!(
                    "initialize answered HTTP {} — IONe requires a 2xx carrying a JSON-RPC object",
                    call.http_status
                ),
            );
            surface.check(
                call.body.get("jsonrpc").and_then(Value::as_str) == Some("2.0"),
                "response declares jsonrpc 2.0",
                "response is missing \"jsonrpc\":\"2.0\"",
            );
            surface.check(
                call.result().is_some(),
                "initialize returned a result object",
                "initialize returned no 'result' object",
            );
            match &client.session {
                Some(_) => surface.ok("initialize returned an MCP-Session-Id header"),
                None => surface
                    .ok("no MCP-Session-Id header (allowed: §1 lets a sessionless peer ignore it)"),
            }
        }
        Err(err) => surface.fail(err),
    }

    match client.paginated("tools/list", "tools").await {
        Ok((tools, pages)) => surface.ok(format!(
            "tools/list returned {} tool(s) over {pages} page(s), terminating cleanly",
            tools.len()
        )),
        Err(err) => surface.fail(err),
    }

    match client.paginated("resources/list", "resources").await {
        Ok((items, pages)) => {
            surface.ok(format!(
                "resources/list returned {} resource(s) over {pages} page(s), terminating cleanly",
                items.len()
            ));
            if pages > 1 {
                // §8.2 requires a v1 peer to support cursor-based pagination on
                // resources/list, and all four panel paths follow nextCursor via
                // src/services/peer_panels.rs (issue #18). Paginating is
                // conforming; the kit used to FAIL it, which told a peer that
                // did exactly what the contract asks that it was broken.
                surface.ok(format!(
                    "resources/list paginates over {pages} page(s); IONe's map/chart/table/\
                     document panel paths follow nextCursor (§8.2), capped at 50 pages (§8.1)"
                ));
            }
            let unnamed = items
                .iter()
                .filter(|r| {
                    r.get("uri")
                        .and_then(Value::as_str)
                        .map(str::is_empty)
                        .unwrap_or(true)
                })
                .count();
            surface.check(
                unnamed == 0,
                "every resource carries a non-empty uri",
                format!("{unnamed} resource(s) have a missing or empty 'uri' and are dropped"),
            );
            resources = Some(items);
        }
        Err(err) => surface.fail(err),
    }

    // A resource that does not exist must be a JSON-RPC -32002 inside an HTTP
    // 2xx. §7.3: IONe maps on the code, and any non-2xx HTTP status is a 502.
    match client
        .call(
            "resources/read",
            json!({ "uri": "conformance://absent-by-design" }),
        )
        .await
    {
        Ok(call) => {
            if !(200..300).contains(&call.http_status) {
                surface.fail(format!(
                    "resources/read of an unknown uri answered HTTP {} — IONe calls \
                     error_for_status() first, so this surfaces as 502 peer-fault instead of 404 \
                     (§7.3). Answer HTTP 200 with a JSON-RPC error body.",
                    call.http_status
                ));
            } else {
                match call.error_code() {
                    Some(-32002) => {
                        surface.ok("unknown resource answers JSON-RPC -32002 inside HTTP 200")
                    }
                    Some(-32601) => surface.fail(
                        "resources/read answers -32601 (method not found) — IONe reports that as \
                         502 peer-fault, not 404 (§7.3)",
                    ),
                    Some(code) => surface.fail(format!(
                        "unknown resource answers JSON-RPC {code}; IONe maps every code but \
                         -32002 to 502 (§7.3). Use -32002 for resource-not-found."
                    )),
                    None => surface.fail(
                        "resources/read of an unknown uri returned a success result; IONe cannot \
                         distinguish a missing resource from an empty one (§7.3)",
                    ),
                }
            }
        }
        Err(err) => surface.fail(err),
    }

    // tools/call must exist. Probe with a name that cannot exist so no candidate
    // peer suffers a side effect from being tested.
    match client
        .call(
            "tools/call",
            json!({ "name": "__ione_conformance_probe__", "arguments": {} }),
        )
        .await
    {
        Ok(call) => match call.error_code() {
            Some(-32601) => surface
                .fail("tools/call is not implemented (-32601 method not found); §1 requires it"),
            _ => surface.ok("tools/call is implemented"),
        },
        Err(err) => surface.fail(err),
    }

    (surface, resources)
}

// ─── Surface 2 — OAuth 2.1 authorization server (§2) ──────────────────────────

async fn check_oauth(options: &Options) -> Surface {
    let mut surface = Surface::new(2, "OAuth 2.1 authorization server", "§2");

    if options.pre_brokered {
        surface.skip(
            "--pre-brokered declared: this deployment uses static per-(workspace, peer) \
             credentials, which §2 permits instead of an authorization server",
        );
        return surface;
    }

    let issuer = match options
        .oauth_issuer
        .clone()
        .map(Ok)
        .unwrap_or_else(|| origin_of(&options.url))
    {
        Ok(issuer) => issuer,
        Err(err) => {
            surface.fail(err);
            return surface;
        }
    };
    let discovery_url = format!(
        "{}/.well-known/oauth-authorization-server",
        issuer.trim_end_matches('/')
    );

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("reqwest client");

    let metadata: Value = match http.get(&discovery_url).send().await {
        Ok(response) if response.status().is_success() => match response.json().await {
            Ok(value) => {
                surface.ok(format!("{discovery_url} is served"));
                value
            }
            Err(err) => {
                surface.fail(format!("{discovery_url} is not valid JSON: {err}"));
                return surface;
            }
        },
        Ok(response) => {
            surface.fail(format!(
                "{discovery_url} answered HTTP {}. Serve the discovery document, or pass \
                 --pre-brokered if this deployment uses static credentials.",
                response.status()
            ));
            return surface;
        }
        Err(err) => {
            surface.fail(format!("{discovery_url} is unreachable: {err}"));
            return surface;
        }
    };

    for field in [
        "issuer",
        "authorization_endpoint",
        "token_endpoint",
        "revocation_endpoint",
    ] {
        let present = metadata
            .get(field)
            .and_then(Value::as_str)
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        surface.check(
            present,
            format!("metadata declares {field}"),
            format!("metadata is missing '{field}' — §2 requires it"),
        );
    }
    surface.check(
        string_array_contains(&metadata, "code_challenge_methods_supported", "S256"),
        "PKCE S256 is advertised",
        "code_challenge_methods_supported does not include 'S256'; §2 requires PKCE",
    );
    surface.check(
        string_array_contains(&metadata, "grant_types_supported", "refresh_token"),
        "the refresh_token grant is advertised",
        "grant_types_supported does not include 'refresh_token'; IONe refreshes delegated \
         tokens automatically (§2)",
    );
    surface.check(
        string_array_contains(&metadata, "grant_types_supported", "authorization_code"),
        "the authorization_code grant is advertised",
        "grant_types_supported does not include 'authorization_code' (§2)",
    );

    // The authorization endpoint is interactive and cannot be driven headlessly.
    // The token endpoint can: an unsupported grant must produce an OAuth error,
    // which proves the endpoint is live and speaks the protocol.
    if let Some(token_endpoint) = metadata.get("token_endpoint").and_then(Value::as_str) {
        let probe = http
            .post(token_endpoint)
            .form(&[("grant_type", "__ione_conformance_probe__")])
            .send()
            .await;
        match probe {
            Ok(response) => {
                let status = response.status();
                let body: Value = response.json().await.unwrap_or(Value::Null);
                let has_error = body.get("error").and_then(Value::as_str).is_some();
                surface.check(
                    status.is_client_error() && has_error,
                    format!(
                        "token_endpoint rejects an unsupported grant with HTTP {status} and an \
                         OAuth error body"
                    ),
                    format!(
                        "token_endpoint answered HTTP {status} body {body} for an unsupported \
                         grant; RFC 6749 §5.2 requires a 4xx with {{\"error\": ...}}"
                    ),
                );
            }
            Err(err) => surface.fail(format!(
                "token_endpoint {token_endpoint} unreachable: {err}"
            )),
        }
    }

    surface.ok(
        "note: the authorization endpoint requires a human, so this kit checks metadata and the \
         token endpoint only",
    );
    surface
}

fn string_array_contains(value: &Value, field: &str, needle: &str) -> bool {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(|items| items.iter().any(|item| item.as_str() == Some(needle)))
        .unwrap_or(false)
}

fn origin_of(raw: &str) -> Result<String, String> {
    let url = Url::parse(raw).map_err(|err| format!("--url '{raw}' is not a URL: {err}"))?;
    let host = url
        .host_str()
        .ok_or_else(|| format!("--url '{raw}' has no host"))?;
    match url.port() {
        Some(port) => Ok(format!("{}://{host}:{port}", url.scheme())),
        None => Ok(format!("{}://{host}", url.scheme())),
    }
}

// ─── Surface 3 — signed webhook sender (§3) ───────────────────────────────────

struct CapturedWebhook {
    path_peer_id: String,
    signature: Option<String>,
    body: Vec<u8>,
}

async fn check_webhook_sender(options: &Options) -> Surface {
    let mut surface = Surface::new(3, "Signed webhook sender", "§3");

    let (Some(peer_id), Some(secret)) = (&options.webhook_peer_id, &options.webhook_secret) else {
        surface.skip(
            "--webhook-peer-id and --webhook-secret were not supplied; §3 is optional in \
             contract v1",
        );
        return surface;
    };

    let (tx, mut rx) = mpsc::channel::<CapturedWebhook>(4);
    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(err) => {
            surface.fail(format!("could not bind a local webhook receiver: {err}"));
            return surface;
        }
    };
    let receiver_base = format!(
        "http://{}",
        listener.local_addr().expect("receiver local addr")
    );
    let router = Router::new()
        .route("/webhooks/peer/:peer_id", post(receive_webhook))
        .with_state(tx);
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    match &options.webhook_trigger {
        Some(trigger) => {
            surface.ok(format!("asking {trigger} to emit one event"));
            let response = reqwest::Client::new()
                .post(trigger)
                .json(&json!({ "ione_base_url": receiver_base }))
                .send()
                .await;
            if let Err(err) = response {
                surface.fail(format!("--webhook-trigger {trigger} failed: {err}"));
                server.abort();
                return surface;
            }
        }
        None => {
            println!(
                "  waiting {}s for one signed event at {receiver_base}/webhooks/peer/{peer_id}",
                options.webhook_timeout
            );
        }
    }

    let captured =
        tokio::time::timeout(Duration::from_secs(options.webhook_timeout), rx.recv()).await;
    server.abort();

    let captured = match captured {
        Ok(Some(captured)) => captured,
        Ok(None) => {
            surface.fail("the webhook receiver closed before an event arrived");
            return surface;
        }
        Err(_) => {
            surface.fail(format!(
                "no signed event arrived within {}s at {receiver_base}/webhooks/peer/{peer_id}",
                options.webhook_timeout
            ));
            return surface;
        }
    };

    surface.check(
        captured.path_peer_id == *peer_id,
        format!("event POSTed to /webhooks/peer/{peer_id}"),
        format!(
            "event POSTed to /webhooks/peer/{} but --webhook-peer-id is {peer_id}; the path \
             peer_id selects which signing secret IONe verifies against (§3.1)",
            captured.path_peer_id
        ),
    );
    surface.check(
        captured.body.len() <= 256 * 1024,
        format!("body is {} bytes (cap 256 KiB)", captured.body.len()),
        format!(
            "body is {} bytes; IONe rejects over 256 KiB with 413 before the handler runs (§3.1)",
            captured.body.len()
        ),
    );

    let Some(header) = captured.signature else {
        surface.fail("no X-IONe-Signature header (§3.1)");
        return surface;
    };

    let timestamp = match verify_signature_header(&header, secret, &captured.body) {
        Ok(timestamp) => {
            surface.ok("X-IONe-Signature grammar, hex digest and HMAC-SHA256 all verify");
            timestamp
        }
        Err(err) => {
            surface.fail(err);
            return surface;
        }
    };

    let now = Utc::now().timestamp();
    surface.check(
        (now - timestamp).abs() <= 300,
        format!("t is {}s from now (window ±300s)", (now - timestamp).abs()),
        format!(
            "t is {}s from now; IONe rejects beyond ±300s (§3.2). Sign at send time, not at \
             build time.",
            (now - timestamp).abs()
        ),
    );

    verify_envelope(&mut surface, &captured.body, peer_id, timestamp);
    surface
}

async fn receive_webhook(
    State(tx): State<mpsc::Sender<CapturedWebhook>>,
    Path(peer_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<Value>) {
    let _ = tx
        .send(CapturedWebhook {
            path_peer_id: peer_id,
            signature: headers
                .get("X-IONe-Signature")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
            body: body.to_vec(),
        })
        .await;
    // Mirror IONe's §3.5 success envelope so a sender's response handling is
    // exercised against the real shape.
    (
        StatusCode::OK,
        Json(json!({ "ok": true, "duplicate": false, "signalIds": [] })),
    )
}

/// Re-implements IONe's §3.1 verification: grammar, hex length, signing input.
///
/// Two places where the kit deliberately matches `src/routes/webhooks.rs`
/// rather than the contract's prose:
///
///   * whitespace around a key or value is trimmed, as `parse_signature` does,
///     so `t=1, v1=…` verifies here exactly as it does in production;
///   * the digest is compared case-insensitively, because production decodes it
///     with `hex::decode`, which accepts `A-F`. The contract's §3.1 table calls
///     lowercase "Enforced"; it is not. Rejecting uppercase here would fail a
///     peer IONe accepts, which is the failure mode this kit exists to prevent.
fn verify_signature_header(header: &str, secret: &str, body: &[u8]) -> Result<i64, String> {
    let mut timestamp: Option<&str> = None;
    let mut digest: Option<&str> = None;
    for part in header.split(',') {
        let (key, value) = part
            .split_once('=')
            .ok_or_else(|| format!("X-IONe-Signature part '{part}' is not key=value (§3.1)"))?;
        let (key, value) = (key.trim(), value.trim());
        match key {
            "t" if timestamp.is_none() => timestamp = Some(value),
            "v1" if digest.is_none() => digest = Some(value),
            "t" | "v1" => return Err(format!("X-IONe-Signature repeats '{key}'; rejected (§3.1)")),
            other => {
                return Err(format!(
                    "X-IONe-Signature carries unknown key '{other}'; only t and v1 are permitted \
                     and any other key is rejected (§3.1)"
                ))
            }
        }
    }
    let timestamp = timestamp.ok_or("X-IONe-Signature has no 't=' component (§3.1)")?;
    let digest = digest.ok_or("X-IONe-Signature has no 'v1=' component (§3.1)")?;
    let timestamp: i64 = timestamp
        .parse()
        .map_err(|_| format!("t='{timestamp}' is not a base-10 integer of unix seconds (§3.1)"))?;
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "v1 must be exactly 64 hex characters decoding to 32 bytes, got {} characters (§3.1)",
            digest.len()
        ));
    }

    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac accepts any key size");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    let expected = hex::encode(mac.finalize().into_bytes());
    if expected != digest.to_ascii_lowercase() {
        return Err(
            "v1 digest does not match HMAC-SHA256(t_ascii ++ \".\" ++ raw_body) with the \
             provisioned secret (§3.1). The usual cause is re-serializing the body after \
             signing — sign the exact bytes you transmit."
                .to_string(),
        );
    }
    Ok(timestamp)
}

/// Re-implements the §3.3 envelope constraints IONe enforces.
fn verify_envelope(surface: &mut Surface, body: &[u8], peer_id: &str, timestamp: i64) {
    let envelope: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(err) => {
            surface.fail(format!("body is not valid JSON: {err}"));
            return;
        }
    };
    let Some(object) = envelope.as_object() else {
        surface.fail("the envelope must be a JSON object (§3.3)");
        return;
    };

    let id_len = object.get("id").and_then(Value::as_str).map(str::len);
    surface.check(
        matches!(id_len, Some(1..=255)),
        "id is a string of 1..=255 characters",
        "id must be a string of 1..=255 characters (§3.3)",
    );

    match object.get("type").and_then(Value::as_str) {
        Some(kind)
            if (1..=255).contains(&kind.len())
                && kind.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b"._/-".contains(&b)
                }) =>
        {
            surface.ok(format!("type '{kind}' matches ^[a-z0-9._/-]+$"))
        }
        Some(kind) => surface.fail(format!(
            "type '{kind}' must be 1..=255 bytes matching ^[a-z0-9._/-]+$ (§3.3)"
        )),
        None => surface.fail("type is required (§3.3)"),
    }

    match object
        .get("occurred_at")
        .and_then(Value::as_str)
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
    {
        Some(occurred_at) => {
            let skew = (timestamp - occurred_at.timestamp()).abs();
            surface.check(
                skew <= 30,
                format!("occurred_at is {skew}s from the signature t (window ±30s)"),
                format!(
                    "occurred_at is {skew}s from the signature t; IONe rejects beyond ±30s (§3.2)"
                ),
            );
        }
        None => surface.fail("occurred_at must be an RFC 3339 timestamp (§3.3)"),
    }

    surface.check(
        object.get("peer_id").and_then(Value::as_str) == Some(peer_id),
        "peer_id in the envelope equals the path peer_id",
        format!(
            "envelope peer_id must equal the path peer_id '{peer_id}' (§3.3), got {:?}",
            object.get("peer_id")
        ),
    );

    let tenant_len = object
        .get("foreign_tenant_id")
        .and_then(Value::as_str)
        .map(str::len);
    surface.check(
        matches!(tenant_len, Some(1..=512)),
        "foreign_tenant_id is a string of 1..=512 characters",
        "foreign_tenant_id must be a string of 1..=512 characters (§3.3). It must also match \
         the whoami:// foreign_tenant_id for the same tenant.",
    );

    match object.get("data") {
        Some(data) if data.is_object() => {
            let size = data.to_string().len();
            surface.check(
                size <= 102_400,
                format!("data is an object of {size} serialized bytes (cap 102 400)"),
                format!("data serializes to {size} bytes; IONe rejects over 102 400 (§3.3)"),
            );
        }
        Some(_) => surface.fail("data must be a JSON object, not a scalar or array (§3.3)"),
        None => surface.fail("data is required (§3.3)"),
    }

    match object.get("approval_required") {
        Some(Value::Bool(flag)) => {
            surface.ok(format!("approval_required is present and boolean ({flag})"))
        }
        Some(other) => surface.fail(format!(
            "approval_required must be a boolean when present, got {other}; any other JSON type \
             fails deserialization and returns a bare 400 carrying no message (§3.3, §7.2)"
        )),
        None => surface.ok(
            "approval_required is absent and defaults to false (§3.3). The field is optional: \
             the ingress policy floor is escalate-only, so an absent flag is exactly equivalent \
             to an explicit false (Appendix A #2).",
        ),
    }

    match object.get("severity").and_then(Value::as_str) {
        Some(severity) if matches!(severity, "routine" | "flagged" | "command") => {
            surface.ok(format!("severity '{severity}' is a known level"))
        }
        Some(severity) => surface.ok(format!(
            "severity '{severity}' is unknown and will be treated as 'routine' (§3.3)"
        )),
        None => surface.ok("severity is absent and defaults to 'routine' (§3.3)"),
    }
}

// ─── Surface 4 — resource view metadata (§4) ──────────────────────────────────

async fn check_view_metadata(client: &McpClient, resources: Option<&[Value]>) -> Surface {
    let mut surface = Surface::new(4, "Resource view metadata (ione_view)", "§4");

    let Some(resources) = resources else {
        surface.fail(
            "resources/list returned no listing (see surface 1), so no renderable resource could              be discovered at all",
        );
        return surface;
    };
    let viewed: Vec<&Value> = resources
        .iter()
        .filter(|r| r.pointer("/metadata/ione_view").is_some())
        .collect();

    if viewed.is_empty() {
        // Surface 4 is "Required to render in the shell", not required outright:
        // a tools-only peer that exposes no renderable resource is conforming
        // and must not be told it is broken.
        surface.skip(
            "no resource carries metadata.ione_view, so nothing renders in IONe's shell. That is \
             conforming for a tools-only peer — surface 4 is required only to render — but if you \
             expect panels, this is why they are empty: a resource without ione_view is silently \
             dropped from every panel (§4.1).",
        );
        return surface;
    }
    surface.ok(format!(
        "{} of {} resource(s) declare metadata.ione_view",
        viewed.len(),
        resources.len()
    ));

    for resource in viewed {
        let uri = resource
            .get("uri")
            .and_then(Value::as_str)
            .unwrap_or("<no uri>");
        let view = resource.pointer("/metadata/ione_view").expect("filtered");
        let Some(view) = view.as_str() else {
            surface.fail(format!(
                "{uri}: ione_view must be a string; it is compared by exact value (§4.1)"
            ));
            continue;
        };
        let metadata = resource.get("metadata").expect("filtered");
        match view {
            "map" => check_map_resource(&mut surface, uri, metadata),
            "chart" => check_chart_resource(&mut surface, client, uri, metadata).await,
            "table" => check_table_resource(&mut surface, client, uri).await,
            "document" => check_document_resource(&mut surface, uri, resource, metadata),
            other => surface.fail(format!(
                "{uri}: ione_view '{other}' is not one of map|chart|table|document, so the \
                 resource is silently dropped from every panel (§4.1). Matching is \
                 case-sensitive with no aliases."
            )),
        }
    }

    surface
}

fn check_map_resource(surface: &mut Surface, uri: &str, metadata: &Value) {
    match metadata.get("tile_url").and_then(Value::as_str) {
        Some(tile_url) if !tile_url.is_empty() => {
            surface.ok(format!("{uri}: map layer renders (tile_url '{tile_url}')"))
        }
        Some(_) => surface.fail(format!(
            "{uri}: tile_url is empty, so the layer is dropped (§4.2)"
        )),
        None => surface.fail(format!(
            "{uri}: tile_url is required and must be a non-empty XYZ template; IONe does not \
             proxy tiles, so it must be reachable from the operator's browser (§4.2)"
        )),
    }
    if let Some(opacity) = metadata.get("opacity").and_then(Value::as_f64) {
        if !(0.0..=1.0).contains(&opacity) {
            surface.ok(format!(
                "{uri}: opacity {opacity} is outside 0.0–1.0; IONe does not clamp it (§4.2)"
            ));
        }
    }
}

async fn check_chart_resource(
    surface: &mut Surface,
    client: &McpClient,
    uri: &str,
    metadata: &Value,
) {
    // §4.3: the nested form wins; in the nested form the three scalars are
    // effectively required, because a missing one drops the resource.
    let nested = metadata.get("spec").or_else(|| metadata.get("chart_spec"));
    if let Some(spec) = nested {
        for field in [
            ("chart_type", "chartType"),
            ("x_axis", "xAxis"),
            ("y_axis", "yAxis"),
        ] {
            let present = spec
                .get(field.0)
                .or_else(|| spec.get(field.1))
                .and_then(Value::as_str)
                .is_some();
            if !present {
                surface.fail(format!(
                    "{uri}: metadata.spec is missing '{}' (or '{}'), so parse_chart_spec returns \
                     None and the chart is dropped (§4.3)",
                    field.0, field.1
                ));
                return;
            }
        }
        surface.ok(format!("{uri}: nested chart spec is complete"));
    } else {
        surface.ok(format!(
            "{uri}: flat chart form — chart_type/x_axis/y_axis/series default to \
             line/bucket_start/value/[\"value\"] (§4.3)"
        ));
    }

    match client.read_json(uri).await {
        Ok(body) => {
            if !body.get("spec").map(Value::is_object).unwrap_or(false) {
                surface.fail(format!(
                    "{uri}: chart body is missing the 'spec' object (§4.3)"
                ));
                return;
            }
            let Some(rows) = body.get("rows").and_then(Value::as_array) else {
                surface.fail(format!(
                    "{uri}: chart body is missing the 'rows' array (§4.3)"
                ));
                return;
            };
            if !rows.iter().all(Value::is_object) {
                surface.fail(format!(
                    "{uri}: every chart row must be a JSON object (§4.3)"
                ));
                return;
            }
            let size = body.to_string().len();
            if size > 2 * 1024 * 1024 {
                surface.fail(format!(
                    "{uri}: chart body is {size} bytes. v1 requires ≤ 2 MiB; IONe does not \
                     enforce it yet, and beginning to is an additive change (§4.3)"
                ));
                return;
            }
            surface.ok(format!(
                "{uri}: chart panel renders ({} row(s), {size} bytes)",
                rows.len()
            ));
        }
        Err(err) => surface.fail(format!("{uri}: {err}")),
    }
}

async fn check_table_resource(surface: &mut Surface, client: &McpClient, uri: &str) {
    match client.read_json(uri).await {
        Ok(body) => {
            let Some(schema) = body.get("schema").and_then(Value::as_array) else {
                surface.fail(format!(
                    "{uri}: table body is missing the 'schema' array (§4.4)"
                ));
                return;
            };
            let Some(rows) = body.get("rows").and_then(Value::as_array) else {
                surface.fail(format!(
                    "{uri}: table body is missing the 'rows' array (§4.4)"
                ));
                return;
            };
            if schema.len() > 64 {
                surface.fail(format!(
                    "{uri}: {} columns exceeds MAX_TABLE_COLUMNS = 64 → 413 (§4.4)",
                    schema.len()
                ));
                return;
            }
            if rows.len() > 5_000 {
                surface.fail(format!(
                    "{uri}: {} rows exceeds MAX_TABLE_ROWS = 5 000 → 413 (§4.4)",
                    rows.len()
                ));
                return;
            }
            for column in schema {
                match column.get("name").and_then(Value::as_str) {
                    Some(name) if !name.trim().is_empty() => {}
                    _ => {
                        surface.fail(format!(
                            "{uri}: every schema column needs a non-empty 'name' (§4.4)"
                        ));
                        return;
                    }
                }
                if let Some(kind) = column.get("type").and_then(Value::as_str) {
                    if !matches!(kind, "string" | "number" | "boolean" | "datetime") {
                        surface.fail(format!(
                            "{uri}: column type '{kind}' is not string|number|boolean|datetime \
                             (§4.4)"
                        ));
                        return;
                    }
                }
            }
            if !rows.iter().all(Value::is_object) {
                surface.fail(format!(
                    "{uri}: every table row must be a JSON object (§4.4)"
                ));
                return;
            }
            let size = body.to_string().len();
            if size > 2 * 1024 * 1024 {
                surface.fail(format!(
                    "{uri}: table body is {size} bytes, over MAX_TABLE_RESOURCE_BYTES = 2 MiB → \
                     413 (§4.4)"
                ));
                return;
            }
            surface.ok(format!(
                "{uri}: table panel renders ({} column(s), {} row(s), {size} bytes)",
                schema.len(),
                rows.len()
            ));
        }
        Err(err) => surface.fail(format!("{uri}: {err}")),
    }
}

fn check_document_resource(surface: &mut Surface, uri: &str, resource: &Value, metadata: &Value) {
    let Some(download_url) = metadata.get("download_url").and_then(Value::as_str) else {
        surface.fail(format!("{uri}: download_url is required (§4.5)"));
        return;
    };
    if let Err(err) = validate_document_url(download_url) {
        surface.fail(format!("{uri}: {err}"));
        return;
    }
    let mime = metadata
        .get("mime_type")
        .or_else(|| metadata.get("mimeType"))
        .or_else(|| resource.get("mimeType"))
        .and_then(Value::as_str);
    match mime {
        Some(mime) => surface.ok(format!("{uri}: document panel renders (mime '{mime}')")),
        None => surface.fail(format!(
            "{uri}: no mime type resolves from metadata.mime_type, metadata.mimeType or the \
             resource's top-level mimeType, so the document is dropped (§4.5)"
        )),
    }
}

/// Mirrors `src/services/document_panels.rs` + `src/util/url_guard.rs`: https
/// only, and link-local blocked for every scheme.
fn validate_document_url(raw: &str) -> Result<(), String> {
    let url = Url::parse(raw).map_err(|err| format!("download_url '{raw}' is not a URL: {err}"))?;
    let host = url
        .host()
        .ok_or_else(|| format!("download_url '{raw}' has no host (§4.5)"))?;
    let link_local = match host {
        Host::Ipv4(ip) => Ipv4Addr::is_link_local(&ip),
        Host::Ipv6(ip) => (Ipv6Addr::segments(&ip)[0] & 0xffc0) == 0xfe80,
        Host::Domain(_) => false,
    };
    if link_local {
        return Err(format!(
            "download_url '{raw}' targets a link-local host; IONe's SSRF guard drops it (§4.5)"
        ));
    }
    if url.scheme() != "https" {
        return Err(format!(
            "download_url '{raw}' uses scheme '{}'; §4.5 requires https, including for on-prem \
             hosts. IONe drops the panel with a warn! and no operator-visible diagnostic.",
            url.scheme()
        ));
    }
    Ok(())
}

// ─── Surface 5 — context slice (§5) ───────────────────────────────────────────

async fn check_slice(client: &McpClient, peer_answered: bool) -> Surface {
    let mut surface = Surface::new(5, "Context slice (slice://)", "§5");

    // Surface 5 is Recommended, not Required: §5.2 has IONe synthesize a
    // schema_version "0" slice for a peer that serves none, which is why the
    // contract's surface table grades it that way. A peer that declines the
    // surface is conforming, so its absence is a SKIP. Everything below —
    // schema_version, summary, size, sentinels — still FAILs, because a peer
    // that *does* serve slice:// has to serve a valid one.
    let body = match client.read_json("slice://").await {
        Ok(body) => body,
        Err(err) if peer_answered => {
            surface.skip(format!(
                "{err}. Surface 5 is Recommended, not Required: §5.2 lets IONe fall back to a \
                 synthesized schema_version \"0\" slice. But then your capability description is \
                 IONe's guess, not yours."
            ));
            return surface;
        }
        Err(err) => {
            surface.fail(format!(
                "{err}. The endpoint answered no MCP listing either (see surface 1), so this is \
                 not a peer declining an optional surface."
            ));
            return surface;
        }
    };

    match client.read_mime("slice://").await {
        Ok(mime) => surface.check(
            mime == "application/vnd.ione.slice+json",
            "mimeType is application/vnd.ione.slice+json",
            format!("mimeType is '{mime}', expected application/vnd.ione.slice+json (§5)"),
        ),
        Err(err) => surface.fail(err),
    }

    surface.check(
        body.get("schema_version").and_then(Value::as_str) == Some("1"),
        "schema_version is \"1\" (peer-authored)",
        format!(
            "schema_version must be the string \"1\"; \"0\" is reserved for IONe-synthesized \
             slices (§5.2). Got {:?}",
            body.get("schema_version")
        ),
    );
    surface.check(
        body.get("summary")
            .and_then(Value::as_str)
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false),
        "summary is present and non-empty",
        "summary is required — one paragraph, target 80–120 tokens (§5)",
    );

    let size = body.to_string().len();
    surface.check(
        size <= 2048,
        format!("slice is {size} bytes (limit 2 048)"),
        format!(
            "slice is {size} bytes. IONe truncates to MAX_SLICE_BYTES = 2 048 on a UTF-8 boundary \
             and does not reject, so you silently lose the tail of your own description (§5.1)"
        ),
    );

    if let Some(tools) = body.get("tool_index").and_then(Value::as_array) {
        let malformed = tools
            .iter()
            .filter(|tool| tool.get("name").and_then(Value::as_str).is_none())
            .count();
        surface.check(
            malformed == 0,
            format!("tool_index describes {} tool(s)", tools.len()),
            format!("{malformed} tool_index entr(ies) have no 'name' (§5)"),
        );
    }

    let serialized = body.to_string();
    surface.check(
        !serialized.contains("<<<IONE_PEER_SLICE")
            && !serialized.contains("<<<END_IONE_PEER_SLICE>>>"),
        "no prompt-fence sentinel substrings",
        "the slice contains an IONe prompt-fence sentinel. IONe strips it before insertion, and \
         injecting model instructions through the slice is a contract violation (§5.3)",
    );

    surface
}

// ─── Surface 6 — whoami:// (§6) ───────────────────────────────────────────────

async fn check_whoami(client: &McpClient) -> Surface {
    let mut surface = Surface::new(6, "whoami:// resource", "§6");

    let body = match client.read_json("whoami://").await {
        Ok(body) => body,
        Err(err) => {
            surface.fail(format!(
                "{err}. Without whoami:// IONe materializes a 'pending' binding the operator must \
                 complete by hand (§6)."
            ));
            return surface;
        }
    };

    match client.read_mime("whoami://").await {
        Ok(mime) => surface.check(
            mime == "application/vnd.ione.whoami+json",
            "mimeType is application/vnd.ione.whoami+json",
            format!("mimeType is '{mime}', expected application/vnd.ione.whoami+json (§6)"),
        ),
        Err(err) => surface.fail(err),
    }

    let Some(object) = body.as_object() else {
        surface.fail("contents[0].text must parse to a JSON object (§6)");
        return surface;
    };

    // §6: all seven keys must be present. A value may be null, and a consumer
    // must not treat a missing key and a null value as equivalent.
    for key in [
        "peer_id",
        "foreign_tenant_id",
        "foreign_tenant_name",
        "foreign_workspace_id",
        "foreign_user_id",
        "foreign_user_email",
        "foreign_roles",
    ] {
        surface.check(
            object.contains_key(key),
            format!("{key} is present"),
            format!(
                "{key} is missing. All seven §6 keys must be present; use an explicit null where \
                 the scope does not exist."
            ),
        );
    }

    match object.get("foreign_tenant_id").and_then(Value::as_str) {
        Some(tenant) if !tenant.is_empty() => surface.ok(format!(
            "foreign_tenant_id '{tenant}' is a non-empty binding key"
        )),
        _ => surface.fail(
            "foreign_tenant_id must be a non-empty string — IONe fails the whoami outright \
             without it, and it is the key that must match your webhook envelopes (§6, §3.3)",
        ),
    }

    match object.get("foreign_roles") {
        Some(Value::Array(_)) | Some(Value::Null) | None => {}
        Some(other) => surface.fail(format!(
            "foreign_roles must be an array of role names (it may be empty), got {other} (§6)"
        )),
    }

    surface
}

// ─── Entry point ──────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return if args.is_empty() {
            ExitCode::from(2)
        } else {
            ExitCode::SUCCESS
        };
    }

    let options = match parse_options(args) {
        Ok(options) => options,
        Err(err) => {
            eprintln!("error: {err}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    println!("IONe peer conformance kit — contract v1");
    println!("Peer MCP endpoint: {}", options.url);
    println!();

    let mut client = McpClient::new(&options);
    let (surface1, resources) = check_mcp_endpoint(&mut client).await;
    let surface2 = check_oauth(&options).await;
    let surface3 = check_webhook_sender(&options).await;
    let surface4 = check_view_metadata(&client, resources.as_deref()).await;
    let surface5 = check_slice(&client, resources.is_some()).await;
    let surface6 = check_whoami(&client).await;

    let surfaces = [surface1, surface2, surface3, surface4, surface5, surface6];
    for surface in &surfaces {
        surface.print();
    }

    let passed = surfaces
        .iter()
        .filter(|s| s.status() == Status::Pass)
        .count();
    let failed = surfaces
        .iter()
        .filter(|s| s.status() == Status::Fail)
        .count();
    let skipped = surfaces
        .iter()
        .filter(|s| s.status() == Status::Skip)
        .count();
    println!("{passed} passed, {failed} failed, {skipped} skipped");

    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEER_ID: &str = "3f0c1f2e-0000-4000-8000-000000000001";

    /// A §3.3-conforming envelope. Individual cases mutate one field so a failure
    /// is attributable to that field rather than to the fixture.
    fn conforming_envelope(timestamp: i64) -> Value {
        json!({
            "id": "evt-conformance",
            "type": "alert.created",
            "occurred_at": DateTime::from_timestamp(timestamp, 0)
                .expect("valid timestamp")
                .to_rfc3339(),
            "peer_id": PEER_ID,
            "foreign_tenant_id": "tenant-abc",
            "severity": "routine",
            "data": { "message": "hello" },
            "approval_required": false
        })
    }

    fn check(envelope: &Value) -> Surface {
        let timestamp = Utc::now().timestamp();
        let mut surface = Surface::new(3, "Signed webhook sender", "§3");
        verify_envelope(
            &mut surface,
            &serde_json::to_vec(envelope).expect("serialize"),
            PEER_ID,
            timestamp,
        );
        surface
    }

    /// The control: the kit must not fail an envelope that satisfies every §3.3
    /// constraint, otherwise the cases below prove nothing.
    #[test]
    fn a_conforming_envelope_passes_every_envelope_check() {
        let surface = check(&conforming_envelope(Utc::now().timestamp()));
        assert_eq!(
            surface.failures, 0,
            "a conforming envelope must pass: {:?}",
            surface.lines
        );
    }

    /// §3.3 freezes `approval_required` as optional, defaulting to `false`. The kit
    /// is what an external peer runs to validate *itself*, so failing this would
    /// tell a conforming peer it is broken. The stub always emits the field, which
    /// is exactly why this case is asserted here rather than through the stub.
    #[test]
    fn an_absent_approval_required_passes_because_the_field_is_optional() {
        let mut envelope = conforming_envelope(Utc::now().timestamp());
        envelope
            .as_object_mut()
            .expect("object")
            .remove("approval_required");
        let surface = check(&envelope);
        assert_eq!(
            surface.failures, 0,
            "§3.3: an absent approval_required defaults to false and must pass: {:?}",
            surface.lines
        );
    }

    /// Optional is not "anything goes": present-but-not-boolean still fails
    /// deserialization inside IONe and returns a bare 400.
    #[test]
    fn a_non_boolean_approval_required_still_fails() {
        for bad in [json!("true"), json!(1), json!(null)] {
            let mut envelope = conforming_envelope(Utc::now().timestamp());
            envelope["approval_required"] = bad.clone();
            let surface = check(&envelope);
            assert_eq!(
                surface.failures, 1,
                "§3.3: approval_required = {bad} must fail: {:?}",
                surface.lines
            );
        }
    }

    // ── signature digest case ─────────────────────────────────────────────

    const SECRET: &str = "whsec-conformance";

    fn signature_header(timestamp: i64, body: &[u8], uppercase: bool) -> String {
        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(SECRET.as_bytes()).expect("hmac key");
        mac.update(timestamp.to_string().as_bytes());
        mac.update(b".");
        mac.update(body);
        let digest = hex::encode(mac.finalize().into_bytes());
        let digest = if uppercase {
            digest.to_ascii_uppercase()
        } else {
            digest
        };
        format!("t={timestamp},v1={digest}")
    }

    /// The control for the two cases below.
    #[test]
    fn a_lowercase_hex_digest_verifies() {
        let body = br#"{"id":"evt-1"}"#;
        let timestamp = Utc::now().timestamp();
        assert_eq!(
            verify_signature_header(&signature_header(timestamp, body, false), SECRET, body),
            Ok(timestamp)
        );
    }

    /// `webhooks.rs:194` decodes the digest with `hex::decode`, which accepts
    /// `A-F`. The contract's §3.1 table marks lowercase "Enforced"; production
    /// does not enforce it. The kit follows the code, because a peer that sends
    /// uppercase hex is accepted by IONe and must not be told it is broken.
    #[test]
    fn an_uppercase_hex_digest_verifies_because_production_hex_decodes_it() {
        let body = br#"{"id":"evt-1"}"#;
        let timestamp = Utc::now().timestamp();
        assert_eq!(
            verify_signature_header(&signature_header(timestamp, body, true), SECRET, body),
            Ok(timestamp)
        );
    }

    /// `parse_signature` trims each key and value, so a sender that puts a space
    /// after the comma is accepted by IONe.
    #[test]
    fn whitespace_around_signature_components_is_trimmed_as_production_does() {
        let body = br#"{"id":"evt-1"}"#;
        let timestamp = Utc::now().timestamp();
        let spaced = signature_header(timestamp, body, false).replace(",v1=", ", v1=");
        assert_eq!(
            verify_signature_header(&spaced, SECRET, body),
            Ok(timestamp)
        );
    }

    /// Accepting either case is not accepting anything: a wrong digest, and a
    /// digest of the wrong length, still fail.
    #[test]
    fn a_wrong_or_short_digest_still_fails() {
        let body = br#"{"id":"evt-1"}"#;
        let timestamp = Utc::now().timestamp();
        assert!(verify_signature_header(
            &format!("t={timestamp},v1={}", "a".repeat(64)),
            SECRET,
            body
        )
        .is_err());
        assert!(
            verify_signature_header(&format!("t={timestamp},v1=DEADBEEF"), SECRET, body).is_err()
        );
    }

    // ── paginating resources/list ─────────────────────────────────────────

    /// A peer whose `resources/list` returns two pages, and is otherwise
    /// surface-1 conforming.
    async fn spawn_paginating_peer() -> String {
        async fn rpc(Json(body): Json<Value>) -> Json<Value> {
            let id = body.get("id").cloned().unwrap_or(json!(1));
            let method = body.get("method").and_then(Value::as_str).unwrap_or("");
            let cursor = body.pointer("/params/cursor").and_then(Value::as_str);
            let result = match (method, cursor) {
                ("initialize", _) => json!({ "protocolVersion": "2025-11-25" }),
                ("tools/list", _) => json!({ "tools": [{ "name": "probe" }] }),
                ("resources/list", None) => json!({
                    "resources": [{ "uri": "peer://page-1" }],
                    "nextCursor": "page-2"
                }),
                ("resources/list", Some(_)) => json!({
                    "resources": [{ "uri": "peer://page-2" }],
                    "nextCursor": null
                }),
                ("resources/read", _) => {
                    return Json(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32002, "message": "resource not found" }
                    }))
                }
                ("tools/call", _) => json!({ "content": [] }),
                _ => {
                    return Json(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32601, "message": "method not found" }
                    }))
                }
            };
            Json(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
        }

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub peer");
        let addr = listener.local_addr().expect("stub peer addr");
        let router = Router::new().route("/mcp", post(rpc));
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        format!("http://{addr}/mcp")
    }

    fn options_for(url: String) -> Options {
        Options {
            url,
            token: None,
            pre_brokered: true,
            oauth_issuer: None,
            webhook_peer_id: None,
            webhook_secret: None,
            webhook_trigger: None,
            webhook_timeout: 1,
        }
    }

    /// §8.2 requires a v1 peer to paginate `resources/list`, and IONe follows
    /// `nextCursor` on all four panel paths (issue #18). The kit used to FAIL a
    /// peer that did exactly that — the same class of bug the pagination work
    /// had just fixed, pointed the other way.
    #[tokio::test]
    async fn a_paginating_resources_list_passes_surface_one() {
        let options = options_for(spawn_paginating_peer().await);
        let mut client = McpClient::new(&options);
        let (surface, resources) = check_mcp_endpoint(&mut client).await;
        assert_eq!(
            surface.status(),
            Status::Pass,
            "a paginating peer is conforming: {:?}",
            surface.lines
        );
        assert_eq!(
            resources.as_deref().map(<[Value]>::len),
            Some(2),
            "both pages must be collected: {resources:?}"
        );
    }

    /// Surface 4 is "Required to render in the shell" and surface 5 is
    /// "Recommended" — a reachable tools-only peer that serves neither is
    /// conforming, so both SKIP. The hard-failure case (an endpoint that is not
    /// an MCP peer at all) is still a FAIL, and is pinned by
    /// `tests/stub_peer_conformance_integration.rs`.
    #[tokio::test]
    async fn a_tools_only_peer_skips_the_optional_surfaces_rather_than_failing_them() {
        let options = options_for(spawn_paginating_peer().await);
        let mut client = McpClient::new(&options);
        let (_, resources) = check_mcp_endpoint(&mut client).await;

        let view = check_view_metadata(&client, resources.as_deref()).await;
        assert_eq!(
            view.status(),
            Status::Skip,
            "no ione_view is conforming for a tools-only peer: {:?}",
            view.lines
        );

        let slice = check_slice(&client, resources.is_some()).await;
        assert_eq!(
            slice.status(),
            Status::Skip,
            "an absent slice:// is conforming (§5.2): {:?}",
            slice.lines
        );

        // The same two surfaces against an endpoint that answered nothing.
        assert_eq!(
            check_view_metadata(&client, None).await.status(),
            Status::Fail
        );
        assert_eq!(check_slice(&client, false).await.status(), Status::Fail);
    }
}
