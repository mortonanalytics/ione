use std::time::Duration;

use tracing::{info, warn};

use crate::{
    repos::{ConnectorRepo, StreamEventRepo, StreamRepo},
    state::AppState,
};

/// Spawn a background scheduler that polls all active connectors, runs the
/// rules engine, and (unless IONE_SKIP_LIVE=1) runs the generator LLM pass.
/// The scheduler runs every IONE_POLL_INTERVAL_SECS seconds (default 60).
pub fn spawn(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let interval_secs: u64 = std::env::var("IONE_POLL_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);

        let skip_live = std::env::var("IONE_SKIP_LIVE").as_deref() == Ok("1");

        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        // Skip the immediate first tick so the app has time to start
        interval.tick().await;

        loop {
            interval.tick().await;
            info!("scheduler tick starting");

            if let Err(e) = run_tick(&state, skip_live).await {
                warn!(error = %e, "scheduler tick error");
            }
        }
    })
}

async fn run_tick(state: &AppState, skip_live: bool) -> anyhow::Result<()> {
    // List all workspaces
    let workspaces: Vec<(uuid::Uuid, uuid::Uuid)> =
        sqlx::query_as("SELECT id, org_id FROM workspaces WHERE closed_at IS NULL")
            .fetch_all(&state.pool)
            .await?;

    for (workspace_id, _org_id) in workspaces {
        // (a) Poll every active connector's streams
        if let Err(e) = poll_workspace_connectors(state, workspace_id).await {
            warn!(workspace_id = %workspace_id, error = %e, "connector poll failed");
        }

        // (b) Run rules engine
        match super::rules::evaluate_workspace(&state.pool, workspace_id).await {
            Ok(n) if n > 0 => {
                info!(workspace_id = %workspace_id, signals = n, "rules produced signals")
            }
            Ok(_) => {}
            Err(e) => warn!(workspace_id = %workspace_id, error = %e, "rules engine error"),
        }

        // (c) Run generator (unless IONE_SKIP_LIVE=1)
        if !skip_live {
            match super::generator::run_for_workspace(&state.pool, workspace_id).await {
                Ok(n) if n > 0 => {
                    info!(workspace_id = %workspace_id, signals = n, "generator produced signals")
                }
                Ok(_) => {}
                Err(e) => warn!(workspace_id = %workspace_id, error = %e, "generator error"),
            }
        }

        // (d) Run critic for new signals (those without a survivor yet).
        //     Budget: at most 20 signals per workspace per tick.
        if !skip_live {
            run_critic_for_workspace(state, workspace_id).await;
        }

        // (e) Run router for surviving survivors without routing decisions yet.
        //     Budget: 20 survivors per workspace per tick. Skipped when IONE_SKIP_LIVE=1.
        if !skip_live {
            run_router_for_workspace(state, workspace_id).await;
        }
    }

    Ok(())
}

async fn run_critic_for_workspace(state: &AppState, workspace_id: uuid::Uuid) {
    const CRITIC_BUDGET: i64 = 20;

    // Find signals in this workspace that have no survivor yet.
    let signal_ids: Vec<uuid::Uuid> = match sqlx::query_scalar(
        "SELECT s.id FROM signals s
         LEFT JOIN survivors sv ON sv.signal_id = s.id
         WHERE s.workspace_id = $1 AND sv.id IS NULL
         ORDER BY s.created_at DESC
         LIMIT $2",
    )
    .bind(workspace_id)
    .bind(CRITIC_BUDGET)
    .fetch_all(&state.pool)
    .await
    {
        Ok(ids) => ids,
        Err(e) => {
            warn!(workspace_id = %workspace_id, error = %e, "critic: failed to query pending signals");
            return;
        }
    };

    for signal_id in signal_ids {
        match super::critic::evaluate_signal(state, signal_id).await {
            Ok(Some(_)) => {
                info!(workspace_id = %workspace_id, signal_id = %signal_id, "critic evaluated signal")
            }
            Ok(None) => {}
            Err(e) => {
                warn!(workspace_id = %workspace_id, signal_id = %signal_id, error = %e, "critic error")
            }
        }
    }
}

async fn run_router_for_workspace(state: &AppState, workspace_id: uuid::Uuid) {
    const ROUTER_BUDGET: i64 = 20;

    // Find surviving survivors in this workspace that have no routing decisions yet.
    let survivor_ids: Vec<uuid::Uuid> = match sqlx::query_scalar(
        "SELECT sv.id FROM survivors sv
         JOIN signals sig ON sig.id = sv.signal_id
         LEFT JOIN routing_decisions rd ON rd.survivor_id = sv.id
         WHERE sig.workspace_id = $1
           AND sv.verdict = 'survive'::critic_verdict
           AND rd.id IS NULL
         ORDER BY sv.created_at DESC
         LIMIT $2",
    )
    .bind(workspace_id)
    .bind(ROUTER_BUDGET)
    .fetch_all(&state.pool)
    .await
    {
        Ok(ids) => ids,
        Err(e) => {
            warn!(workspace_id = %workspace_id, error = %e, "router: failed to query pending survivors");
            return;
        }
    };

    for survivor_id in survivor_ids {
        match super::router::classify_survivor(state, survivor_id).await {
            Ok(decisions) if !decisions.is_empty() => {
                info!(
                    workspace_id = %workspace_id,
                    survivor_id = %survivor_id,
                    count = decisions.len(),
                    "router classified survivor"
                )
            }
            Ok(_) => {}
            Err(e) => {
                warn!(workspace_id = %workspace_id, survivor_id = %survivor_id, error = %e, "router error")
            }
        }
    }
}

async fn poll_workspace_connectors(
    state: &AppState,
    workspace_id: uuid::Uuid,
) -> anyhow::Result<()> {
    let connector_repo = ConnectorRepo::new(state.pool.clone());
    let stream_repo = StreamRepo::new(state.pool.clone());
    let event_repo = StreamEventRepo::new(state.pool.clone());

    let connectors = connector_repo.list(workspace_id).await?;

    for connector in connectors {
        if connector.status != crate::models::ConnectorStatus::Active {
            continue;
        }

        let impl_ = match crate::connectors::build_from_row(&connector) {
            Ok(c) => c,
            Err(e) => {
                warn!(connector_id = %connector.id, error = %e, "failed to build connector impl");
                continue;
            }
        };

        let streams = stream_repo.list(connector.id).await?;

        for stream in streams {
            let cursor = event_repo
                .latest_observed_at(stream.id)
                .await?
                .map(|dt| serde_json::json!({ "observed_at": dt.to_rfc3339() }));

            let poll_result = match impl_.poll(&stream.name, cursor).await {
                Ok(r) => r,
                Err(e) => {
                    warn!(
                        connector_id = %connector.id,
                        stream = %stream.name,
                        error = %e,
                        "connector poll error"
                    );
                    let _ = connector_repo
                        .update_status(
                            connector.id,
                            crate::models::ConnectorStatus::Error,
                            Some(e.to_string().as_str()),
                        )
                        .await;
                    continue;
                }
            };

            for evt in poll_result.events {
                let _ = event_repo
                    .insert_if_absent(stream.id, evt.payload, evt.observed_at)
                    .await;
            }
        }
    }

    Ok(())
}
