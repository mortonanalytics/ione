use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::CatalogEntryKind;

/// Write payload for an upsert into `peer_catalog_entries`. `org_id` is derived
/// from `peers.org_id` by the caller (never the org-blind manifest cache).
pub struct CatalogUpsert {
    pub org_id: Uuid,
    pub peer_id: Uuid,
    pub kind: CatalogEntryKind,
    pub namespaced_name: String,
    pub raw_name: String,
    pub description: String,
    pub sample_queries: Vec<String>,
    pub schema_field_names: Vec<String>,
    pub content_hash: String,
}

/// One candidate row for the RBAC pre-filter. `peer_name` + `raw_name` rebuild
/// the exact `tool_invoke:<peer.name>:<raw_name>` permission string that
/// `route_tool_call` checks, so search visibility equals invocation capability.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PermissionCandidate {
    pub namespaced_name: String,
    pub peer_name: String,
    pub raw_name: String,
}

/// A ranked search hit returned to the service layer.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CatalogSearchRow {
    pub id: Uuid,
    pub peer_id: Uuid,
    pub peer_name: String,
    pub kind: CatalogEntryKind,
    pub namespaced_name: String,
    pub raw_name: String,
    pub description: String,
    pub sample_queries: Vec<String>,
    pub score: f64,
}

pub struct CatalogRepo {
    pool: PgPool,
}

impl CatalogRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Insert or update an entry. The `content_hash` guard makes a refresh with
    /// unchanged content a no-op (no row write, no GIN churn, no updated_at
    /// bump). Returns true when a row was actually inserted or updated.
    pub async fn upsert_entry(&self, e: &CatalogUpsert) -> anyhow::Result<bool> {
        let affected = sqlx::query(
            "INSERT INTO peer_catalog_entries
                 (org_id, peer_id, kind, namespaced_name, raw_name, description,
                  sample_queries, schema_field_names, content_hash)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (org_id, peer_id, namespaced_name) DO UPDATE SET
                 kind = EXCLUDED.kind,
                 raw_name = EXCLUDED.raw_name,
                 description = EXCLUDED.description,
                 sample_queries = EXCLUDED.sample_queries,
                 schema_field_names = EXCLUDED.schema_field_names,
                 content_hash = EXCLUDED.content_hash
             WHERE peer_catalog_entries.content_hash <> EXCLUDED.content_hash",
        )
        .bind(e.org_id)
        .bind(e.peer_id)
        .bind(e.kind)
        .bind(&e.namespaced_name)
        .bind(&e.raw_name)
        .bind(&e.description)
        .bind(&e.sample_queries)
        .bind(&e.schema_field_names)
        .bind(&e.content_hash)
        .execute(&self.pool)
        .await
        .context("failed to upsert catalog entry")?
        .rows_affected();
        Ok(affected > 0)
    }

    /// Current `(namespaced_name, content_hash)` pairs for a peer — drives the
    /// delta decision in `reindex_peer_catalog`.
    pub async fn hashes_for_peer(
        &self,
        org_id: Uuid,
        peer_id: Uuid,
    ) -> anyhow::Result<Vec<(String, String)>> {
        sqlx::query_as::<_, (String, String)>(
            "SELECT namespaced_name, content_hash FROM peer_catalog_entries
             WHERE org_id = $1 AND peer_id = $2",
        )
        .bind(org_id)
        .bind(peer_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to read catalog hashes for peer")
    }

    /// Delete entries for a peer whose `namespaced_name` is no longer in the
    /// manifest. An empty `surviving` slice removes every row for the peer.
    pub async fn delete_orphans(
        &self,
        org_id: Uuid,
        peer_id: Uuid,
        surviving: &[String],
    ) -> anyhow::Result<u64> {
        let affected = sqlx::query(
            "DELETE FROM peer_catalog_entries
             WHERE org_id = $1 AND peer_id = $2 AND namespaced_name <> ALL($3)",
        )
        .bind(org_id)
        .bind(peer_id)
        .bind(surviving)
        .execute(&self.pool)
        .await
        .context("failed to delete orphaned catalog entries")?
        .rows_affected();
        Ok(affected)
    }

    /// Every catalog entry in the org, with the fields needed to rebuild the
    /// `tool_invoke` permission string for the RBAC pre-filter.
    pub async fn permission_candidates_for_org(
        &self,
        org_id: Uuid,
    ) -> anyhow::Result<Vec<PermissionCandidate>> {
        sqlx::query_as::<_, PermissionCandidate>(
            "SELECT e.namespaced_name, p.name AS peer_name, e.raw_name
             FROM peer_catalog_entries e
             JOIN peers p ON p.id = e.peer_id
             WHERE e.org_id = $1",
        )
        .bind(org_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list catalog permission candidates")
    }

    /// Ranked lexical search, pre-filtered to the caller's invokable set.
    /// `websearch_to_tsquery` only (never `to_tsquery`); all values bound.
    pub async fn search(
        &self,
        org_id: Uuid,
        invokable: &[String],
        q: &str,
        kind: Option<CatalogEntryKind>,
        limit: i64,
    ) -> anyhow::Result<Vec<CatalogSearchRow>> {
        sqlx::query_as::<_, CatalogSearchRow>(
            "SELECT e.id, e.peer_id, p.name AS peer_name, e.kind, e.namespaced_name,
                    e.raw_name, e.description, e.sample_queries,
                    (coalesce(ts_rank_cd(e.tsv, websearch_to_tsquery('english', $3)), 0) * 1.5
                     + similarity(e.raw_name || ' ' || e.description, $3) * 0.5) AS score
             FROM peer_catalog_entries e
             JOIN peers p ON p.id = e.peer_id
             WHERE e.org_id = $1 AND e.namespaced_name = ANY($2)
               AND ($5::catalog_entry_kind IS NULL OR e.kind = $5)
               AND (e.tsv @@ websearch_to_tsquery('english', $3)
                    OR (e.raw_name || ' ' || e.description) % $3)
             ORDER BY score DESC LIMIT $4",
        )
        .bind(org_id)
        .bind(invokable)
        .bind(q)
        .bind(limit)
        .bind(kind)
        .fetch_all(&self.pool)
        .await
        .context("failed to search catalog entries")
    }
}
