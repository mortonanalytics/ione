use std::{net::Ipv6Addr, time::Duration};

use anyhow::{bail, Context};
use reqwest::Url;
use url::Host;

pub(crate) fn parse_and_validate_url(raw: &str, label: &str) -> anyhow::Result<Url> {
    let url = Url::parse(raw).with_context(|| format!("invalid {} URL '{}'", label, raw))?;
    ensure_safe_url(&url, label)?;
    Ok(url)
}

pub(crate) fn ensure_safe_url(url: &Url, label: &str) -> anyhow::Result<()> {
    // Use the typed Host enum, not host_str(): host_str() returns IPv6 addresses in
    // bracketed form ("[fe80::1]"), which IpAddr::from_str cannot parse — that gap let
    // IPv6 link-local hosts slip past the link-local check over https.
    let host = url
        .host()
        .ok_or_else(|| anyhow::anyhow!("unsafe URL in {}: missing host", label))?;

    // Link-local is blocked for every scheme — cloud metadata (169.254.169.254,
    // fd00:ec2::254 over fe80::/10) is always link-local.
    match &host {
        Host::Ipv4(ip) if ip.is_link_local() => {
            bail!("unsafe URL in {}: blocked link-local IPv4 host", label)
        }
        Host::Ipv6(ip) if is_ipv6_link_local(ip) => {
            bail!("unsafe URL in {}: blocked link-local IPv6 host", label)
        }
        _ => {}
    }

    match url.scheme() {
        // https: private/loopback hosts are intentionally allowed (on-prem peers);
        // link-local is already rejected above.
        "https" => {}
        "http" => {
            if !host_is_http_allowed(&host) {
                bail!(
                    "unsafe URL in {}: http is only allowed for localhost or private IP hosts",
                    label
                );
            }
        }
        other => bail!("unsafe URL in {}: unsupported scheme '{}'", label, other),
    }

    Ok(())
}

pub(crate) fn guarded_client(timeout_ms: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(timeout_ms))
        .user_agent(std::env::var("IONE_HTTP_UA").unwrap_or_else(|_| "IONe/0.1".into()))
        .build()
        .expect("reqwest guarded client")
}

fn host_is_http_allowed(host: &Host<&str>) -> bool {
    match host {
        Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(ip) => ip.is_private() || ip.is_loopback(),
        Host::Ipv6(ip) => ip.is_loopback() || ip.is_unique_local(),
    }
}

fn is_ipv6_link_local(ip: &Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

#[cfg(test)]
mod tests {
    use super::parse_and_validate_url;

    #[test]
    fn blocks_link_local_hosts_for_all_schemes() {
        for raw in [
            "http://169.254.169.254/latest/meta-data",
            "https://169.254.169.254/",
            "http://169.254.10.10/",
            "http://[fe80::1]/",
            "https://[fe80::1]/",
        ] {
            assert!(
                parse_and_validate_url(raw, "feed_url").is_err(),
                "{raw} must be rejected"
            );
        }
    }

    #[test]
    fn allows_loopback_and_private_http() {
        for raw in ["http://127.0.0.1:8080/feed", "http://10.1.2.3/feed"] {
            assert!(
                parse_and_validate_url(raw, "feed_url").is_ok(),
                "{raw} should be allowed"
            );
        }
    }
}
