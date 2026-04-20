use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{Message, MessageRole};

pub struct MessageRepo {
    pub pool: PgPool,
}

impl MessageRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn append(
        &self,
        conversation_id: Uuid,
        role: MessageRole,
        content: &str,
        model: Option<&str>,
    ) -> anyhow::Result<Message> {
        sqlx::query_as::<_, Message>(
            "INSERT INTO messages (conversation_id, role, content, model)
             VALUES ($1, $2, $3, $4)
             RETURNING id, conversation_id, role, content, model, tokens_in, tokens_out, created_at",
        )
        .bind(conversation_id)
        .bind(role)
        .bind(content)
        .bind(model)
        .fetch_one(&self.pool)
        .await
        .context("failed to append message")
    }

    pub async fn list(&self, conversation_id: Uuid) -> anyhow::Result<Vec<Message>> {
        sqlx::query_as::<_, Message>(
            "SELECT id, conversation_id, role, content, model, tokens_in, tokens_out, created_at
             FROM messages
             WHERE conversation_id = $1
             ORDER BY created_at ASC",
        )
        .bind(conversation_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list messages")
    }
}
