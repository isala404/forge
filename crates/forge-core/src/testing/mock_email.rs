//! Mock email sender for testing.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::email::{Email, EmailSender};
use crate::error::Result;

/// Records sent emails for assertion in tests.
#[derive(Debug, Clone, Default)]
pub struct MockEmailSender {
    sent: Arc<Mutex<Vec<SentEmail>>>,
}

/// A recorded email send.
#[derive(Debug, Clone)]
pub struct SentEmail {
    /// Recipients.
    pub to: Vec<String>,
    /// Subject line.
    pub subject: String,
    /// Text body.
    pub text: Option<String>,
    /// HTML body.
    pub html: Option<String>,
}

impl MockEmailSender {
    /// Create a new mock email sender.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get all sent emails.
    pub async fn sent(&self) -> Vec<SentEmail> {
        self.sent.lock().await.clone()
    }

    /// Assert that exactly one email was sent to the given address.
    pub async fn assert_sent_to(&self, address: &str) {
        let sent = self.sent.lock().await;
        let matching: Vec<_> = sent.iter().filter(|e| e.to.contains(&address.to_string())).collect();
        assert!(
            matching.len() == 1,
            "Expected 1 email to {address}, found {}",
            matching.len()
        );
    }

    /// Assert that no emails were sent.
    pub async fn assert_none_sent(&self) {
        let sent = self.sent.lock().await;
        assert!(sent.is_empty(), "Expected no emails, found {}", sent.len());
    }
}

impl EmailSender for MockEmailSender {
    fn send<'a>(
        &'a self,
        email: &'a Email,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(async move {
            self.sent.lock().await.push(SentEmail {
                to: email.to.clone(),
                subject: email.subject.clone(),
                text: email.text.clone(),
                html: email.html.clone(),
            });
            Ok(format!("mock-{}", uuid::Uuid::new_v4()))
        })
    }
}
