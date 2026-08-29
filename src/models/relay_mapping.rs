//! Which relay connections a workspace may query.
//!
//! A mapping is IONe's half of a two-sided authorization. Relay holds its own
//! grants and rechecks them at every boundary; this record says which of those
//! connections this workspace has a name for. A query is authorized only where
//! the two intersect, and neither side takes the other's word for it.
//!
//! Note what a mapping does not hold: no URL, no token, no key. The relay
//! endpoint and its credentials are server-side configuration. A mapping names
//! a connection by the id relay issued and stops there.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRelayMapping {
    pub id: Uuid,
    pub org_id: Uuid,
    pub workspace_id: Uuid,
    /// The connection id relay issued. Opaque here; relay is the authority on
    /// what it points at and whether the caller may use it.
    pub relay_connection_id: Uuid,
    pub display_name: String,
    /// The alias the generated query is written against. Unique per workspace,
    /// because two mappings sharing one would make the query ambiguous.
    pub alias: String,
    /// `None` means every workspace member holding `data:query`. A value
    /// narrows the mapping to one person.
    pub principal_id: Option<Uuid>,
    pub enabled: bool,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl WorkspaceRelayMapping {
    /// Does this mapping apply to `user_id`? A workspace-wide mapping applies
    /// to everyone; a narrowed one applies to its named principal only.
    pub fn applies_to(&self, user_id: Uuid) -> bool {
        self.enabled && self.principal_id.map(|p| p == user_id).unwrap_or(true)
    }
}

/// What a client sends to create a mapping.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewRelayMapping {
    pub relay_connection_id: Uuid,
    pub display_name: String,
    pub alias: String,
    #[serde(default)]
    pub principal_id: Option<Uuid>,
}

/// A record of a relay run, without its answer.
///
/// Counts, states, and hashes. Never a row value, never the literals inside the
/// generated query, never a credential. The full audit lives in relay; this is
/// the link that lets an IONe operator find it.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct RelayRunLink {
    pub id: Uuid,
    pub org_id: Uuid,
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub relay_run_id: Uuid,
    pub outcome: String,
    pub mapping_ids: Vec<Uuid>,
    pub row_count: Option<i64>,
    pub truncated: bool,
    pub refusal_reason: Option<String>,
    pub plan_hash: Option<String>,
    pub policy_hash: Option<String>,
    pub usage: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// Relay's alias rules, mirrored so a mapping cannot be created that relay
/// would refuse at query time. Lowercase, digits, and underscore; not starting
/// with a digit; at most 63 characters.
pub fn alias_is_valid(alias: &str) -> bool {
    if alias.is_empty() || alias.len() > 63 {
        return false;
    }
    if alias.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return false;
    }
    alias
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_alias_may_not_carry_sql_syntax() {
        assert!(alias_is_valid("pg"));
        assert!(alias_is_valid("lake_2024"));
        for bad in ["pg.main", "pg-main", "PG", "2pg", "", "pg\"; DROP", "pg main"] {
            assert!(!alias_is_valid(bad), "{bad} was accepted");
        }
        assert!(!alias_is_valid(&"a".repeat(64)));
    }

    #[test]
    fn a_workspace_wide_mapping_applies_to_everyone_and_a_narrowed_one_does_not() {
        let user = Uuid::new_v4();
        let other = Uuid::new_v4();
        let base = WorkspaceRelayMapping {
            id: Uuid::new_v4(),
            org_id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            relay_connection_id: Uuid::new_v4(),
            display_name: "Warehouse".into(),
            alias: "pg".into(),
            principal_id: None,
            enabled: true,
            created_by: user,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert!(base.applies_to(user));
        assert!(base.applies_to(other));

        let narrowed = WorkspaceRelayMapping {
            principal_id: Some(user),
            ..base.clone()
        };
        assert!(narrowed.applies_to(user));
        assert!(!narrowed.applies_to(other));

        // Disabled applies to nobody, whatever the principal says.
        let disabled = WorkspaceRelayMapping {
            enabled: false,
            ..base
        };
        assert!(!disabled.applies_to(user));
    }
}
