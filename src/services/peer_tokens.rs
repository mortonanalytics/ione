use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use anyhow::{Context, Result};
use dashmap::DashMap;
use once_cell::sync::Lazy;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::{
    models::Peer, repos::PeerRepo, services::peer_oauth::PeerDiscovery, util::token_crypto,
};

const REFRESH_SKEW_SECONDS: i64 = 60;

/// MCP revision IONe's outbound client speaks. Sent as `MCP-Protocol-Version`
/// on every POST (not just the SSE GET): a spec-conforming server is allowed to
/// reject or misroute a request that omits it once a session is negotiated.
pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

/// The `MCP-Protocol-Version` header name, as spelled by the streamable-HTTP transport.
pub const MCP_PROTOCOL_VERSION_HEADER: &str = "MCP-Protocol-Version";

/// The streamable-HTTP transport lets a server answer a POST with either a plain
/// JSON body or an SSE stream carrying the JSON-RPC reply. The client must
/// advertise that it accepts both, or a conforming server may refuse the request.
pub const MCP_ACCEPT: &str = "application/json, text/event-stream";

/// Monotonic source of JSON-RPC request ids. Every outbound request gets its own
/// id so a reply can be matched to the call that produced it; a hardcoded id
/// makes an out-of-order or unrelated reply indistinguishable from the real one.
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Allocate a fresh JSON-RPC request id.
pub fn next_request_id() -> Value {
    Value::from(NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed))
}

/// Decode one SSE line into its `data:` payload, if it carries one.
///
/// Shared by the long-lived notification stream (which parses line-at-a-time off
/// a byte stream) and by `read_jsonrpc_reply` (which parses a buffered POST
/// body), so SSE framing is interpreted in exactly one place.
pub fn sse_data_payload(line: &str) -> Option<&str> {
    let data = line.strip_prefix("data:")?.trim();
    (!data.is_empty()).then_some(data)
}

/// Read the JSON-RPC reply to a request whose id is `expected_id`.
///
/// A spec-conforming MCP server may answer a POST with `application/json` (one
/// JSON-RPC object) or with `text/event-stream`, delivering the reply in a
/// `data:` frame that may be preceded by unrelated server-initiated requests and
/// notifications. Both are accepted here; the reply is selected by id so an
/// unrelated message is never mis-attributed to this call.
pub async fn read_jsonrpc_reply(resp: reqwest::Response, expected_id: &Value) -> Result<Value> {
    let is_sse = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("text/event-stream")
        })
        .unwrap_or(false);
    let body = resp
        .text()
        .await
        .context("failed to read peer JSON-RPC response body")?;
    if is_sse {
        select_sse_reply(&body, expected_id)
    } else {
        let message: Value =
            serde_json::from_str(&body).context("peer JSON-RPC response is not valid JSON")?;
        match_reply_id(message, expected_id)
    }
}

/// Split an SSE body into one payload per event.
///
/// The SSE spec lets a single event carry several `data:` lines, which a client
/// joins with newlines into one payload. A server that pretty-prints its
/// JSON-RPC reply does exactly that, and treating each line as a whole message
/// turns a conforming reply into a parse failure.
pub fn sse_event_payloads(body: &str) -> Vec<String> {
    let mut events = Vec::new();
    let mut pending: Vec<&str> = Vec::new();
    for line in body.lines() {
        // A blank line terminates the event.
        if line.trim().is_empty() {
            if !pending.is_empty() {
                events.push(pending.join("\n"));
                pending.clear();
            }
            continue;
        }
        if let Some(data) = sse_data_payload(line) {
            pending.push(data);
        }
    }
    if !pending.is_empty() {
        events.push(pending.join("\n"));
    }
    events
}

/// Pick the frame that answers our request out of an SSE-framed POST response.
fn select_sse_reply(body: &str, expected_id: &Value) -> Result<Value> {
    let mut null_id_error: Option<Value> = None;
    for data in sse_event_payloads(body) {
        let Ok(message) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        // Server-initiated requests and notifications share the stream; they are
        // not the reply to this call.
        if message.get("method").is_some() {
            continue;
        }
        if message.get("id") == Some(expected_id) {
            return Ok(message);
        }
        if null_id_error.is_none() && is_null_id_error(&message) {
            null_id_error = Some(message);
        }
    }
    null_id_error.with_context(|| {
        format!("peer SSE response carried no JSON-RPC reply for request id {expected_id}")
    })
}

