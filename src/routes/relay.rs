//! The Data Query proxy.
//!
//! Every browser request stays on IONe. These routes are the only path to
//! relay, and they resolve identity from `AuthContext` -- a session IONe
//! verified -- rather than from anything the browser sent. A caller who puts a
//! different workspace in the body gets their own workspace's data.
//!
//! Authorization is the intersection of three things: the caller's `data:query`
//! permission in this workspace, an enabled mapping that applies to them, and a
//! relay grant relay itself will honour. Each is checked independently, and
//! none of them takes the others' word for it.
//!
//! Nothing here touches the conversation surface. The Ollama client, the
//! generator, the critic, and the router are untouched; this is a second door,
//! not a change to the first one.

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    auth::{ensure_workspace_in_org, require_permission, AuthContext},
    error::AppError,
    models::{alias_is_valid, NewRelayMapping, WorkspaceRelayMapping},
    repos::RelayMappingRepo,
    services::relay_client::{CreateRun, RelayClient, RelayError, RelayScope, SourceSelection},
    state::AppState,
};

/// The permission that gates asking a question of connected data. Separate
/// from the conversation permission on purpose: the two surfaces reach
/// different systems.
pub const DATA_QUERY: &str = "data:query";

/// Relay, or a clean 404. A deployment without relay has no Data Query surface;
/// saying "not found" is more honest than a 500 that suggests something broke.
fn relay(state: &AppState) -> Result<&RelayClient, AppError> {
    state
        .relay
        .as_deref()
        .ok_or_else(|| AppError::NotFound("relay is not configured for this deployment".into()))
}

/// Build the scope relay will run under.
///
/// The actor is the person, and the service account is the identity whose relay
/// grants authorize the read. Both travel, and relay attributes the run to the
/// former while checking the latter. Collapsing them would make every run look
/// like it came from the service account -- true about mechanism, false about
/// who asked.
fn scope_for(ctx: &AuthContext, workspace_id: Uuid) -> RelayScope {
    RelayScope {
        tenant_id: ctx.org_id,
        workspace_id,
        actor_id: ctx.user_id.to_string(),
        service_account_id: format!("ione:{}", ctx.org_id),
        roles: ctx.permissions.clone(),
    }
}

/// Map a relay failure onto an IONe error, keeping relay's stable code.
///
/// The message relay returns was already scrubbed on its side; passing it
/// through lets the UI say what happened instead of "something went wrong".
/// The unreachable and unexpected variants deliberately drop their detail:
/// that detail is a driver or transport string, which is where a DSN hides.
fn map_error(error: RelayError) -> AppError {
    match error {
        RelayError::NotConfigured => {
            AppError::NotFound("relay is not configured for this deployment".into())
        }
        RelayError::Unreachable(_) => {
            AppError::RelayUpstream("the data service is unreachable".into())
        }
        RelayError::Refused { code, message } => match code.as_str() {
            "not_found" | "gone" => AppError::NotFound(message),
            "forbidden" | "unauthorized" | "scope_invalid" | "scope_replay" => AppError::Forbidden,
            "bad_request" | "idempotency_conflict" | "conflict" => AppError::BadRequest(message),
            // A refusal is the system working. It reaches the UI with its
            // reason so a person can see why, not as a generic failure.
            _ => AppError::BadRequest(format!("{code}: {message}")),
        },
        RelayError::Unexpected(_) => {
            AppError::RelayUpstream("the data service returned an unexpected response".into())
        }
    }
}

// ---------------------------------------------------------------------------
// Mappings (workspace administration)
// ---------------------------------------------------------------------------

