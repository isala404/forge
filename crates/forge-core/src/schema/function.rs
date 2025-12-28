//! Function definitions for FORGE schema.
//!
//! This module defines the structure for queries, mutations, and actions
//! that can be registered in the schema registry.

use super::types::RustType;

/// Function kind (query, mutation, action, job, cron, workflow).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionKind {
    /// Read-only database query.
    Query,
    /// Write operation that modifies database state.
    Mutation,
    /// External API call or side-effect.
    Action,
    /// Background job with retry logic.
    Job,
    /// Scheduled cron task.
    Cron,
    /// Multi-step durable workflow.
    Workflow,
}

impl FunctionKind {
    /// Get the string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            FunctionKind::Query => "query",
            FunctionKind::Mutation => "mutation",
            FunctionKind::Action => "action",
            FunctionKind::Job => "job",
            FunctionKind::Cron => "cron",
            FunctionKind::Workflow => "workflow",
        }
    }

    /// Check if this function kind is callable from the frontend.
    pub fn is_client_callable(&self) -> bool {
        matches!(
            self,
            FunctionKind::Query | FunctionKind::Mutation | FunctionKind::Action
        )
    }
}

impl std::fmt::Display for FunctionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Function argument definition.
#[derive(Debug, Clone)]
pub struct FunctionArg {
    /// Argument name (snake_case in Rust).
    pub name: String,
    /// Argument type.
    pub rust_type: RustType,
    /// Documentation comment.
    pub doc: Option<String>,
}

impl FunctionArg {
    /// Create a new function argument.
    pub fn new(name: impl Into<String>, rust_type: RustType) -> Self {
        Self {
            name: name.into(),
            rust_type,
            doc: None,
        }
    }
}

/// Function definition.
#[derive(Debug, Clone)]
pub struct FunctionDef {
    /// Function name (snake_case in Rust).
    pub name: String,
    /// Function kind.
    pub kind: FunctionKind,
    /// Input arguments.
    pub args: Vec<FunctionArg>,
    /// Return type.
    pub return_type: RustType,
    /// Documentation comment.
    pub doc: Option<String>,
    /// Whether the function is async.
    pub is_async: bool,
}

impl FunctionDef {
    /// Create a new function definition.
    pub fn new(name: impl Into<String>, kind: FunctionKind, return_type: RustType) -> Self {
        Self {
            name: name.into(),
            kind,
            args: Vec::new(),
            return_type,
            doc: None,
            is_async: true,
        }
    }

    /// Create a query function.
    pub fn query(name: impl Into<String>, return_type: RustType) -> Self {
        Self::new(name, FunctionKind::Query, return_type)
    }

    /// Create a mutation function.
    pub fn mutation(name: impl Into<String>, return_type: RustType) -> Self {
        Self::new(name, FunctionKind::Mutation, return_type)
    }

    /// Create an action function.
    pub fn action(name: impl Into<String>, return_type: RustType) -> Self {
        Self::new(name, FunctionKind::Action, return_type)
    }

    /// Add an argument.
    pub fn with_arg(mut self, arg: FunctionArg) -> Self {
        self.args.push(arg);
        self
    }

    /// Set documentation.
    pub fn with_doc(mut self, doc: impl Into<String>) -> Self {
        self.doc = Some(doc.into());
        self
    }

    /// Get the camelCase name for TypeScript.
    pub fn ts_name(&self) -> String {
        to_camel_case(&self.name)
    }
}

/// Convert snake_case to camelCase.
fn to_camel_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = false;

    for c in s.chars() {
        if c == '_' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(c.to_uppercase().next().unwrap_or(c));
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_function_def_query() {
        let func = FunctionDef::query("get_user", RustType::Custom("User".to_string()))
            .with_arg(FunctionArg::new("id", RustType::Uuid))
            .with_doc("Get a user by ID");

        assert_eq!(func.name, "get_user");
        assert_eq!(func.kind, FunctionKind::Query);
        assert_eq!(func.args.len(), 1);
        assert_eq!(func.ts_name(), "getUser");
    }

    #[test]
    fn test_function_def_mutation() {
        let func = FunctionDef::mutation("create_user", RustType::Custom("User".to_string()));
        assert_eq!(func.kind, FunctionKind::Mutation);
    }

    #[test]
    fn test_to_camel_case() {
        assert_eq!(to_camel_case("get_user"), "getUser");
        assert_eq!(to_camel_case("create_project_task"), "createProjectTask");
        assert_eq!(to_camel_case("getUser"), "getUser");
    }
}
