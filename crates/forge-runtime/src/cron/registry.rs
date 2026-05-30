use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use forge_core::cron::{CronContext, CronInfo, ForgeCron};

pub type BoxedCronHandler = Arc<
    dyn Fn(&CronContext) -> Pin<Box<dyn Future<Output = forge_core::Result<()>> + Send + '_>>
        + Send
        + Sync,
>;

pub struct CronEntry {
    pub info: CronInfo,
    pub handler: BoxedCronHandler,
}

impl CronEntry {
    pub fn new<C: ForgeCron>() -> Self {
        Self {
            info: C::info(),
            handler: Arc::new(|ctx| C::execute(ctx)),
        }
    }
}

#[derive(Default)]
pub struct CronRegistry {
    crons: HashMap<String, CronEntry>,
}

impl CronRegistry {
    pub fn new() -> Self {
        Self {
            crons: HashMap::new(),
        }
    }

    pub fn register<C: ForgeCron>(&mut self) {
        self.register_entry(CronEntry::new::<C>());
    }

    /// Insert a pre-built [`CronEntry`], keyed by its `info.name`.
    ///
    /// `register::<C>` is the public path; this primitive exists so in-crate
    /// tests can register a handler without a `ForgeCron` impl (the trait is
    /// sealed and cannot be implemented outside `forge-core`).
    pub(crate) fn register_entry(&mut self, entry: CronEntry) {
        self.crons.insert(entry.info.name.to_string(), entry);
    }

    pub fn get(&self, name: &str) -> Option<&CronEntry> {
        self.crons.get(name)
    }

    pub fn list(&self) -> Vec<&CronEntry> {
        self.crons.values().collect()
    }

    pub fn len(&self) -> usize {
        self.crons.len()
    }

    pub fn is_empty(&self) -> bool {
        self.crons.is_empty()
    }

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
