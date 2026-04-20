/// Filesystem / S3 (MinIO-compatible) connector.
///
/// Emits one `StreamEventInput` per file or S3 object, deduplicated via
/// `(stream_id, observed_at=last_modified)`.
///
/// Config shape:
/// ```json
/// {
///   "mode": "fs",
///   "path": "infra/fixtures/docs"
/// }
/// // — or S3 mode —
/// {
///   "mode": "s3",
///   "bucket": "ops-docs",
///   "prefix": "",
///   "endpoint": "http://localhost:9100",
///   "region": "us-east-1"
/// }
/// ```
///
/// S3 credentials are resolved from the environment via `aws-config`'s default
/// provider chain (AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY, ~/.aws/credentials, etc.).
/// For MinIO, set `endpoint` to the MinIO address.
///
/// Feature gate: compiled under the `s3` feature flag.  The `s3` feature is on
/// by default in `Cargo.toml`; remove it from `default-features` if binary size
/// is a concern and S3 is not needed.
use anyhow::Context;
use serde_json::json;

use crate::models::ConnectorKind;

use super::{ConnectorImpl, PollResult, StreamDescriptor, StreamEventInput};

pub struct FsS3Connector {
    pub mode: FsS3Mode,
    pub config: serde_json::Value,
}

#[derive(Debug, Clone)]
pub enum FsS3Mode {
    Fs {
        path: String,
    },
    S3 {
        bucket: String,
        prefix: String,
        endpoint: Option<String>,
        region: String,
    },
}

impl FsS3Connector {
    pub fn from_config(config: &serde_json::Value) -> anyhow::Result<Self> {
        let mode_str = config["mode"].as_str().unwrap_or("fs");
        let mode = match mode_str {
            "s3" => {
                let bucket = config["bucket"]
                    .as_str()
                    .context("fs_s3 s3 mode requires 'bucket'")?
                    .to_string();
                let prefix = config["prefix"].as_str().unwrap_or("").to_string();
                let endpoint = config["endpoint"].as_str().map(str::to_string);
                let region = config["region"].as_str().unwrap_or("us-east-1").to_string();
                FsS3Mode::S3 {
                    bucket,
                    prefix,
                    endpoint,
                    region,
                }
            }
            _ => {
                let path = config["path"]
                    .as_str()
                    .context("fs_s3 fs mode requires 'path'")?
                    .to_string();
                FsS3Mode::Fs { path }
            }
        };
        Ok(Self {
            mode,
            config: config.clone(),
        })
    }
}

#[async_trait::async_trait]
impl ConnectorImpl for FsS3Connector {
    fn kind(&self) -> ConnectorKind {
        ConnectorKind::RustNative
    }

    async fn default_streams(&self) -> anyhow::Result<Vec<StreamDescriptor>> {
        Ok(vec![StreamDescriptor {
            name: "documents".to_string(),
            schema: json!({
                "type": "object",
                "description": "Files or S3 objects"
            }),
        }])
    }

    async fn poll(
        &self,
        stream_name: &str,
        _cursor: Option<serde_json::Value>,
    ) -> anyhow::Result<PollResult> {
        if stream_name != "documents" {
            anyhow::bail!(
                "fs_s3 connector only supports stream 'documents', got '{}'",
                stream_name
            );
        }
        match &self.mode {
            FsS3Mode::Fs { path } => poll_fs(path),
            FsS3Mode::S3 {
                bucket,
                prefix,
                endpoint,
                region,
            } => poll_s3(bucket, prefix, endpoint.as_deref(), region).await,
        }
    }
}

// ─── Filesystem mode ─────────────────────────────────────────────────────────

fn poll_fs(root: &str) -> anyhow::Result<PollResult> {
    let resolved = resolve_path(root);
    let mut events = Vec::new();
    walk_dir(&resolved, &mut events).context("fs walk failed")?;
    Ok(PollResult {
        events,
        next_cursor: None,
    })
}

fn resolve_path(path: &str) -> String {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return path.to_string();
    }
    // Try from CARGO_MANIFEST_DIR first (for tests), then cwd.
    let manifest = env!("CARGO_MANIFEST_DIR");
    let from_manifest = format!("{}/{}", manifest, path);
    if std::path::Path::new(&from_manifest).exists() {
        return from_manifest;
    }
    path.to_string()
}

fn walk_dir(dir: &str, events: &mut Vec<StreamEventInput>) -> anyhow::Result<()> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("cannot read directory '{}'", dir))?;

    for entry in entries {
        let entry = entry.context("directory entry error")?;
        let path = entry.path();
        let meta = entry.metadata().context("metadata error")?;

        if meta.is_dir() {
            walk_dir(path.to_str().unwrap_or(""), events)?;
            continue;
        }

        let last_modified = meta.modified().context("mtime not available")?;
        let observed_at: chrono::DateTime<chrono::Utc> = chrono::DateTime::from(last_modified);

        let size = meta.len();
        let key = path.to_string_lossy().to_string();
        let blob_ref = format!("file://{}", key);

        events.push(StreamEventInput {
            payload: json!({
                "key": key,
                "size": size,
                "blob_ref": blob_ref,
                "last_modified": observed_at.to_rfc3339(),
            }),
            observed_at,
        });
    }
    Ok(())
}

// ─── S3 mode ─────────────────────────────────────────────────────────────────

async fn poll_s3(
    bucket: &str,
    prefix: &str,
    endpoint: Option<&str>,
    region: &str,
) -> anyhow::Result<PollResult> {
    let sdk_config = build_sdk_config(endpoint, region).await;
    let client = aws_sdk_s3::Client::new(&sdk_config);

    let mut events = Vec::new();
    let mut continuation_token: Option<String> = None;

    loop {
        let mut req = client.list_objects_v2().bucket(bucket).prefix(prefix);

        if let Some(token) = continuation_token.take() {
            req = req.continuation_token(token);
        }

        let output = req.send().await.context("S3 list_objects_v2 failed")?;

        for obj in output.contents.unwrap_or_default() {
            let key = obj.key.clone().unwrap_or_default();
            let size = obj.size.unwrap_or(0);
            let etag = obj.e_tag.clone().unwrap_or_default();
            let last_modified_sdk = obj.last_modified;

            let observed_at = last_modified_sdk
                .map(|t| {
                    let secs = t.secs();
                    let nanos = t.subsec_nanos();
                    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, nanos)
                        .unwrap_or_else(chrono::Utc::now)
                })
                .unwrap_or_else(chrono::Utc::now);

            let blob_ref = format!("s3://{}/{}", bucket, key);

            events.push(StreamEventInput {
                payload: json!({
                    "key": key,
                    "size": size,
                    "etag": etag,
                    "last_modified": observed_at.to_rfc3339(),
                    "blob_ref": blob_ref,
                }),
                observed_at,
            });
        }

        if output.is_truncated.unwrap_or(false) {
            continuation_token = output.next_continuation_token;
        } else {
            break;
        }
    }

    Ok(PollResult {
        events,
        next_cursor: None,
    })
}

async fn build_sdk_config(endpoint: Option<&str>, region: &str) -> aws_config::SdkConfig {
    let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest()).region(
        aws_config::meta::region::RegionProviderChain::first_try(aws_sdk_s3::config::Region::new(
            region.to_string(),
        )),
    );

    if let Some(ep) = endpoint {
        loader = loader.endpoint_url(ep);
    }

    loader.load().await
}
