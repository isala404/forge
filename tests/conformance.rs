#![cfg(all(feature = "pg-tests", feature = "conformance"))]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::print_stdout
)]

use async_trait::async_trait;
use forgelib::conformance::ForgeFactory;
use forgelib::testing::TestDatabase;
use forgelib::Forge;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Mutex;

const SCENARIO_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/conformance/scenarios");
const GAPS_FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/conformance/known_gaps.json"
);

/// Signing secret wired into every variant so the blob presign/verify scenarios have a
/// configured key regardless of which backend stores the bytes.
const SIGNING_SECRET: &str = "conformance-signing-secret";

/// Which backend a scenario runs against. The same scenario JSON must pass on every
/// applicable variant, which is the point of the matrix.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Variant {
    /// Every primitive on Postgres (the reference, durable backend).
    Postgres,
    /// Every primitive on its in-process memory backend; the system Postgres is still
    /// connected (init migrates it), but no primitive state touches it.
    Memory,
    /// Blob bytes on a per-scenario filesystem directory, metadata in Postgres. Only the
    /// blob scenarios exercise this; the other primitives keep their Postgres default.
    Filesystem,
}

impl Variant {
    fn label(self) -> &'static str {
        match self {
            Variant::Postgres => "postgres",
            Variant::Memory => "memory",
            Variant::Filesystem => "fs",
        }
    }
}

/// The variants a primitive's scenarios run against. Blob also runs on the filesystem
/// backend; everything else runs on Postgres + memory.
fn variants_for(primitive: &str) -> &'static [Variant] {
    match primitive {
        "blob" => &[Variant::Postgres, Variant::Memory, Variant::Filesystem],
        _ => &[Variant::Postgres, Variant::Memory],
    }
}

/// Whether a scenario applies to `variant`. A scenario may pin itself to a subset of
/// backends via an optional `"backends"` array when it asserts a capability one backend
/// documents it cannot provide (e.g. the in-process scheduler holds no queue handle, so
/// scheduler->queue delivery is Postgres-only). Absent the field, every variant applies
/// (the default), so a scenario is backend-agnostic unless it explicitly says otherwise.
fn scenario_runs_on(scenario: &Value, variant: Variant) -> bool {
    match scenario.get("backends").and_then(Value::as_array) {
        None => true,
        Some(list) => list.iter().any(|b| b.as_str() == Some(variant.label())),
    }
}

/// Names a unique scratch directory for a filesystem-blob scenario.
static FS_TMP_SEQ: AtomicUsize = AtomicUsize::new(0);

/// Supplies the kit with namespaced Forges that share one throwaway backing store, with
/// the per-namespace config shaped by the `variant`.
///
/// A fresh factory is created per (scenario, variant), so each run gets a clean DB (and,
/// for the filesystem variant, a clean directory). The store is created lazily on the
/// first `forge()` call and reused for every subsequent namespace in that scenario, so the
/// namespace-isolation scenarios share one backing store.
struct VariantFactory {
    variant: Variant,
    db: Mutex<Option<TestDatabase>>,
    /// The shared filesystem-blob root for this scenario (filesystem variant only).
    tmp: StdMutex<Option<PathBuf>>,
}

impl VariantFactory {
    fn new(variant: Variant) -> Self {
        Self {
            variant,
            db: Mutex::new(None),
            tmp: StdMutex::new(None),
        }
    }

    /// The per-scenario filesystem-blob root, created on first use and shared across the
    /// scenario's namespaces (so namespace isolation rides on the Postgres metadata, not on
    /// separate directories).
    fn fs_root(&self) -> Result<PathBuf, String> {
        let mut guard = self
            .tmp
            .lock()
            .map_err(|_| "tmp lock poisoned".to_string())?;
        if guard.is_none() {
            let n = FS_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("forge_conf_blob_{}_{n}", std::process::id()));
            std::fs::create_dir_all(&dir).map_err(|e| format!("create blob tempdir: {e}"))?;
            *guard = Some(dir);
        }
        Ok(guard.clone().unwrap())
    }
}

