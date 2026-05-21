use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use forge_core::Result;
use forge_core::webhook::{ForgeWebhook, WebhookContext, WebhookInfo, WebhookResult};
use serde_json::Value;

/// Boxed webhook handler function.
pub type BoxedWebhookHandler = Arc<
    dyn Fn(
            &WebhookContext,
            Value,
        ) -> Pin<Box<dyn Future<Output = Result<WebhookResult>> + Send + '_>>
        + Send
        + Sync,
>;

/// Entry in the webhook registry.
pub struct WebhookEntry {
    pub info: WebhookInfo,
    pub handler: BoxedWebhookHandler,
}

/// Registry for webhook handlers.
#[derive(Clone, Default)]
pub struct WebhookRegistry {
    webhooks: HashMap<String, Arc<WebhookEntry>>,
    by_path: HashMap<String, String>,
}

impl WebhookRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<W: ForgeWebhook>(&mut self) {
        let info = W::info();
        let name = info.name.to_string();
        let path = info.path.to_string();

        let handler: BoxedWebhookHandler = Arc::new(move |ctx, payload| {
            Box::pin(async move {
                let typed: W::Payload = serde_json::from_value(payload)
                    .map_err(|e| forge_core::ForgeError::Deserialization(e.to_string()))?;
                W::execute(ctx, typed).await
            })
        });

        self.by_path.insert(path, name.clone());
        self.webhooks
            .insert(name, Arc::new(WebhookEntry { info, handler }));
    }

    pub fn get(&self, name: &str) -> Option<Arc<WebhookEntry>> {
        self.webhooks.get(name).cloned()
    }

    pub fn get_by_path(&self, path: &str) -> Option<Arc<WebhookEntry>> {
        self.by_path.get(path).and_then(|name| self.get(name))
    }

    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.by_path.keys().map(|s| s.as_str())
    }

    pub fn webhooks(&self) -> impl Iterator<Item = (&str, &Arc<WebhookEntry>)> {
        self.webhooks.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn len(&self) -> usize {
        self.webhooks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.webhooks.is_empty()
    }
}
