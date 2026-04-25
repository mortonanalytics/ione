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

    let args: Vec<String> = std::env::args().collect();

    if args.get(1).map(String::as_str) == Some("demo-purge") {
        let pool = ione::db::connect(&database_url).await?;
        ione::db::migrate(&pool).await?;
        ione::demo::seeder::purge_demo(&pool).await?;
        return Ok(());
    }

    ione::util::token_crypto::validate_env_key()?;

    let pool = ione::db::connect(&database_url).await?;
    ione::db::migrate(&pool).await?;

    ione::demo::seeder::seed_demo_if_enabled(&pool).await?;

    let config = ione::config::Config::from_env();
    let bind = config.bind.clone();

    let (app, state) = ione::app_with_state(pool).await;

    ione::services::scheduler::spawn(state);

    let listener = TcpListener::bind(&bind).await?;
    tracing::info!(addr = %bind, "listening");

    axum::serve(listener, app).await?;

    Ok(())
}
