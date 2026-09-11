//! The server-side client for relay.
//!
//! Every browser request stays on IONe. This client is the only thing that
//! talks to relay, it runs server-side, and the credentials it holds never
//! reach a response or a static asset. That is the whole reason the Data Query
//! surface is a proxy rather than a direct call from the page.
//!
//! Two properties worth naming:
//!
//! The envelope carries both the initiating actor and the executing service
//! account, and relay attributes the run to the former while authorizing it
//! against the latter's grants. Collapsing them would make every run look like
//! it came from the service account, which is a true statement about mechanism
//! and a false one about who asked.
//!
//! Nothing the browser sends decides scope. The org, workspace, and actor come
//! from `AuthContext`, which came from a session IONe verified. A caller who
//! puts a different workspace in the body gets their own workspace's data.

use std::sync::Arc;
use std::time::Duration;

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::RelayConfig;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug)]
pub enum RelayError {
    /// Relay is not configured for this deployment. Distinct from an outage:
    /// the surface should be absent, not broken.
    NotConfigured,
    Unreachable(String),
    /// Relay refused, with its stable code. Passed through so the UI can say
    /// what happened rather than "something went wrong".
    Refused {
        code: String,
        message: String,
    },
    Unexpected(String),
}

impl std::fmt::Display for RelayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelayError::NotConfigured => write!(f, "relay is not configured"),
            RelayError::Unreachable(_) => write!(f, "relay is unreachable"),
            RelayError::Refused { code, message } => write!(f, "{code}: {message}"),
            RelayError::Unexpected(_) => write!(f, "relay returned an unexpected response"),
        }
    }
}

