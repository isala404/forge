use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use console::style;
use std::fs;
use std::path::Path;

/// Add a new component.
#[derive(Parser)]
pub struct AddCommand {
    #[command(subcommand)]
    pub component: AddType,
}

/// Component types that can be added.
#[derive(Subcommand)]
pub enum AddType {
    /// Add a new model.
    Model {
        /// Model name (PascalCase).
        name: String,
    },
    /// Add a new query function.
    Query {
        /// Function name (snake_case).
        name: String,
    },
    /// Add a new mutation function.
    Mutation {
        /// Function name (snake_case).
        name: String,
    },
    /// Add a new action function.
    Action {
        /// Function name (snake_case).
        name: String,
    },
    /// Add a new background job.
    Job {
        /// Job name (snake_case).
        name: String,
    },
    /// Add a new cron task.
    Cron {
        /// Cron name (snake_case).
        name: String,
    },
    /// Add a new workflow.
    Workflow {
        /// Workflow name (snake_case).
        name: String,
    },
}

impl AddCommand {
    /// Execute the add command.
    pub async fn execute(self) -> Result<()> {
        match self.component {
            AddType::Model { name } => add_model(&name),
            AddType::Query { name } => add_function(&name, FunctionType::Query),
            AddType::Mutation { name } => add_function(&name, FunctionType::Mutation),
            AddType::Action { name } => add_function(&name, FunctionType::Action),
            AddType::Job { name } => add_job(&name),
            AddType::Cron { name } => add_cron(&name),
            AddType::Workflow { name } => add_workflow(&name),
        }
    }
}

/// Function types.
enum FunctionType {
    Query,
    Mutation,
    Action,
}

/// Add a new model.
fn add_model(name: &str) -> Result<()> {
    let pascal_name = to_pascal_case(name);
    let snake_name = to_snake_case(&pascal_name);

    let schema_dir = Path::new("src/schema");
    if !schema_dir.exists() {
        anyhow::bail!("Not in a FORGE project (src/schema not found)");
    }

    let file_path = schema_dir.join(format!("{}.rs", snake_name));
    if file_path.exists() {
        anyhow::bail!("Model already exists: {}", file_path.display());
    }

    let content = format!(
        r#"use forge::prelude::*;

/// {pascal_name} model.
#[forge::model]
pub struct {pascal_name} {{
    #[id]
    pub id: Uuid,

    // Add your fields here
    // pub name: String,

    #[default = "now()"]
    pub created_at: Timestamp,

    #[updated_at]
    pub updated_at: Timestamp,
}}
"#
    );

    fs::write(&file_path, content)?;
    update_schema_mod(&snake_name, &pascal_name)?;

    println!(
        "{} Created model: {}",
        style("✅").green(),
        style(&file_path.display()).cyan()
    );
    println!("   Don't forget to add your fields!");

    Ok(())
}

/// Add a new function.
fn add_function(name: &str, fn_type: FunctionType) -> Result<()> {
    let snake_name = to_snake_case(name);

    let functions_dir = Path::new("src/functions");
    if !functions_dir.exists() {
        anyhow::bail!("Not in a FORGE project (src/functions not found)");
    }

    let file_path = functions_dir.join(format!("{}.rs", snake_name));
    if file_path.exists() {
        anyhow::bail!("Function file already exists: {}", file_path.display());
    }

    let (attr, ctx_type, description) = match fn_type {
        FunctionType::Query => ("query", "QueryContext", "query"),
        FunctionType::Mutation => ("mutation", "MutationContext", "mutation"),
        FunctionType::Action => ("action", "ActionContext", "action"),
    };

    let content = format!(
        r#"use forge::prelude::*;

/// {snake_name} {description}.
#[forge::{attr}]
pub async fn {snake_name}(ctx: {ctx_type}) -> Result<()> {{
    // Add your logic here
    Ok(())
}}
"#
    );

    fs::write(&file_path, content)?;
    update_functions_mod(&snake_name)?;

    println!(
        "{} Created {}: {}",
        style("✅").green(),
        description,
        style(&file_path.display()).cyan()
    );

    Ok(())
}

/// Add a new job.
fn add_job(name: &str) -> Result<()> {
    let snake_name = to_snake_case(name);
    let pascal_name = to_pascal_case(name);

    let functions_dir = Path::new("src/functions");
    if !functions_dir.exists() {
        anyhow::bail!("Not in a FORGE project (src/functions not found)");
    }

    let file_path = functions_dir.join(format!("{}_job.rs", snake_name));
    if file_path.exists() {
        anyhow::bail!("Job file already exists: {}", file_path.display());
    }

    let content = format!(
        r#"use forge::prelude::*;

/// {pascal_name} job arguments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct {pascal_name}Args {{
    // Add your arguments here
}}

