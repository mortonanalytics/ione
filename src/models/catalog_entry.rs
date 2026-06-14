use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Kind of federated capability indexed in `peer_catalog_entries` (migration 0044).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "catalog_entry_kind", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum CatalogEntryKind {
    Tool,
    Resource,
}

/// Row shape for `peer_catalog_entries`. The generated `tsv` and reserved
/// `embedding` columns are never selected into Rust, so they are absent here.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CatalogEntry {
    pub id: Uuid,
    pub org_id: Uuid,
    pub peer_id: Uuid,
    pub kind: CatalogEntryKind,
    pub namespaced_name: String,
    pub raw_name: String,
    pub description: String,
    pub sample_queries: Vec<String>,
    pub schema_field_names: Vec<String>,
    pub content_hash: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
