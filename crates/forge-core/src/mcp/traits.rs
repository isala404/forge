use std::future::Future;
use std::pin::Pin;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::context::McpToolContext;
use crate::error::{ForgeError, Result};
use crate::metadata::HandlerMetadata;

/// Icon metadata exposed for an MCP tool.
///
/// Constructed by the `#[mcp_tool]` macro (or by the runtime when emitting
/// `tools/list`). Adding a field is breaking for hand-written `ForgeMcpTool`
/// impls; stage extensions through a major bump.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct McpToolIcon {
    pub src: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sizes: Option<&'static [&'static str]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<&'static str>,
}

/// Tool behavior annotations exposed to clients.
///
/// Constructed by the `#[mcp_tool]` macro. Adding a field is breaking for
/// hand-written `ForgeMcpTool` impls; stage extensions through a major bump.
#[derive(Debug, Clone, Copy, Serialize, Default)]
pub struct McpToolAnnotations {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
}

/// Metadata describing an MCP tool.
///
/// Constructed by the `#[mcp_tool]` macro. Adding a field is a breaking change
/// for hand-written `ForgeMcpTool` impls; stage extensions through a builder
/// or major bump.
#[derive(Debug, Clone, Copy)]
pub struct McpToolInfo {
    pub name: &'static str,
    pub title: Option<&'static str>,
    pub description: Option<&'static str>,
    pub required_role: Option<&'static str>,
    pub is_public: bool,
    pub timeout: Option<std::time::Duration>,
    pub rate_limit_requests: Option<u32>,
    pub rate_limit_per_secs: Option<u64>,
    pub rate_limit_key: Option<&'static str>,
    pub annotations: McpToolAnnotations,
    pub icons: &'static [McpToolIcon],
}

impl McpToolInfo {
    pub fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            return Err(ForgeError::config(
                "MCP tool name cannot be empty",
            ));
        }
        Ok(())
    }
}

/// Content block for MCP tool call results.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum McpContentBlock {
    Text { text: String },
    ResourceLink(McpContent),
}

/// Resource link content for MCP tool results.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct McpContent {
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// Tool execution payload returned by MCP tool handlers.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct McpToolResult {
    pub content: Vec<McpContentBlock>,
    #[serde(rename = "structuredContent", skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<serde_json::Value>,
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

impl McpToolResult {
    pub fn success_text(text: impl Into<String>) -> Self {
        Self {
            content: vec![McpContentBlock::Text { text: text.into() }],
            structured_content: None,
            is_error: None,
        }
    }

    pub fn success_json(value: serde_json::Value) -> Self {
        let text = match serde_json::to_string(&value) {
            Ok(v) => v,
            Err(_) => "{}".to_string(),
        };
        let structured = match value {
            serde_json::Value::Object(_) => Some(value),
            _ => None,
        };

        Self {
            content: vec![McpContentBlock::Text { text }],
            structured_content: structured,
            is_error: None,
        }
    }

    pub fn tool_error(message: impl Into<String>) -> Self {
        Self {
            content: vec![McpContentBlock::Text {
                text: message.into(),
            }],
            structured_content: None,
            is_error: Some(true),
        }
    }
}

/// Trait implemented by all FORGE MCP tools.
pub trait ForgeMcpTool: crate::__sealed::Sealed + Send + Sync + 'static {
    type Args: DeserializeOwned + JsonSchema + Send + Sync;
    type Output: Serialize + JsonSchema + Send;

    fn info() -> McpToolInfo;

    /// Unified metadata for uniform consumers (observability, admin, codegen).
    fn metadata() -> HandlerMetadata {
        HandlerMetadata::from(&Self::info())
    }

    fn execute(
        ctx: &McpToolContext,
        args: Self::Args,
    ) -> Pin<Box<dyn Future<Output = Result<Self::Output>> + Send + '_>>;

    fn input_schema() -> serde_json::Value {
        let schema = schemars::schema_for!(Self::Args);
        serde_json::to_value(schema)
            .unwrap_or_else(|_| serde_json::json!({ "type": "object", "properties": {} }))
    }

    fn output_schema() -> Option<serde_json::Value> {
        let schema = schemars::schema_for!(Self::Output);
        Some(
            serde_json::to_value(schema)
                .unwrap_or_else(|_| serde_json::json!({ "type": "object", "properties": {} })),
        )
    }
}