fn match_reply_id(message: Value, expected_id: &Value) -> Result<Value> {
    if message.get("id") == Some(expected_id) {
        return Ok(message);
    }
    // JSON-RPC 2.0: a server that could not determine the request id answers
    // with a null id. That reply is unambiguous, so it is surfaced rather than
    // swallowed behind a correlation failure.
    if is_null_id_error(&message) {
        return Ok(message);
    }
    anyhow::bail!(
        "peer JSON-RPC reply id {} does not match request id {expected_id}",
        message.get("id").unwrap_or(&Value::Null)
    )
}

fn is_null_id_error(message: &Value) -> bool {
    message.get("id").map(Value::is_null).unwrap_or(true)
        && message
            .get("error")
            .map(|error| !error.is_null())
            .unwrap_or(false)
}
static PEER_GOVERNORS: Lazy<
    DashMap<uuid::Uuid, Arc<crate::services::peer_governor::PeerGovernor>>,
> = Lazy::new(DashMap::new);

#[derive(Debug, Deserialize)]
struct RefreshTokenResp {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

/// Which precedence tier produced an outbound bearer.
///
/// Carried alongside the token so the 401 retry can tell a peer-global OAuth
/// token — which a peer-global refresh legitimately replaces — from a
/// workspace-scoped credential, which it does not. Without this the retry has
/// to guess from `can_refresh(peer)` and silently downgrades a tier-1 or tier-3
/// bearer to the peer-global grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialTier {
    /// Tier 1: brokered delegated token for (`peer.workspace_scope`, peer).
    Delegated,
    /// Tier 2: peer-global brokered OAuth token (`peers.access_token_ciphertext`).
    PeerOauth,
    /// Tier 3: pre-broker static credential for (`peer.workspace_scope`, peer).
    WorkspaceStatic,
    /// Tier 4: process-global `IONE_OAUTH_STATIC_BEARER`.
    EnvStatic,
}

/// An outbound bearer plus the tier it came from.
pub struct ResolvedBearer {
    pub token: String,
    pub tier: CredentialTier,
}

impl ResolvedBearer {
    fn new(token: String, tier: CredentialTier) -> Self {
        Self { token, tier }
    }

    /// A bearer supplied by the caller in place of the tier-4 env fallback —
    /// the `mcp_client` connector's config `bearer_token` literal. It sits at
    /// tier 4, so a 401 against it can no more trigger a peer-global refresh
    /// than a 401 against `IONE_OAUTH_STATIC_BEARER` can.
    pub fn last_resort(token: String) -> Self {
        Self::new(token, CredentialTier::EnvStatic)
    }

    /// Whether a 401 against this bearer may be retried with a freshly
    /// refreshed peer-global OAuth token.
    ///
    /// True only for tier 2, the one credential a peer-global refresh
    /// legitimately replaces. Retrying a tier-1 delegated token or a tier-3
    /// per-workspace credential with the peer-global grant would present a
    /// credential the operator never scoped to this workspace — the silent
    /// downgrade `md/design/pre-broker-peer-credentials.md` rules out. Every
    /// outbound path asks this question here rather than re-deriving it from
    /// `can_refresh(peer)`, which cannot see which tier was used.
    pub fn allows_peer_global_refresh(&self) -> bool {
        self.tier == CredentialTier::PeerOauth
    }
}

/// Outbound bearer precedence, highest first:
///
/// 1. the brokered delegated token for (`peer.workspace_scope`, peer) (issue #12),
/// 2. the peer's brokered OAuth access token (`peers.access_token_ciphertext`),
/// 3. the pre-broker static credential for `peer.workspace_scope` (issue #19),
/// 4. the process-global `IONE_OAUTH_STATIC_BEARER` env fallback.
///
/// The workspace-scoped delegated token outranks the peer-global one because it
/// is the more specific grant: the operator delegated it for exactly this
/// workspace, and a peer-global token that shadowed it would silently widen the
/// scope the operator consented to. When no delegation exists for the handle,
/// tiers 2–4 resolve exactly as they did before #12 — peer-global tokens keep
/// working untouched.
///
/// OAuth outranks the static credential deliberately: a peer that gains a
/// brokered token starts using it on the next request with no flag day and no
/// operator action, and the now-dormant static credential can be deleted at
/// leisure. All four produce the identical `Authorization: Bearer <credential>`
/// header, so the peer cannot tell which mode IONe is in.
pub async fn resolve_access_token(
    pool: &PgPool,
    http: &reqwest::Client,
    peer: &Peer,
) -> Result<String> {
    Ok(resolve_bearer(pool, http, peer).await?.token)
}

