use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use dashmap::DashMap;
use sqlx::PgPool;
use tokio::{
    sync::{broadcast, mpsc},
    task::JoinHandle,
};
use tokio_stream::Stream;
use uuid::Uuid;

use crate::{models::InteractionEvent, repos::InteractionEventRepo};

const WRITE_CHANNEL_CAPACITY: usize = 4096;
const BROADCAST_CHANNEL_CAPACITY: usize = 256;
const BATCH_MAX: usize = 256;
const FLUSH_INTERVAL: Duration = Duration::from_millis(500);

pub type InteractionWriterRx = mpsc::Receiver<InteractionEvent>;

#[derive(Clone)]
pub struct InteractionSink {
    tx: mpsc::Sender<InteractionEvent>,
    bus: broadcast::Sender<InteractionEvent>,
    dropped: Arc<AtomicU64>,
    session_sequences: Arc<DashMap<Uuid, AtomicU64>>,
}

impl InteractionSink {
    pub fn new() -> (Arc<Self>, InteractionWriterRx) {
        let (tx, rx) = mpsc::channel(WRITE_CHANNEL_CAPACITY);
        let (bus, _rx) = broadcast::channel(BROADCAST_CHANNEL_CAPACITY);
        (
            Arc::new(Self {
                tx,
                bus,
                dropped: Arc::new(AtomicU64::new(0)),
                session_sequences: Arc::new(DashMap::new()),
            }),
            rx,
        )
    }

    pub fn next_sequence(&self, session_id: Option<Uuid>) -> Option<i64> {
        let session_id = session_id?;
        let entry = self
            .session_sequences
            .entry(session_id)
            .or_insert_with(|| AtomicU64::new(0));
        let next = entry.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        Some(next.min(i64::MAX as u64) as i64)
    }

    pub fn emit(&self, event: InteractionEvent) {
        let _ = self.bus.send(event.clone());
        if self.tx.try_send(event).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<InteractionEvent> {
        self.bus.subscribe()
    }

    pub fn subscribe_workspace(&self, workspace_id: Uuid) -> impl Stream<Item = InteractionEvent> {
        let mut rx = self.subscribe();
        async_stream::stream! {
            loop {
                match rx.recv().await {
                    Ok(event) if event.workspace_id == workspace_id => yield event,
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, workspace_id = %workspace_id, "interaction SSE subscriber lagged");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn subscriber_count(&self) -> usize {
        self.bus.receiver_count()
    }
}

pub fn spawn_writer(pool: PgPool, mut rx: InteractionWriterRx) -> JoinHandle<()> {
    tokio::spawn(async move {
        let repo = InteractionEventRepo::new(pool);
        let mut batch = Vec::with_capacity(BATCH_MAX);
        let mut tick = tokio::time::interval(FLUSH_INTERVAL);

        loop {
            tokio::select! {
                maybe_event = rx.recv() => {
                    match maybe_event {
                        Some(event) => {
                            batch.push(event);
                            if batch.len() >= BATCH_MAX {
                                flush_batch(&repo, &mut batch).await;
                            }
                        }
                        None => {
                            flush_batch(&repo, &mut batch).await;
                            break;
                        }
                    }
                }
                _ = tick.tick() => {
                    flush_batch(&repo, &mut batch).await;
                }
            }
        }
    })
}

async fn flush_batch(repo: &InteractionEventRepo, batch: &mut Vec<InteractionEvent>) {
    if batch.is_empty() {
        return;
    }
    let rows = std::mem::take(batch);
    if let Err(err) = repo.insert_batch(&rows).await {
        tracing::error!(error = %err, count = rows.len(), "failed to persist interaction events");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ActorKind, InteractionEvent};
    use chrono::Utc;

    fn sample_event(session_id: Option<Uuid>, sequence_number: Option<i64>) -> InteractionEvent {
        InteractionEvent {
            id: Uuid::new_v4(),
            org_id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            peer_id: Uuid::new_v4(),
            peer_name: "peer".into(),
            tool_name: "tool".into(),
            caller_kind: ActorKind::User,
            caller_user_id: Some(Uuid::new_v4()),
            caller_peer_id: None,
            caller_token_id: None,
            session_id,
            sequence_number,
            outcome: "allow".into(),
            latency_ms: Some(12),
            detail: serde_json::json!({}),
            recorded_at: Utc::now(),
        }
    }

    #[test]
    fn per_session_sequence_is_monotonic() {
        let (sink, _rx) = InteractionSink::new();
        let session = Uuid::new_v4();
        assert_eq!(sink.next_sequence(Some(session)), Some(1));
        assert_eq!(sink.next_sequence(Some(session)), Some(2));
        assert_eq!(sink.next_sequence(None), None);
    }

    #[tokio::test]
    async fn publish_with_no_subscribers_still_queues_for_writer() {
        let (sink, mut rx) = InteractionSink::new();
        let event = sample_event(None, None);
        sink.emit(event.clone());
        let queued = rx.recv().await.expect("writer queue receives event");
        assert_eq!(queued.id, event.id);
    }
}
