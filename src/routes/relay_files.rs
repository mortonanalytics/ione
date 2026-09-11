use super::relay::{authorize_source_admin, map_error, scope_for};
use crate::{
    auth::{ensure_workspace_in_org, AuthContext},
    error::AppError,
    models::{NewRelayMapping, WorkspaceRelayMapping},
    repos::RelayMappingRepo,
    state::AppState,
};
use axum::{
    extract::{Path, State},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegisterFile {
    name: String,
    alias: String,
    endpoint: String,
    region: String,
    bucket: String,
    prefix: String,
    table: String,
    path: String,
    format: Format,
    #[serde(default)]
    classification: Classification,
    #[serde(default)]
    columns: Vec<Column>,
    csv: Option<Csv>,
    policy_receipt: Receipt,
    access_key_id: String,
    secret_access_key: String,
}
#[derive(Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Format {
    Json,
    Ndjson,
    IpcFile,
    IpcStream,
    Csv,
    Parquet,
}
#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum Classification {
    Public,
    Internal,
    Confidential,
    #[default]
    Restricted,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Column {
    name: String,
    ty: ColumnType,
    nullable: bool,
    #[serde(default)]
    classification: Option<Classification>,
}
#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ColumnType {
    Boolean,
    Int8,
    Int16,
    Int32,
    Int64,
    #[serde(alias = "uint64")]
    UInt64,
    Float32,
    Float64,
    Utf8,
    Date32,
    Decimal {
        precision: u8,
        scale: u8,
    },
    Timestamp {
        timezone: Option<String>,
    },
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Csv {
    delimiter: String,
    quote: String,
    escape: Option<String>,
    header: bool,
    null_value: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    attested_by: String,
    attested_at: chrono::DateTime<chrono::Utc>,
    actions: Vec<String>,
    bucket: String,
    prefix: String,
    signature: String,
    expires_at: chrono::DateTime<chrono::Utc>,
}

