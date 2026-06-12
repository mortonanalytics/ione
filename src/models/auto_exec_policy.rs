use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Row shape for `auto_exec_policies` (migration 0040). API serialization is
/// hand-built in `routes::auto_exec_policies::policy_json` so `org_id` (an
/// internal tenancy column) is never returned.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AutoExecPolicy {
    pub id: Uuid,
    pub org_id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub trigger_signal_title_prefix: Option<String>,
    pub trigger_severity_at_most: Option<String>,
    pub connector_id: Uuid,
    pub op: String,
    pub args_template: serde_json::Value,
    pub rate_limit_per_min: i32,
    pub severity_cap: String,
    pub authorized_by_permission: String,
    pub enabled: bool,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
