use std::collections::HashMap;
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use forge_core::ForgeError;
use forge_core::config::SignatureCheckMode;
use forge_core::util::normalize_handler_args as normalize_args;
use forge_core::workflow::{ForgeWorkflow, WorkflowContext, WorkflowInfo};
use sqlx::PgPool;
use uuid::Uuid;

pub type BoxedWorkflowHandler = Arc<
    dyn Fn(
            &WorkflowContext,
            serde_json::Value,
        )
            -> Pin<Box<dyn Future<Output = forge_core::Result<serde_json::Value>> + Send + '_>>
        + Send
        + Sync,
>;

pub struct WorkflowEntry {
    pub info: WorkflowInfo,
    pub handler: BoxedWorkflowHandler,
}

impl WorkflowEntry {
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
pub struct WorkflowRegistry {
    entries: HashMap<WorkflowVersionKey, WorkflowEntry>,
    active_versions: HashMap<String, String>,
    pub signature_check: SignatureCheckMode,
}

impl Default for WorkflowRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkflowRegistry {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            active_versions: HashMap::new(),
            signature_check: SignatureCheckMode::Strict,
        }
    }

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

    /// Returns the active version entry for a workflow; used when starting new runs.
    pub fn get_active(&self, name: &str) -> Option<&WorkflowEntry> {
        let version = self.active_versions.get(name)?;
        let key = WorkflowVersionKey {
            name: name.to_string(),
            version: version.clone(),
        };
        self.entries.get(&key)
    }

    /// Returns a specific workflow version; used when resuming runs pinned to a version.
    pub fn get_version(&self, name: &str, version: &str) -> Option<&WorkflowEntry> {
        let key = WorkflowVersionKey {
            name: name.to_string(),
            version: version.to_string(),
        };
        self.entries.get(&key)
    }

    pub fn has_version_with_signature(&self, name: &str, version: &str, signature: &str) -> bool {
        self.get_version(name, version)
            .is_some_and(|entry| entry.info.signature == signature)
    }

    /// Validate that a run can be safely resumed.
    /// Returns the matching entry, or a blocking reason.
    ///
    /// When [`SignatureCheckMode::Relaxed`] is configured, only the
    /// `(name, version)` pair is checked and the signature hash is ignored.
    /// Use relaxed mode during rolling deploys to prevent in-flight runs from
    /// being blocked while nodes are transitioning between binary versions.
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

        if self.signature_check == SignatureCheckMode::Strict && entry.info.signature != signature {
            return Err(ResumeBlockReason::SignatureMismatch {
                expected: signature.to_string(),
                actual: entry.info.signature.to_string(),
            });
        }

        Ok(entry)
    }

    pub fn list(&self) -> impl Iterator<Item = &WorkflowEntry> {
        self.entries.values()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.active_versions.keys().map(|s| s.as_str())
    }

    pub fn definitions(&self) -> Vec<&WorkflowInfo> {
        self.entries.values().map(|e| &e.info).collect()
    }

    /// Persist every workflow definition into `forge_workflow_definitions`,
    /// failing if a previously-registered name+version row has a different
    /// signature (the contract changed without a version bump). New rows are
    /// inserted, existing matching rows get their `status` refreshed.
    ///
    /// Each definition is upserted in a single transaction with
    /// `INSERT ... ON CONFLICT DO UPDATE ... RETURNING workflow_signature`
    /// so two nodes booting concurrently can't produce a generic
    /// unique-violation in place of the helpful signature-mismatch message
    /// (#16 in issues doc).
    pub async fn persist_definitions(&self, pool: &PgPool) -> forge_core::Result<()> {
        for info in self.definitions() {
            let status = info.status.as_str();

            let mut tx = pool.begin().await.map_err(ForgeError::Database)?;

            // Atomic upsert: only the status is updated on conflict; signature
            // is preserved so we can compare it to the incoming one without
            // a separate SELECT round-trip.
            #[allow(clippy::disallowed_methods)]
            let returned: (String,) = sqlx::query_as(
                r#"
                INSERT INTO forge_workflow_definitions (workflow_name, workflow_version, workflow_signature, status)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (workflow_name, workflow_version) DO UPDATE SET status = EXCLUDED.status
                RETURNING workflow_signature
                "#,
            )
            .bind(info.name)
            .bind(info.version)
            .bind(info.signature)
            .bind(status)
            .fetch_one(&mut *tx)
            .await
            .map_err(ForgeError::Database)?;

            if returned.0 != info.signature {
                tx.rollback().await.map_err(ForgeError::Database)?;
                return Err(ForgeError::config(format!(
                    "Workflow '{}' version '{}' has a different signature than previously registered. \
                     Persisted contract changed under the same version. \
                     Expected signature: {}, got: {}. \
                     Create a new version instead of modifying the existing one.",
                    info.name, info.version, returned.0, info.signature
                )));
            }

            tx.commit().await.map_err(ForgeError::Database)?;

            tracing::debug!(
                workflow = info.name,
                version = info.version,
                signature = info.signature,
                status = status,
                "Workflow definition registered"
            );
        }
        Ok(())
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
    pub workflow_name: String,
    pub workflow_version: String,
    pub in_flight_count: u64,
    pub oldest_started_at: DateTime<Utc>,
    pub sample_run_ids: Vec<Uuid>,
}

