use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use serde_json::json;
use sqlx::PgPool;
use tracing::{info, warn};

use crate::{
    models::{PipelineEventInput, PipelineEventStage},
    repos::PipelineEventRepo,
    repos::{ConnectorRepo, StreamEventRepo, StreamRepo},
    services::pipeline_bus::PipelineBus,
    state::AppState,
};

async fn emit_stage(
    pool: &PgPool,
    bus: &Arc<PipelineBus>,
    workspace_id: uuid::Uuid,
    connector_id: Option<uuid::Uuid>,
    stream_id: Option<uuid::Uuid>,
    stage: PipelineEventStage,
    detail: Option<serde_json::Value>,
) {
    let repo = PipelineEventRepo::new(pool.clone());
    let input = PipelineEventInput {
        workspace_id,
        connector_id,
        stream_id,
        stage,
        detail,
    };
    match repo.append(input).await {
        Ok(event) => bus.publish(event),
        Err(e) => tracing::warn!("pipeline_events append failed: {e}"),
    }
}

async fn emit_error_stage(
    pool: &PgPool,
    bus: &Arc<PipelineBus>,
    workspace_id: uuid::Uuid,
    connector_id: Option<uuid::Uuid>,
    stream_id: Option<uuid::Uuid>,
    stage_name: &str,
    error: impl ToString,
) {
    emit_stage(
        pool,
        bus,
        workspace_id,
        connector_id,
        stream_id,
        PipelineEventStage::Error,
        Some(json!({
            "stage": stage_name,
            "error": error.to_string(),
        })),
    )
    .await;
}

async fn first_inserted_signal_id(
    pool: &PgPool,
    workspace_id: uuid::Uuid,
    source: &str,
    inserted_count: usize,
) -> anyhow::Result<Option<uuid::Uuid>> {
    if inserted_count == 0 {
        return Ok(None);
    }

    sqlx::query_scalar(
        "SELECT id
         FROM (
             SELECT id, created_at
             FROM signals
             WHERE workspace_id = $1
               AND source = $2::signal_source
             ORDER BY created_at DESC
             LIMIT $3
         ) recent
         ORDER BY created_at ASC, id ASC
         LIMIT 1",
    )
    .bind(workspace_id)
    .bind(source)
    .bind(inserted_count as i64)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

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
        let mut first_signal_emitted = false;
        let mut first_survivor_emitted = false;
        let mut first_decision_emitted = false;
        let mut first_event_emitted = false;

        // (a) Poll every active connector's streams
        if let Err(e) =
            poll_workspace_connectors(state, workspace_id, &mut first_event_emitted).await
        {
            emit_error_stage(
                &state.pool,
                &state.pipeline_bus,
                workspace_id,
                None,
                None,
                "poll",
                &e,
            )
            .await;
            warn!(workspace_id = %workspace_id, error = %e, "connector poll failed");
        }

        // (b) Run rules engine
        match super::rules::evaluate_workspace(&state.pool, workspace_id).await {
            Ok(n) if n > 0 => {
                info!(workspace_id = %workspace_id, signals = n, "rules produced signals");
                if !first_signal_emitted {
                    match first_inserted_signal_id(&state.pool, workspace_id, "rule", n).await {
                        Ok(Some(signal_id)) => {
                            emit_stage(
                                &state.pool,
                                &state.pipeline_bus,
                                workspace_id,
                                None,
                                None,
                                PipelineEventStage::FirstSignal,
                                Some(json!({
                                    "signal_id": signal_id,
                                    "source": "rule",
                                })),
                            )
                            .await;
                            emit_first_real_signal(state, workspace_id, signal_id, "rule").await;
                            first_signal_emitted = true;
                        }
                        Ok(None) => {}
                        Err(e) => warn!(
                            workspace_id = %workspace_id,
                            error = %e,
                            "failed to load first rule signal for pipeline event"
                        ),
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                emit_error_stage(
                    &state.pool,
                    &state.pipeline_bus,
                    workspace_id,
                    None,
                    None,
                    "rules",
                    &e,
                )
                .await;
                warn!(workspace_id = %workspace_id, error = %e, "rules engine error");
            }
        }

        // (c) Run generator (unless IONE_SKIP_LIVE=1)
        if !skip_live {
            match super::generator::run_for_workspace(&state.pool, workspace_id).await {
                Ok(n) if n > 0 => {
                    info!(workspace_id = %workspace_id, signals = n, "generator produced signals");
                    if !first_signal_emitted {
                        match first_inserted_signal_id(&state.pool, workspace_id, "generator", n)
                            .await
                        {
                            Ok(Some(signal_id)) => {
                                emit_stage(
                                    &state.pool,
                                    &state.pipeline_bus,
                                    workspace_id,
                                    None,
                                    None,
                                    PipelineEventStage::FirstSignal,
                                    Some(json!({
                                        "signal_id": signal_id,
                                        "source": "generator",
                                    })),
                                )
                                .await;
                                emit_first_real_signal(state, workspace_id, signal_id, "generator")
                                    .await;
                            }
                            Ok(None) => {}
                            Err(e) => warn!(
                                workspace_id = %workspace_id,
                                error = %e,
                                "failed to load first generator signal for pipeline event"
                            ),
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    emit_error_stage(
                        &state.pool,
                        &state.pipeline_bus,
                        workspace_id,
                        None,
                        None,
                        "generator",
                        &e,
                    )
                    .await;
                    warn!(workspace_id = %workspace_id, error = %e, "generator error");
                }
            }
        }

        // (d) Run critic for new signals (those without a survivor yet).
        //     Budget: at most 20 signals per workspace per tick.
        if !skip_live {
            run_critic_for_workspace(state, workspace_id, &mut first_survivor_emitted).await;
        }

        // (e) Run router for surviving survivors without routing decisions yet.
        //     Budget: 20 survivors per workspace per tick. Skipped when IONE_SKIP_LIVE=1.
        if !skip_live {
            run_router_for_workspace(state, workspace_id, &mut first_decision_emitted).await;
        }
    }

    Ok(())
}

async fn emit_first_real_signal(
    state: &AppState,
    workspace_id: uuid::Uuid,
    signal_id: uuid::Uuid,
    source: &str,
) {
    if workspace_id == crate::demo::DEMO_WORKSPACE_ID {
        return;
    }

    let already_emitted: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM funnel_events
            WHERE event_kind = 'first_real_signal'
              AND workspace_id = $1
              AND occurred_at > now() - interval '1 day'
        )",
    )
    .bind(workspace_id)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(false);
    if already_emitted {
        return;
    }

    let session_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, workspace_id.as_bytes());
    crate::services::funnel::track(
        state,
        session_id,
        None,
        Some(workspace_id),
        "first_real_signal",
        Some(json!({
            "signalId": signal_id,
            "source": source,
        })),
    );

    let activation_repo = crate::repos::ActivationRepo::new(state.pool.clone());
    let inserted = activation_repo
        .mark(
            state.default_user_id,
            workspace_id,
            crate::models::ActivationTrack::RealActivation,
            crate::models::ActivationStepKey::FirstSignal,
        )
        .await
        .unwrap_or(false);
    if inserted
        && activation_repo
            .is_track_complete(
                state.default_user_id,
                workspace_id,
                crate::models::ActivationTrack::RealActivation,
            )
            .await
            .unwrap_or(false)
    {
        crate::services::funnel::track(
            state,
            session_id,
            Some(state.default_user_id),
            Some(workspace_id),
            "activation_completed",
            Some(json!({ "track": "real_activation" })),
        );
    }
}

