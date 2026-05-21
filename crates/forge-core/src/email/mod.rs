//! Email sending trait and types.
//!
//! Defines the `EmailSender` trait used by handler contexts via `ctx.email()`.
//! The runtime provides concrete implementations (SMTP, HTTP-based providers).

use std::future::Future;
use std::pin::Pin;

use crate::error::Result;

/// An email message.
#[derive(Debug, Clone)]
pub struct Email {
    /// Overrides the default `from` in config if set.
    pub from: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub text: Option<String>,
    pub html: Option<String>,
    pub reply_to: Option<String>,
}

impl Email {
    /// Create a new email to a single recipient.
    pub fn to(recipient: impl Into<String>) -> EmailBuilder {
        EmailBuilder {
            email: Self {
                from: None,
                to: vec![recipient.into()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: String::new(),
                text: None,
                html: None,
                reply_to: None,
            },
        }
    }
}

/// Builder for constructing email messages.
pub struct EmailBuilder {
    email: Email,
}

impl EmailBuilder {
    pub fn to(mut self, recipient: impl Into<String>) -> Self {
        self.email.to.push(recipient.into());
        self
    }

    pub fn from(mut self, sender: impl Into<String>) -> Self {
        self.email.from = Some(sender.into());
        self
    }

    pub fn cc(mut self, recipient: impl Into<String>) -> Self {
        self.email.cc.push(recipient.into());
        self
    }

    pub fn bcc(mut self, recipient: impl Into<String>) -> Self {
        self.email.bcc.push(recipient.into());
        self
    }

    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.email.subject = subject.into();
        self
    }

    pub fn text(mut self, body: impl Into<String>) -> Self {
        self.email.text = Some(body.into());
        self
    }

    pub fn html(mut self, body: impl Into<String>) -> Self {
        self.email.html = Some(body.into());
        self
    }

    pub fn reply_to(mut self, address: impl Into<String>) -> Self {
        self.email.reply_to = Some(address.into());
        self
    }

    pub fn build(self) -> Email {
        self.email
    }
}

/// Trait for sending emails from handler contexts.
///
/// Implemented by the runtime for SMTP and HTTP-based providers (Resend, SES).
/// Mocked in test contexts.
pub trait EmailSender: Send + Sync + 'static {
    /// Send an email. Returns the provider's message ID on success.
    fn send<'a>(
        &'a self,
        email: &'a Email,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>>;
}

/// Email configuration from forge.toml.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct EmailConfig {
    pub enabled: bool,
    /// Provider: "smtp", "resend", "ses", "log" (development).
    pub provider: String,
    /// Default sender address.
    pub from: String,
    pub smtp_host: Option<String>,
    /// Default 587.
    pub smtp_port: Option<u16>,
    /// Env var containing the API key or SMTP password.
    pub secret_env: Option<String>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn email_builder_creates_message() {
        let email = Email::to("user@example.com")
            .from("noreply@app.com")
            .subject("Hello")
            .text("Hi there")
            .html("<h1>Hi there</h1>")
            .cc("cc@example.com")
            .bcc("bcc@example.com")
            .reply_to("reply@app.com")
            .build();

        assert_eq!(email.to, vec!["user@example.com"]);
        assert_eq!(email.from.as_deref(), Some("noreply@app.com"));
        assert_eq!(email.subject, "Hello");
        assert_eq!(email.text.as_deref(), Some("Hi there"));
        assert_eq!(email.html.as_deref(), Some("<h1>Hi there</h1>"));
        assert_eq!(email.cc, vec!["cc@example.com"]);
        assert_eq!(email.bcc, vec!["bcc@example.com"]);
        assert_eq!(email.reply_to.as_deref(), Some("reply@app.com"));
    }

    #[test]
    fn email_builder_multiple_recipients() {
        let email = Email::to("a@example.com")
            .to("b@example.com")
            .subject("Test")
            .build();

        assert_eq!(email.to.len(), 2);
    }
}