/// `resolve_access_token`, but reporting which tier produced the bearer.
pub async fn resolve_bearer(
    pool: &PgPool,
    http: &reqwest::Client,
    peer: &Peer,
) -> Result<ResolvedBearer> {
    match resolve_bearer_above_env_tiers(pool, http, peer).await? {
        Some(bearer) => Ok(bearer),
        None => Ok(ResolvedBearer::new(
            static_bearer()?,
            CredentialTier::EnvStatic,
        )),
    }
}

/// Tiers 1–3 only: the brokered delegated token, the peer-global OAuth token,
/// then the per-(workspace, peer) static credential. `Ok(None)` means none of
/// them applies and the caller supplies the tier-4 last resort.
async fn resolve_bearer_above_env_tiers(
    pool: &PgPool,
    http: &reqwest::Client,
    peer: &Peer,
) -> Result<Option<ResolvedBearer>> {
    if let Some(delegated) = crate::services::peer_delegation::resolve(pool, http, peer).await? {
        return Ok(Some(ResolvedBearer::new(
            delegated,
            CredentialTier::Delegated,
        )));
    }
    if peer.access_token_ciphertext.is_some() {
        let token = if token_is_fresh(peer) {
            decrypt_access_token(peer)?
        } else {
            refresh_access_token(pool, http, peer).await?
        };
        return Ok(Some(ResolvedBearer::new(token, CredentialTier::PeerOauth)));
    }
    Ok(workspace_credential(pool, peer)
        .await?
        .map(|credential| ResolvedBearer::new(credential, CredentialTier::WorkspaceStatic)))
}

/// Tiers 1–3 of `resolve_access_token`, without the tier-4 env fallback.
///
/// For callers that carry their own last resort in place of
/// `IONE_OAUTH_STATIC_BEARER` — the `mcp_client` connector's literal
/// `bearer_token` from connector config. Keeping the precedence chain in this
/// module is what stops that caller from re-deriving a partial ordering, and
/// reporting the tier is what stops it from re-deriving the 401 retry rule.
pub async fn resolve_bearer_above_env(
    pool: &PgPool,
    http: &reqwest::Client,
    peer: &Peer,
) -> Result<Option<ResolvedBearer>> {
    resolve_bearer_above_env_tiers(pool, http, peer).await
}

/// Like `resolve_access_token` but serializes concurrent refresh for a single peer
/// using a per-peer mutex stored in `AppState`. Prevents the token-overwrite race
/// when multiple requests for the same peer race to refresh simultaneously.
pub async fn resolve_access_token_locked(
    state: &crate::state::AppState,
    peer: &Peer,
) -> Result<String> {
    Ok(resolve_bearer_locked(state, peer).await?.token)
}

/// `resolve_access_token_locked`, but reporting which tier produced the bearer.
async fn resolve_bearer_locked(
    state: &crate::state::AppState,
    peer: &Peer,
) -> Result<ResolvedBearer> {
    if let Some(delegated) =
        crate::services::peer_delegation::resolve(&state.pool, &state.http, peer).await?
    {
        return Ok(ResolvedBearer::new(delegated, CredentialTier::Delegated));
    }
    if peer.access_token_ciphertext.is_none() {
        return pre_broker_bearer(&state.pool, peer).await;
    }
    // Fast path: token is fresh — no need to take the lock.
    if token_is_fresh(peer) {
        return Ok(ResolvedBearer::new(
            decrypt_access_token(peer)?,
            CredentialTier::PeerOauth,
        ));
    }
    // Acquire per-peer lock before refresh to serialize concurrent callers.
    let lock = state
        .peer_refresh_locks
        .entry(peer.id)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone();
    let _guard = lock.lock().await;
    // Re-check freshness under the lock: the thread that won the lock may have
    // already refreshed the token, so reload from DB before deciding to refresh.
    let fresh_peer = PeerRepo::new(state.pool.clone())
        .get(peer.id)
        .await
        .ok()
        .flatten();
    let token = if let Some(ref reloaded) = fresh_peer {
        if token_is_fresh(reloaded) {
            decrypt_access_token(reloaded)?
        } else {
            refresh_access_token(&state.pool, &state.http, reloaded).await?
        }
    } else {
        refresh_access_token(&state.pool, &state.http, peer).await?
    };
    Ok(ResolvedBearer::new(token, CredentialTier::PeerOauth))
}

