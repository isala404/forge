//! Registry for daemon handlers.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use forge_core::Result;
use forge_core::daemon::{DaemonContext, DaemonInfo, ForgeDaemon};

pub type BoxedDaemonHandler = Arc<
    dyn Fn(&DaemonContext) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> + Send + Sync,
>;

pub struct DaemonEntry {
    pub info: DaemonInfo,
    pub handler: BoxedDaemonHandler,
}

#[derive(Clone, Default)]
pub struct DaemonRegistry {
    daemons: HashMap<String, Arc<DaemonEntry>>,
}

impl DaemonRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<D: ForgeDaemon>(&mut self) {
        let info = D::info();
        let name = info.name.to_string();

        let handler: BoxedDaemonHandler = Arc::new(move |ctx| D::execute(ctx));

        self.daemons
            .insert(name, Arc::new(DaemonEntry { info, handler }));
    }

    pub fn get(&self, name: &str) -> Option<Arc<DaemonEntry>> {
        self.daemons.get(name).cloned()
    }

    pub fn info(&self, name: &str) -> Option<&DaemonInfo> {
        self.daemons.get(name).map(|e| &e.info)
    }

    pub fn exists(&self, name: &str) -> bool {
        self.daemons.contains_key(name)
    }

    pub fn daemon_names(&self) -> impl Iterator<Item = &str> {
        self.daemons.keys().map(|s| s.as_str())
    }

    pub fn daemons(&self) -> impl Iterator<Item = (&str, &Arc<DaemonEntry>)> {
        self.daemons.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn len(&self) -> usize {
        self.daemons.len()
    }

    pub fn is_empty(&self) -> bool {
        self.daemons.is_empty()
    }
}