impl RegisterFile {
    fn configuration(&self) -> Result<(&'static str, serde_json::Value), AppError> {
        let invalid = || {
            AppError::BadRequest(
                "invalid file source fields, schema, dialect or current read-only policy receipt"
                    .into(),
            )
        };
        let alias = |value: &str| {
            !value.is_empty()
                && value.len() <= 63
                && value.bytes().enumerate().all(|(i, b)| {
                    b == b'_' || b.is_ascii_lowercase() || (i > 0 && b.is_ascii_digit())
                })
        };
        if !alias(&self.alias)
            || !alias(&self.table)
            || self.name.trim().is_empty()
            || self.name.len() > 128
            || self.name.chars().any(char::is_control)
            || self.region.is_empty()
            || self.region.len() > 128
            || !self
                .region
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || self.bucket.len() < 3
            || self.bucket.len() > 63
            || !self
                .bucket
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
            || self.access_key_id.is_empty()
            || self.access_key_id.len() > 1024
            || self.secret_access_key.is_empty()
            || self.secret_access_key.len() > 4096
        {
            return Err(invalid());
        }
        for path in [&self.prefix, &self.path] {
            if path.is_empty()
                || path.len() > 1024
                || path.contains(['%', '?', '#', '\\'])
                || path.chars().any(char::is_control)
                || path
                    .split('/')
                    .any(|part| part.is_empty() || part == "." || part == "..")
            {
                return Err(invalid());
            }
        }
        let endpoint = url::Url::parse(&self.endpoint).map_err(|_| invalid())?;
        let loopback = match endpoint.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            Some(url::Host::Domain("localhost")) => true,
            _ => false,
        };
        if self.endpoint.len() > 2048
            || !matches!(endpoint.scheme(), "http" | "https")
            || (endpoint.scheme() == "http" && !loopback)
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || endpoint.path() != "/"
        {
            return Err(AppError::BadRequest("use an HTTPS object-store origin without credentials or paths; HTTP is limited to loopback".into()));
        }
        let receipt = &self.policy_receipt;
        let now = chrono::Utc::now();
        if serde_json::to_vec(receipt)
            .map_err(anyhow::Error::from)?
            .len()
            > 16384
            || receipt.bucket != self.bucket
            || receipt.prefix != self.prefix
            || receipt.attested_at > now
            || receipt.expires_at <= now
            || receipt.attested_at >= receipt.expires_at
            || receipt.attested_by.trim().is_empty()
            || receipt.signature.trim().is_empty()
            || receipt.actions.len() > 3
            || !receipt.actions.iter().any(|a| a == "s3:GetObject")
            || !receipt.actions.iter().any(|a| a == "s3:ListBucket")
            || receipt.actions.iter().any(|a| {
                !matches!(
                    a.as_str(),
                    "s3:GetObject" | "s3:GetObjectVersion" | "s3:ListBucket"
                )
            })
        {
            return Err(invalid());
        }
        if self.format == Format::Parquet {
            if !self.columns.is_empty() || self.csv.is_some() {
                return Err(invalid());
            }
            return Ok((
                "parquet",
                json!({"endpoint":endpoint.as_str(),"region":self.region,"bucket":self.bucket,"prefix":self.prefix,"tables":{&self.table:&self.path},"policy_receipt":receipt,"allow_http":endpoint.scheme()=="http","classification":self.classification,"bounds":{"max_footer_bytes":8388608,"max_schema_depth":16,"max_row_groups":4096,"max_pages":1000000,"max_dictionary_bytes":67108864,"max_expansion_ratio":100,"max_object_bytes":8388608,"max_objects":1,"max_geometry_bytes":8388608,"verify_checksums":true}}),
            ));
        }
        if self.columns.is_empty() || self.columns.len() > 64 {
            return Err(invalid());
        }
        let mut names = std::collections::BTreeSet::new();
        for column in &self.columns {
            if !alias(&column.name) || !names.insert(&column.name) {
                return Err(invalid());
            }
            match &column.ty {
                ColumnType::Decimal { precision, scale }
                    if *precision == 0 || *precision > 38 || scale > precision =>
                {
                    return Err(invalid())
                }
                ColumnType::Timestamp { timezone }
                    if !matches!(self.format, Format::IpcFile | Format::IpcStream)
                        || timezone.as_deref().is_some_and(|zone| zone != "UTC") =>
                {
                    return Err(invalid())
                }
                _ => {}
            }
        }
        let columns:Vec<_>=self.columns.iter().map(|column|json!({"name":column.name,"ty":column.ty,"nullable":column.nullable,"classification":self.classification.max(column.classification.unwrap_or(self.classification)),"description":null,"is_key":false})).collect();
        let mut table = json!({"path":self.path,"columns":columns});
        let mut config = json!({"endpoint":endpoint.as_str(),"region":self.region,"policy_receipt":receipt,"max_object_bytes":8388608,"batch_rows":1024,"allow_http":endpoint.scheme()=="http"});
        let kind = if self.format == Format::Csv {
            let dialect = self.csv.as_ref().ok_or_else(invalid)?;
            if dialect.delimiter.len() != 1
                || dialect.quote.len() != 1
                || !dialect
                    .delimiter
                    .bytes()
                    .all(|b| b.is_ascii_graphic() || b == b'\t')
                || !dialect.quote.bytes().all(|b| b.is_ascii_graphic())
                || dialect.delimiter == dialect.quote
                || dialect.escape.as_ref().is_some_and(|v| {
                    v.len() != 1
                        || !v.bytes().all(|b| b.is_ascii_graphic())
                        || v == &dialect.delimiter
                })
                || dialect.null_value.len() > 64
            {
                return Err(invalid());
            }
            table["delimiter"] = json!(dialect.delimiter.as_bytes()[0]);
            table["quote"] = json!(dialect.quote.as_bytes()[0]);
            table["escape"] = json!(dialect.escape.as_ref().map(|v| v.as_bytes()[0]));
            table["header"] = json!(dialect.header);
            table["null_value"] = json!(dialect.null_value);
            config["max_objects"] = json!(1);
            "csv"
        } else {
            if self.csv.is_some() {
                return Err(invalid());
            }
            table["format"] = json!(self.format);
            config["max_decoded_bytes"] = json!(67108864);
            config["max_rows"] = json!(100000);
            "file"
        };
        config["tables"] = json!({&self.table:table});
        Ok((kind, config))
    }
}