pub async fn refresh_access_token(
    pool: &PgPool,
    http: &reqwest::Client,
    peer: &Peer,
) -> Result<String> {
    let refresh_ciphertext = peer
        .refresh_token_ciphertext
        .as_deref()
        .context("peer has no refresh token ciphertext; re-authorization required")?;
    let refresh_token = token_crypto::decrypt_token(refresh_ciphertext)
        .context("failed to decrypt peer refresh token")?;
    let client_id = peer
        .oauth_client_id
        .as_deref()
        .context("peer has no oauth client id")?;
    let discovery = discover_peer(peer, http).await?;

    let tokens: RefreshTokenResp = http
        .post(&discovery.token_endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", client_id),
        ])
        .send()
        .await
        .context("peer refresh token request failed")?
        .error_for_status()
        .context("peer refresh token status")?
        .json()
        .await
        .context("peer refresh token json")?;

    let access_hash = sha256_hex(&tokens.access_token);
    let access_ciphertext = token_crypto::encrypt_token(&tokens.access_token)
        .context("failed to encrypt refreshed peer access token")?;
    let refresh_hash = tokens.refresh_token.as_deref().map(sha256_hex);
    let refresh_ciphertext = tokens
        .refresh_token
        .as_deref()
        .map(token_crypto::encrypt_token)
        .transpose()
        .context("failed to encrypt refreshed peer refresh token")?;
    let expires_at =
        chrono::Utc::now() + chrono::Duration::seconds(tokens.expires_in.unwrap_or(3600));

    PeerRepo::new(pool.clone())
        .update_refreshed_tokens(
            peer.id,
            &access_hash,
            refresh_hash.as_deref(),
            &access_ciphertext,
            refresh_ciphertext.as_deref(),
            expires_at,
        )
        .await?;

    Ok(tokens.access_token)
}

pub async fn send_mcp_request(
    pool: &PgPool,
    http: &reqwest::Client,
    peer: &Peer,
    endpoint: &str,
    body: &Value,
) -> Result<reqwest::Response> {
    send_mcp_request_with_session(pool, http, peer, endpoint, body, None).await
}

pub async fn send_mcp_request_with_session(
    pool: &PgPool,
    http: &reqwest::Client,
    peer: &Peer,
    endpoint: &str,
    body: &Value,
    mcp_session_id: Option<&str>,
) -> Result<reqwest::Response> {
    let governor = governor_for(peer.id);
    governor.acquire().await?;
    let bearer = resolve_bearer(pool, http, peer).await?;
    let first = match send_with_token(http, endpoint, body, &bearer.token, mcp_session_id).await {
        Ok(response) => {
            record_peer_response(pool, peer, &governor, response.status()).await;
            response
        }
        Err(e) => {
            record_peer_failure(pool, peer, &governor, &e).await;
            return Err(e);
        }
    };
    if !peer_global_refresh_applies(first.status(), &bearer, peer) {
        return Ok(first);
    }

    governor.acquire().await?;
    let token = refresh_access_token(pool, http, peer).await?;
    match send_with_token(http, endpoint, body, &token, mcp_session_id).await {
        Ok(response) => {
            record_peer_response(pool, peer, &governor, response.status()).await;
            Ok(response)
        }
        Err(e) => {
            record_peer_failure(pool, peer, &governor, &e).await;
            Err(e)
        }
    }
}