async fn run_critic_for_workspace(
    state: &AppState,
    workspace_id: uuid::Uuid,
    first_survivor_emitted: &mut bool,
) {
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
            Ok(Some(survivor)) => {
                if !*first_survivor_emitted {
                    emit_stage(
                        &state.pool,
                        &state.pipeline_bus,
                        workspace_id,
                        None,
                        None,
                        PipelineEventStage::FirstSurvivor,
                        Some(json!({
                            "survivor_id": survivor.id,
                            "verdict": survivor.verdict.as_str(),
                        })),
                    )
                    .await;
                    *first_survivor_emitted = true;
                }
                info!(workspace_id = %workspace_id, signal_id = %signal_id, "critic evaluated signal")
            }
            Ok(None) => {}
            Err(e) => {
                emit_error_stage(
                    &state.pool,
                    &state.pipeline_bus,
                    workspace_id,
                    None,
                    None,
                    "critic",
                    &e,
                )
                .await;
                warn!(workspace_id = %workspace_id, signal_id = %signal_id, error = %e, "critic error")
            }
        }
    }
}

async fn run_router_for_workspace(
    state: &AppState,
    workspace_id: uuid::Uuid,
    first_decision_emitted: &mut bool,
) {
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
                if !*first_decision_emitted {
                    let first = &decisions[0];
                    emit_stage(
                        &state.pool,
                        &state.pipeline_bus,
                        workspace_id,
                        None,
                        None,
                        PipelineEventStage::FirstDecision,
                        Some(json!({
                            "routing_decision_id": first.id,
                            "target": first.target_kind.as_str(),
                        })),
                    )
                    .await;
                    *first_decision_emitted = true;
                }
                info!(
                    workspace_id = %workspace_id,
                    survivor_id = %survivor_id,
                    count = decisions.len(),
                    "router classified survivor"
                )
            }
            Ok(_) => {}
            Err(e) => {
                emit_error_stage(
                    &state.pool,
                    &state.pipeline_bus,
                    workspace_id,
                    None,
                    None,
                    "router",
                    &e,
                )
                .await;
                warn!(workspace_id = %workspace_id, survivor_id = %survivor_id, error = %e, "router error")
            }
        }
    }
}