/// Create a mapping. Requires workspace write, and refuses a mapping relay
/// would not honour: the connection has to be one relay currently offers this
/// caller, checked against relay rather than assumed.
pub async fn create_mapping(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
    Json(input): Json<NewRelayMapping>,
) -> Result<Json<WorkspaceRelayMapping>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "workspace:write").await?;

    if !alias_is_valid(&input.alias) {
        return Err(AppError::BadRequest(
            "an alias may contain only lowercase letters, digits, and underscores, \
             and may not start with a digit"
                .into(),
        ));
    }
    if input.display_name.trim().is_empty() {
        return Err(AppError::BadRequest("a display name is required".into()));
    }

    // The connection has to be one relay will actually honour. Creating a
    // mapping for a connection relay has never heard of produces a name that
    // fails on first use, which is a worse time to find out.
    let client = relay(&state)?;
    let available = client
        .available_connections(&scope_for(&ctx, workspace_id))
        .await
        .map_err(map_error)?;
    let offered = available.connections.iter().any(|c| {
        c.get("connection_id")
            .and_then(|id| id.as_str())
            .and_then(|id| Uuid::parse_str(id).ok())
            == Some(input.relay_connection_id)
    });
    if !offered {
        return Err(AppError::BadRequest(
            "the data service does not offer that connection to this workspace".into(),
        ));
    }

    let mapping = RelayMappingRepo::new(state.pool.clone())
        .create(ctx.org_id, workspace_id, ctx.user_id, input)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(mapping))
}

pub async fn list_mappings(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Vec<WorkspaceRelayMapping>>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "workspace:write").await?;

    let mappings = RelayMappingRepo::new(state.pool.clone())
        .list_for_workspace(ctx.org_id, workspace_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(mappings))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetEnabled {
    pub enabled: bool,
}

pub async fn set_mapping_enabled(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, mapping_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<SetEnabled>,
) -> Result<Json<WorkspaceRelayMapping>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "workspace:write").await?;

    RelayMappingRepo::new(state.pool.clone())
        .set_enabled(ctx.org_id, workspace_id, mapping_id, input.enabled)
        .await
        .map_err(AppError::Internal)?
        .map(Json)
        .ok_or_else(|| AppError::NotFound("mapping not found".into()))
}

// ---------------------------------------------------------------------------
// Querying
// ---------------------------------------------------------------------------