/// Reason a workflow run cannot be resumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeBlockReason {
    MissingHandler,
    MissingVersion,
    SignatureMismatch { expected: String, actual: String },
}

impl ResumeBlockReason {
    pub fn description(&self) -> String {
        match self {
            Self::MissingHandler => "No handler registered for this workflow".to_string(),
            Self::MissingVersion => "Workflow version not present in current binary".to_string(),
            Self::SignatureMismatch { expected, actual } => {
                format!("Signature mismatch: run expects {expected}, binary has {actual}")
            }
        }
    }

    pub fn to_blocked_status(&self) -> forge_core::workflow::WorkflowStatus {
        match self {
            Self::MissingHandler => forge_core::workflow::WorkflowStatus::BlockedMissingHandler,
            Self::MissingVersion => forge_core::workflow::WorkflowStatus::BlockedMissingVersion,
            Self::SignatureMismatch { .. } => {
                forge_core::workflow::WorkflowStatus::BlockedSignatureMismatch
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
            signature_check: self.signature_check,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use forge_core::workflow::WorkflowDefStatus;

    use serde_json::Value;

    // normalize_args contract is exercised via `forge_core::util` tests; this
    // file now delegates to the shared helper to keep the two registries in sync.

    // ForgeWorkflow is sealed, so tests build entries directly through pub fields
    // with noop handlers — same insertion shape as register::<W>.

    fn noop_handler() -> BoxedWorkflowHandler {
        Arc::new(|_ctx, _input| Box::pin(async { Ok(Value::Null) }))
    }

    fn info(name: &'static str, version: &'static str, signature: &'static str) -> WorkflowInfo {
        WorkflowInfo {
            name,
            version,
            signature,
            ..Default::default()
        }
    }

    fn insert(
        reg: &mut WorkflowRegistry,
        name: &'static str,
        version: &'static str,
        signature: &'static str,
        status: WorkflowDefStatus,
    ) {
        let mut i = info(name, version, signature);
        i.status = status;
        if i.is_active() {
            reg.active_versions
                .insert(name.to_string(), version.to_string());
        }
        let key = WorkflowVersionKey {
            name: name.to_string(),
            version: version.to_string(),
        };
        reg.entries.insert(
            key,
            WorkflowEntry {
                info: i,
                handler: noop_handler(),
            },
        );
    }

    #[test]
    fn new_registry_is_empty() {
        let reg = WorkflowRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert!(reg.get_active("anything").is_none());
        assert!(reg.get_version("x", "v1").is_none());
        assert_eq!(reg.names().count(), 0);
        assert!(reg.list().next().is_none());
        assert!(reg.definitions().is_empty());
    }

    #[test]
    fn get_active_returns_only_active_version() {
        // Two versions registered for "wf": v1 deprecated, v2 active.
        // get_active must resolve to v2 (the one in active_versions), and
        // get_version must reach either.
        let mut reg = WorkflowRegistry::new();
        insert(&mut reg, "wf", "v1", "sig1", WorkflowDefStatus::Deprecated);
        insert(&mut reg, "wf", "v2", "sig2", WorkflowDefStatus::Active);

        let active = reg.get_active("wf").expect("active entry");
        assert_eq!(active.info.version, "v2");
        assert_eq!(active.info.signature, "sig2");

        assert_eq!(reg.get_version("wf", "v1").expect("v1").info.version, "v1");
        assert_eq!(reg.get_version("wf", "v2").expect("v2").info.version, "v2");
        assert!(reg.get_version("wf", "v3").is_none());
    }

    #[test]
    fn get_active_returns_none_when_only_staging_or_deprecated_registered() {
        // Without an Active version, get_active must return None — even
        // though entries exist. This protects new runs from latching onto
        // a draining or pre-release version.
        let mut reg = WorkflowRegistry::new();
        insert(&mut reg, "wf", "v1", "sig1", WorkflowDefStatus::Deprecated);
        insert(&mut reg, "wf", "v2", "sig2", WorkflowDefStatus::Staging);

        assert!(reg.get_active("wf").is_none());
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn has_version_with_signature_checks_both_axes() {
        let mut reg = WorkflowRegistry::new();
        insert(&mut reg, "wf", "v1", "sig1", WorkflowDefStatus::Active);

        assert!(reg.has_version_with_signature("wf", "v1", "sig1"));
        // signature mismatch
        assert!(!reg.has_version_with_signature("wf", "v1", "sig2"));
        // version mismatch
        assert!(!reg.has_version_with_signature("wf", "v2", "sig1"));
        // name mismatch
        assert!(!reg.has_version_with_signature("other", "v1", "sig1"));
    }

    #[test]
    fn validate_resume_returns_missing_handler_when_name_unknown() {
        let mut reg = WorkflowRegistry::new();
        insert(&mut reg, "known", "v1", "sig1", WorkflowDefStatus::Active);

        match reg.validate_resume("unknown", "v1", "sig1") {
            Err(ResumeBlockReason::MissingHandler) => (),
            other => panic!("expected MissingHandler, got {:?}", other.err()),
        }
    }

    #[test]
    fn validate_resume_returns_missing_version_when_only_other_version_present() {
        // Name is known but the run's pinned version is no longer in the binary.
        let mut reg = WorkflowRegistry::new();
        insert(&mut reg, "wf", "v1", "sig1", WorkflowDefStatus::Active);

        match reg.validate_resume("wf", "v2", "sig2") {
            Err(ResumeBlockReason::MissingVersion) => (),
            other => panic!("expected MissingVersion, got {:?}", other.err()),
        }
    }

    #[test]
    fn validate_resume_returns_signature_mismatch_when_contract_drifted() {
        let mut reg = WorkflowRegistry::new();
        insert(
            &mut reg,
            "wf",
            "v1",
            "current-sig",
            WorkflowDefStatus::Active,
        );

        match reg.validate_resume("wf", "v1", "old-sig") {
            Err(ResumeBlockReason::SignatureMismatch { expected, actual }) => {
                assert_eq!(expected, "old-sig");
                assert_eq!(actual, "current-sig");
            }
            other => panic!("expected SignatureMismatch, got {:?}", other.err()),
        }
    }

    #[test]
    fn validate_resume_returns_entry_when_version_and_signature_match() {
        let mut reg = WorkflowRegistry::new();
        insert(&mut reg, "wf", "v1", "sig1", WorkflowDefStatus::Active);

        let entry = reg.validate_resume("wf", "v1", "sig1").expect("resume ok");
        assert_eq!(entry.info.name, "wf");
        assert_eq!(entry.info.version, "v1");
    }

    #[test]
    fn validate_resume_succeeds_for_deprecated_version() {
        // Deprecated versions must still resume — they exist precisely to
        // drain in-flight runs. The block reasons gate on presence and
        // signature, not lifecycle status.
        let mut reg = WorkflowRegistry::new();
        insert(&mut reg, "wf", "v1", "sig1", WorkflowDefStatus::Deprecated);
        insert(&mut reg, "wf", "v2", "sig2", WorkflowDefStatus::Active);

        let entry = reg
            .validate_resume("wf", "v1", "sig1")
            .expect("deprecated must resume");
        assert_eq!(entry.info.version, "v1");
    }

    #[test]
    fn names_dedupes_to_active_names_only() {
        // names() iterates active_versions, so two versions of the same
        // workflow appear once and a deprecated-only name appears zero times.
        let mut reg = WorkflowRegistry::new();
        insert(&mut reg, "wf_a", "v1", "a1", WorkflowDefStatus::Deprecated);
        insert(&mut reg, "wf_a", "v2", "a2", WorkflowDefStatus::Active);
        insert(&mut reg, "wf_b", "v1", "b1", WorkflowDefStatus::Active);
        insert(&mut reg, "wf_c", "v1", "c1", WorkflowDefStatus::Deprecated);

        let mut names: Vec<&str> = reg.names().collect();
        names.sort_unstable();
        assert_eq!(names, vec!["wf_a", "wf_b"]);
    }

    #[test]
    fn definitions_returns_every_entry() {
        // definitions() drives startup persistence — it must surface
        // deprecated and staging versions, not just active ones.
        let mut reg = WorkflowRegistry::new();
        insert(&mut reg, "wf", "v1", "s1", WorkflowDefStatus::Deprecated);
        insert(&mut reg, "wf", "v2", "s2", WorkflowDefStatus::Active);
        insert(&mut reg, "wf", "v3", "s3", WorkflowDefStatus::Staging);

        assert_eq!(reg.definitions().len(), 3);
        assert_eq!(reg.len(), 3);
    }

    #[test]
    fn clone_shares_handlers_but_isolates_maps() {
        // Clone must deep-copy the maps (mutations to the clone must not
        // leak back) while sharing the Arc-wrapped handler bodies.
        let mut original = WorkflowRegistry::new();
        insert(&mut original, "wf", "v1", "sig1", WorkflowDefStatus::Active);

        let mut clone = original.clone();
        insert(&mut clone, "wf", "v2", "sig2", WorkflowDefStatus::Active);

        assert_eq!(original.len(), 1);
        assert_eq!(clone.len(), 2);
        // Original's active pointer must not be redirected by the clone's insert.
        assert_eq!(
            original.get_active("wf").expect("active").info.version,
            "v1"
        );
        assert_eq!(clone.get_active("wf").expect("active").info.version, "v2");
    }

    #[test]
    fn validate_resume_relaxed_mode_accepts_signature_mismatch() {
        // In relaxed mode a run whose stored signature differs from the binary's
        // compiled signature must still resume. This is the rolling-deploy case:
        // a minor change produced a new hash but the workflow logic is compatible.
        let mut reg = WorkflowRegistry::new();
        reg.signature_check = SignatureCheckMode::Relaxed;
        insert(&mut reg, "wf", "v1", "new-sig", WorkflowDefStatus::Active);

        let entry = reg
            .validate_resume("wf", "v1", "old-sig")
            .expect("relaxed mode must accept signature mismatch");
        assert_eq!(entry.info.version, "v1");
    }

    #[test]
    fn validate_resume_relaxed_mode_still_blocks_on_missing_version() {
        // Relaxed mode skips the hash check but cannot conjure a handler for a
        // version that was never registered in this binary.
        let mut reg = WorkflowRegistry::new();
        reg.signature_check = SignatureCheckMode::Relaxed;
        insert(&mut reg, "wf", "v1", "sig1", WorkflowDefStatus::Active);

        match reg.validate_resume("wf", "v2", "sig2") {
            Err(ResumeBlockReason::MissingVersion) => (),
            other => panic!("expected MissingVersion, got {:?}", other.err()),
        }
    }

    #[test]
    fn resume_block_reason_descriptions_are_human_readable() {
        assert!(
            ResumeBlockReason::MissingHandler
                .description()
                .contains("No handler")
        );
        assert!(
            ResumeBlockReason::MissingVersion
                .description()
                .contains("version")
        );
        let sm = ResumeBlockReason::SignatureMismatch {
            expected: "abc".to_string(),
            actual: "def".to_string(),
        };
        let desc = sm.description();
        assert!(desc.contains("abc"));
        assert!(desc.contains("def"));
    }
}
