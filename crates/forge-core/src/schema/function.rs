//! Function definitions for FORGE schema.
//!
//! This module defines the structure for queries, mutations, and actions
//! that can be registered in the schema registry.

use super::types::RustType;

/// Function kind (query, mutation, job, cron, workflow).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionKind {
    /// Read-only database query.
    Query,
    /// Write operation that modifies database state (may include HTTP calls).
    Mutation,
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
            FunctionKind::Job => "job",
            FunctionKind::Cron => "cron",
            FunctionKind::Workflow => "workflow",
        }
    }

    /// Check if this function kind is callable from the frontend.
    pub fn is_client_callable(&self) -> bool {
        matches!(self, FunctionKind::Query | FunctionKind::Mutation)
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
        crate::util::to_camel_case(&self.name)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
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
        use crate::util::to_camel_case;
        assert_eq!(to_camel_case("get_user"), "getUser");
        assert_eq!(to_camel_case("create_project_task"), "createProjectTask");
        assert_eq!(to_camel_case("getUser"), "getUser");
    }
}
