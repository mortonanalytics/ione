use sqlx::PgPool;

pub struct MfaRepo {
    #[allow(dead_code)]
    pool: PgPool,
}

impl MfaRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
