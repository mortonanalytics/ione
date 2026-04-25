use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use axum::http::StatusCode;
use futures_util::StreamExt;
use serde_json::Value;
use url::Url;

use crate::error::AppError;

pub fn parse_public_url(raw: &str) -> Result<Url, AppError> {
    let url = Url::parse(raw).map_err(|_| AppError::BadRequest("invalid URL".into()))?;
    match url.scheme() {
        "https" => {}
        "http" if std::env::var("IONE_SSRF_DEV").is_ok() => {}
        _ => return Err(AppError::BadRequest("URL must use https".into())),
    }
    if url.host_str().is_none() {
        return Err(AppError::BadRequest("URL host is required".into()));
    }
    Ok(url)
}

pub async fn ensure_public_url(raw: &str) -> Result<Url, AppError> {
    let url = parse_public_url(raw)?;
    reject_private_resolutions(&url).await?;
    Ok(url)
}

pub async fn fetch_public_metadata(
    raw: &str,
    max_bytes: usize,
    timeout: Duration,
) -> Result<Value, AppError> {
    ensure_public_url(raw).await?;

    let resp = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(3))
        .build()
        .map_err(|_| AppError::BadRequest("invalid client metadata".into()))?
        .get(raw)
        .send()
        .await
        .map_err(|_| AppError::BadRequest("invalid client metadata".into()))?;

    if !resp.status().is_success() {
        return Err(AppError::BadRequest("invalid client metadata".into()));
    }

    read_json_body(resp, max_bytes)
        .await
        .map_err(|_| AppError::BadRequest("invalid client metadata".into()))
}

pub async fn public_head(raw: &str, timeout: Duration) -> Result<StatusCode, AppError> {
    ensure_public_url(raw).await?;
    let status = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(3))
        .build()
        .map_err(|_| AppError::BadRequest("invalid endpoint".into()))?
        .head(raw)
        .send()
        .await
        .map_err(|_| AppError::BadRequest("invalid endpoint".into()))?
        .status();
    Ok(status)
}

async fn reject_private_resolutions(url: &Url) -> Result<(), AppError> {
    let host = url
        .host_str()
        .ok_or_else(|| AppError::BadRequest("URL host is required".into()))?;
    let port = url.port_or_known_default().unwrap_or(443);

    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| AppError::BadRequest("invalid URL host".into()))?
        .collect();

    if addrs.is_empty() || addrs.iter().any(|addr| is_private_ip(addr.ip())) {
        return Err(AppError::BadRequest("URL host is not public".into()));
    }

    Ok(())
}

async fn read_json_body(resp: reqwest::Response, max_bytes: usize) -> anyhow::Result<Value> {
    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if body.len() + chunk.len() > max_bytes {
            anyhow::bail!("body too large");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&body)?)
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_private_ipv4(ip),
        IpAddr::V6(ip) => is_private_ipv6(ip),
    }
}

fn is_private_ipv4(ip: Ipv4Addr) -> bool {
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.octets()[0] == 0
        || (ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1]))
}

fn is_private_ipv6(ip: Ipv6Addr) -> bool {
    ip.is_loopback()
        || ip.is_unspecified()
        || ((ip.segments()[0] & 0xffc0) == 0xfe80)
        || ((ip.segments()[0] & 0xfe00) == 0xfc00)
}
