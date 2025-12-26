//! Sample Todo App demonstrating FORGE framework features.
//!
//! This example shows how to:
//! - Define models using #[model] attribute (for code generation)
//! - Define enums using #[forge_enum] attribute
//! - Create queries and mutations using #[query] and #[mutation] macros
//! - Use the TypeScript code generator

use chrono::{DateTime, Utc};
use forge_macros::{forge_enum, mutation, query};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Re-export forge for macro compatibility
use forge;

// ============================================================================
// Models - These are simple structs that can be used with forge-codegen
// ============================================================================

/// Status of a todo item.
#[forge_enum]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TodoStatus {
    Pending = 0,
    InProgress = 1,
    Completed = 2,
    Cancelled = 3,
}

/// Priority level for todos.
#[forge_enum]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Priority {
    Low = 0,
    Medium = 1,
    High = 2,
    Critical = 3,
}

/// A todo item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Todo {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub status: TodoStatus,
    pub priority: Priority,
    pub user_id: Uuid,
    pub due_date: Option<DateTime<Utc>>,
    pub tags: Vec<String>,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// A user in the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub avatar_url: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A project containing multiple todos.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub owner_id: Uuid,
    pub is_archived: bool,
    pub created_at: DateTime<Utc>,
}

// ============================================================================
// Queries
// ============================================================================

/// Query: Get all todos for a user.
#[query]
pub async fn get_user_todos(
    _ctx: &forge::forge_core::QueryContext,
    _user_id: Uuid,
) -> forge::forge_core::Result<Vec<Todo>> {
    // In a real app, this would query the database
    Ok(vec![])
}

/// Query: Get todos by status.
#[query]
pub async fn get_todos_by_status(
    _ctx: &forge::forge_core::QueryContext,
    _status: TodoStatus,
) -> forge::forge_core::Result<Vec<Todo>> {
    Ok(vec![])
}

/// Query: Get a single todo by ID.
#[query]
pub async fn get_todo(
    _ctx: &forge::forge_core::QueryContext,
    _id: Uuid,
) -> forge::forge_core::Result<Option<Todo>> {
    Ok(None)
}

// ============================================================================
// Mutations
// ============================================================================

/// Input for creating a new todo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTodoInput {
    pub title: String,
    pub description: Option<String>,
    pub priority: Priority,
    pub due_date: Option<DateTime<Utc>>,
    pub tags: Vec<String>,
}

/// Mutation: Create a new todo.
#[mutation]
pub async fn create_todo(
    ctx: &forge::forge_core::MutationContext,
    input: CreateTodoInput,
) -> forge::forge_core::Result<Todo> {
    let now = Utc::now();
    let user_id = ctx.auth.user_id().unwrap_or_else(Uuid::nil);
    let todo = Todo {
        id: Uuid::new_v4(),
        title: input.title,
        description: input.description,
        status: TodoStatus::Pending,
        priority: input.priority,
        user_id,
        due_date: input.due_date,
        tags: input.tags,
        updated_at: now,
        created_at: now,
    };
    Ok(todo)
}

/// Mutation: Update todo status.
#[mutation]
pub async fn update_todo_status(
    _ctx: &forge::forge_core::MutationContext,
    _id: Uuid,
    _status: TodoStatus,
) -> forge::forge_core::Result<Todo> {
    // In a real app, this would update the database
    Err(forge::forge_core::ForgeError::NotFound(
        "Todo not found".into(),
    ))
}

/// Mutation: Delete a todo.
#[mutation]
pub async fn delete_todo(
    _ctx: &forge::forge_core::MutationContext,
    _id: Uuid,
) -> forge::forge_core::Result<bool> {
    Ok(true)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::schema::{EnumDef, EnumVariant, FieldDef, RustType, SchemaRegistry, TableDef};
    use std::path::PathBuf;

    #[test]
    fn test_todo_status_values() {
        assert_eq!(TodoStatus::Pending as i32, 0);
        assert_eq!(TodoStatus::InProgress as i32, 1);
        assert_eq!(TodoStatus::Completed as i32, 2);
        assert_eq!(TodoStatus::Cancelled as i32, 3);
    }

    #[test]
    fn test_priority_values() {
        assert_eq!(Priority::Low as i32, 0);
        assert_eq!(Priority::Medium as i32, 1);
        assert_eq!(Priority::High as i32, 2);
        assert_eq!(Priority::Critical as i32, 3);
    }

    #[test]
    fn test_codegen_typescript() {
        use forge_codegen::TypeGenerator;

        let generator = TypeGenerator::new(PathBuf::from("/tmp/test-output"));
        let registry = SchemaRegistry::new();

        // Register a simple table
        let mut table = TableDef::new("todos", "Todo");
        table.fields.push(FieldDef::new("id", RustType::Uuid));
        table
            .fields
            .push(FieldDef::new("title", RustType::String));
        table
            .fields
            .push(FieldDef::new("completed", RustType::Bool));
        registry.register_table(table);

        // Register an enum
        let mut enum_def = EnumDef::new("Priority");
        enum_def.variants.push(EnumVariant::new("Low"));
        enum_def.variants.push(EnumVariant::new("Medium"));
        enum_def.variants.push(EnumVariant::new("High"));
        registry.register_enum(enum_def);

        // Generate TypeScript types
        let result = generator.generate(&registry);
        assert!(result.is_ok(), "TypeScript generation should succeed");

        let generated = result.unwrap();
        assert!(generated.contains("Todo"), "Should contain Todo type");
        assert!(
            generated.contains("Priority"),
            "Should contain Priority enum"
        );
    }

    #[test]
    fn test_todo_serialization() {
        let todo = Todo {
            id: Uuid::new_v4(),
            title: "Test todo".to_string(),
            description: Some("A test description".to_string()),
            status: TodoStatus::Pending,
            priority: Priority::High,
            user_id: Uuid::new_v4(),
            due_date: None,
            tags: vec!["test".to_string(), "sample".to_string()],
            updated_at: Utc::now(),
            created_at: Utc::now(),
        };

        // Serialize to JSON
        let json = serde_json::to_string(&todo).expect("Should serialize");
        assert!(json.contains("Test todo"));

        // Deserialize back
        let parsed: Todo = serde_json::from_str(&json).expect("Should deserialize");
        assert_eq!(parsed.title, "Test todo");
        assert_eq!(parsed.status, TodoStatus::Pending);
    }
}
