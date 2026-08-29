use anyhow::Context;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

/// Pool size when `IONE_DB_MAX_CONNECTIONS` says nothing usable.
const DEFAULT_MAX_CONNECTIONS: u32 = 10;

/// Resolve the pool size from `IONE_DB_MAX_CONNECTIONS`.
///
/// Ten connections is too many for an edge node on constrained hardware and too
/// few for a loaded one, and every other runtime knob is already env-driven
/// (`config.rs`). A value that is missing, unparseable, or zero falls back to
/// the default and logs: a bad env var must not stop the process from booting.
fn max_connections_from(raw: Option<&str>) -> u32 {
    let Some(raw) = raw else {
        return DEFAULT_MAX_CONNECTIONS;
    };
    match raw.trim().parse::<u32>() {
        Ok(n) if n > 0 => n,
        _ => {
            tracing::warn!(
                value = raw,
                default = DEFAULT_MAX_CONNECTIONS,
                "IONE_DB_MAX_CONNECTIONS is not a positive integer; using the default"
            );
            DEFAULT_MAX_CONNECTIONS
        }
    }
}

fn max_connections() -> u32 {
    max_connections_from(std::env::var("IONE_DB_MAX_CONNECTIONS").ok().as_deref())
}

pub async fn connect(database_url: &str) -> anyhow::Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(max_connections())
        .connect(database_url)
        .await
        .context("failed to connect to Postgres")
}

pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .context("migration failed")
}

#[cfg(test)]
mod tests {
    use super::{max_connections_from, DEFAULT_MAX_CONNECTIONS};

    #[test]
    fn unset_uses_the_default() {
        assert_eq!(max_connections_from(None), DEFAULT_MAX_CONNECTIONS);
    }

    #[test]
    fn a_positive_integer_is_honoured() {
        assert_eq!(max_connections_from(Some("25")), 25);
        assert_eq!(max_connections_from(Some(" 4 ")), 4);
    }

    #[test]
    fn zero_and_garbage_fall_back_rather_than_failing_the_boot() {
        assert_eq!(max_connections_from(Some("0")), DEFAULT_MAX_CONNECTIONS);
        assert_eq!(max_connections_from(Some("abc")), DEFAULT_MAX_CONNECTIONS);
        assert_eq!(max_connections_from(Some("-3")), DEFAULT_MAX_CONNECTIONS);
        assert_eq!(max_connections_from(Some("")), DEFAULT_MAX_CONNECTIONS);
    }
}
