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

fn assert_absolute_url(url: &str) {
    let parsed = reqwest::Url::parse(url).expect("IONE_OAUTH_ISSUER must be an absolute URL");
    assert!(
        matches!(parsed.scheme(), "http" | "https") && parsed.host().is_some(),
        "IONE_OAUTH_ISSUER must be an absolute URL"
    );
}