/// What this caller can actually query: the intersection of enabled mappings
/// that apply to them and connections relay will honour.
///
/// Both halves are needed. A mapping without a relay grant is a name for
/// nothing; a relay grant without a mapping is a connection this workspace was
/// never given.
pub async fn available_sources(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, DATA_QUERY).await?;

    let client = relay(&state)?;
    let mappings = RelayMappingRepo::new(state.pool.clone())
        .effective_for_user(ctx.org_id, workspace_id, ctx.user_id)
        .await
        .map_err(AppError::Internal)?;

    let available = client
        .available_connections(&scope_for(&ctx, workspace_id))
        .await
        .map_err(map_error)?;

    let sources: Vec<serde_json::Value> = mappings
        .iter()
        .filter_map(|mapping| {
            let matched = available.connections.iter().find(|c| {
                c.get("connection_id")
                    .and_then(|id| id.as_str())
                    .and_then(|id| Uuid::parse_str(id).ok())
                    == Some(mapping.relay_connection_id)
            })?;
            Some(serde_json::json!({
                "mappingId": mapping.id,
                "alias": mapping.alias,
                "displayName": mapping.display_name,
                // Relay's view of what is readable, which is the authoritative
                // half. IONe's mapping supplies only the name and the alias.
                "kind": matched.get("kind"),
                "entities": matched.get("entities"),
                "assurance": matched.get("assurance"),
            }))
        })
        .collect();

    Ok(Json(serde_json::json!({ "sources": sources })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskInput {
    #[serde(default)]
    pub request_id: Option<Uuid>,
    pub ask: String,
    /// Mapping ids, not connection ids. The browser names something IONe
    /// issued; the connection id it resolves to comes from the database.
    pub mapping_ids: Vec<Uuid>,
    /// Caller limits may only lower what the grant allows. Relay enforces that;
    /// this passes them through.
    #[serde(default)]
    pub limits: Option<serde_json::Value>,
    #[serde(default)]
    pub result_mode: Option<String>,
}

/// Ask a question. The run is attributed to the person and authorized against
/// the service account's relay grants.
pub async fn ask(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
    Json(input): Json<AskInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, DATA_QUERY).await?;

    if input.ask.trim().is_empty() {
        return Err(AppError::BadRequest("a question is required".into()));
    }
    if input.mapping_ids.is_empty() {
        return Err(AppError::BadRequest("select at least one source".into()));
    }

    let repo = RelayMappingRepo::new(state.pool.clone());
    let effective = repo
        .effective_for_user(ctx.org_id, workspace_id, ctx.user_id)
        .await
        .map_err(AppError::Internal)?;

    // Resolve the browser's mapping ids against what this caller may use. An id
    // for a mapping that is disabled, belongs to another workspace, or is named
    // to somebody else simply does not resolve.
    let mut sources = Vec::with_capacity(input.mapping_ids.len());
    let mut used = Vec::with_capacity(input.mapping_ids.len());
    for id in &input.mapping_ids {
        let mapping = effective
            .iter()
            .find(|m| m.id == *id && m.applies_to(ctx.user_id))
            .ok_or_else(|| AppError::NotFound("source not found".into()))?;
        sources.push(SourceSelection {
            alias: mapping.alias.clone(),
            connection_id: mapping.relay_connection_id,
        });
        used.push(mapping.id);
    }

    let client = relay(&state)?;
    let config = state
        .config
        .relay
        .as_ref()
        .ok_or_else(|| AppError::NotFound("relay is not configured".into()))?;

    let request = CreateRun {
        ask: input.ask.clone(),
        sources,
        model: config.default_model.clone(),
        // A retrievable result, because the UI polls and reconnects. A one-shot
        // stream could not serve a browser that reloads.
        result_mode: input.result_mode.unwrap_or_else(|| "preview".into()),
        delivery: vec!["server_sent_events".into()],
        limits: input.limits,
    };

    let idempotency_key = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(workspace_id.as_bytes());
        h.update(ctx.user_id.as_bytes());
        h.update(input.request_id.unwrap_or_else(Uuid::new_v4).as_bytes());
        h.update(serde_json::to_vec(&request).map_err(|e| AppError::Internal(e.into()))?);
        hex::encode(h.finalize())
    };

    let response = client
        .create_run(&scope_for(&ctx, workspace_id), &request, &idempotency_key)
        .await
        .map_err(map_error)?;

    // The link, so an IONe operator can find the run in relay's audit. Counts
    // and hashes; the answer itself is not recorded here.
    if let Some(run_id) = response
        .get("run")
        .and_then(|r| r.get("id"))
        .and_then(|id| id.as_str())
        .and_then(|id| Uuid::parse_str(id).ok())
    {
        let outcome = response
            .get("outcome")
            .and_then(|o| o.get("kind"))
            .and_then(|k| k.as_str())
            .unwrap_or("started");
        repo.record_run(
            ctx.org_id,
            workspace_id,
            ctx.user_id,
            run_id,
            outcome,
            &used,
            response
                .get("outcome")
                .and_then(|o| o.get("rows"))
                .and_then(|r| r.as_i64()),
            response
                .get("outcome")
                .and_then(|o| o.get("truncated"))
                .and_then(|t| t.as_bool())
                .unwrap_or(false),
            response
                .get("outcome")
                .and_then(|o| o.get("reason"))
                .and_then(|r| r.as_str()),
            response
                .get("run")
                .and_then(|r| r.get("plan_hash"))
                .and_then(|h| h.as_str()),
            None,
            serde_json::json!({}),
        )
        .await
        .map_err(AppError::Internal)?;
    } else {
        return Err(AppError::RelayUpstream(
            "the data service returned no run identity".into(),
        ));
    }

    Ok(Json(response))
}

