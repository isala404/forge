use ahash::AHashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use forge_core::{
    ForgeMutation, ForgeQuery, FunctionInfo, FunctionKind, MutationContext, QueryContext, Result,
};
use serde_json::Value;

/// Normalize args for deserialization.
/// - Keeps `null` as-is so unit `()` deserializes correctly.
/// - Treats `{}` as `null` so no-arg functions accept empty objects.
/// - Unwraps `{"args": ...}` or `{"input": ...}` wrapper if present (callers may use either format).
fn normalize_args(args: Value) -> Value {
    match &args {
        Value::Object(map) if map.is_empty() => Value::Null,
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
    }
}

/// Type alias for a boxed function that executes with JSON args and returns JSON result.
pub type BoxedQueryFn = Arc<
    dyn Fn(&QueryContext, Value) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + '_>>
        + Send
        + Sync,
>;

pub type BoxedMutationFn = Arc<
    dyn Fn(&MutationContext, Value) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + '_>>
        + Send
        + Sync,
>;

/// Entry in the function registry.
pub enum FunctionEntry {
    Query {
        info: FunctionInfo,
        handler: BoxedQueryFn,
    },
    Mutation {
        info: FunctionInfo,
        handler: BoxedMutationFn,
    },
    /// Webhook registered for metadata access (info lookup, MCP tool list,
    /// observability). Execution always goes through the dedicated webhook HTTP
    /// route with signature validation — this entry carries no handler.
    Webhook { info: FunctionInfo },
}

impl FunctionEntry {
    pub fn info(&self) -> &FunctionInfo {
        match self {
            FunctionEntry::Query { info, .. } => info,
            FunctionEntry::Mutation { info, .. } => info,
            FunctionEntry::Webhook { info } => info,
        }
    }

    pub fn kind(&self) -> FunctionKind {
        match self {
            FunctionEntry::Query { .. } => FunctionKind::Query,
            FunctionEntry::Mutation { .. } => FunctionKind::Mutation,
            FunctionEntry::Webhook { .. } => FunctionKind::Webhook,
        }
    }
}

/// Registry of all FORGE functions.
#[derive(Clone)]
pub struct FunctionRegistry {
    functions: AHashMap<String, FunctionEntry>,
}

impl Clone for FunctionEntry {
    fn clone(&self) -> Self {
        match self {
            FunctionEntry::Query { info, handler } => FunctionEntry::Query {
                info: info.clone(),
                handler: Arc::clone(handler),
            },
            FunctionEntry::Mutation { info, handler } => FunctionEntry::Mutation {
                info: info.clone(),
                handler: Arc::clone(handler),
            },
            FunctionEntry::Webhook { info } => FunctionEntry::Webhook { info: info.clone() },
        }
    }
}

impl FunctionRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            functions: AHashMap::new(),
        }
    }

    /// Register a query function.
    pub fn register_query<Q: ForgeQuery>(&mut self)
    where
        Q::Args: serde::de::DeserializeOwned + Send + 'static,
        Q::Output: serde::Serialize + Send + 'static,
    {
        let info = Q::info();
        let name = info.name.to_string();

        let handler: BoxedQueryFn = Arc::new(move |ctx, args| {
            Box::pin(async move {
                let parsed_args: Q::Args = serde_json::from_value(normalize_args(args))
                    .map_err(|e| forge_core::ForgeError::Validation(e.to_string()))?;
                let result = Q::execute(ctx, parsed_args).await?;
                serde_json::to_value(result).map_err(|e| {
                    forge_core::ForgeError::internal_with("Failed to serialize result", e)
                })
            })
        });

        self.functions
            .insert(name, FunctionEntry::Query { info, handler });
    }

    /// Register a mutation function.
    pub fn register_mutation<M: ForgeMutation>(&mut self)
    where
        M::Args: serde::de::DeserializeOwned + Send + 'static,
        M::Output: serde::Serialize + Send + 'static,
    {
        let info = M::info();
        let name = info.name.to_string();

        let handler: BoxedMutationFn = Arc::new(move |ctx, args| {
            Box::pin(async move {
                let parsed_args: M::Args = serde_json::from_value(normalize_args(args))
                    .map_err(|e| forge_core::ForgeError::Validation(e.to_string()))?;
                let result = M::execute(ctx, parsed_args).await?;
                serde_json::to_value(result).map_err(|e| {
                    forge_core::ForgeError::internal_with("Failed to serialize result", e)
                })
            })
        });

        self.functions
            .insert(name, FunctionEntry::Mutation { info, handler });
    }

    /// Get a function by name.
    pub fn get(&self, name: &str) -> Option<&FunctionEntry> {
        self.functions.get(name)
    }

    /// Get all function names.
    pub fn function_names(&self) -> impl Iterator<Item = &str> {
        self.functions.keys().map(|s| s.as_str())
    }

    /// Get all functions.
    pub fn functions(&self) -> impl Iterator<Item = (&str, &FunctionEntry)> {
        self.functions.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Get the number of registered functions.
    pub fn len(&self) -> usize {
        self.functions.len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.functions.is_empty()
    }

    /// Get all queries.
    pub fn queries(&self) -> impl Iterator<Item = (&str, &FunctionInfo)> {
        self.functions.iter().filter_map(|(name, entry)| {
            if let FunctionEntry::Query { info, .. } = entry {
                Some((name.as_str(), info))
            } else {
                None
            }
        })
    }

    /// Register a webhook's metadata into the function registry.
    ///
    /// Webhooks are registered here for metadata access (info lookup, MCP tool
    /// list, observability) only. They carry no executable handler because
    /// execution requires signature validation that lives in the dedicated
    /// webhook HTTP route. Direct RPC calls to webhook names are rejected by
    /// `FunctionRouter`.
    pub fn register_webhook_info(&mut self, info: FunctionInfo) {
        let name = info.name.to_string();
        self.functions.insert(name, FunctionEntry::Webhook { info });
    }

    /// Get all mutations.
    pub fn mutations(&self) -> impl Iterator<Item = (&str, &FunctionInfo)> {
        self.functions.iter().filter_map(|(name, entry)| {
            if let FunctionEntry::Mutation { info, .. } = entry {
                Some((name.as_str(), info))
            } else {
                None
            }
        })
    }

    /// Get all webhooks.
    pub fn webhooks(&self) -> impl Iterator<Item = (&str, &FunctionInfo)> {
        self.functions.iter().filter_map(|(name, entry)| {
            if let FunctionEntry::Webhook { info } = entry {
                Some((name.as_str(), info))
            } else {
                None
            }
        })
    }
}

