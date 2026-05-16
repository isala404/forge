use std::collections::HashMap;
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use forge_core::ForgeError;
use forge_core::workflow::{ForgeWorkflow, WorkflowContext, WorkflowInfo};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// Normalize args for deserialization.
/// - Converts `null` to `{}` so both unit `()` and empty structs deserialize correctly.
/// - Unwraps `{"args": ...}` or `{"input": ...}` wrapper if present (callers may use either format).
fn normalize_args(args: Value) -> Value {
    let unwrapped = match &args {
        Value::Object(map) if map.len() == 1 => {
            if map.contains_key("args") {
                map.get("args").cloned().unwrap_or(Value::Null)
            } else if map.contains_key("input") {
                map.get("input").cloned().unwrap_or(Value::Null)
            } else {
                args
            }
        }
        _ => args,
    };

    match &unwrapped {
        Value::Null => Value::Object(serde_json::Map::new()),
        _ => unwrapped,
    }
}

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
                    let typed_input: W::Input = serde_json::from_value(normalize_args(input))
                        .map_err(|e| forge_core::ForgeError::Validation(e.to_string()))?;
                    let result = W::execute(ctx, typed_input).await?;
                    serde_json::to_value(result).map_err(forge_core::ForgeError::from)
                })
            }),
        }
    }
}

/// Composite key for versioned workflow lookup.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WorkflowVersionKey {
    pub name: String,
    pub version: String,
}

/// Registry of all workflows, supporting multiple versions per workflow name.
#[derive(Default)]
pub struct WorkflowRegistry {
    /// All entries keyed by (name, version).
    entries: HashMap<WorkflowVersionKey, WorkflowEntry>,
    /// Maps workflow name to its active version string.
    active_versions: HashMap<String, String>,
}

impl WorkflowRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            active_versions: HashMap::new(),
        }
    }

    /// Register a workflow handler.
    pub fn register<W: ForgeWorkflow>(&mut self)
    where
        W::Input: serde::de::DeserializeOwned,
        W::Output: serde::Serialize,
    {
        let entry = WorkflowEntry::new::<W>();
        let info = &entry.info;

        if info.is_active() {
            self.active_versions
                .insert(info.name.to_string(), info.version.to_string());
        }

        let key = WorkflowVersionKey {
            name: info.name.to_string(),
            version: info.version.to_string(),
        };
        self.entries.insert(key, entry);
    }

    /// Get the active version entry for a workflow by name.
    /// Used when starting new runs.
    pub fn get_active(&self, name: &str) -> Option<&WorkflowEntry> {
        let version = self.active_versions.get(name)?;
        let key = WorkflowVersionKey {
            name: name.to_string(),
            version: version.clone(),
        };
        self.entries.get(&key)
    }

    /// Get a specific workflow version.
    /// Used when resuming runs pinned to a specific version.
    pub fn get_version(&self, name: &str, version: &str) -> Option<&WorkflowEntry> {
        let key = WorkflowVersionKey {
            name: name.to_string(),
            version: version.to_string(),
        };
        self.entries.get(&key)
    }

    /// Check if a specific version+signature combination is available.
    pub fn has_version_with_signature(&self, name: &str, version: &str, signature: &str) -> bool {
        self.get_version(name, version)
            .is_some_and(|entry| entry.info.signature == signature)
    }

    /// Validate that a run can be safely resumed.
    /// Returns the matching entry, or a blocking reason.
    pub fn validate_resume(
        &self,
        name: &str,
        version: &str,
        signature: &str,
    ) -> Result<&WorkflowEntry, ResumeBlockReason> {
        // Check if any version of this workflow is registered
        let has_any = self.entries.keys().any(|k| k.name == name);
        if !has_any {
            return Err(ResumeBlockReason::MissingHandler);
        }

        let entry = self
            .get_version(name, version)
            .ok_or(ResumeBlockReason::MissingVersion)?;

        if entry.info.signature != signature {
            return Err(ResumeBlockReason::SignatureMismatch {
                expected: signature.to_string(),
                actual: entry.info.signature.to_string(),
            });
        }

        Ok(entry)
    }

    /// List all registered workflow entries.
    pub fn list(&self) -> impl Iterator<Item = &WorkflowEntry> {
        self.entries.values()
    }

    /// Get the number of registered workflow entries (all versions).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get all workflow names (deduplicated).
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.active_versions.keys().map(|s| s.as_str())
    }

    /// Get all registered definitions for startup persistence.
    pub fn definitions(&self) -> Vec<&WorkflowInfo> {
        self.entries.values().map(|e| &e.info).collect()
    }

    /// Find non-terminal workflow runs whose `(name, version)` is no longer
    /// in this binary's registry. These are stranded — the operator must
    /// either redeploy with the missing handler or terminate the runs in
    /// PG with `UPDATE forge_workflow_runs SET status = 'cancelled_by_operator'`
    /// (or `'retired_unresumable'`) before the runtime can become ready again.
    pub async fn drain_check(&self, pool: &PgPool) -> forge_core::Result<Vec<DrainEntry>> {
        let registered: HashSet<(String, String)> = self
            .entries
            .keys()
            .map(|k| (k.name.clone(), k.version.clone()))
            .collect();

        // Pull aggregate stats for every non-terminal (name, version) tuple.
        let rows = sqlx::query!(
            r#"
            SELECT
                workflow_name AS "workflow_name!",
                workflow_version AS "workflow_version!",
                COUNT(*) AS "in_flight_count!",
                MIN(started_at) AS "oldest_started_at!",
                (ARRAY_AGG(id ORDER BY started_at ASC))[:10] AS "sample_run_ids!: Vec<Uuid>"
            FROM forge_workflow_runs
            WHERE status NOT IN ('completed', 'failed')
            GROUP BY workflow_name, workflow_version
            "#
        )
        .fetch_all(pool)
        .await
        .map_err(ForgeError::Database)?;

        let mut stranded = Vec::new();
        for row in rows {
            let key = (row.workflow_name.clone(), row.workflow_version.clone());
            if registered.contains(&key) {
                continue;
            }
            stranded.push(DrainEntry {
                workflow_name: row.workflow_name,
                workflow_version: row.workflow_version,
                in_flight_count: row.in_flight_count as u64,
                oldest_started_at: row.oldest_started_at,
                sample_run_ids: row.sample_run_ids,
            });
        }

        Ok(stranded)
    }
}