/// Confirm this caller may see this run before proxying anything about it.
/// Relay checks too; this stops the request before it leaves IONe.
async fn authorize_run(
    state: &AppState,
    ctx: &AuthContext,
    workspace_id: Uuid,
    run_id: Uuid,
) -> Result<(), AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    require_permission(ctx, &state.pool, workspace_id, DATA_QUERY).await?;

    let known = RelayMappingRepo::new(state.pool.clone())
        .has_run(ctx.org_id, workspace_id, ctx.user_id, run_id)
        .await
        .map_err(AppError::Internal)?;
    if !known {
        // The same not-found a nonexistent run gets, so a caller cannot learn
        // which run ids exist by asking.
        return Err(AppError::NotFound("run not found".into()));
    }
    Ok(())
}

pub async fn run_status(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, run_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, AppError> {
    authorize_run(&state, &ctx, workspace_id, run_id).await?;
    relay(&state)?
        .run_status(&scope_for(&ctx, workspace_id), run_id)
        .await
        .map(Json)
        .map_err(map_error)
}

pub async fn cancel_run(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, run_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, AppError> {
    authorize_run(&state, &ctx, workspace_id, run_id).await?;
    relay(&state)?
        .cancel_run(&scope_for(&ctx, workspace_id), run_id)
        .await
        .map(Json)
        .map_err(map_error)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClarificationAnswer {
    pub seq: i32,
    pub answer: String,
}

pub async fn answer_clarification(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, run_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<ClarificationAnswer>,
) -> Result<Json<serde_json::Value>, AppError> {
    authorize_run(&state, &ctx, workspace_id, run_id).await?;
    relay(&state)?
        .answer_clarification(
            &scope_for(&ctx, workspace_id),
            run_id,
            input.seq,
            &input.answer,
        )
        .await
        .map(Json)
        .map_err(map_error)
}

pub async fn run_result(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, run_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, AppError> {
    authorize_run(&state, &ctx, workspace_id, run_id).await?;
    relay(&state)?
        .result(&scope_for(&ctx, workspace_id), run_id)
        .await
        .map(Json)
        .map_err(map_error)
}

pub async fn run_receipts(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, run_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, AppError> {
    authorize_run(&state, &ctx, workspace_id, run_id).await?;
    relay(&state)?
        .receipts(&scope_for(&ctx, workspace_id), run_id)
        .await
        .map(Json)
        .map_err(map_error)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventsQuery {
    /// Forwarded verbatim so relay replays exactly what the browser missed.
    /// Re-deriving it here would replay approximately.
    pub last_event_id: Option<String>,
}

/// Proxy the event stream. Frames pass through unchanged, including their ids,
/// so a browser reconnecting through IONe resumes at the same place it would
/// have resumed talking to relay directly.
pub async fn run_events(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, run_id)): Path<(Uuid, Uuid)>,
    Query(query): Query<EventsQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    authorize_run(&state, &ctx, workspace_id, run_id).await?;

    let last_event_id = match headers.get("last-event-id") {
        Some(value) => Some(
            value
                .to_str()
                .map_err(|_| AppError::BadRequest("invalid event cursor".into()))?,
        ),
        None => query.last_event_id.as_deref(),
    };
    let upstream = relay(&state)?
        .events(&scope_for(&ctx, workspace_id), run_id, last_event_id)
        .await
        .map_err(map_error)?;

    if !upstream.status().is_success() {
        return Err(AppError::RelayUpstream(
            "the data service could not open the event stream".into(),
        ));
    }

    let stream = upstream.bytes_stream();
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
            // Proxies that buffer would defeat the point of a stream.
            (header::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response())
}

/// The audit link: what this workspace has asked, without the answers.
pub async fn recent_runs(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "audit:read").await?;

    let runs = RelayMappingRepo::new(state.pool.clone())
        .recent_runs(ctx.org_id, workspace_id, 100)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(serde_json::json!({ "runs": runs })))
}

pub async fn source_admin_status(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    authorize_source_admin(&state, &ctx, workspace_id).await?;
    let client = relay(&state)?;
    if !client.source_management_enabled() {
        return Err(AppError::NotFound(
            "source administration is not configured".into(),
        ));
    }
    Ok(Json(serde_json::json!({"postgres": true})))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegisterSource {
    pub name: String,
    pub alias: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub schema: String,
    pub tables: Vec<String>,
    pub username: String,
    pub password: String,
    pub tls: String,
    pub isolation: String,
}

pub async fn register_source(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
    Json(input): Json<RegisterSource>,
) -> Result<Json<WorkspaceRelayMapping>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    authorize_source_admin(&state, &ctx, workspace_id).await?;
    let client = relay(&state)?;
    if !client.source_management_enabled() {
        return Err(AppError::NotFound(
            "source administration is not configured".into(),
        ));
    }
    if !alias_is_valid(&input.alias)
        || input.name.trim().is_empty()
        || input.name.len() > 128
        || input.tables.is_empty()
        || input.tables.len() > 100
        || input
            .tables
            .iter()
            .chain([&input.schema])
            .any(|v| !alias_is_valid(v))
        || input.username.is_empty()
        || input.password.is_empty()
        || input.database.is_empty()
        || input.port == 0
        || !matches!(input.tls.as_str(), "verify-full" | "disable")
        || !matches!(
            input.isolation.as_str(),
            "forced_rls" | "dedicated_role" | "single_tenant"
        )
    {
        return Err(AppError::BadRequest(
            "invalid source fields; name approved schema/tables and a read-only role".into(),
        ));
    }
    if input.tls == "disable" && !matches!(input.host.as_str(), "localhost" | "127.0.0.1" | "::1") {
        return Err(AppError::BadRequest(
            "TLS verification is required for remote sources".into(),
        ));
    }
    let repo = RelayMappingRepo::new(state.pool.clone());
    if repo
        .by_alias(ctx.org_id, workspace_id, &input.alias)
        .await
        .map_err(AppError::Internal)?
        .is_some()
    {
        return Err(AppError::BadRequest(
            "that source alias already exists".into(),
        ));
    }
    let mut dsn = url::Url::parse("postgres://localhost").expect("static URL");
    dsn.set_host(Some(&input.host))
        .map_err(|_| AppError::BadRequest("invalid source host".into()))?;
    dsn.set_port(Some(input.port))
        .map_err(|_| AppError::BadRequest("invalid source port".into()))?;
    dsn.set_username(&input.username)
        .map_err(|_| AppError::BadRequest("invalid source user".into()))?;
    dsn.set_password(Some(&input.password))
        .map_err(|_| AppError::BadRequest("invalid source credential".into()))?;
    dsn.set_path(&format!("/{}", input.database));
    dsn.query_pairs_mut().append_pair("sslmode", &input.tls);
    let mut intent = crate::rls::org_scoped_tx(&state.pool, ctx.org_id).await?;
    sqlx::query("INSERT INTO relay_source_registrations (org_id,workspace_id,actor_id,alias) VALUES ($1,$2,$3,$4) ON CONFLICT DO NOTHING")
        .bind(ctx.org_id).bind(workspace_id).bind(ctx.user_id).bind(&input.alias)
        .execute(&mut *intent).await.map_err(anyhow::Error::from)?;
    intent.commit().await.map_err(anyhow::Error::from)?;

    let mut registration = crate::rls::org_scoped_tx(&state.pool, ctx.org_id).await?;
    let locked: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!(
                "relay-source:{}:{}:{}",
                ctx.org_id, workspace_id, input.alias
            ))
            .fetch_one(&mut *registration)
            .await
            .map_err(anyhow::Error::from)?;
    if !locked {
        return Err(AppError::ConflictJson(
            serde_json::json!({"error":"registration_in_progress", "message":"This source is being registered. Retry after it finishes."}),
        ));
    }
    if repo
        .by_alias(ctx.org_id, workspace_id, &input.alias)
        .await
        .map_err(AppError::Internal)?
        .is_some()
    {
        return Err(AppError::BadRequest(
            "that source alias already exists".into(),
        ));
    }
    let request_id: Uuid = sqlx::query_scalar("SELECT request_id FROM relay_source_registrations WHERE workspace_id=$1 AND actor_id=$2 AND alias=$3")
        .bind(workspace_id).bind(ctx.user_id).bind(&input.alias)
        .fetch_optional(&mut *registration).await.map_err(anyhow::Error::from)?
        .ok_or_else(|| AppError::ConflictJson(serde_json::json!({"error":"registration_finished", "message":"The prior registration finished. Retry to start a new one."})))?;
    authorize_source_admin(&state, &ctx, workspace_id).await?;
    let scope = scope_for(&ctx, workspace_id);
    let created = client.manage_source(reqwest::Method::POST, "/v1/connections", &scope,
        serde_json::json!({"request_id": request_id, "workspace_id": workspace_id, "kind": "postgres", "name": input.name,
            "public_config": {"schema": input.schema, "relations": input.tables, "isolation": input.isolation},
            "credential": dsn.as_str()})).await.map_err(map_error)?;
    let id = created
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(|v| Uuid::parse_str(v).ok())
        .ok_or_else(|| {
            AppError::RelayUpstream("source registration returned no identity".into())
        })?;
    let result = async {
        let validation = client.manage_source(reqwest::Method::POST, &format!("/v1/connections/{id}/validate"), &scope,
            serde_json::json!({"claimed_assurance":"provider_verified"})).await.map_err(map_error)?;
        if validation.get("state").and_then(|v| v.as_str()) != Some("active") {
            return Err(AppError::BadRequest("source validation failed; use a read-only role with the selected isolation".into()));
        }
        let catalog = client.manage_source(reqwest::Method::POST, &format!("/v1/connections/{id}/catalog"), &scope,
            serde_json::json!({})).await.map_err(map_error)?;
        if catalog.get("entities").and_then(|v| v.as_u64()).unwrap_or(0) != input.tables.len() as u64 {
            return Err(AppError::BadRequest("the requested tables were not all discovered".into()));
        }
        client.manage_source(reqwest::Method::POST, &format!("/v1/connections/{id}/grants"), &scope,
            serde_json::json!({"workspace_id": workspace_id, "principal_kind":"principal", "principal_id":ctx.user_id.to_string(),
                "entity_allowlist": input.tables})).await.map_err(map_error)?;
        ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
        authorize_source_admin(&state, &ctx, workspace_id).await?;
        repo.create(ctx.org_id, workspace_id, ctx.user_id, NewRelayMapping {
            relay_connection_id: id, display_name: input.name, alias: input.alias.clone(), principal_id: Some(ctx.user_id),
        }).await.map_err(|_| AppError::BadRequest("source mapping could not be saved; retry with an unused alias".into()))
    }.await;
    if result.is_err() {
        let cleanup = client
            .manage_source(
                reqwest::Method::DELETE,
                &format!("/v1/connections/{id}"),
                &scope,
                serde_json::json!({}),
            )
            .await;
        if cleanup.is_err() {
            return Err(AppError::RelayUpstream(format!(
                "source registration failed; administrator must revoke incomplete connection {id}"
            )));
        }
        sqlx::query("DELETE FROM relay_source_registrations WHERE workspace_id=$1 AND actor_id=$2 AND alias=$3 AND request_id=$4")
            .bind(workspace_id).bind(ctx.user_id).bind(&input.alias).bind(request_id)
            .execute(&mut *registration).await.map_err(anyhow::Error::from)?;
    }
    registration.commit().await.map_err(anyhow::Error::from)?;
    result.map(Json)
}

async fn authorize_source_admin(
    state: &AppState,
    ctx: &AuthContext,
    workspace_id: Uuid,
) -> Result<(), AppError> {
    if ctx.is_service_account {
        return Err(AppError::Forbidden);
    }
    let (permissions, _) = crate::repos::RoleRepo::new(state.pool.clone())
        .effective_permissions(ctx.user_id, workspace_id)
        .await
        .map_err(AppError::Internal)?;
    if !permissions.contains("data:sources:write") {
        return Err(AppError::Forbidden);
    }
    Ok(())
}
