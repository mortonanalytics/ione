use axum::{
    body::Body,
    extract::DefaultBodyLimit,
    http::{header, HeaderValue, Method, Request},
    middleware::{from_fn, from_fn_with_state, Next},
    response::Response,
    routing::{delete, get, post, put},
    Router,
};
use tower_http::{cors::CorsLayer, services::ServeDir, trace::TraceLayer};

use crate::{
    auth::{auth_middleware, enforce_auth, AuthContext},
    mcp_server,
    middleware::demo_guard::demo_write_guard,
    state::AppState,
};

pub mod activation;
pub mod admin;
pub mod approvals;
pub mod artifacts;
pub mod audit_events;
pub mod auth_routes;
pub mod bindings;
pub mod broker;
pub mod chart_data;
pub mod chart_panels;
pub mod chat;
pub mod connectors;
pub mod conversations;
pub mod document_panels;
pub mod event_aggregates;
pub mod event_layers;
pub mod event_table;
pub mod feed;
pub mod health;
pub mod map_layers;
pub mod mcp_clients;
pub mod me;
pub mod mfa;
pub mod oauth;
pub mod peers;
pub mod pipeline_events;
pub mod public_issuers;
pub mod signals;
pub mod survivors;
pub mod table_data;
pub mod table_panels;
pub mod telemetry;
pub mod webhooks;
pub mod well_known;
pub mod workspaces;

pub fn router(state: AppState) -> Router {
    let static_dir = state.config.static_dir.clone();

    // Routes that are always public (no auth middleware).
    let public = Router::new()
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth::discovery),
        )
        .route(
            "/.well-known/mcp-client",
            get(well_known::mcp_client_metadata),
        )
        .route("/mcp/oauth/register", post(oauth::register))
        .route("/mcp/oauth/token", post(oauth::token))
        .route("/mcp/oauth/revoke", post(oauth::revoke))
        .route("/api/v1/health", get(health::health))
        .route("/api/v1/health/ollama", get(health::health_ollama))
        .route("/auth/login", get(auth_routes::login))
        .route("/auth/callback", get(auth_routes::callback))
        .route("/auth/issuers", get(public_issuers::list))
        .route("/auth/broker/callback", get(broker::callback))
        .route("/auth/logout", post(auth_routes::logout))
        .route("/api/v1/peers/callback", get(peers::callback))
        .route(
            "/webhooks/peer/:peer_id",
            post(webhooks::receive_webhook).layer(DefaultBodyLimit::max(256 * 1024)),
        )
        .with_state(state.clone());

    // Routes that run through the auth middleware.
    let protected = Router::new()
        .route(
            "/mcp/oauth/authorize",
            get(oauth::authorize).post(oauth::authorize_consent),
        )
        .route("/api/v1/activation", get(activation::list))
        .route("/api/v1/activation/events", post(activation::mark))
        .route("/api/v1/activation/dismiss", post(activation::dismiss))
        .route("/api/v1/mcp/clients", get(mcp_clients::list_clients))
        .route(
            "/api/v1/mcp/clients/:id",
            axum::routing::delete(mcp_clients::revoke_client),
        )
        .route("/api/v1/chat", post(chat::chat))
        .route(
            "/api/v1/conversations",
            get(conversations::list_conversations).post(conversations::create_conversation),
        )
        .route(
            "/api/v1/conversations/:id",
            get(conversations::get_conversation),
        )
        .route(
            "/api/v1/conversations/:id/messages",
            post(conversations::post_message),
        )
        .route(
            "/api/v1/workspaces",
            get(workspaces::list_workspaces).post(workspaces::create_workspace),
        )
        .route("/api/v1/workspaces/:id", get(workspaces::get_workspace))
        .route(
            "/api/v1/workspaces/:id/map-layers",
            get(map_layers::list_map_layers),
        )
        .route(
            "/api/v1/workspaces/:id/chart-panels",
            get(chart_panels::list_chart_panels),
        )
        .route(
            "/api/v1/workspaces/:id/chart-data",
            get(chart_data::get_chart_data),
        )
        .route(
            "/api/v1/workspaces/:id/table-panels",
            get(table_panels::list_table_panels),
        )
        .route(
            "/api/v1/workspaces/:id/table-data",
            get(table_data::get_table_data),
        )
        .route(
            "/api/v1/workspaces/:id/document-panels",
            get(document_panels::list_document_panels),
        )
        .route(
            "/api/v1/workspaces/:id/event-aggregates",
            get(event_aggregates::get_event_aggregates),
        )
        .route(
            "/api/v1/workspaces/:id/event-table",
            get(event_table::get_event_table),
        )
        .route(
            "/api/v1/workspaces/:id/event-layers",
            get(event_layers::list_event_layers),
        )
        .route(
            "/api/v1/workspaces/:id/close",
            post(workspaces::close_workspace),
        )
        .route(
            "/api/v1/workspaces/:id/connectors",
            get(connectors::list_connectors).post(connectors::create_connector),
        )
        .route(
            "/api/v1/connectors/validate",
            post(connectors::validate_connector),
        )
        .route(
            "/api/v1/connectors/:id/streams",
            get(connectors::list_streams),
        )
        .route("/api/v1/streams/:id/poll", post(connectors::poll_stream))
        .route(
            "/api/v1/streams/:id/view-config",
            put(connectors::put_stream_view_config),
        )
        .route("/api/v1/workspaces/:id/signals", get(signals::list_signals))
        .route(
            "/api/v1/workspaces/:id/survivors",
            get(survivors::list_survivors),
        )
        .route("/api/v1/workspaces/:id/feed", get(feed::get_feed))
        .route(
            "/api/v1/workspaces/:id/events",
            get(pipeline_events::list_events),
        )
        .route(
            "/api/v1/workspaces/:id/events/stream",
            get(pipeline_events::stream_events),
        )
        .route("/api/v1/workspaces/:id/roles", get(workspaces::list_roles))
        .route(
            "/api/v1/workspaces/:id/bindings",
            get(bindings::list_for_workspace).post(bindings::create_binding),
        )
        .route(
            "/api/v1/workspaces/:id/bindings/:bindingId",
            get(bindings::get_binding)
                .patch(bindings::patch_binding)
                .delete(bindings::delete_binding),
        )
        .route(
            "/api/v1/workspaces/:id/bindings/:bindingId/refresh",
            post(bindings::refresh_binding),
        )
        .route(
            "/api/v1/workspaces/:id/artifacts",
            get(artifacts::list_artifacts),
        )
        .route(
            "/api/v1/workspaces/:id/approvals",
            get(approvals::list_approvals),
        )
        .route(
            "/api/v1/workspaces/:id/audit_events",
            get(audit_events::list_audit_events),
        )
        .route("/api/v1/telemetry/events", post(telemetry::track_event))
        .route("/api/v1/admin/funnel", get(telemetry::admin_funnel))
        .route(
            "/api/v1/admin/trust-issuers",
            get(admin::trust_issuers::list).post(admin::trust_issuers::create),
        )
        .route(
            "/api/v1/admin/trust-issuers/:id",
            delete(admin::trust_issuers::delete),
        )
        .route("/api/v1/me/mfa", get(mfa::status))
        .route("/api/v1/me/mfa/totp/enroll", post(mfa::enroll_totp))
        .route("/api/v1/me/mfa/totp/confirm", post(mfa::confirm_totp))
        .route("/api/v1/me/mfa/totp", delete(mfa::delete_totp))
        .route("/api/v1/me/mfa/challenge", post(mfa::challenge))
        .route("/api/v1/me/mfa/recovery-codes", get(mfa::recovery_codes))
        .route(
            "/api/v1/broker/connections",
            get(broker::list).post(broker::begin),
        )
        .route("/api/v1/broker/connections/:id", delete(broker::revoke))
        .route(
            "/api/v1/broker/connections/:id/refresh",
            post(broker::refresh),
        )
        .route("/api/v1/approvals/:id", post(approvals::decide_approval))
        .route(
            "/api/v1/peers",
            get(peers::list_peers).post(peers::create_peer),
        )
        .route("/api/v1/peers/:id/manifest", get(peers::get_manifest))
        .route(
            "/api/v1/peers/:id/webhook/provision",
            post(webhooks::provision_webhook),
        )
        .route(
            "/api/v1/peers/:id/authorize",
            post(peers::authorize_allowlist),
        )
        .route("/api/v1/peers/:id/bindings", get(bindings::list_for_peer))
        .route("/api/v1/peers/:id", delete(peers::delete_peer))
        .route(
            "/api/v1/workspaces/:id/peers/:peerId/subscribe",
            post(peers::subscribe_peer),
        )
        .route("/api/v1/me", get(me::me))
        .route_layer(from_fn_with_state(state.clone(), demo_write_guard))
        .route_layer(from_fn(enforce_auth))
        .route_layer(from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state.clone());

    let cors = cors_layer_from_env();

    // MCP server routes use OAuth bearer tokens; /mcp/oauth/* is registered separately.
    let mcp = mcp_server::router()
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::mcp_bearer::mcp_bearer,
        ))
        .with_state(state.clone());

    Router::new()
        .merge(public)
        .merge(protected)
        .merge(mcp)
        .nest_service("/", ServeDir::new(static_dir))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .layer(from_fn(nosniff))
        .layer(axum::middleware::from_fn(
            crate::middleware::session_cookie::session_cookie,
        ))
}

