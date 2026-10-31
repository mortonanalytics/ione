//! Parity between production's contract rules and the conformance kit's.
//!
//! `src/bin/ione-conformance.rs` restates IONe's rules in its own code so an app
//! team can lift that one file into a standalone crate and validate against it
//! without building IONe. On PR #23 six production fixes each left the kit
//! asserting the opposite of what production did, and every one told a
//! *conforming* peer it was broken.
//!
//! Each test here runs one input vector through both implementations and
//! asserts they agree. Design: `md/design/contract-parity.md`.

use crate::conformance_kit as kit;
use serde_json::{json, Value};

/// §3.3. `approval_required` was made optional (defaults false) because the
/// ingress policy floor is escalate-only, so omitting it is exactly equivalent
/// to sending `false`. The kit went on failing peers that omitted it.
#[test]
fn approval_required_is_optional_in_both() {
    // The kit also checks `occurred_at` against the signature timestamp (±30s),
    // so both must come from the same clock or the vector fails for a reason
    // that has nothing to do with the rule under test.
    let timestamp = chrono::Utc::now().timestamp();
    let occurred_at = chrono::DateTime::from_timestamp(timestamp, 0)
        .expect("valid timestamp")
        .to_rfc3339();
    let without = json!({
        "id": "evt-parity",
        "type": "alert.created",
        "occurred_at": occurred_at,
        "peer_id": "00000000-0000-0000-0000-000000000000",
        "foreign_tenant_id": "tenant-abc",
        "severity": "routine",
        "data": { "message": "hello" }
    });
    let body = serde_json::to_vec(&without).expect("serialize");

    let production: Result<crate::routes::webhooks::WebhookEnvelope, _> =
        serde_json::from_slice(&body);
    let production = production.expect("production accepts an envelope without approval_required");
    assert!(
        !production.approval_required,
        "an absent approval_required must read as false"
    );

    let mut surface = kit::Surface::new(3, "Signed webhook sender", "§3");
    kit::verify_envelope(
        &mut surface,
        &body,
        "00000000-0000-0000-0000-000000000000",
        timestamp,
    );
    assert_eq!(
        surface.failures(),
        0,
        "the kit must accept what production accepts: {:?}",
        surface.lines()
    );
}

/// §8.1/§8.2. A `null` or empty-string cursor terminates pagination, and
/// `cursor` is accepted as the legacy spelling of `nextCursor`. The kit once
/// failed any peer that paginated at all.
#[test]
fn cursor_termination_agrees() {
    let vectors = [
        json!({ "tools": [], "nextCursor": "page-2" }),
        json!({ "tools": [], "nextCursor": null }),
        json!({ "tools": [], "nextCursor": "" }),
        json!({ "tools": [] }),
        json!({ "tools": [], "cursor": "legacy" }),
        json!({ "tools": [], "cursor": null }),
        json!({ "tools": [], "cursor": "" }),
    ];
    for vector in vectors {
        assert_eq!(
            crate::services::federation::next_cursor(&vector),
            kit::next_cursor(&vector),
            "cursor handling diverged on {vector}"
        );
    }
}

/// §5.4 transport. The SSE spec lets one event carry several `data:` lines,
/// joined with newlines. The kit could not parse an SSE-framed reply at all
/// after production learned streamable-HTTP, and failed four of six surfaces.
#[test]
fn sse_event_framing_agrees() {
    let vectors = [
        "data: {\"id\":1}\n\n",
        "event: message\ndata: {\ndata:   \"id\": 7\ndata: }\n\n",
        "data: {\"method\":\"notifications/message\"}\n\ndata: {\"id\":2}\n\n",
        "data: {\"id\":3}",
        ": a comment line\ndata: {\"id\":4}\n\n",
        "",
    ];
    for body in vectors {
        assert_eq!(
            crate::services::peer_tokens::sse_event_payloads(body),
            kit::sse_event_payloads(body),
            "SSE framing diverged on {body:?}"
        );
    }
}

/// §4.2/§4.4. Panel URLs are https-only and pass the SSRF guard, which allows
/// loopback and private addresses so an on-prem peer works, and blocks
/// link-local.
#[test]
fn panel_url_validation_agrees() {
    let vectors = [
        "https://tiles.example.com/{z}/{x}/{y}.png",
        "https://10.0.0.5/tile.png",
        "https://127.0.0.1/tile.png",
        "http://tiles.example.com/tile.png",
        "file:///tmp/tile.png",
        "javascript:alert(1)",
        "data:text/html,<script>alert(1)</script>",
        "https://169.254.169.254/tile.png",
        "not-a-url",
        "",
    ];
    for raw in vectors {
        let production = production_panel_url_ok(raw);
        let kit_ok = kit::validate_panel_url(raw, "parity").is_ok();
        assert_eq!(
            production, kit_ok,
            "URL validation diverged on {raw:?}: production={production}, kit={kit_ok}"
        );
    }
}

/// Production's panel-URL bar, as `services::map_layers` and
/// `services::document_panels` apply it: parse, require https, then the shared
/// SSRF guard.
fn production_panel_url_ok(raw: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(raw) else {
        return false;
    };
    if url.scheme() != "https" {
        return false;
    }
    crate::util::url_guard::ensure_safe_url(&url, "parity").is_ok()
}

