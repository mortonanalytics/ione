use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    pub ollama_base_url: String,
    pub ollama_model: String,
    pub static_dir: PathBuf,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            bind: std::env::var("IONE_BIND").unwrap_or_else(|_| "0.0.0.0:3000".to_string()),
            ollama_base_url: std::env::var("OLLAMA_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:11434".to_string()),
            ollama_model: std::env::var("OLLAMA_MODEL")
                .unwrap_or_else(|_| "llama3.2:latest".to_string()),
            static_dir: std::env::var("STATIC_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./static")),
        }
    }
}
