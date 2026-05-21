use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use forge_core::{ForgeMcpTool, McpToolContext, McpToolInfo, Result};
use serde_json::Value;

fn normalize_args(args: Value) -> Value {
    let unwrapped = match args {
        Value::Object(map) if map.len() == 1 => map
            .get("args")
            .or_else(|| map.get("input"))
            .cloned()
            .unwrap_or(Value::Object(map)),
        other => other,
    };

    match unwrapped {
        Value::Null => Value::Object(serde_json::Map::new()),
        other => other,
    }
}

pub type BoxedMcpToolFn = Arc<
    dyn Fn(&McpToolContext, Value) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + '_>>
        + Send
        + Sync,
>;

/// A registered MCP tool entry.
pub struct McpToolEntry {
    pub info: McpToolInfo,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    pub handler: BoxedMcpToolFn,
}

/// Registry of MCP tools.
#[derive(Clone, Default)]
pub struct McpToolRegistry {
    tools: HashMap<String, Arc<McpToolEntry>>,
}

impl McpToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register<T: ForgeMcpTool>(&mut self)
    where
        T::Args: serde::de::DeserializeOwned + Send + 'static,
        T::Output: serde::Serialize + Send + 'static,
    {
        let info = T::info();
        if let Err(e) = info.validate() {
            tracing::error!(error = %e, "Skipping invalid MCP tool registration");
            return;
        }

        let name = info.name.to_string();
        let input_schema = T::input_schema();
        let output_schema = T::output_schema();

        let handler: BoxedMcpToolFn = Arc::new(move |ctx, args| {
            Box::pin(async move {
                let parsed_args: T::Args = serde_json::from_value(normalize_args(args))
                    .map_err(|e| forge_core::ForgeError::Validation(e.to_string()))?;
                let result = T::execute(ctx, parsed_args).await?;
                serde_json::to_value(result).map_err(|e| {
                    forge_core::ForgeError::internal_with("Failed to serialize MCP tool result", e)
                })
            })
        });

        self.tools.insert(
            name,
            Arc::new(McpToolEntry {
                info,
                input_schema,
                output_schema,
                handler,
            }),
        );
    }

    pub fn get(&self, name: &str) -> Option<Arc<McpToolEntry>> {
        self.tools.get(name).cloned()
    }

    pub fn list(&self) -> impl Iterator<Item = Arc<McpToolEntry>> + '_ {
        self.tools.values().cloned()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}
