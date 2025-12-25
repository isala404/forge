use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use forge_core::cron::{CronContext, CronInfo, ForgeCron};

/// A registered cron entry.
pub struct CronEntry {
    /// Cron metadata.
    pub info: CronInfo,
    /// Execution handler.
    pub handler: Arc<
        dyn Fn(&CronContext) -> Pin<Box<dyn Future<Output = forge_core::Result<()>> + Send + '_>>
            + Send
            + Sync,
    >,
}

impl CronEntry {
    /// Create a new cron entry from a ForgeCron implementor.
    pub fn new<C: ForgeCron>() -> Self {
        Self {
            info: C::info(),
            handler: Arc::new(|ctx| C::execute(ctx)),
        }
    }
}

/// Registry of all cron jobs.
#[derive(Default)]
pub struct CronRegistry {
    crons: HashMap<String, CronEntry>,
}

impl CronRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            crons: HashMap::new(),
        }
    }

    /// Register a cron handler.
    pub fn register<C: ForgeCron>(&mut self) {
        let entry = CronEntry::new::<C>();
        self.crons.insert(entry.info.name.to_string(), entry);
    }

    /// Get a cron entry by name.
    pub fn get(&self, name: &str) -> Option<&CronEntry> {
        self.crons.get(name)
    }

    /// List all registered crons.
    pub fn list(&self) -> Vec<&CronEntry> {
        self.crons.values().collect()
    }

    /// Get the number of registered crons.
    pub fn len(&self) -> usize {
        self.crons.len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.crons.is_empty()
    }

    /// Get all cron names.
    pub fn names(&self) -> Vec<&str> {
        self.crons.keys().map(|s| s.as_str()).collect()
    }
}

impl Clone for CronRegistry {
    fn clone(&self) -> Self {
        Self {
            crons: self
                .crons
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        CronEntry {
                            info: v.info.clone(),
                            handler: v.handler.clone(),
                        },
                    )
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_registry() {
        let registry = CronRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }
}