impl Default for FunctionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use serde_json::json;

    fn info(name: &'static str, kind: FunctionKind) -> FunctionInfo {
        FunctionInfo {
            name,
            description: None,
            kind,
            required_role: None,
            is_public: true,
            cache_ttl: None,
            timeout: None,
            http_timeout: None,
            rate_limit_requests: None,
            rate_limit_per_secs: None,
            rate_limit_key: None,
            log_level: None,
            table_dependencies: &[],
            selected_columns: &[],
            changed_columns: &[],
            transactional: false,
            consistent: false,
            max_upload_size_bytes: None,
            requires_tenant_scope: false,
        }
    }

    #[test]
    fn normalize_args_passes_null_through_untouched() {
        // Unit `()` deserializes from JSON null; do not collapse it elsewhere.
        assert_eq!(normalize_args(json!(null)), json!(null));
    }

    #[test]
    fn normalize_args_treats_empty_object_as_null() {
        // No-arg handlers accept `{}` so frontends don't have to special-case
        // unit args when serializing a typed payload.
        assert_eq!(normalize_args(json!({})), json!(null));
    }

    #[test]
    fn normalize_args_unwraps_args_envelope() {
        assert_eq!(normalize_args(json!({"args": {"id": 7}})), json!({"id": 7}));
        assert_eq!(normalize_args(json!({"args": 42})), json!(42));
        assert_eq!(normalize_args(json!({"args": null})), json!(null));
    }

    #[test]
    fn normalize_args_unwraps_input_envelope() {
        assert_eq!(
            normalize_args(json!({"input": [1, 2, 3]})),
            json!([1, 2, 3])
        );
    }

    #[test]
    fn normalize_args_keeps_other_single_key_objects_intact() {
        // A handler with `struct Args { id: u32 }` should receive {"id": ...}
        // as-is — the envelope-stripping only fires for `args`/`input`.
        let v = json!({"id": 7});
        assert_eq!(normalize_args(v.clone()), v);
    }

    #[test]
    fn normalize_args_keeps_multi_key_objects_intact() {
        let v = json!({"name": "alice", "age": 30});
        assert_eq!(normalize_args(v.clone()), v);
    }

    #[test]
    fn normalize_args_keeps_non_object_values_intact() {
        assert_eq!(normalize_args(json!(42)), json!(42));
        assert_eq!(normalize_args(json!("hello")), json!("hello"));
        assert_eq!(normalize_args(json!([1, 2])), json!([1, 2]));
        assert_eq!(normalize_args(json!(true)), json!(true));
    }

    // --- Registry construction + lookup ---

    #[test]
    fn new_registry_is_empty() {
        let reg = FunctionRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert!(reg.get("anything").is_none());
        assert_eq!(reg.function_names().count(), 0);
    }

    #[test]
    fn register_webhook_info_stores_entry_under_function_name() {
        let mut reg = FunctionRegistry::new();
        reg.register_webhook_info(info("stripe_webhook", FunctionKind::Webhook));

        assert_eq!(reg.len(), 1);
        assert!(!reg.is_empty());
        let entry = reg.get("stripe_webhook").expect("registered");
        assert_eq!(entry.kind(), FunctionKind::Webhook);
        assert_eq!(entry.info().name, "stripe_webhook");
    }

    #[test]
    fn register_same_name_overwrites_existing_entry() {
        // Last writer wins — startup ordering decides which handler is active.
        let mut reg = FunctionRegistry::new();
        reg.register_webhook_info(info("dup", FunctionKind::Webhook));
        let mut second = info("dup", FunctionKind::Webhook);
        second.is_public = false;
        reg.register_webhook_info(second);

        assert_eq!(reg.len(), 1);
        assert!(!reg.get("dup").expect("present").info().is_public);
    }

    #[test]
    fn iterators_partition_by_kind() {
        let mut reg = FunctionRegistry::new();
        // Use webhook entries for each kind variant — only the FunctionEntry
        // variant is exercised, not handler invocation.
        reg.register_webhook_info(info("hook_a", FunctionKind::Webhook));
        reg.register_webhook_info(info("hook_b", FunctionKind::Webhook));

        let names: Vec<&str> = reg.function_names().collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"hook_a"));
        assert!(names.contains(&"hook_b"));

        // Webhook accessor returns both.
        let webhooks: Vec<&str> = reg.webhooks().map(|(n, _)| n).collect();
        assert_eq!(webhooks.len(), 2);
        // Query/Mutation accessors skip webhook entries.
        assert_eq!(reg.queries().count(), 0);
        assert_eq!(reg.mutations().count(), 0);
    }
}
