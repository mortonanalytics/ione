use tokio::net::TcpListener;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set (copy .env.example to .env and edit)");

    let pool = ione::db::connect(&database_url).await?;
    ione::db::migrate(&pool).await?;

    let config = ione::config::Config::from_env();
    let bind = config.bind.clone();

    let (app, state) = ione::app_with_state(pool).await;

    ione::services::scheduler::spawn(state);

    let listener = TcpListener::bind(&bind).await?;
    tracing::info!(addr = %bind, "listening");

    axum::serve(listener, app).await?;

    Ok(())
}
