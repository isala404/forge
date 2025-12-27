use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use forge_core::workflow::{ForgeWorkflow, WorkflowContext, WorkflowInfo};

/// Type alias for boxed workflow handler function.
pub type BoxedWorkflowHandler = Arc<
    dyn Fn(
            &WorkflowContext,
            serde_json::Value,
        )
            -> Pin<Box<dyn Future<Output = forge_core::Result<serde_json::Value>> + Send + '_>>
        + Send
        + Sync,
>;

/// A registered workflow entry.
pub struct WorkflowEntry {
    /// Workflow metadata.
    pub info: WorkflowInfo,
    /// Execution handler (takes serialized input, returns serialized output).
    pub handler: BoxedWorkflowHandler,
}

impl WorkflowEntry {
    /// Create a new workflow entry from a ForgeWorkflow implementor.
    pub fn new<W: ForgeWorkflow>() -> Self
    where
        W::Input: serde::de::DeserializeOwned,
        W::Output: serde::Serialize,
    {
        Self {
            info: W::info(),
            handler: Arc::new(|ctx, input| {
                Box::pin(async move {
                    let typed_input: W::Input = serde_json::from_value(input)
                        .map_err(|e| forge_core::ForgeError::Validation(e.to_string()))?;
                    let result = W::execute(ctx, typed_input).await?;
                    serde_json::to_value(result).map_err(forge_core::ForgeError::from)
                })
            }),
        }
    }
}

/// Registry of all workflows.
#[derive(Default)]
pub struct WorkflowRegistry {
    workflows: HashMap<String, WorkflowEntry>,
}

impl WorkflowRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            workflows: HashMap::new(),
        }
    }

    /// Register a workflow handler.
    pub fn register<W: ForgeWorkflow>(&mut self)
    where
        W::Input: serde::de::DeserializeOwned,
        W::Output: serde::Serialize,
    {
        let entry = WorkflowEntry::new::<W>();
        self.workflows.insert(entry.info.name.to_string(), entry);
    }

    /// Get a workflow entry by name.
    pub fn get(&self, name: &str) -> Option<&WorkflowEntry> {
        self.workflows.get(name)
    }

    /// Get a workflow entry by name and version.
    pub fn get_version(&self, name: &str, version: u32) -> Option<&WorkflowEntry> {
        self.workflows
            .get(name)
            .filter(|e| e.info.version == version)
    }

    /// List all registered workflows.
    pub fn list(&self) -> Vec<&WorkflowEntry> {
        self.workflows.values().collect()
    }

    /// Get the number of registered workflows.
    pub fn len(&self) -> usize {
        self.workflows.len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.workflows.is_empty()
    }

    /// Get all workflow names.
    pub fn names(&self) -> Vec<&str> {
        self.workflows.keys().map(|s| s.as_str()).collect()
    }
}

impl Clone for WorkflowRegistry {
    fn clone(&self) -> Self {
        Self {
            workflows: self
                .workflows
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        WorkflowEntry {
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
        let registry = WorkflowRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }
}