/// One stranded `(workflow_name, workflow_version)` group surfaced by
/// [`WorkflowRegistry::drain_check`].
#[derive(Debug, Clone)]
pub struct DrainEntry {
    /// Workflow name as persisted on the run.
    pub workflow_name: String,
    /// Version as persisted on the run.
    pub workflow_version: String,
    /// How many non-terminal runs reference this `(name, version)`.
    pub in_flight_count: u64,
    /// Start time of the oldest non-terminal run in this group.
    pub oldest_started_at: DateTime<Utc>,
    /// Up to 10 representative run IDs for operators to inspect.
    pub sample_run_ids: Vec<Uuid>,
}

/// Reason a workflow run cannot be resumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeBlockReason {
    /// No handler registered for this workflow name at all.
    MissingHandler,
    /// The specific version is not present in the current binary.
    MissingVersion,
    /// The version exists but its signature does not match.
    SignatureMismatch { expected: String, actual: String },
}

impl ResumeBlockReason {
    /// Human-readable description for the error column.
    pub fn description(&self) -> String {
        match self {
            Self::MissingHandler => "No handler registered for this workflow".to_string(),
            Self::MissingVersion => "Workflow version not present in current binary".to_string(),
            Self::SignatureMismatch { expected, actual } => {
                format!("Signature mismatch: run expects {expected}, binary has {actual}")
            }
        }
    }
}

impl Clone for WorkflowRegistry {
    fn clone(&self) -> Self {
        Self {
            entries: self
                .entries
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
            active_versions: self.active_versions.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resume_block_reasons() {
        let reason = ResumeBlockReason::MissingHandler;
        assert!(reason.description().contains("No handler"));

        let reason = ResumeBlockReason::SignatureMismatch {
            expected: "abc".to_string(),
            actual: "def".to_string(),
        };
        assert!(reason.description().contains("abc"));
        assert!(reason.description().contains("def"));
    }
}
