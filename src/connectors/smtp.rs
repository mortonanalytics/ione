use anyhow::Context;
use lettre::{
    message::header::ContentType, transport::smtp::AsyncSmtpTransport, AsyncTransport, Message,
    Tokio1Executor,
};

use crate::models::ConnectorKind;

use super::{ConnectorImpl, PollResult, StreamDescriptor};

// ── Test hook ────────────────────────────────────────────────────────────────

/// Captured email for the test hook.
#[derive(Debug, Clone)]
pub struct EmailCapture {
    pub to: String,
    pub subject: String,
    pub body: String,
}

#[cfg(test)]
use std::sync::Mutex;

#[cfg(test)]
static LAST_TEST_SENT: Mutex<Option<EmailCapture>> = Mutex::new(None);

/// Returns the last email captured in `IONE_SMTP_TEST_MODE=1` mode.
#[cfg(test)]
pub fn last_test_sent() -> Option<EmailCapture> {
    LAST_TEST_SENT.lock().ok()?.clone()
}

// ── Connector ────────────────────────────────────────────────────────────────

pub struct SmtpConnector {
    pub host: String,
    pub port: u16,
    pub from: String,
    pub starttls: bool,
}

impl SmtpConnector {
    pub fn from_config(config: &serde_json::Value) -> anyhow::Result<Self> {
        let host = config["host"]
            .as_str()
            .context("SMTP connector config missing 'host'")?
            .to_string();
        let port = config["port"]
            .as_u64()
            .context("SMTP connector config missing 'port'")? as u16;
        let from = config["from"]
            .as_str()
            .context("SMTP connector config missing 'from'")?
            .to_string();
        let starttls = config["starttls"].as_bool().unwrap_or(false);
        Ok(Self {
            host,
            port,
            from,
            starttls,
        })
    }
}

#[async_trait::async_trait]
impl ConnectorImpl for SmtpConnector {
    fn kind(&self) -> ConnectorKind {
        ConnectorKind::RustNative
    }

    async fn default_streams(&self) -> anyhow::Result<Vec<StreamDescriptor>> {
        Ok(vec![])
    }

    async fn poll(
        &self,
        _stream_name: &str,
        _cursor: Option<serde_json::Value>,
    ) -> anyhow::Result<PollResult> {
        Ok(PollResult {
            events: vec![],
            next_cursor: None,
        })
    }

    async fn invoke(&self, op: &str, args: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        if op != "send" {
            anyhow::bail!("SmtpConnector: unsupported op '{}'; expected 'send'", op);
        }

        let to = args["to"]
            .as_str()
            .context("SMTP invoke 'send' requires args.to")?
            .to_string();
        let subject = args["subject"].as_str().unwrap_or("IONe notification");
        let body = args["body"].as_str().unwrap_or("");

        // Test-mode short-circuit: store in thread-local instead of sending.
        if std::env::var("IONE_SMTP_TEST_MODE").as_deref() == Ok("1") {
            #[cfg(test)]
            {
                if let Ok(mut guard) = LAST_TEST_SENT.lock() {
                    *guard = Some(EmailCapture {
                        to: to.clone(),
                        subject: subject.to_string(),
                        body: body.to_string(),
                    });
                }
            }
            return Ok(serde_json::json!({ "ok": true, "test_mode": true }));
        }

        let email = Message::builder()
            .from(
                self.from
                    .parse()
                    .context("SMTP connector: invalid 'from' address")?,
            )
            .to(to.parse().context("SMTP connector: invalid 'to' address")?)
            .subject(subject)
            .header(ContentType::TEXT_PLAIN)
            .body(body.to_string())
            .context("failed to build SMTP message")?;

        let mailer: AsyncSmtpTransport<Tokio1Executor> =
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&self.host)
                .port(self.port)
                .build();

        mailer.send(email).await.context("SMTP send failed")?;

        Ok(serde_json::json!({ "ok": true }))
    }
}