impl Drop for VariantFactory {
    fn drop(&mut self) {
        if let Ok(guard) = self.tmp.lock()
            && let Some(dir) = guard.as_ref()
        {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[async_trait]
impl ForgeFactory for VariantFactory {
    async fn forge(&self, namespace: &str) -> Result<Forge, String> {
        let mut guard = self.db.lock().await;
        if guard.is_none() {
            *guard = Some(
                TestDatabase::new()
                    .await
                    .map_err(|e| format!("db setup: {e}"))?,
            );
        }
        let url = guard.as_ref().unwrap().url().to_string();
        // init migrates the throwaway DB's schema (idempotent across the per-namespace
        // inits against the same DB). One `[blob]` table per variant so signing_secret and
        // the filesystem backend don't collide as duplicate tables.
        let base = format!("[postgres]\nurl = \"{url}\"\n[forge]\nnamespace = \"{namespace}\"\n");
        let toml = match self.variant {
            Variant::Postgres => {
                format!("{base}[blob]\nsigning_secret = \"{SIGNING_SECRET}\"\n")
            }
            Variant::Memory => format!(
                "{base}[blob]\nsigning_secret = \"{SIGNING_SECRET}\"\n\
                 [backends]\ndefault = \"memory\"\nblob = \"memory\"\n"
            ),
            Variant::Filesystem => format!(
                "{base}[blob]\nsigning_secret = \"{SIGNING_SECRET}\"\n\
                 backend = \"fs\"\nfs_root = \"{}\"\n",
                self.fs_root()?.display(),
            ),
        };
        Forge::init_from_str(&toml)
            .await
            .map_err(|e| format!("forge init (ns {namespace:?}): {e}"))
    }
}

#[tokio::test]
async fn conformance_rust() {
    let gaps = load_rust_gaps();
    let mut passed = 0usize;
    let mut problems: Vec<String> = Vec::new();

    for file in scenario_files() {
        let text = std::fs::read_to_string(&file).unwrap();
        let doc: Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("invalid scenario json {}: {e}", file.display()));
        let primitive = doc["primitive"].as_str().unwrap().to_string();
        let variants = variants_for(&primitive);
        for scenario in doc["scenarios"].as_array().unwrap() {
            let name = scenario["name"].as_str().unwrap().to_string();
            let key = (primitive.clone(), name.clone());
            let expected_fail = gaps.contains(&key);
            for &variant in variants {
                if !scenario_runs_on(scenario, variant) {
                    continue;
                }
                let label = variant.label();
                let factory = VariantFactory::new(variant);
                let result = forgelib::conformance::run_one(&factory, scenario).await;
                match (result, expected_fail) {
                    (Ok(()), false) => {
                        passed += 1;
                        println!("PASS  {primitive}/{name} [{label}]");
                    }
                    (Err(e), true) => {
                        passed += 1;
                        println!("XFAIL {primitive}/{name} [{label}]: {e}");
                    }
                    (Ok(()), true) => problems.push(format!(
                        "{primitive}/{name} [{label}]: PASSED but is a registered rust gap; remove it from known_gaps.json"
                    )),
                    (Err(e), false) => problems.push(format!("{primitive}/{name} [{label}]: {e}")),
                }
            }
        }
    }

    println!(
        "\nconformance(rust): {passed} ok, {} unexpected",
        problems.len()
    );
    assert!(
        problems.is_empty(),
        "unexpected conformance results:\n  {}",
        problems.join("\n  ")
    );
}

/// `(primitive, scenario)` pairs registered as expected-fail for the `rust` runner.
fn load_rust_gaps() -> std::collections::HashSet<(String, String)> {
    let text = std::fs::read_to_string(GAPS_FILE).unwrap();
    let doc: Value = serde_json::from_str(&text).unwrap();
    let mut set = std::collections::HashSet::new();
    for gap in doc["gaps"].as_array().unwrap() {
        let langs = gap["languages"].as_array().unwrap();
        if langs.iter().any(|l| l.as_str() == Some("rust")) {
            set.insert((
                gap["primitive"].as_str().unwrap().to_string(),
                gap["scenario"].as_str().unwrap().to_string(),
            ));
        }
    }
    set
}

fn scenario_files() -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(SCENARIO_DIR)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    files
}
