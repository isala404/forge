//! Function binding intermediate representation.
//!
//! `FunctionBinding` captures everything about an RPC function that code
//! generators need, computed once from the schema registry. This eliminates
//! the pattern where each generator independently re-derives the same facts
//! (has_upload? is_custom_args? camelCase name?) with subtle inconsistencies.

use forge_core::schema::{
    FunctionArg, FunctionDef, FunctionKind, RustType, SchemaRegistry, TableDef,
};

use crate::emit;

/// Pre-computed, target-agnostic binding for a single function.
#[derive(Debug)]
pub struct FunctionBinding {
    /// Original function name (snake_case).
    pub name: String,
    pub kind: FunctionKind,
    /// Context argument already stripped by the parser.
    pub args: Vec<FunctionArg>,
    pub return_type: RustType,
    /// Whether the single argument is a known custom Args/Input struct.
    pub is_custom_args: bool,
    /// Whether any argument contains an Upload type.
    pub has_upload: bool,
}

impl FunctionBinding {
    pub fn has_args(&self) -> bool {
        !self.args.is_empty()
    }
}

/// All bindings grouped by kind, sorted for deterministic output.
pub struct BindingSet {
    pub queries: Vec<FunctionBinding>,
    pub mutations: Vec<FunctionBinding>,
    pub jobs: Vec<FunctionBinding>,
    pub workflows: Vec<FunctionBinding>,
}

impl BindingSet {
    /// Build from the schema registry.
    pub fn from_registry(registry: &SchemaRegistry) -> Self {
        let functions = registry.all_functions();
        let tables = registry.all_tables();

        let mut queries = Vec::new();
        let mut mutations = Vec::new();
        let mut jobs = Vec::new();
        let mut workflows = Vec::new();

        for func in functions {
            let binding = build_binding(func, &tables);
            match binding.kind {
                FunctionKind::Query => queries.push(binding),
                FunctionKind::Mutation => mutations.push(binding),
                FunctionKind::Job => jobs.push(binding),
                FunctionKind::Workflow => workflows.push(binding),
                FunctionKind::Cron => {} // Not client-callable.
            }
        }

        queries.sort_by(|a, b| a.name.cmp(&b.name));
        mutations.sort_by(|a, b| a.name.cmp(&b.name));
        jobs.sort_by(|a, b| a.name.cmp(&b.name));
        workflows.sort_by(|a, b| a.name.cmp(&b.name));

        Self {
            queries,
            mutations,
            jobs,
            workflows,
        }
    }

    pub fn has_subscriptions(&self) -> bool {
        !self.queries.is_empty()
    }

    pub fn has_jobs(&self) -> bool {
        !self.jobs.is_empty()
    }

    pub fn has_workflows(&self) -> bool {
        !self.workflows.is_empty()
    }

    pub fn all(&self) -> impl Iterator<Item = &FunctionBinding> {
        self.queries
            .iter()
            .chain(self.mutations.iter())
            .chain(self.jobs.iter())
            .chain(self.workflows.iter())
    }
}

fn build_binding(func: FunctionDef, tables: &[TableDef]) -> FunctionBinding {
    let is_custom_args = func.args.len() == 1
        && func
            .args
            .first()
            .is_some_and(|arg| is_custom_args_type(&arg.rust_type, tables));

    let has_upload = func
        .args
        .iter()
        .any(|arg| emit::contains_upload(&arg.rust_type));

    FunctionBinding {
        name: func.name,
        kind: func.kind,
        args: func.args,
        return_type: func.return_type,
        is_custom_args,
        has_upload,
    }
}

/// Check if a type is a custom Args/Input struct that exists in the registry.
///
/// We require BOTH a naming convention match AND existence in the registry.
/// This prevents false positives on types like "InputHandler" or "ArgumentParser".
fn is_custom_args_type(rust_type: &RustType, tables: &[TableDef]) -> bool {
    match rust_type {
        RustType::Custom(name) => {
            (name.ends_with("Args") || name.ends_with("Input"))
                && tables.iter().any(|t| t.struct_name == *name)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::schema::{FunctionArg, FunctionDef, SchemaRegistry, TableDef};

    #[test]
    fn binding_set_groups_by_kind() {
        let registry = SchemaRegistry::new();

        registry.register_function(FunctionDef::query(
            "list_users",
            RustType::Vec(Box::new(RustType::Custom("User".into()))),
        ));
        registry.register_function(FunctionDef::mutation(
            "create_user",
            RustType::Custom("User".into()),
        ));
        registry.register_function(FunctionDef::new(
            "send_email",
            FunctionKind::Job,
            RustType::Custom("()".into()),
        ));

        let bindings = BindingSet::from_registry(&registry);

        assert_eq!(bindings.queries.len(), 1);
        assert_eq!(bindings.mutations.len(), 1);
        assert_eq!(bindings.jobs.len(), 1);
        assert_eq!(bindings.workflows.len(), 0);
    }

    #[test]
    fn binding_set_filters_crons() {
        let registry = SchemaRegistry::new();
        registry.register_function(FunctionDef::new(
            "daily_cleanup",
            FunctionKind::Cron,
            RustType::Custom("()".into()),
        ));

        let bindings = BindingSet::from_registry(&registry);
        assert_eq!(bindings.all().count(), 0);
    }

    #[test]
    fn custom_args_requires_registry_entry() {
        let registry = SchemaRegistry::new();

        let table = TableDef::new("CreateUserArgs", "CreateUserArgs");
        registry.register_table(table);

        let mut func = FunctionDef::mutation("create_user", RustType::Custom("User".into()));
        func.args.push(FunctionArg::new(
            "args",
            RustType::Custom("CreateUserArgs".into()),
        ));
        registry.register_function(func);

        let bindings = BindingSet::from_registry(&registry);
        assert!(
            bindings
                .mutations
                .first()
                .expect("expected custom args mutation binding")
                .is_custom_args
        );
    }

    #[test]
    fn custom_args_false_without_registry_entry() {
        let registry = SchemaRegistry::new();

        let mut func = FunctionDef::mutation("create_user", RustType::Custom("User".into()));
        func.args.push(FunctionArg::new(
            "args",
            RustType::Custom("CreateUserArgs".into()),
        ));
        registry.register_function(func);

        let bindings = BindingSet::from_registry(&registry);
        assert!(
            !bindings
                .mutations
                .first()
                .expect("expected mutation binding")
                .is_custom_args
        );
    }

    #[test]
    fn deterministic_sort_order() {
        let registry = SchemaRegistry::new();
        registry.register_function(FunctionDef::query("z_last", RustType::String));
        registry.register_function(FunctionDef::query("a_first", RustType::String));
        registry.register_function(FunctionDef::query("m_middle", RustType::String));

        let bindings = BindingSet::from_registry(&registry);
        let names: Vec<&str> = bindings.queries.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, vec!["a_first", "m_middle", "z_last"]);
    }
}
