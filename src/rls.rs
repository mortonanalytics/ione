//! Per-transaction org context for the row-level-security policies declared in
//! migrations 0019–0048 and activated by 0050.
//!
//! The policies read `current_setting('app.current_org_id', true)`. That setting
//! can only be established with transaction scope (`SET LOCAL`), and sqlx here
//! runs on a pooled connection: a session-scoped `SET` would leak one request's
//! org context onto the next request that happens to reuse the connection. So
//! every org-scoped query must run inside a transaction opened by
//! [`org_scoped_tx`], which sets the context and drops it at commit or rollback.

use anyhow::Context;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

/// Open a transaction whose `app.current_org_id` is pinned to `org_id`.
///
/// The org id is bound as a query parameter to `set_config(..., is_local => true)`
/// rather than interpolated into a `SET LOCAL` statement, because `SET` does not
/// accept parameters and string-building the value would put a caller-supplied
/// id into SQL text.
///
/// The caller runs its queries on the returned handle and must `commit()` it —
/// dropping the handle rolls back, which is the correct default for a read that
/// errors partway.
pub async fn org_scoped_tx(
    pool: &PgPool,
    org_id: Uuid,
) -> anyhow::Result<Transaction<'static, Postgres>> {
    let mut tx = pool
        .begin()
        .await
        .context("failed to begin org-scoped transaction")?;
    sqlx::query("SELECT set_config('app.current_org_id', $1, true)")
        .bind(org_id.to_string())
        .execute(&mut *tx)
        .await
        .context("failed to set app.current_org_id")?;
    Ok(tx)
}