/// Variant of `send_mcp_request_with_session` that uses the per-peer refresh mutex
/// in `AppState` to prevent concurrent token overwrites on the same peer.
pub async fn send_mcp_request_with_state(
    state: &crate::state::AppState,
    peer: &Peer,
    endpoint: &str,
    body: &Value,
    mcp_session_id: Option<&str>,
) -> Result<reqwest::Response> {
    let governor = governor_for(peer.id);
    governor.acquire().await?;
    let bearer = resolve_bearer_locked(state, peer).await?;
    let first =
        match send_with_token(&state.http, endpoint, body, &bearer.token, mcp_session_id).await {
            Ok(response) => {
                record_peer_response(&state.pool, peer, &governor, response.status()).await;
                response
            }
            Err(e) => {
                record_peer_failure(&state.pool, peer, &governor, &e).await;
                return Err(e);
            }
        };
    if !peer_global_refresh_applies(first.status(), &bearer, peer) {
        return Ok(first);
    }

    governor.acquire().await?;
    let token = refresh_access_token(&state.pool, &state.http, peer).await?;
    match send_with_token(&state.http, endpoint, body, &token, mcp_session_id).await {
        Ok(response) => {
            record_peer_response(&state.pool, peer, &governor, response.status()).await;
            Ok(response)
        }
        Err(e) => {
            record_peer_failure(&state.pool, peer, &governor, &e).await;
            Err(e)
        }
    }
}

pub fn governor_snapshot(
    peer_id: uuid::Uuid,
) -> Option<crate::services::peer_governor::PeerGovernorSnapshot> {
    PEER_GOVERNORS
        .get(&peer_id)
        .map(|entry| entry.value().snapshot())
}

async fn send_with_token(
    http: &reqwest::Client,
    endpoint: &str,
    body: &Value,
    token: &str,
    mcp_session_id: Option<&str>,
) -> Result<reqwest::Response> {
    let mut request = http
        .post(endpoint)
        .header(reqwest::header::ACCEPT, MCP_ACCEPT)
        .header(MCP_PROTOCOL_VERSION_HEADER, MCP_PROTOCOL_VERSION)
        .json(body);
    if !token.is_empty() {
        request = request.bearer_auth(token);
    }
    if let Some(session_id) = mcp_session_id {
        request = request.header("MCP-Session-Id", session_id);
    }
    request.send().await.context("HTTP send failed")
}

/// Streamable HTTP session termination: `DELETE {endpoint}` carrying
/// `MCP-Session-Id`, which asks the peer to release a session IONe opened.
///
/// Presents the same bearer the session was negotiated under, resolved from the
/// peer handle's scope, so a workspace-scoped session is torn down with that
/// workspace's credential rather than a peer-global one.
pub async fn send_mcp_session_delete(
    pool: &PgPool,
    http: &reqwest::Client,
    peer: &Peer,
    endpoint: &str,
    mcp_session_id: &str,
) -> Result<StatusCode> {
    let governor = governor_for(peer.id);
    governor.acquire().await?;
    let bearer = resolve_bearer(pool, http, peer).await?;
    let mut request = http
        .delete(endpoint)
        .header(reqwest::header::ACCEPT, MCP_ACCEPT)
        .header(MCP_PROTOCOL_VERSION_HEADER, MCP_PROTOCOL_VERSION)
        .header("MCP-Session-Id", mcp_session_id);
    if !bearer.token.is_empty() {
        request = request.bearer_auth(&bearer.token);
    }
    let response = request
        .send()
        .await
        .context("MCP session DELETE send failed")?;
    Ok(response.status())
}

/// Send `notifications/initialized`, which the MCP lifecycle requires the client
/// to send once `initialize` succeeds.
///
/// Best-effort by design: the notification carries no id and has no reply worth
/// waiting on, and a peer that answers it with `-32601` (IONe's own server does)
/// must not fail an otherwise-good handshake.
pub async fn send_initialized_notification(
    pool: &PgPool,
    http: &reqwest::Client,
    peer: &Peer,
    endpoint: &str,
    mcp_session_id: &str,
) {
    let body = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    if let Err(e) =
        send_mcp_request_with_session(pool, http, peer, endpoint, &body, Some(mcp_session_id)).await
    {
        tracing::warn!(peer_id = %peer.id, error = %e, "notifications/initialized send failed");
    }
}

fn token_is_fresh(peer: &Peer) -> bool {
    peer.token_expires_at
        .map(|expires_at| {
            expires_at > chrono::Utc::now() + chrono::Duration::seconds(REFRESH_SKEW_SECONDS)
        })
        .unwrap_or(true)
}

