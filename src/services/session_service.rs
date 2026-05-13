use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::set_session_cookie_header_for_session,
    models::UserSession,
    repos::UserSessionRepo,
    services::identity_audit_writer::{IdentityAuditWriter, IdentityEvent},
};

pub struct SessionService<'a> {
    pool: &'a PgPool,
    audit: &'a IdentityAuditWriter<'a>,
}

impl<'a> SessionService<'a> {
    pub fn new(pool: &'a PgPool, audit: &'a IdentityAuditWriter<'a>) -> Self {
        Self { pool, audit }
    }

    pub async fn create(
        &self,
        user_id: Uuid,
        org_id: Uuid,
        idp_type: &str,
    ) -> anyhow::Result<(Uuid, String)> {
        let expires_at = Utc::now() + chrono::Duration::hours(24);
        let session = UserSessionRepo::new(self.pool.clone())
            .create(user_id, org_id, idp_type, expires_at)
            .await?;
        let cookie = set_session_cookie_header_for_session(session.id, expires_at);
        Ok((session.id, cookie))
    }

    pub async fn revoke(&self, session_id: Uuid) -> anyhow::Result<()> {
        UserSessionRepo::new(self.pool.clone())
            .revoke(session_id)
            .await?;
        Ok(())
    }

    pub async fn revoke_with_audit(
        &self,
        session: &UserSession,
        actor_ip: Option<std::net::IpAddr>,
    ) -> anyhow::Result<()> {
        self.revoke(session.id).await?;
        self.audit
            .write(
                IdentityEvent::Logout,
                session.org_id,
                Some(session.user_id),
                Some(session.id),
                None,
                actor_ip,
                "success",
                serde_json::json!({}),
            )
            .await?;
        Ok(())
    }

    pub async fn mark_mfa_verified(&self, session_id: Uuid) -> anyhow::Result<()> {
        UserSessionRepo::new(self.pool.clone())
            .mark_mfa_verified(session_id)
            .await
    }

    pub async fn find_active(&self, session_id: Uuid) -> anyhow::Result<Option<UserSession>> {
        UserSessionRepo::new(self.pool.clone())
            .find_active(session_id)
            .await
    }
}
