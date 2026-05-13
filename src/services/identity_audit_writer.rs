use std::net::IpAddr;

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Clone, Copy, Debug)]
pub enum IdentityEvent {
    OidcLogin,
    OidcLoginFailure,
    Logout,
    SessionRevoke,
    MfaEnroll,
    MfaVerify,
    MfaFail,
    MfaDisable,
    MfaRecoveryConsume,
    MfaRecoveryViewed,
    TokenBrokerGrant,
    TokenBrokerRefresh,
    TokenBrokerRevoke,
    TokenBrokerRevokeUpstreamFailed,
    TrustIssuerCreate,
    TrustIssuerDelete,
}

impl IdentityEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OidcLogin => "oidc_login",
            Self::OidcLoginFailure => "oidc_login_failure",
            Self::Logout => "logout",
            Self::SessionRevoke => "session_revoke",
            Self::MfaEnroll => "mfa_enroll",
            Self::MfaVerify => "mfa_verify",
            Self::MfaFail => "mfa_fail",
            Self::MfaDisable => "mfa_disable",
            Self::MfaRecoveryConsume => "mfa_recovery_consume",
            Self::MfaRecoveryViewed => "mfa_recovery_viewed",
            Self::TokenBrokerGrant => "token_broker_grant",
            Self::TokenBrokerRefresh => "token_broker_refresh",
            Self::TokenBrokerRevoke => "token_broker_revoke",
            Self::TokenBrokerRevokeUpstreamFailed => "token_broker_revoke_upstream_failed",
            Self::TrustIssuerCreate => "trust_issuer_create",
            Self::TrustIssuerDelete => "trust_issuer_delete",
        }
    }
}

pub struct IdentityAuditWriter<'a> {
    pool: &'a PgPool,
}

impl<'a> IdentityAuditWriter<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    pub async fn write(
        &self,
        event: IdentityEvent,
        org_id: Uuid,
        user_id: Option<Uuid>,
        session_id: Option<Uuid>,
        peer_id: Option<Uuid>,
        actor_ip: Option<IpAddr>,
        outcome: &str,
        detail: Value,
    ) -> anyhow::Result<()> {
        let actor_ip = actor_ip.map(|ip| ip.to_string());
        sqlx::query(
            "INSERT INTO identity_audit_events
                (event_type, org_id, user_id, actor_ip, session_id, peer_id, outcome, detail)
             VALUES ($1, $2, $3, $4::inet, $5, $6, $7, $8)",
        )
        .bind(event.as_str())
        .bind(org_id)
        .bind(user_id)
        .bind(actor_ip)
        .bind(session_id)
        .bind(peer_id)
        .bind(outcome)
        .bind(detail)
        .execute(self.pool)
        .await?;
        Ok(())
    }
}
