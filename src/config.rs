use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    pub oauth_issuer: String,
    pub ollama_base_url: String,
    pub ollama_model: String,
    pub static_dir: PathBuf,
    pub allow_private_peers: bool,
    pub private_peer_allowlist: Vec<String>,
    /// Relay, when it is configured. `None` means the Data Query surface is
    /// absent rather than broken -- a deployment without relay should not show
    /// a tab that errors.
    pub relay: Option<std::sync::Arc<RelayConfig>>,
}

/// Server-side relay configuration.
///
/// None of this reaches a browser. The URL, the runtime token, and both keys
/// are read here and used only by `RelayClient`; the static assets carry no
/// relay endpoint and no relay secret, which is what makes the Data Query
/// surface a proxy rather than a direct call from the page.
#[derive(Clone)]
pub struct RelayConfig {
    pub base_url: String,
    /// The deployment relay expects to be addressed as. One deployment serves
    /// one host app, and relay refuses an envelope addressed to another.
    pub deployment_id: uuid::Uuid,
    pub runtime_token: String,
    pub management_token: Option<String>,
    pub signing_key_id: String,
    pub signing_key: String,
    /// Verifies inbound signed callbacks from relay.
    pub callback_verification_key: String,
    /// The model slug to ask for. A slug, not a family: relay resolves it
    /// against the catalog it discovered and refuses one that is not there.
    pub default_model: String,
    pub timeout_ms: u64,
}

impl std::fmt::Debug for RelayConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Named fields only. A derived Debug here would put the runtime token
        // and both keys into any log line that formatted the config.
        f.debug_struct("RelayConfig")
            .field("base_url", &self.base_url)
            .field("deployment_id", &self.deployment_id)
            .field("signing_key_id", &self.signing_key_id)
            .field("default_model", &self.default_model)
            .field("timeout_ms", &self.timeout_ms)
            .finish_non_exhaustive()
    }
}

impl RelayConfig {
    /// Read relay configuration, or `None` when it is absent.
    ///
    /// Half-configured is a startup failure, not a degraded mode. A deployment
    /// with a relay URL and no signing key would start, show the tab, and fail
    /// on the first question -- which is a worse outcome than not starting.
    fn from_env() -> Option<Self> {
        let present = |key: &str| std::env::var(key).ok().filter(|v| !v.trim().is_empty());

        let base_url = present("IONE_RELAY_URL")?;

        let mut missing: Vec<&str> = Vec::new();
        let mut require = |key: &'static str| match present(key) {
            Some(value) => value,
            None => {
                missing.push(key);
                String::new()
            }
        };
        let deployment_id_raw = require("IONE_RELAY_DEPLOYMENT_ID");
        let runtime_token = require("IONE_RELAY_RUNTIME_TOKEN");
        let signing_key_id = require("IONE_RELAY_SIGNING_KEY_ID");
        let signing_key = require("IONE_RELAY_SIGNING_KEY");
        let callback_verification_key = require("IONE_RELAY_CALLBACK_KEY");
        let default_model = require("IONE_RELAY_MODEL");

        if !missing.is_empty() {
            panic!(
                "IONE_RELAY_URL is set but relay is half-configured; missing: {}",
                missing.join(", ")
            );
        }

        let deployment_id = uuid::Uuid::parse_str(deployment_id_raw.trim())
            .expect("IONE_RELAY_DEPLOYMENT_ID must be a UUID");

        Some(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            deployment_id,
            runtime_token,
            management_token: present("IONE_RELAY_MANAGEMENT_TOKEN"),
            signing_key_id,
            signing_key,
            callback_verification_key,
            default_model,
            timeout_ms: present("IONE_RELAY_TIMEOUT_MS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(60_000),
        })
    }
}

impl Config {
    pub fn from_env() -> Self {
        validate_static_bearer_mode();
        let bind = std::env::var("IONE_BIND").unwrap_or_else(|_| "0.0.0.0:3000".to_string());
        let oauth_issuer = std::env::var("IONE_OAUTH_ISSUER")
            .unwrap_or_else(|_| format!("http://{}", bind.replace("0.0.0.0", "localhost")));
        assert_absolute_url(&oauth_issuer);

        Self {
            bind,
            oauth_issuer,
            ollama_base_url: std::env::var("OLLAMA_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:11434".to_string()),
            ollama_model: std::env::var("OLLAMA_MODEL")
                .unwrap_or_else(|_| "llama3.2:latest".to_string()),
            static_dir: std::env::var("IONE_STATIC_DIR")
                .or_else(|_| std::env::var("STATIC_DIR"))
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./static")),
            allow_private_peers: env_bool("IONE_ALLOW_PRIVATE_PEERS"),
            private_peer_allowlist: std::env::var("IONE_PRIVATE_PEER_ALLOWLIST")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            relay: RelayConfig::from_env().map(std::sync::Arc::new),
        }
    }
}

fn env_bool(key: &str) -> bool {
    std::env::var(key)
        .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

fn validate_static_bearer_mode() {
    let static_bearer_set = std::env::var("IONE_OAUTH_STATIC_BEARER")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    let auth_mode = std::env::var("IONE_AUTH_MODE")
        .unwrap_or_default()
        .to_lowercase();
    let dev_mode = std::env::var("IONE_DEV_MODE")
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    assert!(
        !(static_bearer_set && auth_mode == "oidc" && !dev_mode),
        "IONE_OAUTH_STATIC_BEARER is only allowed with IONE_AUTH_MODE=oidc when IONE_DEV_MODE=true"
    );
}

fn assert_absolute_url(url: &str) {
    let parsed = reqwest::Url::parse(url).expect("IONE_OAUTH_ISSUER must be an absolute URL");
    assert!(
        matches!(parsed.scheme(), "http" | "https") && parsed.host().is_some(),
        "IONE_OAUTH_ISSUER must be an absolute URL"
    );
}

#[cfg(test)]
mod tests {
    use super::validate_static_bearer_mode;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn static_bearer_is_rejected_in_oidc_without_dev_mode() {
        let _guard = env_lock().lock().expect("env lock");
        let old_auth = std::env::var("IONE_AUTH_MODE").ok();
        let old_bearer = std::env::var("IONE_OAUTH_STATIC_BEARER").ok();
        let old_dev = std::env::var("IONE_DEV_MODE").ok();
        std::env::set_var("IONE_AUTH_MODE", "oidc");
        std::env::set_var("IONE_OAUTH_STATIC_BEARER", "test-static");
        std::env::remove_var("IONE_DEV_MODE");
        let result = std::panic::catch_unwind(validate_static_bearer_mode);
        if let Some(v) = old_auth {
            std::env::set_var("IONE_AUTH_MODE", v);
        } else {
            std::env::remove_var("IONE_AUTH_MODE");
        }
        if let Some(v) = old_bearer {
            std::env::set_var("IONE_OAUTH_STATIC_BEARER", v);
        } else {
            std::env::remove_var("IONE_OAUTH_STATIC_BEARER");
        }
        if let Some(v) = old_dev {
            std::env::set_var("IONE_DEV_MODE", v);
        } else {
            std::env::remove_var("IONE_DEV_MODE");
        }
        assert!(result.is_err());
    }
}