async fn poll_workspace_connectors(
    state: &AppState,
    workspace_id: uuid::Uuid,
    first_event_emitted: &mut bool,
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
                emit_error_stage(
                    &state.pool,
                    &state.pipeline_bus,
                    workspace_id,
                    Some(connector.id),
                    None,
                    "connector_build",
                    &e,
                )
                .await;
                warn!(connector_id = %connector.id, error = %e, "failed to build connector impl");
                continue;
            }
        };

        let streams = match stream_repo.list(connector.id).await {
            Ok(streams) => streams,
            Err(e) => {
                emit_error_stage(
                    &state.pool,
                    &state.pipeline_bus,
                    workspace_id,
                    Some(connector.id),
                    None,
                    "stream_list",
                    &e,
                )
                .await;
                warn!(
                    connector_id = %connector.id,
                    error = %e,
                    "failed to list connector streams"
                );
                continue;
            }
        };

        for stream in streams {
            emit_stage(
                &state.pool,
                &state.pipeline_bus,
                workspace_id,
                Some(connector.id),
                Some(stream.id),
                PipelineEventStage::PublishStarted,
                None,
            )
            .await;

            let first_event_seen = Arc::new(AtomicBool::new(false));
            let watchdog_seen = Arc::clone(&first_event_seen);
            let watchdog_pool = state.pool.clone();
            let watchdog_bus = Arc::clone(&state.pipeline_bus);
            let connector_id = connector.id;
            let stream_id = stream.id;
            let stall_watchdog = tokio::spawn(async move {
                let started_at = tokio::time::Instant::now();
                tokio::time::sleep(Duration::from_secs(10)).await;
                if !watchdog_seen.load(Ordering::SeqCst) {
                    emit_stage(
                        &watchdog_pool,
                        &watchdog_bus,
                        workspace_id,
                        Some(connector_id),
                        Some(stream_id),
                        PipelineEventStage::Stall,
                        Some(json!({
                            "waiting_on": "first_event",
                            "elapsed_ms": started_at.elapsed().as_millis() as u64,
                        })),
                    )
                    .await;
                }
            });

            let cursor = match event_repo.latest_observed_at(stream.id).await {
                Ok(cursor) => cursor.map(|dt| json!({ "observed_at": dt.to_rfc3339() })),
                Err(e) => {
                    stall_watchdog.abort();
                    emit_error_stage(
                        &state.pool,
                        &state.pipeline_bus,
                        workspace_id,
                        Some(connector.id),
                        Some(stream.id),
                        "poll_cursor",
                        &e,
                    )
                    .await;
                    warn!(
                        connector_id = %connector.id,
                        stream_id = %stream.id,
                        error = %e,
                        "failed to load stream cursor"
                    );
                    continue;
                }
            };

            let poll_future = async {
                let poll_result = impl_.poll(&stream.name, cursor).await?;
                let mut inserted_count = 0usize;

                for evt in poll_result.events {
                    if event_repo
                        .insert_if_absent(stream.id, evt.payload, evt.observed_at)
                        .await?
                    {
                        inserted_count += 1;
                        first_event_seen.store(true, Ordering::SeqCst);
                    }
                }

                Ok::<usize, anyhow::Error>(inserted_count)
            };

            let poll_outcome = tokio::time::timeout(Duration::from_secs(60), poll_future).await;
            stall_watchdog.abort();

            match poll_outcome {
                Ok(Ok(inserted_count)) => {
                    if inserted_count > 0 && !*first_event_emitted {
                        emit_stage(
                            &state.pool,
                            &state.pipeline_bus,
                            workspace_id,
                            Some(connector.id),
                            Some(stream.id),
                            PipelineEventStage::FirstEvent,
                            Some(json!({ "event_count": inserted_count })),
                        )
                        .await;
                        *first_event_emitted = true;
                    }
                }
                Ok(Err(e)) => {
                    emit_error_stage(
                        &state.pool,
                        &state.pipeline_bus,
                        workspace_id,
                        Some(connector.id),
                        Some(stream.id),
                        "poll",
                        &e,
                    )
                    .await;
                    warn!(
                        connector_id = %connector.id,
                        stream = %stream.name,
                        error = %e,
                        "connector poll error"
                    );
                    let err = e.to_string();
                    let _ = connector_repo
                        .update_status(
                            connector.id,
                            crate::models::ConnectorStatus::Error,
                            Some(err.as_str()),
                        )
                        .await;
                    continue;
                }
                Err(_) => {
                    let timeout_error = format!(
                        "poll timed out after {} ms",
                        Duration::from_secs(60).as_millis()
                    );
                    emit_error_stage(
                        &state.pool,
                        &state.pipeline_bus,
                        workspace_id,
                        Some(connector.id),
                        Some(stream.id),
                        "poll",
                        &timeout_error,
                    )
                    .await;
                    warn!(
                        connector_id = %connector.id,
                        stream = %stream.name,
                        "connector poll timed out"
                    );
                    let _ = connector_repo
                        .update_status(
                            connector.id,
                            crate::models::ConnectorStatus::Error,
                            Some(timeout_error.as_str()),
                        )
                        .await;
                    continue;
                }
            }
        }
    }

    Ok(())
}
