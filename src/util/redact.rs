/// Error-string scrubbing for persisted audit/pipeline payloads.
///
/// Applied at the repo write layer (every `audit_events.payload` /
/// `pipeline_events.detail` INSERT) and re-applied at read time on the
/// list/export surfaces so rows written before this module existed are
/// also clean when bulk-read.
const SECRET_KEYWORDS: [&str; 5] = ["authorization", "password", "secret", "token", "key"];

const MAX_ERROR_CHARS: usize = 256;

/// Strip credential-bearing substrings and truncate to 256 chars.
pub fn scrub_error_text(input: &str) -> String {
    let scrubbed = redact_kv_secrets(&redact_url_userinfo(input));
    truncate_chars(&scrubbed, MAX_ERROR_CHARS)
}

/// Recursively walk a JSON value; every object entry whose key is "error"
/// and whose value is a string is replaced with `scrub_error_text(value)`.
pub fn scrub_error_fields(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if key == "error" {
                    if let serde_json::Value::String(s) = v {
                        *s = scrub_error_text(s);
                        continue;
                    }
                }
                scrub_error_fields(v);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                scrub_error_fields(item);
            }
        }
        _ => {}
    }
}

/// `scheme://user:pass@host` -> `scheme://[redacted]@host`
fn redact_url_userinfo(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(idx) = rest.find("://") {
        let after = idx + 3;
        out.push_str(&rest[..after]);
        let tail = &rest[after..];
        let authority_end = tail
            .find(|c: char| c == '/' || c.is_whitespace())
            .unwrap_or(tail.len());
        if let Some(at) = tail[..authority_end].rfind('@') {
            out.push_str("[redacted]");
            rest = &tail[at..];
        } else {
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}

/// `(authorization|token|key|secret|password)[=:\s]<value>` -> keep keyword +
/// separator, replace the value token with `[redacted]`. For auth-scheme
/// values (`Bearer x`, `Basic x`) the scheme's credential token is consumed too.
fn redact_kv_secrets(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let mut matched = false;
        for kw in SECRET_KEYWORDS {
            if !lower[i..].starts_with(kw) {
                continue;
            }
            let after_kw = i + kw.len();
            let Some(sep) = s[after_kw..].chars().next() else {
                continue;
            };
            if sep != '=' && sep != ':' && !sep.is_whitespace() {
                continue;
            }
            let mut j = after_kw + sep.len_utf8();
            j += leading_whitespace_len(&s[j..]);
            let value_len = non_whitespace_len(&s[j..]);
            if value_len == 0 {
                continue;
            }
            let value = &s[j..j + value_len];
            j += value_len;
            if value.eq_ignore_ascii_case("bearer") || value.eq_ignore_ascii_case("basic") {
                let mut k = j;
                k += leading_whitespace_len(&s[k..]);
                let cred_len = non_whitespace_len(&s[k..]);
                if cred_len > 0 {
                    j = k + cred_len;
                }
            }
            out.push_str(&s[i..after_kw]);
            out.push(sep);
            out.push_str("[redacted]");
            i = j;
            matched = true;
            break;
        }
        if !matched {
            let ch = s[i..].chars().next().expect("i < s.len()");
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

fn leading_whitespace_len(s: &str) -> usize {
    s.len() - s.trim_start().len()
}

fn non_whitespace_len(s: &str) -> usize {
    s.find(char::is_whitespace).unwrap_or(s.len())
}

fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((byte_idx, _)) => {
            let mut out = s[..byte_idx].to_string();
            out.push('…');
            out
        }
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redacts_url_userinfo() {
        let out = scrub_error_text("fetch failed: https://user:secret@host/path timed out");
        assert!(!out.contains("secret"), "{out}");
        assert!(!out.contains("user:"), "{out}");
        assert!(out.contains("https://[redacted]@host/path"), "{out}");
    }

    #[test]
    fn redacts_authorization_bearer() {
        let out = scrub_error_text("upstream said: Authorization: Bearer abc123 rejected");
        assert!(!out.contains("abc123"), "{out}");
        assert!(out.contains("Authorization:[redacted]"), "{out}");
        assert!(out.contains("rejected"), "{out}");
    }

    #[test]
    fn redacts_token_kv() {
        let out = scrub_error_text("request to ?token=tok_live_999 failed");
        assert!(!out.contains("tok_live_999"), "{out}");
        assert!(out.contains("token=[redacted]"), "{out}");
    }

    #[test]
    fn truncates_4kb_body_to_256_chars() {
        let body = "x".repeat(4096);
        let out = scrub_error_text(&body);
        assert_eq!(out.chars().count(), MAX_ERROR_CHARS + 1);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn short_clean_text_unchanged() {
        assert_eq!(scrub_error_text("connection refused"), "connection refused");
    }

    #[test]
    fn scrubs_nested_error_fields() {
        let mut v = json!({
            "a": { "error": "password=hunter2 oops" },
            "items": [ { "error": "token=t1" }, { "ok": true } ],
            "error": 42,
            "not_error": "token=keepme-is-still-scrubbed-no"
        });
        scrub_error_fields(&mut v);
        let a = v["a"]["error"].as_str().unwrap();
        assert!(!a.contains("hunter2"), "{a}");
        let i0 = v["items"][0]["error"].as_str().unwrap();
        assert!(!i0.contains("t1"), "{i0}");
        assert_eq!(v["error"], 42);
        // non-"error" keys are left alone
        assert!(v["not_error"].as_str().unwrap().contains("keepme"));
    }
}
