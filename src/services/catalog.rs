use uuid::Uuid;

use crate::{
    auth::{ensure_workspace_in_org, permission_grants, AuthContext},
    error::AppError,
    models::CatalogEntryKind,
    repos::{CatalogRepo, CatalogSearchRow, RoleRepo},
    state::AppState,
};

const MIN_QUERY_LEN: usize = 2;
const MAX_LIMIT: i64 = 50;
const DEFAULT_LIMIT: i64 = 20;

/// RBAC-pre-filtered relevance search over the federated catalog, shared by the
/// REST endpoint (Slice 2) and the `search_catalog` MCP tool (Slice 3).
pub struct CatalogService;

impl CatalogService {
    /// Search the org's catalog, returning only entries the caller can actually
    /// invoke (FCS-C1), relevance-ranked. Rejects the unauthenticated
    /// default-user fallback (FCS-C2) and re-validates workspace-in-org (FCS-L2).
    pub async fn search(
        state: &AppState,
        workspace_id: Uuid,
        auth: &AuthContext,
        q: &str,
        kind: Option<CatalogEntryKind>,
        limit: Option<i64>,
    ) -> Result<Vec<CatalogSearchRow>, AppError> {
        // FCS-C2: never serve the unauthenticated default-user fallback.
        if !auth.is_authenticated(state.default_user_id) {
            return Err(AppError::Forbidden);
        }
        // FCS-L2: re-validate the workspace belongs to the caller's org at query
        // time (a stale session could carry a since-removed workspace).
        ensure_workspace_in_org(&state.pool, workspace_id, auth.org_id).await?;

        let q = q.trim();
        if q.len() < MIN_QUERY_LEN {
            return Err(AppError::BadRequest(
                "query must be at least 2 characters".into(),
            ));
        }
        let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

        let invokable = Self::invokable_names(state, workspace_id, auth).await?;
        let repo = CatalogRepo::new(state.pool.clone());
        repo.search(auth.org_id, &invokable, q, kind, limit)
            .await
            .map_err(AppError::Internal)
    }

    /// The caller's invokable `namespaced_name` set: every org catalog entry
    /// whose `tool_invoke:<peer.name>:<raw_name>` grant the caller holds — the
    /// exact code path `route_tool_call` traverses, so search visibility equals
    /// invocation capability. A later fast-follow can reuse this for the
    /// `tools/list` filter (FCS-L1).
    async fn invokable_names(
        state: &AppState,
        workspace_id: Uuid,
        auth: &AuthContext,
    ) -> Result<Vec<String>, AppError> {
        let (held, _) = RoleRepo::new(state.pool.clone())
            .effective_permissions(auth.user_id, workspace_id)
            .await
            .map_err(AppError::Internal)?;
        let candidates = CatalogRepo::new(state.pool.clone())
            .permission_candidates_for_org(auth.org_id)
            .await
            .map_err(AppError::Internal)?;

        let is_admin = held.contains("admin");
        Ok(candidates
            .into_iter()
            .filter(|c| {
                let needed = format!("tool_invoke:{}:{}", c.peer_name, c.raw_name);
                is_admin || permission_grants(&held, &needed)
            })
            .map(|c| c.namespaced_name)
            .collect())
    }
}
