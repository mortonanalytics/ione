use tokio::net::TcpListener;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = ione::config::Config::from_env();
    let bind = config.bind.clone();

    let app = {
        let state = ione::state::AppState::new(config);
        ione::routes::router(state)
    };

    let listener = TcpListener::bind(&bind).await?;
    tracing::info!(addr = %bind, "listening");

    axum::serve(listener, app).await?;

    Ok(())
}