async fn nosniff(req: Request<Body>, next: Next) -> Response {
    let mut response = next.run(req).await;
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn cors_layer_from_env() -> CorsLayer {
    let origins = std::env::var("IONE_CORS_ALLOWED_ORIGINS").unwrap_or_default();
    let parsed: Vec<HeaderValue> = origins
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    if parsed.is_empty() {
        CorsLayer::new()
    } else {
        CorsLayer::new()
            .allow_origin(parsed)
            .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
            .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION])
    }
}

pub async fn mfa_gate(
    ctx: &AuthContext,
    pool: &sqlx::PgPool,
) -> Result<(), crate::error::AppError> {
    let enrolled: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM mfa_enrollments WHERE user_id = $1 AND activated_at IS NOT NULL)",
    )
    .bind(ctx.user_id)
    .fetch_one(pool)
    .await
    .unwrap_or(false);
    let org_requires_admin: bool = sqlx::query_scalar(
        "SELECT COALESCE(mfa_required_for_admins, false) FROM organizations WHERE id = $1",
    )
    .bind(ctx.org_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(false);
    let is_admin = if let Some(role_id) = ctx.active_role_id {
        sqlx::query_scalar::<_, i32>("SELECT coc_level FROM roles WHERE id = $1")
            .bind(role_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .unwrap_or(0)
            >= 80
    } else {
        false
    };
    if org_requires_admin && is_admin && !enrolled {
        return Err(crate::error::AppError::MfaEnrollmentRequired);
    }
    if enrolled && !ctx.mfa_verified {
        return Err(crate::error::AppError::MfaRequired);
    }
    Ok(())
}
