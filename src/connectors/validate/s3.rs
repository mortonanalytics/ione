use serde_json::{json, Value};

use super::{ValidateErr, ValidateOk, ValidateResult, Validator};

pub struct S3Validator;

#[async_trait::async_trait]
impl Validator for S3Validator {
    async fn validate(config: &Value) -> ValidateResult {
        let bucket = config
            .get("bucket")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ValidateErr::new("validation_failed", "bucket is required.").with_field("bucket")
            })?;
        let prefix = config.get("prefix").map_or_else(
            || Ok(String::new()),
            |value| {
                value.as_str().map(str::to_string).ok_or_else(|| {
                    ValidateErr::new("validation_failed", "prefix must be a string.")
                        .with_field("prefix")
                })
            },
        )?;
        let endpoint = config
            .get("endpoint")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let region = config
            .get("region")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("us-east-1");

        if let Some(ep) = endpoint {
            crate::util::safe_http::ensure_public_url(ep)
                .await
                .map_err(|_| {
                    ValidateErr::new("validation_failed", "endpoint is not a valid URL.")
                        .with_field("endpoint")
                })?;
        }

        #[cfg(feature = "s3")]
        {
            let sdk_config = build_sdk_config(endpoint, region).await;
            let client = aws_sdk_s3::Client::new(&sdk_config);
            let output = client
                .list_objects_v2()
                .bucket(bucket)
                .prefix(&prefix)
                .max_keys(1)
                .send()
                .await
                .map_err(map_s3_error)?;

            let object_count = output.key_count().unwrap_or_default();
            return Ok(ValidateOk {
                sample: json!({ "objectCount": object_count }),
            });
        }

        #[cfg(not(feature = "s3"))]
        {
            let _ = bucket;
            let _ = prefix;
            let _ = region;
            Ok(ValidateOk {
                sample: json!({ "note": "config shape ok - full dry-run pending aws_sdk_s3 dep" }),
            })
        }
    }
}

#[cfg(feature = "s3")]
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

#[cfg(feature = "s3")]
fn map_s3_error(
    err: aws_sdk_s3::error::SdkError<aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Error>,
) -> ValidateErr {
    let text = err.to_string();
    let lower = text.to_ascii_lowercase();

    let base = if lower.contains("accessdenied") || lower.contains("access denied") {
        ValidateErr::new(
            "s3_access_denied",
            "S3 denied access to the bucket or prefix.",
        )
        .with_hint("Check your credentials and bucket permissions, then click Test again.")
    } else if lower.contains("timeout")
        || lower.contains("dns")
        || lower.contains("dispatch failure")
    {
        ValidateErr::new("network_timeout", &format!("Couldn't reach S3: {text}"))
            .with_hint("Check your network or firewall, then click Test again.")
    } else {
        ValidateErr::new(
            "s3_upstream_error",
            &format!("S3 validation failed: {text}"),
        )
        .with_hint("Verify the bucket, endpoint, region, and credentials.")
    };

    base.with_field("bucket")
}

pub async fn validate(config: &Value) -> ValidateResult {
    S3Validator::validate(config).await
}