/// The identity a request runs under. Assembled from `AuthContext`, never from
/// anything the browser sent.
#[derive(Debug, Clone)]
pub struct RelayScope {
    pub tenant_id: Uuid,
    pub workspace_id: Uuid,
    /// The person. Preserved through to relay's audit.
    pub actor_id: String,
    /// The account whose relay grants authorize the read.
    pub service_account_id: String,
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SourceSelection {
    pub alias: String,
    pub connection_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    rename_all(deserialize = "camelCase", serialize = "snake_case"),
    deny_unknown_fields
)]
pub struct Publication {
    pub destination_id: Uuid,
    pub dataset_name: String,
    pub ttl_seconds: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct CreateRun {
    pub ask: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication: Option<Publication>,
    pub sources: Vec<SourceSelection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipe_version_id: Option<Uuid>,
    pub result_mode: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub delivery: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limits: Option<serde_json::Value>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeVersion {
    pub recipe_id: Uuid,
    pub version_id: Uuid,
    pub version: u64,
    pub name: String,
    pub ask: String,
    #[serde(default)]
    pub clarifications: Vec<RecipeClarification>,
    pub sources: Vec<SourceSelection>,
    pub output_schema: serde_json::Value,
    pub definition_hash: String,
    pub dataset_id: Uuid,
    pub dataset_version_id: Uuid,
    pub run_id: Uuid,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeClarification {
    pub question: String,
    pub answer: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AvailableConnections {
    pub connections: Vec<serde_json::Value>,
}

pub struct DatasetReadResponse {
    pub bytes: Vec<u8>,
    pub source_digest: String,
    pub content_digest: String,
    pub row_count: u64,
}

pub struct RelayClient {
    http: reqwest::Client,
    config: Arc<RelayConfig>,
}

impl std::fmt::Debug for RelayClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayClient")
            .field("base_url", &self.config.base_url)
            .finish()
    }
}

impl RelayClient {
    pub fn new(config: Arc<RelayConfig>) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms))
            .connect_timeout(Duration::from_secs(5))
            // Relay is a configured host. A redirect from it is a redirect to
            // somewhere nobody configured.
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self { http, config })
    }

    pub fn base_url(&self) -> &str {
        &self.config.base_url
    }

    /// Sign a request envelope the way relay verifies one: length-prefixed
    /// fields in a fixed order, so two different envelopes cannot serialize to
    /// the same bytes by running together.
    fn sign(
        &self,
        method: &str,
        path: &str,
        scope: &RelayScope,
        request_id: &str,
        nonce: &str,
        timestamp: i64,
        body: &[u8],
    ) -> (String, String) {
        let body_hash = {
            let mut h = Sha256::new();
            h.update(body);
            hex::encode(h.finalize())
        };

        // Roles ride inside the actor field, which keeps the signature over a
        // fixed field set while still binding them. A role claim outside the
        // signature is a role claim the caller could add.
        let mut roles = scope.roles.clone();
        roles.sort();
        let signed_actor = format!("{}|roles={}", scope.actor_id, roles.join(","));

        let mut canonical = Vec::with_capacity(256);
        let mut push = |s: &str| {
            canonical.extend_from_slice(&(s.len() as u32).to_be_bytes());
            canonical.extend_from_slice(s.as_bytes());
        };
        push("relay.sig.v1");
        push(method);
        push(path);
        push(&self.config.deployment_id.to_string());
        push(&scope.tenant_id.to_string());
        push(&scope.workspace_id.to_string());
        push(&signed_actor);
        push(&scope.service_account_id);
        push(request_id);
        push(nonce);
        push(&body_hash);
        canonical.extend_from_slice(&timestamp.to_be_bytes());

        let mut mac = HmacSha256::new_from_slice(self.config.signing_key.as_bytes())
            .expect("hmac accepts any key length");
        mac.update(&canonical);
        let signature = base64_encode(&mac.finalize().into_bytes());

        let scope_header = serde_json::json!({
            "deployment_id": self.config.deployment_id,
            "tenant_id": scope.tenant_id,
            "workspace_id": scope.workspace_id,
            "actor_id": scope.actor_id,
            "service_account_id": scope.service_account_id,
            "roles": scope.roles,
            "request_id": request_id,
            "timestamp": timestamp,
            "nonce": nonce,
        })
        .to_string();

        (
            scope_header,
            format!("keyId={},sig={}", self.config.signing_key_id, signature),
        )
    }

    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        scope: &RelayScope,
        body: Option<serde_json::Value>,
        idempotency_key: Option<&str>,
    ) -> Result<serde_json::Value, RelayError> {
        let serialized = match &body {
            Some(v) => serde_json::to_vec(v).map_err(|e| RelayError::Unexpected(e.to_string()))?,
            None => Vec::new(),
        };

        let request_id = Uuid::new_v4().to_string();
        // A fresh nonce every time. Relay consumes each one exactly once, so a
        // retry has to re-sign rather than resend.
        let nonce = Uuid::new_v4().to_string();
        let timestamp = chrono::Utc::now().timestamp();

        let (scope_header, signature) = self.sign(
            method.as_str(),
            path,
            scope,
            &request_id,
            &nonce,
            timestamp,
            &serialized,
        );

        let mut request = self
            .http
            .request(method, format!("{}{}", self.config.base_url, path))
            .bearer_auth(&self.config.runtime_token)
            .header("x-relay-scope", scope_header)
            .header("x-relay-signature", signature)
            .header("content-type", "application/json");
        if let Some(key) = idempotency_key {
            request = request.header("idempotency-key", key);
        }
        if !serialized.is_empty() {
            request = request.body(serialized);
        }

        let response = request
            .send()
            .await
            .map_err(|e| RelayError::Unreachable(e.to_string()))?;

        let status = response.status();
        let payload: serde_json::Value = response
            .json()
            .await
            .map_err(|e| RelayError::Unexpected(e.to_string()))?;

        if status.is_success() {
            return Ok(payload);
        }

        // Relay's errors carry a stable code and a message it already
        // scrubbed. Both pass through so the UI can be specific.
        let code = payload
            .get("code")
            .and_then(|c| c.as_str())
            .unwrap_or("relay_error")
            .to_string();
        let message = payload
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("relay refused the request")
            .to_string();
        Err(RelayError::Refused { code, message })
    }

    pub fn source_management_enabled(&self) -> bool {
        self.config.management_token.is_some()
    }

    pub async fn manage_source(
        &self,
        method: reqwest::Method,
        path: &str,
        scope: &RelayScope,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, RelayError> {
        let token = self
            .config
            .management_token
            .as_ref()
            .ok_or(RelayError::NotConfigured)?;
        let serialized = serde_json::to_vec(&body)
            .map_err(|_| RelayError::Unexpected("invalid source request".into()))?;
        let (scope_header, signature) = self.sign(
            method.as_str(),
            path,
            scope,
            &Uuid::new_v4().to_string(),
            &Uuid::new_v4().to_string(),
            chrono::Utc::now().timestamp(),
            &serialized,
        );
        let response = self
            .http
            .request(method, format!("{}{}", self.config.base_url, path))
            .bearer_auth(token)
            .header("x-relay-scope", scope_header)
            .header("x-relay-signature", signature)
            .header("content-type", "application/json")
            .body(serialized)
            .send()
            .await
            .map_err(|_| RelayError::Unreachable("source management unavailable".into()))?;
        if !response.status().is_success() {
            return Err(RelayError::Unexpected("source management refused".into()));
        }
        response
            .json()
            .await
            .map_err(|_| RelayError::Unexpected("invalid source response".into()))
    }

    /// The connections relay will honour for this caller. IONe intersects this
    /// with its own mappings; a mapping pointing at a connection relay has
    /// revoked is a name for nothing.
    pub async fn available_connections(
        &self,
        scope: &RelayScope,
    ) -> Result<AvailableConnections, RelayError> {
        let payload = self
            .send(
                reqwest::Method::GET,
                "/v1/connections/available",
                scope,
                None,
                None,
            )
            .await?;
        serde_json::from_value(payload).map_err(|e| RelayError::Unexpected(e.to_string()))
    }

    pub async fn dataset_bytes(
        &self,
        scope: &RelayScope,
        path: &str,
    ) -> Result<Vec<u8>, RelayError> {
        const MAX: usize = 64 * 1024 * 1024;
        let (claims, signature) = self.sign(
            "GET",
            path,
            scope,
            &Uuid::new_v4().to_string(),
            &Uuid::new_v4().to_string(),
            chrono::Utc::now().timestamp(),
            &[],
        );
        let mut response = self
            .http
            .get(format!("{}{}", self.config.base_url, path))
            .bearer_auth(&self.config.runtime_token)
            .header("x-relay-scope", claims)
            .header("x-relay-signature", signature)
            .send()
            .await
            .map_err(|_| RelayError::Unreachable("dataset unavailable".into()))?;
        if !response.status().is_success() {
            return Err(RelayError::Refused {
                code: if response.status().as_u16() == 404 || response.status().as_u16() == 410 {
                    "not_found"
                } else {
                    "forbidden"
                }
                .into(),
                message: "dataset access was refused".into(),
            });
        }
        if response.content_length().is_some_and(|n| n > MAX as u64) {
            return Err(RelayError::Unexpected(
                "dataset exceeds download limit".into(),
            ));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| RelayError::Unreachable("dataset download interrupted".into()))?
        {
            if chunk.len() > MAX - body.len() {
                return Err(RelayError::Unexpected(
                    "dataset exceeds download limit".into(),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    pub async fn dataset_read(
        &self,
        scope: &RelayScope,
        path: &str,
        input: &crate::models::dataset_delegation::DatasetRead,
    ) -> Result<DatasetReadResponse, RelayError> {
        let invalid = || RelayError::Unexpected("invalid dataset read response".into());
        if input.max_bytes == 0 || input.max_bytes > 64 * 1024 * 1024 {
            return Err(invalid());
        }
        let body = serde_json::to_vec(input).map_err(|_| invalid())?;
        let (claims, signature) = self.sign(
            "POST",
            path,
            scope,
            &Uuid::new_v4().to_string(),
            &Uuid::new_v4().to_string(),
            chrono::Utc::now().timestamp(),
            &body,
        );
        let mut response = self
            .http
            .post(format!("{}{}", self.config.base_url, path))
            .bearer_auth(&self.config.runtime_token)
            .header("x-relay-scope", claims)
            .header("x-relay-signature", signature)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| invalid())?;
        if !response.status().is_success() {
            return Err(RelayError::Refused {
                code: "forbidden".into(),
                message: "dataset read refused".into(),
            });
        }
        let headers = response.headers();
        let source_digest = headers
            .get("x-relay-source-digest")
            .and_then(|h| h.to_str().ok())
            .ok_or_else(invalid)?
            .to_owned();
        let content_digest = headers
            .get("x-relay-content-digest")
            .and_then(|h| h.to_str().ok())
            .ok_or_else(invalid)?
            .to_owned();
        let row_count: u64 = headers
            .get("x-relay-row-count")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse().ok())
            .ok_or_else(invalid)?;
        if row_count > input.max_rows
            || response
                .content_length()
                .is_some_and(|n| n > input.max_bytes)
        {
            return Err(invalid());
        }
        let declared = response.content_length();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| invalid())? {
            if chunk.len() as u64 > input.max_bytes - bytes.len() as u64 {
                return Err(invalid());
            }
            bytes.extend_from_slice(&chunk);
        }
        if declared.is_some_and(|n| n != bytes.len() as u64)
            || content_digest != format!("sha256:{}", hex::encode(Sha256::digest(&bytes)))
        {
            return Err(invalid());
        }
        Ok(DatasetReadResponse {
            bytes,
            source_digest,
            content_digest,
            row_count,
        })
    }

    pub async fn save_recipe(
        &self,
        scope: &RelayScope,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, RelayError> {
        const MAX: usize = 64 * 1024 * 1024;
        let body = serde_json::to_vec(&input)
            .map_err(|_| RelayError::Unexpected("invalid recipe input".into()))?;
        let (claims, signature) = self.sign(
            "POST",
            "/v1/recipes",
            scope,
            &Uuid::new_v4().to_string(),
            &Uuid::new_v4().to_string(),
            chrono::Utc::now().timestamp(),
            &body,
        );
        let mut response = self
            .http
            .post(format!("{}/v1/recipes", self.config.base_url))
            .bearer_auth(&self.config.runtime_token)
            .header("x-relay-scope", claims)
            .header("x-relay-signature", signature)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| RelayError::Unreachable("recipe save unavailable".into()))?;
        if !response.status().is_success() {
            let code = match response.status().as_u16() {
                400 | 413 | 415 | 422 => "bad_request",
                401 | 403 => "forbidden",
                404 | 410 => "not_found",
                409 => "conflict",
                429 => "too_many_requests",
                _ => return Err(RelayError::Unexpected("recipe save failed".into())),
            };
            return Err(RelayError::Refused {
                code: code.into(),
                message: "recipe save refused".into(),
            });
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX as u64)
        {
            return Err(RelayError::Unexpected(
                "recipe metadata exceeds limit".into(),
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| RelayError::Unexpected("invalid recipe response".into()))?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX {
                return Err(RelayError::Unexpected(
                    "recipe metadata exceeds limit".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| RelayError::Unexpected("invalid recipe response".into()))
    }

    pub async fn create_run(
        &self,
        scope: &RelayScope,
        input: &CreateRun,
        idempotency_key: &str,
    ) -> Result<serde_json::Value, RelayError> {
        let body =
            serde_json::to_value(input).map_err(|e| RelayError::Unexpected(e.to_string()))?;
        self.send(
            reqwest::Method::POST,
            "/v1/runs",
            scope,
            Some(body),
            Some(idempotency_key),
        )
        .await
    }

    pub async fn run_status(
        &self,
        scope: &RelayScope,
        run_id: Uuid,
    ) -> Result<serde_json::Value, RelayError> {
        self.send(
            reqwest::Method::GET,
            &format!("/v1/runs/{run_id}"),
            scope,
            None,
            None,
        )
        .await
    }

    pub async fn cancel_run(
        &self,
        scope: &RelayScope,
        run_id: Uuid,
    ) -> Result<serde_json::Value, RelayError> {
        self.send(
            reqwest::Method::POST,
            &format!("/v1/runs/{run_id}/cancel"),
            scope,
            None,
            None,
        )
        .await
    }

    pub async fn answer_clarification(
        &self,
        scope: &RelayScope,
        run_id: Uuid,
        seq: i32,
        answer: &str,
    ) -> Result<serde_json::Value, RelayError> {
        self.send(
            reqwest::Method::POST,
            &format!("/v1/runs/{run_id}/clarification"),
            scope,
            Some(serde_json::json!({"seq": seq, "answer": answer})),
            None,
        )
        .await
    }

    pub async fn result(
        &self,
        scope: &RelayScope,
        run_id: Uuid,
    ) -> Result<serde_json::Value, RelayError> {
        self.send(
            reqwest::Method::GET,
            &format!("/v1/runs/{run_id}/result"),
            scope,
            None,
            None,
        )
        .await
    }

    pub async fn receipts(
        &self,
        scope: &RelayScope,
        run_id: Uuid,
    ) -> Result<serde_json::Value, RelayError> {
        self.send(
            reqwest::Method::GET,
            &format!("/v1/runs/{run_id}/receipts"),
            scope,
            None,
            None,
        )
        .await
    }

    pub async fn models(&self, scope: &RelayScope) -> Result<serde_json::Value, RelayError> {
        self.send(reqwest::Method::GET, "/v1/models", scope, None, None)
            .await
    }

    /// Open the event stream and return the raw response, so the proxy route
    /// can forward frames without buffering them.
    pub async fn events(
        &self,
        scope: &RelayScope,
        run_id: Uuid,
        last_event_id: Option<&str>,
    ) -> Result<reqwest::Response, RelayError> {
        let path = format!("/v1/runs/{run_id}/events");
        let request_id = Uuid::new_v4().to_string();
        let nonce = Uuid::new_v4().to_string();
        let timestamp = chrono::Utc::now().timestamp();
        let (scope_header, signature) =
            self.sign("GET", &path, scope, &request_id, &nonce, timestamp, b"");

        let mut request = self
            .http
            .get(format!("{}{}", self.config.base_url, path))
            .bearer_auth(&self.config.runtime_token)
            .header("x-relay-scope", scope_header)
            .header("x-relay-signature", signature)
            .header("accept", "text/event-stream")
            // The stream outlives the ordinary request timeout.
            .timeout(Duration::from_secs(600));
        if let Some(id) = last_event_id {
            // Forwarded verbatim so relay replays exactly what the browser
            // missed. Re-deriving it here would replay approximately.
            request = request.header("last-event-id", id);
        }

        request
            .send()
            .await
            .map_err(|e| RelayError::Unreachable(e.to_string()))
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Arc<RelayConfig> {
        Arc::new(RelayConfig {
            base_url: "http://relay.internal:8080".into(),
            deployment_id: Uuid::nil(),
            runtime_token: "runtime-token".into(),
            management_token: None,
            signing_key_id: "k1".into(),
            signing_key: "0123456789abcdef0123456789abcdef".into(),
            callback_verification_key: "cb-key".into(),
            default_model: "some-discovered-slug".into(),
            timeout_ms: 30_000,
        })
    }

    fn scope() -> RelayScope {
        RelayScope {
            tenant_id: Uuid::nil(),
            workspace_id: Uuid::nil(),
            actor_id: "user-1".into(),
            service_account_id: "ione".into(),
            roles: vec!["analyst".into()],
        }
    }

    #[tokio::test]
    async fn runtime_credentials_never_enable_source_management() {
        let client = RelayClient::new(config()).unwrap();
        assert!(!client.source_management_enabled());
        assert!(matches!(
            client
                .manage_source(
                    reqwest::Method::POST,
                    "/v1/connections",
                    &scope(),
                    serde_json::json!({})
                )
                .await,
            Err(RelayError::NotConfigured)
        ));
    }

    #[test]
    fn the_signature_covers_every_identity_field() {
        let client = RelayClient::new(config()).unwrap();
        let base = client.sign("GET", "/v1/runs", &scope(), "r", "n", 1, b"");

        // Any change to what is signed changes the signature.
        let other_actor = RelayScope {
            actor_id: "user-2".into(),
            ..scope()
        };
        assert_ne!(
            base.1,
            client
                .sign("GET", "/v1/runs", &other_actor, "r", "n", 1, b"")
                .1
        );

        let other_workspace = RelayScope {
            workspace_id: Uuid::new_v4(),
            ..scope()
        };
        assert_ne!(
            base.1,
            client
                .sign("GET", "/v1/runs", &other_workspace, "r", "n", 1, b"")
                .1
        );

        // Including the roles, which is why they ride inside the actor field.
        let escalated = RelayScope {
            roles: vec!["admin".into()],
            ..scope()
        };
        assert_ne!(
            base.1,
            client
                .sign("GET", "/v1/runs", &escalated, "r", "n", 1, b"")
                .1
        );

        // And the method, path, nonce, timestamp, and body.
        assert_ne!(
            base.1,
            client
                .sign("POST", "/v1/runs", &scope(), "r", "n", 1, b"")
                .1
        );
        assert_ne!(
            base.1,
            client
                .sign("GET", "/v1/models", &scope(), "r", "n", 1, b"")
                .1
        );
        assert_ne!(
            base.1,
            client
                .sign("GET", "/v1/runs", &scope(), "r", "n2", 1, b"")
                .1
        );
        assert_ne!(
            base.1,
            client.sign("GET", "/v1/runs", &scope(), "r", "n", 2, b"").1
        );
        assert_ne!(
            base.1,
            client
                .sign("GET", "/v1/runs", &scope(), "r", "n", 1, b"{}")
                .1
        );
    }

    #[test]
    fn length_prefixing_prevents_a_field_boundary_collision() {
        let client = RelayClient::new(config()).unwrap();
        // Two scopes whose concatenated fields would be identical without the
        // length prefixes.
        let a = RelayScope {
            actor_id: "ab".into(),
            service_account_id: "c".into(),
            roles: vec![],
            ..scope()
        };
        let b = RelayScope {
            actor_id: "a".into(),
            service_account_id: "bc".into(),
            roles: vec![],
            ..scope()
        };
        assert_ne!(
            client.sign("GET", "/v1/runs", &a, "r", "n", 1, b"").1,
            client.sign("GET", "/v1/runs", &b, "r", "n", 1, b"").1
        );
    }

    #[test]
    fn the_scope_header_carries_no_credential() {
        let client = RelayClient::new(config()).unwrap();
        let (header, signature) = client.sign("GET", "/v1/runs", &scope(), "r", "n", 1, b"");
        for rendered in [&header, &signature] {
            assert!(!rendered.contains("runtime-token"));
            assert!(!rendered.contains("0123456789abcdef"));
        }
    }

    #[test]
    fn the_debug_form_carries_no_credential() {
        let client = RelayClient::new(config()).unwrap();
        let rendered = format!("{client:?}");
        assert!(!rendered.contains("runtime-token"));
        assert!(!rendered.contains("0123456789abcdef"));
        assert!(!rendered.contains("cb-key"));
    }
}