pub async fn register_file(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace): Path<Uuid>,
    Json(input): Json<RegisterFile>,
) -> Result<Json<WorkspaceRelayMapping>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace, ctx.org_id).await?;
    authorize_source_admin(&state, &ctx, workspace).await?;
    let (kind, config) = input.configuration()?;
    let client = state
        .relay
        .as_ref()
        .filter(|client| client.source_management_enabled())
        .ok_or_else(|| AppError::NotFound("source administration is not configured".into()))?;
    let scope = scope_for(&ctx, workspace);
    let repo = RelayMappingRepo::new(state.pool.clone());
    let mut intent = crate::rls::org_scoped_tx(&state.pool, ctx.org_id).await?;
    sqlx::query("INSERT INTO relay_source_registrations (org_id,workspace_id,actor_id,alias) SELECT $1,$2,$3,$4 WHERE NOT EXISTS (SELECT 1 FROM workspace_relay_mappings WHERE org_id=$1 AND workspace_id=$2 AND alias=$4) ON CONFLICT DO NOTHING").bind(ctx.org_id).bind(workspace).bind(ctx.user_id).bind(&input.alias).execute(&mut *intent).await.map_err(anyhow::Error::from)?;
    intent.commit().await.map_err(anyhow::Error::from)?;
    let mut registration = crate::rls::org_scoped_tx(&state.pool, ctx.org_id).await?;
    let locked: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!(
                "relay-source:{}:{}:{}",
                ctx.org_id, workspace, input.alias
            ))
            .fetch_one(&mut *registration)
            .await
            .map_err(anyhow::Error::from)?;
    if !locked {
        return Err(AppError::ConflictJson(
            json!({"error":"registration_in_progress","message":"This source is being registered. Retry after it finishes."}),
        ));
    }
    let request_id:Uuid=sqlx::query_scalar("SELECT request_id FROM relay_source_registrations WHERE workspace_id=$1 AND actor_id=$2 AND alias=$3").bind(workspace).bind(ctx.user_id).bind(&input.alias).fetch_optional(&mut *registration).await.map_err(anyhow::Error::from)?.ok_or_else(||AppError::BadRequest("that alias has no matching registration identity".into()))?;
    authorize_source_admin(&state, &ctx, workspace).await?;
    let credential = serde_json::to_string(
        &json!({"access_key_id":input.access_key_id,"secret_access_key":input.secret_access_key}),
    )
    .map_err(anyhow::Error::from)?;
    if let Some(existing) = repo
        .by_alias(ctx.org_id, workspace, &input.alias)
        .await
        .map_err(AppError::Internal)?
    {
        if existing.principal_id != Some(ctx.user_id) || !existing.enabled {
            return Err(AppError::BadRequest(
                "that source alias already exists".into(),
            ));
        }
        let current = client
            .manage_source(
                reqwest::Method::GET,
                &format!("/v1/connections/{}", existing.relay_connection_id),
                &scope,
                json!({}),
            )
            .await
            .map_err(map_error)?;
        ensure_workspace_in_org(&state.pool, workspace, ctx.org_id).await?;
        authorize_source_admin(&state, &ctx, workspace).await?;
        if current["state"] != "active"
            || current["kind"] != kind
            || current["public_config"] != config
            || current["name"] != input.name
        {
            return Err(AppError::BadRequest(
                "that source alias belongs to a different registration".into(),
            ));
        }
        let replay=client.manage_source(reqwest::Method::POST,"/v1/connections",&scope,json!({"request_id":request_id,"workspace_id":workspace,"kind":kind,"name":input.name,"public_config":config,"credential":credential})).await.map_err(map_error)?;
        ensure_workspace_in_org(&state.pool, workspace, ctx.org_id).await?;
        authorize_source_admin(&state, &ctx, workspace).await?;
        if replay["id"] != json!(existing.relay_connection_id) {
            return Err(AppError::RelayUpstream(
                "registration identity mismatch".into(),
            ));
        }
        let fresh = repo
            .by_alias(ctx.org_id, workspace, &input.alias)
            .await
            .map_err(AppError::Internal)?
            .filter(|mapping| {
                mapping.id == existing.id
                    && mapping.enabled
                    && mapping.principal_id == Some(ctx.user_id)
                    && mapping.relay_connection_id == existing.relay_connection_id
            })
            .ok_or_else(|| AppError::NotFound("source mapping unavailable".into()))?;
        registration.commit().await.map_err(anyhow::Error::from)?;
        return Ok(Json(fresh));
    }
    let created=client.manage_source(reqwest::Method::POST,"/v1/connections",&scope,json!({"request_id":request_id,"workspace_id":workspace,"kind":kind,"name":input.name,"public_config":config,"credential":credential})).await.map_err(map_error)?;
    let id = created["id"]
        .as_str()
        .and_then(|v| Uuid::parse_str(v).ok())
        .ok_or_else(|| {
            AppError::RelayUpstream("source registration returned no identity".into())
        })?;
    let result=async {
        ensure_workspace_in_org(&state.pool,workspace,ctx.org_id).await?;
        authorize_source_admin(&state,&ctx,workspace).await?;
        let validation=client.manage_source(reqwest::Method::POST,&format!("/v1/connections/{id}/validate"),&scope,json!({"claimed_assurance":"operator_attested"})).await.map_err(map_error)?;
        ensure_workspace_in_org(&state.pool,workspace,ctx.org_id).await?;
        authorize_source_admin(&state,&ctx,workspace).await?;
        if validation["state"]!="active" {return Err(AppError::BadRequest("source policy validation failed".into()));}
        let catalog=client.manage_source(reqwest::Method::POST,&format!("/v1/connections/{id}/catalog"),&scope,json!({})).await.map_err(map_error)?;
        ensure_workspace_in_org(&state.pool,workspace,ctx.org_id).await?;
        authorize_source_admin(&state,&ctx,workspace).await?;
        if catalog["entities"].as_u64()!=Some(1) {return Err(AppError::BadRequest("the requested table was not discovered".into()));}
        client.manage_source(reqwest::Method::POST,&format!("/v1/connections/{id}/grants"),&scope,json!({"workspace_id":workspace,"principal_kind":"principal","principal_id":ctx.user_id.to_string(),"entity_allowlist":[input.table]})).await.map_err(map_error)?;
        ensure_workspace_in_org(&state.pool,workspace,ctx.org_id).await?;
        authorize_source_admin(&state,&ctx,workspace).await?;
        repo.create(ctx.org_id,workspace,ctx.user_id,NewRelayMapping{relay_connection_id:id,display_name:input.name,alias:input.alias.clone(),principal_id:Some(ctx.user_id)}).await.map_err(|_|AppError::BadRequest("source mapping could not be saved".into()))
    }.await;
    if result.is_err() {
        if client
            .manage_source(
                reqwest::Method::DELETE,
                &format!("/v1/connections/{id}"),
                &scope,
                json!({}),
            )
            .await
            .is_err()
        {
            return Err(AppError::RelayUpstream(format!(
                "source registration failed; administrator must revoke incomplete connection {id}"
            )));
        }
        sqlx::query("DELETE FROM relay_source_registrations WHERE workspace_id=$1 AND actor_id=$2 AND alias=$3 AND request_id=$4").bind(workspace).bind(ctx.user_id).bind(&input.alias).bind(request_id).execute(&mut *registration).await.map_err(anyhow::Error::from)?;
    }
    registration.commit().await.map_err(anyhow::Error::from)?;
    result.map(Json)
}