fn can_refresh(peer: &Peer) -> bool {
    peer.refresh_token_ciphertext.is_some() && peer.oauth_client_id.is_some()
}

/// Whether a rejected request should be retried with a freshly refreshed
/// peer-global OAuth token: only a 401, only against the peer-global token
/// itself (`ResolvedBearer::allows_peer_global_refresh`), and only when the peer
/// actually holds refresh material.
fn peer_global_refresh_applies(status: StatusCode, bearer: &ResolvedBearer, peer: &Peer) -> bool {
    status == StatusCode::UNAUTHORIZED && bearer.allows_peer_global_refresh() && can_refresh(peer)
}

/// Throttle inbound peer notifications using the peer's governor. Returns false
/// when the peer has exceeded `max_per_minute` notifications in the trailing
/// minute, so callers can drop the flood instead of landing it in `stream_events`.
pub fn protocol_notification_allowed(peer_id: uuid::Uuid, max_per_minute: usize) -> bool {
    governor_for(peer_id).allow_protocol_notification(max_per_minute)
}

fn governor_for(peer_id: uuid::Uuid) -> Arc<crate::services::peer_governor::PeerGovernor> {
    PEER_GOVERNORS
        .entry(peer_id)
        .or_insert_with(|| {
            let rps = std::env::var("IONE_PEER_CALL_RPS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(10);
            let burst = std::env::var("IONE_PEER_CALL_BURST")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(20);
            Arc::new(crate::services::peer_governor::PeerGovernor::new(
                rps, burst,
            ))
        })
        .clone()
}

async fn record_peer_response(
    pool: &PgPool,
    peer: &Peer,
    governor: &crate::services::peer_governor::PeerGovernor,
    status: StatusCode,
) {
    if status.is_server_error() {
        if governor.record_peer_failure() {
            let _ = PeerRepo::new(pool.clone())
                .set_session_status(peer.id, "error", Some("peer circuit breaker opened"))
                .await;
        }
    } else if !status.is_client_error() {
        governor.record_success();
    }
}

async fn record_peer_failure(
    pool: &PgPool,
    peer: &Peer,
    governor: &crate::services::peer_governor::PeerGovernor,
    error: &anyhow::Error,
) {
    if governor.record_peer_failure() {
        let _ = PeerRepo::new(pool.clone())
            .set_session_status(peer.id, "error", Some(&error.to_string()))
            .await;
    }
}

fn decrypt_access_token(peer: &Peer) -> Result<String> {
    let ciphertext = peer
        .access_token_ciphertext
        .as_deref()
        .context("peer access token is unavailable")?;
    token_crypto::decrypt_token(ciphertext).context("failed to decrypt peer access token")
}

/// Precedence tiers 3 and 4: the per-(workspace, peer) static credential when
/// this peer handle carries a workspace scope, else the process-global env
/// bearer. Peer-global handles (`workspace_scope == None`) skip tier 3 — there
/// is no workspace to resolve a credential for, and guessing one would present
/// a credential the operator scoped to a different workspace.
async fn pre_broker_bearer(pool: &PgPool, peer: &Peer) -> Result<ResolvedBearer> {
    match workspace_credential(pool, peer).await? {
        Some(credential) => Ok(ResolvedBearer::new(
            credential,
            CredentialTier::WorkspaceStatic,
        )),
        None => Ok(ResolvedBearer::new(
            static_bearer()?,
            CredentialTier::EnvStatic,
        )),
    }
}

/// The pre-broker static credential for this peer handle's workspace scope, if
/// the handle carries one and a credential is stored for it.
pub async fn workspace_credential(pool: &PgPool, peer: &Peer) -> Result<Option<String>> {
    let Some(workspace_id) = peer.workspace_scope else {
        return Ok(None);
    };
    crate::repos::WorkspacePeerCredentialRepo::new(pool.clone())
        .secret_for(workspace_id, peer.id)
        .await
}

fn static_bearer() -> Result<String> {
    std::env::var("IONE_OAUTH_STATIC_BEARER")
        .context("peer has no token and IONE_OAUTH_STATIC_BEARER is not set")
}

/// Resolve the peer's authorization-server metadata the same way the join path
/// does: RFC 8414 origin location first, legacy `{mcp_url}/.well-known/…` only as
/// a fallback. Rebuilding just the legacy URL here meant a peer publishing solely
/// at the origin could join successfully and then fail every token refresh.
async fn discover_peer(peer: &Peer, http: &reqwest::Client) -> Result<PeerDiscovery> {
    let origin_url = crate::services::peer_oauth::origin_discovery_url(&peer.mcp_url)
        .map_err(|e| anyhow::anyhow!("invalid peer mcp_url for discovery: {e:?}"))?;
    let legacy_url = format!(
        "{}/.well-known/oauth-authorization-server",
        peer.mcp_url.trim_end_matches('/')
    );

    let mut last_err = None;
    for url in [origin_url.as_str(), legacy_url.as_str()] {
        match fetch_discovery_at(url, http).await {
            Ok(discovery) => {
                if url == legacy_url {
                    tracing::warn!(
                        peer_id = %peer.id,
                        "peer serves OAuth metadata at the deprecated {{mcp_url}}/.well-known \
                         location; RFC 8414 places it at the origin"
                    );
                }
                verify_refresh_endpoint_host(peer, &discovery)?;
                return Ok(discovery);
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("peer discovery failed")))
}

async fn fetch_discovery_at(url: &str, http: &reqwest::Client) -> Result<PeerDiscovery> {
    http.get(url)
        .send()
        .await
        .context("peer discovery request failed")?
        .error_for_status()
        .context("peer discovery status")?
        .json()
        .await
        .context("peer discovery json")
}

fn verify_refresh_endpoint_host(peer: &Peer, discovery: &PeerDiscovery) -> Result<()> {
    let peer_url = url::Url::parse(&peer.mcp_url).context("invalid peer mcp_url")?;
    let peer_host = peer_url
        .host_str()
        .context("peer mcp_url missing host")?
        .to_string();
    let token_url =
        url::Url::parse(&discovery.token_endpoint).context("invalid peer token endpoint")?;
    let token_host = token_url
        .host_str()
        .context("peer token endpoint missing host")?
        .to_string();
    anyhow::ensure!(token_host == peer_host, "peer token endpoint host mismatch");
    anyhow::ensure!(
        token_url.scheme() == peer_url.scheme(),
        "peer token endpoint scheme mismatch"
    );
    Ok(())
}

fn sha256_hex(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        write!(hex, "{byte:02x}").unwrap();
    }
    hex
}

#[cfg(test)]
mod sse_tests {
    use super::{select_sse_reply, sse_event_payloads};
    use serde_json::json;

    /// The SSE spec joins an event's `data:` lines with newlines. A server that
    /// pretty-prints its JSON-RPC reply emits exactly this, and reading each
    /// line as a whole message turned a conforming reply into a parse failure.
    #[test]
    fn a_reply_split_across_data_lines_is_reassembled() {
        let body = "event: message\ndata: {\ndata:   \"jsonrpc\": \"2.0\",\ndata:   \"id\": 7,\ndata:   \"result\": {\"ok\": true}\ndata: }\n\n";
        let reply = select_sse_reply(body, &json!(7)).expect("reply should be found");
        assert_eq!(reply["result"]["ok"], json!(true));
    }

    #[test]
    fn a_single_line_reply_is_unaffected() {
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n";
        let reply = select_sse_reply(body, &json!(1)).expect("reply should be found");
        assert_eq!(reply["id"], json!(1));
    }

    /// Events are separated by a blank line, so an unrelated notification ahead
    /// of the reply must not be glued onto it.
    #[test]
    fn events_do_not_bleed_into_each_other() {
        let body = concat!(
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\"}\n",
            "\n",
            "data: {\n",
            "data:   \"jsonrpc\": \"2.0\",\n",
            "data:   \"id\": 2,\n",
            "data:   \"result\": {}\n",
            "data: }\n",
            "\n",
        );
        assert_eq!(sse_event_payloads(body).len(), 2);
        let reply = select_sse_reply(body, &json!(2)).expect("reply should be found");
        assert_eq!(reply["id"], json!(2));
    }

    /// A stream that ends without a trailing blank line still has an event.
    #[test]
    fn a_final_event_without_a_terminator_still_parses() {
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{}}";
        let reply = select_sse_reply(body, &json!(3)).expect("reply should be found");
        assert_eq!(reply["id"], json!(3));
    }
}
