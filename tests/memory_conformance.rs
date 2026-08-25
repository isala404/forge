#![cfg(feature = "conformance")]
#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::collections::HashMap;

use async_trait::async_trait;
use forgelib::Forge;
use forgelib::conformance::ForgeFactory;
use serde_json::Value;
use tokio::sync::Mutex;

const SCENARIO_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/conformance/scenarios");
const SIGNING_SECRET: &str = "conformance-signing-secret";

struct MemoryFactory {
    handles: Mutex<HashMap<String, Forge>>,
}

impl MemoryFactory {
    fn new() -> Self {
        Self {
            handles: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl ForgeFactory for MemoryFactory {
    async fn forge(&self, namespace: &str) -> Result<Forge, String> {
        let mut handles = self.handles.lock().await;
        if let Some(forge) = handles.get(namespace) {
            return Ok(forge.clone());
        }
        let config = format!(
            "[forge]\nmode = \"memory\"\nenvironment = \"test\"\nnamespace = \"{namespace}\"\n\
             [blob]\nsigning_secret = \"{SIGNING_SECRET}\"\n"
        );
        let forge = Forge::init_from_str(&config)
            .await
            .map_err(|error| error.to_string())?;
        handles.insert(namespace.to_string(), forge.clone());
        Ok(forge)
    }
}

#[tokio::test]
async fn complete_memory_conformance_needs_no_database() {
    for path in scenario_files() {
        let document: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for scenario in document["scenarios"].as_array().unwrap() {
            if scenario
                .get("backends")
                .and_then(Value::as_array)
                .is_some_and(|backends| {
                    !backends
                        .iter()
                        .any(|value| value.as_str() == Some("memory"))
                })
            {
                continue;
            }
            let result = forgelib::conformance::run_one(&MemoryFactory::new(), scenario).await;
            assert!(
                result.is_ok(),
                "{}/{}: {}",
                document["primitive"].as_str().unwrap(),
                scenario["name"].as_str().unwrap(),
                result.unwrap_err()
            );
        }
    }
}

fn scenario_files() -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(SCENARIO_DIR)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    files.sort();
    files
}
