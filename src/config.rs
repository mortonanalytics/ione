use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    pub oauth_issuer: String,
    pub ollama_base_url: String,
    pub ollama_model: String,
    pub static_dir: PathBuf,
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
        }
    }
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
