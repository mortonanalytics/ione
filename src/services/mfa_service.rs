use sqlx::PgPool;

use crate::services::IdentityAuditWriter;

pub struct MfaService<'a> {
    #[allow(dead_code)]
    pool: &'a PgPool,
    #[allow(dead_code)]
    audit: &'a IdentityAuditWriter<'a>,
}

impl<'a> MfaService<'a> {
    pub fn new(pool: &'a PgPool, audit: &'a IdentityAuditWriter<'a>) -> Self {
        Self { pool, audit }
    }
}
