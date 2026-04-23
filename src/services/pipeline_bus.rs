//! Process-local broadcast channel for pipeline events.
//!
//! The scheduler (and synchronous connector-create handler) call `publish`
//! after persisting a `pipeline_events` row; the SSE endpoint calls
//! `subscribe` to fan out to connected browsers. Channel is bounded at
//! 256 events; slow subscribers lag — SSE client re-fills from the
//! list endpoint on reconnect.

use std::sync::Arc;

use tokio::sync::broadcast;
use tokio_stream::Stream;
use uuid::Uuid;

use crate::models::PipelineEvent;

const CHANNEL_CAPACITY: usize = 256;

#[derive(Clone)]
pub struct PipelineBus {
    inner: Arc<Inner>,
}

struct Inner {
    tx: broadcast::Sender<PipelineEvent>,
}

impl PipelineBus {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            inner: Arc::new(Inner { tx }),
        }
    }

    /// Publish a pipeline event to all subscribers. Silently drops if no
    /// subscribers — that's correct: DB is the source of truth, bus is
    /// a realtime fanout.
    pub fn publish(&self, event: PipelineEvent) {
        let _ = self.inner.tx.send(event);
    }

    /// Subscribe to all pipeline events. Filter per-workspace at the
    /// consumer — keeps the bus shape simple.
    pub fn subscribe(&self) -> broadcast::Receiver<PipelineEvent> {
        self.inner.tx.subscribe()
    }

    /// Count of active receivers — useful for health + tests.
    pub fn subscriber_count(&self) -> usize {
        self.inner.tx.receiver_count()
    }

    /// Scoped subscriber: wraps `subscribe` with a workspace filter.
    /// The SSE handler will use this.
    pub fn subscribe_workspace(&self, workspace_id: Uuid) -> impl Stream<Item = PipelineEvent> {
        let mut rx = self.subscribe();
        async_stream::stream! {
            loop {
                match rx.recv().await {
                    Ok(event) if event.workspace_id == workspace_id => yield event,
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

impl Default for PipelineBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::PipelineEventStage;
    use chrono::Utc;

    fn sample_event(ws: Uuid) -> PipelineEvent {
        PipelineEvent {
            id: Uuid::new_v4(),
            workspace_id: ws,
            connector_id: None,
            stream_id: None,
            stage: PipelineEventStage::PublishStarted,
            detail: None,
            occurred_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn publish_with_no_subscribers_is_noop() {
        let bus = PipelineBus::new();
        bus.publish(sample_event(Uuid::new_v4()));
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn subscribe_receives_published_events() {
        let bus = PipelineBus::new();
        let mut rx = bus.subscribe();
        let ws = Uuid::new_v4();
        let ev = sample_event(ws);
        bus.publish(ev.clone());
        let got = rx.recv().await.expect("recv");
        assert_eq!(got.id, ev.id);
    }
}