/// {snake_name} background job.
#[forge::job(
    timeout = "5m",
    max_attempts = 3
)]
pub async fn {snake_name}(ctx: JobContext, args: {pascal_name}Args) -> Result<()> {{
    // Add your job logic here
    Ok(())
}}
"#
    );

    fs::write(&file_path, content)?;
    update_functions_mod(&format!("{}_job", snake_name))?;

    println!(
        "{} Created job: {}",
        style("✅").green(),
        style(&file_path.display()).cyan()
    );

    Ok(())
}

/// Add a new cron task.
fn add_cron(name: &str) -> Result<()> {
    let snake_name = to_snake_case(name);

    let functions_dir = Path::new("src/functions");
    if !functions_dir.exists() {
        anyhow::bail!("Not in a FORGE project (src/functions not found)");
    }

    let file_path = functions_dir.join(format!("{}_cron.rs", snake_name));
    if file_path.exists() {
        anyhow::bail!("Cron file already exists: {}", file_path.display());
    }

    let content = format!(
        r#"use forge::prelude::*;

/// {snake_name} scheduled task.
#[forge::cron(schedule = "0 0 * * *")]  // Daily at midnight
pub async fn {snake_name}(ctx: CronContext) -> Result<()> {{
    ctx.log.info("Running {snake_name}");

    // Add your cron logic here

    Ok(())
}}
"#
    );

    fs::write(&file_path, content)?;
    update_functions_mod(&format!("{}_cron", snake_name))?;

    println!(
        "{} Created cron: {}",
        style("✅").green(),
        style(&file_path.display()).cyan()
    );
    println!("   Edit the schedule in the #[forge::cron] attribute");

    Ok(())
}

/// Add a new workflow.
fn add_workflow(name: &str) -> Result<()> {
    let snake_name = to_snake_case(name);
    let pascal_name = to_pascal_case(name);

    let functions_dir = Path::new("src/functions");
    if !functions_dir.exists() {
        anyhow::bail!("Not in a FORGE project (src/functions not found)");
    }

    let file_path = functions_dir.join(format!("{}_workflow.rs", snake_name));
    if file_path.exists() {
        anyhow::bail!("Workflow file already exists: {}", file_path.display());
    }

    let content = format!(
        r#"use forge::prelude::*;

/// {pascal_name} workflow input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct {pascal_name}Input {{
    // Add your input fields here
}}

/// {pascal_name} workflow output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct {pascal_name}Output {{
    // Add your output fields here
    pub success: bool,
}}

/// {snake_name} workflow.
#[forge::workflow(version = 1)]
pub async fn {snake_name}(ctx: WorkflowContext, input: {pascal_name}Input) -> Result<{pascal_name}Output> {{
    // Step 1: First step
    let step1_result = ctx.step("step1")
        .run(|| async {{
            // Add step 1 logic
            Ok(())
        }})
        .await?;

    // Step 2: Second step
    let step2_result = ctx.step("step2")
        .run(|| async {{
            // Add step 2 logic
            Ok(())
        }})
        .await?;

    Ok({pascal_name}Output {{ success: true }})
}}
"#
    );

    fs::write(&file_path, content)?;
    update_functions_mod(&format!("{}_workflow", snake_name))?;

    println!(
        "{} Created workflow: {}",
        style("✅").green(),
        style(&file_path.display()).cyan()
    );

    Ok(())
}

/// Update src/schema/mod.rs to include the new model.
fn update_schema_mod(snake_name: &str, pascal_name: &str) -> Result<()> {
    let mod_path = Path::new("src/schema/mod.rs");
    let content = fs::read_to_string(mod_path).unwrap_or_default();

    let new_content = format!(
        "{}pub mod {};\n\npub use {}::{};\n",
        content, snake_name, snake_name, pascal_name
    );

    fs::write(mod_path, new_content)?;
    Ok(())
}

/// Update src/functions/mod.rs to include the new function.
fn update_functions_mod(snake_name: &str) -> Result<()> {
    let mod_path = Path::new("src/functions/mod.rs");
    let content = fs::read_to_string(mod_path).unwrap_or_default();

    let new_content = format!(
        "{}pub mod {};\n\npub use {}::*;\n",
        content, snake_name, snake_name
    );

    fs::write(mod_path, new_content)?;
    Ok(())
}

/// Convert to PascalCase.
fn to_pascal_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = true;

    for c in s.chars() {
        if c == '_' || c == '-' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(c.to_uppercase().next().unwrap());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }

    result
}

/// Convert to snake_case.
fn to_snake_case(s: &str) -> String {
    let mut result = String::new();

    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(c.to_lowercase().next().unwrap());
        } else if c == '-' {
            result.push('_');
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
    fn test_to_pascal_case() {
        assert_eq!(to_pascal_case("user"), "User");
        assert_eq!(to_pascal_case("order_item"), "OrderItem");
        assert_eq!(to_pascal_case("my-component"), "MyComponent");
    }

    #[test]
    fn test_to_snake_case() {
        assert_eq!(to_snake_case("User"), "user");
        assert_eq!(to_snake_case("OrderItem"), "order_item");
        assert_eq!(to_snake_case("MyComponent"), "my_component");
    }
}