/// §3.1. `X-IONe-Signature` is `t=<unix seconds>,v1=<64 hex>`, each key once
/// and nothing else, with the HMAC over the raw `t` text. Production decodes the
/// digest with `hex::decode`, which accepts `A-F`; the kit once failed every peer
/// that signed with an uppercase digest.
#[test]
fn signature_grammar_agrees() {
    const SECRET: &str = "parity-secret";
    let body: &[u8] = br#"{"id":"evt-parity"}"#;
    let t = "1753440000";
    let digest = hmac_hex(SECRET, t, body);
    let vectors = [
        format!("t={t},v1={digest}"),
        format!("t={t},v1={}", digest.to_ascii_uppercase()),
        format!("v1={digest},t={t}"),
        format!(" t = {t} , v1 = {digest} "),
        format!("t=+{t},v1={}", hmac_hex(SECRET, &format!("+{t}"), body)),
        format!("t={t}"),
        format!("v1={digest}"),
        format!("t={t},v1={digest},v2={digest}"),
        format!("t={t},t={t},v1={digest}"),
        format!("t={t},v1={}", &digest[..63]),
        format!("t={t},v1={}g", &digest[..63]),
        format!("t=soon,v1={digest}"),
        format!("t={t},v1={}", hmac_hex("wrong-secret", t, body)),
        format!("t={t};v1={digest}"),
        String::new(),
    ];
    let bytes = axum::body::Bytes::copy_from_slice(body);
    for header in vectors {
        let production = crate::routes::webhooks::parse_signature_header(&header)
            .and_then(|sig| {
                crate::routes::webhooks::verify_signature(SECRET.as_bytes(), &sig, &bytes)
            })
            .is_ok();
        let kit_ok = kit::verify_signature_header(&header, SECRET, body).is_ok();
        assert_eq!(
            production, kit_ok,
            "signature grammar diverged on {header:?}: production={production}, kit={kit_ok}"
        );
    }
}

fn hmac_hex(secret: &str, timestamp_raw: &str, body: &[u8]) -> String {
    use hmac::{KeyInit, Mac};
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(timestamp_raw.as_bytes());
    mac.update(b".");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// §2. `registration_endpoint` is optional: a peer advertising
/// `client_id_metadata_document_supported` needs no registration step. The kit
/// once failed every peer that omitted it. A document with neither is rejected
/// by both, but production rejects it inside `begin_federation`, not on
/// deserialization, so that case is covered by the kit's own tests.
#[test]
fn registration_endpoint_is_optional_in_both() {
    let base = json!({
        "issuer": "https://peer.example.com",
        "authorization_endpoint": "https://peer.example.com/oauth/authorize",
        "token_endpoint": "https://peer.example.com/oauth/token",
        "revocation_endpoint": "https://peer.example.com/oauth/revoke",
        "code_challenge_methods_supported": ["S256"],
        "grant_types_supported": ["authorization_code", "refresh_token"]
    });
    let with = |extra: Value| {
        let mut doc = base.clone();
        for (key, value) in extra.as_object().expect("object") {
            doc[key] = value.clone();
        }
        doc
    };
    let vectors = [
        with(json!({ "registration_endpoint": "https://peer.example.com/oauth/register" })),
        with(json!({ "client_id_metadata_document_supported": true })),
        with(json!({
            "registration_endpoint": "https://peer.example.com/oauth/register",
            "client_id_metadata_document_supported": true
        })),
    ];
    for doc in vectors {
        let production: Result<crate::services::peer_oauth::PeerDiscovery, _> =
            serde_json::from_value(doc.clone());
        assert!(
            production.is_ok(),
            "production rejected discovery document {doc}: {:?}",
            production.err()
        );
        let mut surface = kit::Surface::new(2, "OAuth discovery", "§2");
        kit::verify_oauth_metadata(&mut surface, &doc);
        assert_eq!(
            surface.failures(),
            0,
            "the kit must accept what production accepts on {doc}: {:?}",
            surface.lines()
        );
    }
}

/// §6. `foreign_roles` may be an explicit `null`, which production reads as no
/// roles. The kit once failed every peer that sent `null`. Wrongly-typed values
/// fail production's typed deserializer, so the kit must fail them too.
#[test]
fn foreign_roles_shapes_agree() {
    let roles = [
        Value::Null,
        json!([]),
        json!(["operator", "viewer"]),
        json!([1]),
        json!("operator"),
        json!({}),
    ];
    for value in roles {
        let whoami = json!({
            "peer_id": "peer-1",
            "foreign_tenant_id": "tenant-abc",
            "foreign_tenant_name": null,
            "foreign_workspace_id": null,
            "foreign_user_id": null,
            "foreign_user_email": null,
            "foreign_roles": value
        });
        let production = serde_json::from_value::<
            crate::services::workspace_peer_binding::WhoamiResponse,
        >(whoami.clone())
        .is_ok();
        let mut surface = kit::Surface::new(6, "whoami:// resource", "§6");
        kit::verify_whoami_document(&mut surface, &whoami);
        let kit_ok = surface.failures() == 0;
        assert_eq!(
            production,
            kit_ok,
            "foreign_roles handling diverged on {value}: production={production}, kit={kit_ok} {:?}",
            surface.lines()
        );
    }
}

/// A guard on the harness itself: if the kit stops exposing one of the
/// predicates paired above, that is a restructuring the parity tests must be
/// revisited for, not something to discover as a silent gap.
#[test]
fn every_paired_kit_predicate_is_still_reachable() {
    let _: fn(&Value) -> Option<Value> = kit::next_cursor;
    let _: fn(&str) -> Vec<String> = kit::sse_event_payloads;
    let _: fn(&str, &str) -> Result<(), String> = kit::validate_panel_url;
    let _: fn(&str, &str, &[u8]) -> Result<i64, String> = kit::verify_signature_header;
    let _: fn(&mut kit::Surface, &Value) = kit::verify_oauth_metadata;
    let _: fn(&mut kit::Surface, &Value) = kit::verify_whoami_document;
}
