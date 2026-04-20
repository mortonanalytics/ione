use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::Conversation;

pub struct ConversationRepo {
    pub pool: PgPool,
}

impl ConversationRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        user_id: Uuid,
        title: &str,
        workspace_id: Option<Uuid>,
    ) -> anyhow::Result<Conversation> {
        // workspace_id is Option here to keep the signature compatible with callers
        // that may not yet have resolved it; the caller is responsible for ensuring
        // it is Some after Phase 3.
        sqlx::query_as::<_, Conversation>(
            "INSERT INTO conversations (user_id, title, workspace_id)
             VALUES ($1, $2, $3)
             RETURNING id, workspace_id, user_id, title, created_at",
        )
        .bind(user_id)
        .bind(title)
        .bind(workspace_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to create conversation")
    }

    pub async fn list(&self, user_id: Uuid) -> anyhow::Result<Vec<Conversation>> {
        sqlx::query_as::<_, Conversation>(
            "SELECT id, workspace_id, user_id, title, created_at
             FROM conversations
             WHERE user_id = $1
             ORDER BY created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list conversations")
    }

    pub async fn get(&self, id: Uuid) -> anyhow::Result<Option<Conversation>> {
        sqlx::query_as::<_, Conversation>(
            "SELECT id, workspace_id, user_id, title, created_at
             FROM conversations
             WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get conversation")
    }
}
