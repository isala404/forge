//! SQL string extraction and table dependency parsing.

use std::collections::HashSet;

use sqlparser::ast::{
    BinaryOperator, Expr, Query, Select, SelectItem, SetExpr, Statement, TableFactor,
    TableWithJoins,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use syn::visit::Visit;
use syn::{Expr as SynExpr, ExprCall, ExprLit, ExprMacro, ExprMethodCall};

/// Detects `.pool()` calls in a handler body, signalling DB work delegated
/// to a helper function whose SQL is invisible to `SqlStringExtractor`.
pub struct DbDelegationDetector {
    pub found: bool,
}

impl DbDelegationDetector {
    pub fn new() -> Self {
        Self { found: false }
    }
}

impl<'ast> Visit<'ast> for DbDelegationDetector {
    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        if node.method == "pool" {
            self.found = true;
        }
        syn::visit::visit_expr_method_call(self, node);
    }
}

/// Visitor that extracts SQL string literals from function bodies.
pub struct SqlStringExtractor {
    pub sql_strings: Vec<String>,
}

impl SqlStringExtractor {
    pub fn new() -> Self {
        Self {
            sql_strings: Vec::new(),
        }
    }

    /// Check if a string looks like SQL by requiring: minimum length, starts
    /// with a SQL keyword (after whitespace), and contains a matching pair.
    fn looks_like_sql(s: &str) -> bool {
        if s.len() < 10 {
            return false;
        }

        let trimmed = s.trim_start();
        if trimmed.starts_with("http://")
            || trimmed.starts_with("https://")
            || trimmed.starts_with("import ")
            || trimmed.starts_with("export ")
        {
            return false;
        }

        let upper = trimmed.to_uppercase();

        let starts_with_keyword = upper.starts_with("SELECT")
            || upper.starts_with("INSERT")
            || upper.starts_with("UPDATE")
            || upper.starts_with("DELETE")
            || upper.starts_with("WITH");
        if !starts_with_keyword {
            return false;
        }

        (upper.contains("SELECT") && upper.contains("FROM"))
            || (upper.contains("INSERT") && upper.contains("INTO"))
            || (upper.contains("UPDATE") && upper.contains("SET"))
            || (upper.contains("DELETE") && upper.contains("FROM"))
            || (upper.starts_with("WITH") && upper.contains("SELECT"))
    }

    fn extract_sql_from_tokens(&mut self, tokens: &proc_macro2::TokenStream) {
        for token in tokens.clone() {
            match token {
                proc_macro2::TokenTree::Literal(lit) => {
                    let lit_str = lit.to_string();
                    if let Some(sql) = Self::extract_string_content(&lit_str)
                        && Self::looks_like_sql(&sql)
                    {
                        self.sql_strings.push(sql);
                    }
                }
                proc_macro2::TokenTree::Group(group) => {
                    self.extract_sql_from_tokens(&group.stream());
                }
                _ => {}
            }
        }
    }

    /// Extract the actual string content from a literal representation.
    /// Delegates parsing and unescaping to syn so raw, byte, and escaped
    /// forms all decode through the same canonical path.
    fn extract_string_content(lit: &str) -> Option<String> {
        syn::parse_str::<syn::LitStr>(lit.trim())
            .ok()
            .map(|s| s.value())
    }
}

impl<'ast> Visit<'ast> for SqlStringExtractor {
    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        let method_name = node.method.to_string();

        if matches!(
            method_name.as_str(),
            "query"
                | "query_as"
                | "query_scalar"
                | "query_as_unchecked"
                | "query_scalar_unchecked"
                | "query_with"
                | "raw_sql"
        ) && let Some(first_arg) = node.args.first()
        {
            self.visit_expr(first_arg);
        }

        syn::visit::visit_expr_method_call(self, node);
    }

    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let SynExpr::Path(path) = &*node.func {
            let path_str = path
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect::<Vec<_>>()
                .join("::");

            if (path_str.contains("query")
                || path_str.ends_with("query_as")
                || path_str.ends_with("raw_sql"))
                && let Some(first_arg) = node.args.first()
            {
                self.visit_expr(first_arg);
            }
        }

        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_lit(&mut self, _node: &'ast ExprLit) {
        // Intentionally a no-op. SQL extraction is anchored to known call-site
        // contexts (sqlx method calls, sqlx macros, sqlx function calls) rather
        // than scanning all string literals with a heuristic. This prevents
        // false positives from log messages, doc comments, and other non-SQL
        // strings that happen to look like SQL.
    }

    fn visit_expr_macro(&mut self, node: &'ast ExprMacro) {
        let macro_name = node
            .mac
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default();

        if matches!(
            macro_name.as_str(),
            "query" | "query_as" | "query_scalar" | "query_as_unchecked" | "query_scalar_unchecked"
        ) {
            self.extract_sql_from_tokens(&node.mac.tokens);
        }

        syn::visit::visit_expr_macro(self, node);
    }
}

/// Parse SQL strings and extract all selected column names.
/// Returns bare column names (without table qualifiers).
pub fn extract_columns_from_sql(sql_strings: &[String]) -> HashSet<String> {
    let mut columns = HashSet::new();
    let dialect = PostgreSqlDialect {};

    for sql in sql_strings {
        if let Ok(statements) = Parser::parse_sql(&dialect, sql) {
            for stmt in &statements {
                if let Statement::Query(query) = stmt {
                    extract_columns_from_query(query, &mut columns);
                }
            }
        }
    }

    columns
}

/// Extract column names from a query's SELECT list.
fn extract_columns_from_query(query: &Query, columns: &mut HashSet<String>) {
    extract_columns_from_set_expr(&query.body, columns);
}

fn extract_columns_from_set_expr(set_expr: &SetExpr, columns: &mut HashSet<String>) {
    match set_expr {
        SetExpr::Select(select) => {
            for item in &select.projection {
                match item {
                    SelectItem::UnnamedExpr(Expr::Identifier(ident)) => {
                        columns.insert(ident.value.clone());
                    }
                    SelectItem::UnnamedExpr(Expr::CompoundIdentifier(parts)) => {
                        // table.column -> take the last part
                        if let Some(last) = parts.last() {
                            columns.insert(last.value.clone());
                        }
                    }
                    SelectItem::ExprWithAlias { alias, .. } => {
                        columns.insert(alias.value.clone());
                    }
                    SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => {
                        // SELECT * means all columns, can't narrow down
                        columns.clear();
                        return;
                    }
                    _ => {}
                }
            }
        }
        SetExpr::SetOperation { left, right, .. } => {
            extract_columns_from_set_expr(left, columns);
            extract_columns_from_set_expr(right, columns);
        }
        SetExpr::Query(query) => {
            extract_columns_from_query(query, columns);
        }
        _ => {}
    }
}

/// Parse SQL strings and extract columns written by INSERT/UPDATE statements.
///
/// Used by the mutation macro to populate [`FunctionInfo::changed_columns`],
/// which lets the cache invalidator skip queries whose `selected_columns`
/// don't overlap with what the mutation actually changed.
///
/// Returns an empty set if any statement uses dynamic column lists or a form
/// the extractor can't reason about — empty signals "could touch anything",
/// which the invalidator treats as a conservative full invalidation.
pub fn extract_changed_columns_from_sql(sql_strings: &[String]) -> HashSet<String> {
    let mut columns = HashSet::new();
    let dialect = PostgreSqlDialect {};

    for sql in sql_strings {
        match Parser::parse_sql(&dialect, sql) {
            Ok(statements) => {
                for stmt in statements {
                    if !extract_changed_columns_from_statement(&stmt, &mut columns) {
                        // Statement touches unknown columns; widen to "any".
                        columns.clear();
                        return columns;
                    }
                }
            }
            Err(_) => {
                columns.clear();
                return columns;
            }
        }
    }

    columns
}

/// Walk a single statement, recording any column it writes. Returns `false`
/// when the statement might mutate columns the extractor can't enumerate
/// (e.g. UPDATE without a SET list, DELETE which touches every column).
fn extract_changed_columns_from_statement(stmt: &Statement, columns: &mut HashSet<String>) -> bool {
    match stmt {
        Statement::Insert(insert) => {
            if insert.columns.is_empty() {
                // No column list means positional insertion; we can't know which
                // columns are written without schema info.
                return false;
            }
            for col in &insert.columns {
                columns.insert(col.value.clone());
            }
            true
        }
        Statement::Update { assignments, .. } => {
            if assignments.is_empty() {
                return false;
            }
            for assignment in assignments {
                match &assignment.target {
                    sqlparser::ast::AssignmentTarget::ColumnName(name) => {
                        if let Some(last) = name.0.last() {
                            columns.insert(last.value.clone());
                        }
                    }
                    sqlparser::ast::AssignmentTarget::Tuple(tuples) => {
                        for name in tuples {
                            if let Some(last) = name.0.last() {
                                columns.insert(last.value.clone());
                            }
                        }
                    }
                }
            }
            true
        }
        Statement::Delete(_) => {
            // DELETE invalidates every column; widen to full invalidation.
            false
        }
        Statement::Query(_) => true,
        _ => true,
    }
}

/// Result of SQL table extraction.
pub enum TableExtractionResult {
    /// All SQL strings parsed successfully.
    Ok(HashSet<String>),
    /// At least one SQL string failed to parse; contains the unparseable SQL.
    ParseFailed(String),
}

pub fn extract_tables_from_sql(sql_strings: &[String]) -> TableExtractionResult {
    let mut tables = HashSet::new();
    let dialect = PostgreSqlDialect {};

    for sql in sql_strings {
        match Parser::parse_sql(&dialect, sql) {
            Ok(statements) => {
                for stmt in statements {
                    extract_tables_from_statement(&stmt, &mut tables);
                }
            }
            Err(_) => {
                return TableExtractionResult::ParseFailed(sql.clone());
            }
        }
    }

    TableExtractionResult::Ok(tables)
}

fn extract_tables_from_statement(stmt: &Statement, tables: &mut HashSet<String>) {
    match stmt {
        Statement::Query(query) => {
            extract_tables_from_query(query, tables);
        }
        Statement::Insert(insert) => {
            let name = normalize_table_name(&insert.table.to_string());
            tables.insert(name);

            if let Some(src) = &insert.source {
                extract_tables_from_query(src, tables);
            }
        }
        Statement::Update {
            table, selection, ..
        } => {
            extract_tables_from_table_with_joins(table, tables);
            if let Some(sel) = selection {
                extract_tables_from_expr(sel, tables);
            }
        }
        Statement::Delete(delete) => {
            extract_tables_from_from_table(&delete.from, tables);

            if let Some(sel) = &delete.selection {
                extract_tables_from_expr(sel, tables);
            }
        }
        _ => {}
    }
}

fn extract_tables_from_from_table(from: &sqlparser::ast::FromTable, tables: &mut HashSet<String>) {
    match from {
        sqlparser::ast::FromTable::WithFromKeyword(table_with_joins_list) => {
            for twj in table_with_joins_list {
                extract_tables_from_table_with_joins(twj, tables);
            }
        }
        sqlparser::ast::FromTable::WithoutKeyword(table_with_joins_list) => {
            for twj in table_with_joins_list {
                extract_tables_from_table_with_joins(twj, tables);
            }
        }
    }
}

fn extract_tables_from_query(query: &Query, tables: &mut HashSet<String>) {
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            extract_tables_from_query(&cte.query, tables);
        }
    }

    extract_tables_from_set_expr(&query.body, tables);
}

fn extract_tables_from_set_expr(set_expr: &SetExpr, tables: &mut HashSet<String>) {
    match set_expr {
        SetExpr::Select(select) => {
            extract_tables_from_select(select, tables);
        }
        SetExpr::Query(query) => {
            extract_tables_from_query(query, tables);
        }
        SetExpr::SetOperation { left, right, .. } => {
            extract_tables_from_set_expr(left, tables);
            extract_tables_from_set_expr(right, tables);
        }
        SetExpr::Values(_) => {}
        SetExpr::Insert(insert_stmt) => {
            extract_tables_from_statement(insert_stmt, tables);
        }
        SetExpr::Table(t) => {
            if let Some(name) = &t.table_name {
                tables.insert(normalize_table_name(name));
            }
        }
        SetExpr::Update(_) => {}
    }
}

fn extract_tables_from_select(select: &Select, tables: &mut HashSet<String>) {
    for table_with_joins in &select.from {
        extract_tables_from_table_with_joins(table_with_joins, tables);
    }

    for item in &select.projection {
        match item {
            SelectItem::ExprWithAlias { expr, .. } => {
                extract_tables_from_expr(expr, tables);
            }
            SelectItem::UnnamedExpr(expr) => {
                extract_tables_from_expr(expr, tables);
            }
            _ => {}
        }
    }

    if let Some(selection) = &select.selection {
        extract_tables_from_expr(selection, tables);
    }

    if let Some(having) = &select.having {
        extract_tables_from_expr(having, tables);
    }
}

fn extract_tables_from_table_with_joins(twj: &TableWithJoins, tables: &mut HashSet<String>) {
    extract_tables_from_table_factor(&twj.relation, tables);

    for join in &twj.joins {
        extract_tables_from_table_factor(&join.relation, tables);
    }
}

fn extract_tables_from_table_factor(factor: &TableFactor, tables: &mut HashSet<String>) {
    match factor {
        TableFactor::Table { name, .. } => {
            let table_name = normalize_table_name(&name.to_string());
            tables.insert(table_name);
        }
        TableFactor::Derived { subquery, .. } => {
            extract_tables_from_query(subquery, tables);
        }
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => {
            extract_tables_from_table_with_joins(table_with_joins, tables);
        }
        _ => {}
    }
}

fn extract_tables_from_expr(expr: &Expr, tables: &mut HashSet<String>) {
    match expr {
        Expr::Subquery(query) => {
            extract_tables_from_query(query, tables);
        }
        Expr::InSubquery { subquery, .. } => {
            extract_tables_from_query(subquery, tables);
        }
        Expr::Exists { subquery, .. } => {
            extract_tables_from_query(subquery, tables);
        }
        Expr::BinaryOp { left, right, .. } => {
            extract_tables_from_expr(left, tables);
            extract_tables_from_expr(right, tables);
        }
        Expr::UnaryOp { expr, .. } => {
            extract_tables_from_expr(expr, tables);
        }
        Expr::Nested(expr) => {
            extract_tables_from_expr(expr, tables);
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            extract_tables_from_expr(expr, tables);
            extract_tables_from_expr(low, tables);
            extract_tables_from_expr(high, tables);
        }
        Expr::Case {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => {
            if let Some(op) = operand {
                extract_tables_from_expr(op, tables);
            }
            for cond in conditions {
                extract_tables_from_expr(cond, tables);
            }
            for res in results {
                extract_tables_from_expr(res, tables);
            }
            if let Some(else_r) = else_result {
                extract_tables_from_expr(else_r, tables);
            }
        }
        Expr::Function(func) => {
            if let sqlparser::ast::FunctionArguments::List(arg_list) = &func.args {
                for arg in &arg_list.args {
                    if let sqlparser::ast::FunctionArg::Unnamed(
                        sqlparser::ast::FunctionArgExpr::Expr(e),
                    ) = arg
                    {
                        extract_tables_from_expr(e, tables);
                    }
                }
            }
        }
        Expr::InList { list, .. } => {
            for e in list {
                extract_tables_from_expr(e, tables);
            }
        }
        Expr::IsFalse(e)
        | Expr::IsNotFalse(e)
        | Expr::IsTrue(e)
        | Expr::IsNotTrue(e)
        | Expr::IsNull(e)
        | Expr::IsNotNull(e)
        | Expr::IsUnknown(e)
        | Expr::IsNotUnknown(e) => {
            extract_tables_from_expr(e, tables);
        }
        _ => {}
    }
}

/// Strips schema qualifiers and outer quotes: `"public.users"` → `"users"`.
fn normalize_table_name(name: &str) -> String {
    let name = name.trim();
    let name = name.trim_matches('"').trim_matches('\'');
    if let Some(pos) = name.rfind('.') {
        name[pos + 1..].trim_matches('"').to_string()
    } else {
        name.to_string()
    }
}

/// Scope columns that identify a row's owning principal.
///
/// Matching is name-based only: any column literally named one of these passes
/// the scope check regardless of whether it's actually a foreign key to the
/// users table. Until RLS enforcement lands, this is a compile-time lint, not
/// a security boundary.
const SCOPE_COLS: &[&str] = &["user_id", "owner_id", "tenant_id"];

/// Check whether the SQL scope depends on `tenant_id` (as opposed to
/// `user_id`/`owner_id`). When true, the runtime must verify the auth
/// context has a tenant claim before dispatching the query.
pub fn sql_scope_requires_tenant(sql_strings: &[String]) -> bool {
    for sql in sql_strings {
        if let Ok(stmts) = Parser::parse_sql(&PostgreSqlDialect {}, sql) {
            for stmt in &stmts {
                if stmt_mentions_tenant(stmt) {
                    return true;
                }
            }
        }
    }
    false
}

fn stmt_mentions_tenant(stmt: &Statement) -> bool {
    match stmt {
        Statement::Query(q) => query_mentions_tenant(q),
        Statement::Update { selection, .. } => selection.as_ref().is_some_and(expr_mentions_tenant),
        Statement::Delete(d) => d.selection.as_ref().is_some_and(expr_mentions_tenant),
        _ => false,
    }
}

fn query_mentions_tenant(q: &Query) -> bool {
    if let Some(with) = &q.with {
        for cte in &with.cte_tables {
            if query_mentions_tenant(&cte.query) {
                return true;
            }
        }
    }
    set_expr_mentions_tenant(&q.body)
}

fn set_expr_mentions_tenant(e: &SetExpr) -> bool {
    match e {
        SetExpr::Select(s) => s.selection.as_ref().is_some_and(expr_mentions_tenant),
        SetExpr::Query(q) => query_mentions_tenant(q),
        SetExpr::SetOperation { left, right, .. } => {
            set_expr_mentions_tenant(left) || set_expr_mentions_tenant(right)
        }
        _ => false,
    }
}

fn expr_mentions_tenant(e: &Expr) -> bool {
    match e {
        Expr::Identifier(ident) => ident.value.eq_ignore_ascii_case("tenant_id"),
        Expr::CompoundIdentifier(parts) => parts
            .last()
            .is_some_and(|p| p.value.eq_ignore_ascii_case("tenant_id")),
        Expr::BinaryOp { left, right, .. } => {
            expr_mentions_tenant(left) || expr_mentions_tenant(right)
        }
        Expr::UnaryOp { expr, .. } | Expr::Nested(expr) | Expr::Cast { expr, .. } => {
            expr_mentions_tenant(expr)
        }
        Expr::InList { expr, list, .. } => {
            expr_mentions_tenant(expr) || list.iter().any(expr_mentions_tenant)
        }
        Expr::InSubquery { expr, .. } => expr_mentions_tenant(expr),
        Expr::Between {
            expr, low, high, ..
        } => expr_mentions_tenant(expr) || expr_mentions_tenant(low) || expr_mentions_tenant(high),
        Expr::IsNull(e) | Expr::IsNotNull(e) => expr_mentions_tenant(e),
        _ => false,
    }
}

/// Result of scope checking.
pub enum ScopeCheckResult {
    /// SQL parsed and scope was found.
    Scoped,
    /// SQL parsed but no scope predicate found.
    Unscoped,
    /// SQL could not be parsed.
    ParseFailed,
}

/// Check whether every data path in the SQL flows through a scope predicate
/// (`user_id`, `owner_id`, or `tenant_id` in a WHERE or JOIN ON clause).
///
/// Walks the full sqlparser AST including CTE bodies and nested subqueries.
/// Each SELECT context must be scoped either directly (own WHERE/JOIN ON) or
/// indirectly (all FROM sources resolve to scoped CTEs or scoped subqueries).
/// Scope propagates forward through CTE definitions so that later CTEs and
/// the main query can inherit scope from earlier CTEs.
///
/// ALL statements across all SQL strings must be scoped for the function to be
/// considered scoped. A single unscoped statement (e.g. one SELECT without a
/// WHERE filter) makes the whole function unscoped.
pub fn sql_references_identity_scope(sql_strings: &[String]) -> ScopeCheckResult {
    let mut found_any_statement = false;

    for sql in sql_strings {
        match Parser::parse_sql(&PostgreSqlDialect {}, sql) {
            Ok(stmts) => {
                for stmt in &stmts {
                    found_any_statement = true;
                    if !stmt_is_scoped(stmt) {
                        return ScopeCheckResult::Unscoped;
                    }
                }
            }
            Err(_) => {
                return ScopeCheckResult::ParseFailed;
            }
        }
    }

    if found_any_statement {
        ScopeCheckResult::Scoped
    } else {
        ScopeCheckResult::Unscoped
    }
}

struct ScopeCtx {
    scoped_ctes: HashSet<String>,
    all_ctes: HashSet<String>,
}

impl ScopeCtx {
    fn new() -> Self {
        Self {
            scoped_ctes: HashSet::new(),
            all_ctes: HashSet::new(),
        }
    }
}

fn stmt_is_scoped(stmt: &Statement) -> bool {
    let mut ctx = ScopeCtx::new();
    match stmt {
        Statement::Query(q) => query_is_scoped(q, &mut ctx),
        Statement::Update { selection, .. } => selection.as_ref().is_some_and(expr_has_scope),
        Statement::Delete(d) => d.selection.as_ref().is_some_and(expr_has_scope),
        _ => false,
    }
}

fn query_is_scoped(q: &Query, ctx: &mut ScopeCtx) -> bool {
    if let Some(with) = &q.with {
        for cte in &with.cte_tables {
            let cte_name = cte.alias.name.value.to_lowercase();
            ctx.all_ctes.insert(cte_name.clone());
            if query_is_scoped(
                &cte.query,
                &mut ScopeCtx {
                    scoped_ctes: ctx.scoped_ctes.clone(),
                    all_ctes: ctx.all_ctes.clone(),
                },
            ) {
                ctx.scoped_ctes.insert(cte_name);
            }
        }
    }
    set_expr_is_scoped(&q.body, ctx)
}

fn set_expr_is_scoped(e: &SetExpr, ctx: &ScopeCtx) -> bool {
    match e {
        SetExpr::Select(s) => select_is_scoped(s, ctx),
        SetExpr::Query(q) => query_is_scoped(
            q,
            &mut ScopeCtx {
                scoped_ctes: ctx.scoped_ctes.clone(),
                all_ctes: ctx.all_ctes.clone(),
            },
        ),
        SetExpr::SetOperation { left, right, .. } => {
            // Both branches must be scoped for a UNION/INTERSECT/EXCEPT
            set_expr_is_scoped(left, ctx) && set_expr_is_scoped(right, ctx)
        }
        SetExpr::Insert(stmt) => stmt_is_scoped(stmt),
        _ => false,
    }
}

/// A SELECT is scoped if its WHERE clause contains a scope predicate,
/// OR if every FROM source is itself scoped (CTE reference to a scoped CTE,
/// or derived subquery that is scoped).
///
/// WHERE scope only applies when FROM sources are plain tables or already-
/// scoped references. An unscoped CTE that reads a real table materializes
/// all rows — the outer WHERE filters the output but the CTE body had
/// unscoped access, so we require the CTE itself to be scoped.
fn select_is_scoped(s: &Select, ctx: &ScopeCtx) -> bool {
    let has_where_scope = s.selection.as_ref().is_some_and(expr_has_scope);
    if has_where_scope && !any_source_is_unscoped_cte(s, ctx) {
        return true;
    }
    if s.from.is_empty() {
        return false;
    }
    s.from.iter().all(|twj| all_sources_in_twj_scoped(twj, ctx))
}

/// Check if any FROM source references a CTE that was NOT determined to be
/// scoped. Plain table names that happen to match are also caught here,
/// but that's safe: if a plain table name collides with a CTE, the CTE
/// takes precedence in SQL semantics.
fn any_source_is_unscoped_cte(s: &Select, ctx: &ScopeCtx) -> bool {
    s.from.iter().any(|twj| {
        source_is_unscoped_cte(&twj.relation, ctx)
            || twj
                .joins
                .iter()
                .any(|j| source_is_unscoped_cte(&j.relation, ctx))
    })
}

/// A source is an unscoped CTE reference if it's a table name that appears
/// in `all_ctes` (a known CTE) but NOT in `scoped_ctes`.
fn source_is_unscoped_cte(factor: &TableFactor, ctx: &ScopeCtx) -> bool {
    if let TableFactor::Table { name, .. } = factor {
        let table_name = normalize_table_name(&name.to_string()).to_lowercase();
        ctx.all_ctes.contains(&table_name) && !ctx.scoped_ctes.contains(&table_name)
    } else {
        false
    }
}

/// Check if every source in a TableWithJoins is scoped through CTE or subquery.
fn all_sources_in_twj_scoped(twj: &TableWithJoins, ctx: &ScopeCtx) -> bool {
    if !source_is_scoped(&twj.relation, ctx) {
        return false;
    }
    twj.joins
        .iter()
        .all(|join| source_is_scoped(&join.relation, ctx))
}

/// A single FROM source is scoped if it's a known-scoped CTE reference, a
/// scoped derived subquery, or a scoped nested join. Plain table references
/// are never inherently scoped (they need external WHERE/JOIN ON).
fn source_is_scoped(factor: &TableFactor, ctx: &ScopeCtx) -> bool {
    match factor {
        TableFactor::Table { name, .. } => {
            let table_name = normalize_table_name(&name.to_string());
            ctx.scoped_ctes.contains(&table_name.to_lowercase())
        }
        TableFactor::Derived { subquery, .. } => query_is_scoped(
            subquery,
            &mut ScopeCtx {
                scoped_ctes: ctx.scoped_ctes.clone(),
                all_ctes: ctx.all_ctes.clone(),
            },
        ),
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => all_sources_in_twj_scoped(table_with_joins, ctx),
        _ => false,
    }
}

fn expr_has_scope(e: &Expr) -> bool {
    match e {
        Expr::Identifier(ident) => is_scope_col(&ident.value),
        Expr::CompoundIdentifier(parts) => parts.last().is_some_and(|p| is_scope_col(&p.value)),
        // Handle PostgreSQL JSON operators: claims->>'user_id' parses as
        // BinaryOp { left, op: Arrow/LongArrow, right: Value('user_id') }.
        // Check if the right-hand side of a JSON operator is a scope column name.
        Expr::BinaryOp { left, op, right } => {
            if matches!(
                op,
                BinaryOperator::Arrow
                    | BinaryOperator::LongArrow
                    | BinaryOperator::HashArrow
                    | BinaryOperator::HashLongArrow
            ) {
                expr_has_scope(left) || value_is_scope_col(right)
            } else if matches!(op, BinaryOperator::Eq | BinaryOperator::NotEq)
                && (is_direct_scope_ref(left) && is_literal_value(right)
                    || is_direct_scope_ref(right) && is_literal_value(left))
            {
                false
            } else {
                expr_has_scope(left) || expr_has_scope(right)
            }
        }
        Expr::UnaryOp { expr, .. } | Expr::Nested(expr) => expr_has_scope(expr),
        Expr::Between {
            expr, low, high, ..
        } => expr_has_scope(expr) || expr_has_scope(low) || expr_has_scope(high),
        Expr::IsNull(e)
        | Expr::IsNotNull(e)
        | Expr::IsTrue(e)
        | Expr::IsNotTrue(e)
        | Expr::IsFalse(e)
        | Expr::IsNotFalse(e) => expr_has_scope(e),
        Expr::InList { expr, list, .. } => expr_has_scope(expr) || list.iter().any(expr_has_scope),
        Expr::InSubquery { expr, subquery, .. } => {
            let sub_scoped = query_is_scoped(subquery, &mut ScopeCtx::new());
            if is_direct_scope_ref(expr) {
                sub_scoped
            } else {
                expr_has_scope(expr) || sub_scoped
            }
        }
        Expr::Subquery(q) | Expr::Exists { subquery: q, .. } => {
            query_is_scoped(q, &mut ScopeCtx::new())
        }
        // Handle (claims->>'user_id')::uuid = $1 patterns:
        // Cast wraps json access; walk into the inner expression.
        Expr::Cast { expr, .. } => expr_has_scope(expr),
        // Snowflake/Databricks-style JSON path access (obj:foo.bar).
        // Check if any path element names a scope column.
        Expr::JsonAccess { value, path } => expr_has_scope(value) || json_path_has_scope(path),
        _ => false,
    }
}

/// True if the expression resolves directly to a scope column (not merely contains one).
fn is_direct_scope_ref(e: &Expr) -> bool {
    match e {
        Expr::Identifier(ident) => is_scope_col(&ident.value),
        Expr::CompoundIdentifier(parts) => parts.last().is_some_and(|p| is_scope_col(&p.value)),
        Expr::Cast { expr, .. } | Expr::Nested(expr) => is_direct_scope_ref(expr),
        Expr::BinaryOp { left, op, right } => {
            matches!(
                op,
                BinaryOperator::Arrow
                    | BinaryOperator::LongArrow
                    | BinaryOperator::HashArrow
                    | BinaryOperator::HashLongArrow
            ) && (is_direct_scope_ref(left) || value_is_scope_col(right))
        }
        _ => false,
    }
}

/// True if the expression is a non-placeholder literal. Placeholders ($1, $2) are
/// acceptable scope column counterparts; hardcoded literals are not.
fn is_literal_value(e: &Expr) -> bool {
    match e {
        Expr::Value(v) => !matches!(v, sqlparser::ast::Value::Placeholder(_)),
        Expr::Cast { expr, .. } | Expr::Nested(expr) => is_literal_value(expr),
        _ => false,
    }
}

/// True if the expression is a string literal whose value names a scope column.
/// Used for the RHS of JSON arrow operators like `->>'user_id'`.
fn value_is_scope_col(e: &Expr) -> bool {
    match e {
        Expr::Value(sqlparser::ast::Value::SingleQuotedString(s)) => is_scope_col(s),
        Expr::Value(sqlparser::ast::Value::DoubleQuotedString(s)) => is_scope_col(s),
        _ => false,
    }
}

fn json_path_has_scope(path: &sqlparser::ast::JsonPath) -> bool {
    path.path.iter().any(|elem| match elem {
        sqlparser::ast::JsonPathElem::Dot { key, .. } => is_scope_col(key),
        sqlparser::ast::JsonPathElem::Bracket { key } => value_is_scope_col(key),
    })
}

fn is_scope_col(name: &str) -> bool {
    SCOPE_COLS.iter().any(|&c| name.eq_ignore_ascii_case(c))
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;

    fn unwrap_tables(result: TableExtractionResult) -> HashSet<String> {
        let TableExtractionResult::Ok(tables) = result else {
            panic!("expected successful extraction");
        };
        tables
    }

    #[test]
    fn test_simple_select() {
        let tables = unwrap_tables(extract_tables_from_sql(
            &["SELECT * FROM users".to_string()],
        ));
        assert!(tables.contains("users"));
    }

    #[test]
    fn test_join() {
        let tables = unwrap_tables(extract_tables_from_sql(&[
            "SELECT u.*, p.name FROM users u JOIN projects p ON u.id = p.user_id".to_string(),
        ]));
        assert!(tables.contains("users"));
        assert!(tables.contains("projects"));
    }

    #[test]
    fn test_left_join() {
        let tables = unwrap_tables(extract_tables_from_sql(&[
            "SELECT * FROM users u LEFT JOIN orders o ON u.id = o.user_id".to_string(),
        ]));
        assert!(tables.contains("users"));
        assert!(tables.contains("orders"));
    }

    #[test]
    fn test_schema_qualified() {
        let tables = unwrap_tables(extract_tables_from_sql(&[
            "SELECT * FROM public.users".to_string()
        ]));
        assert!(tables.contains("users"));
    }

    #[test]
    fn test_subquery_in_where() {
        let tables = unwrap_tables(extract_tables_from_sql(&[
            "SELECT * FROM users WHERE id IN (SELECT user_id FROM orders)".to_string(),
        ]));
        assert!(tables.contains("users"));
        assert!(tables.contains("orders"));
    }

    #[test]
    fn test_exists_subquery() {
        let tables = unwrap_tables(extract_tables_from_sql(&[
            "SELECT * FROM users u WHERE EXISTS(SELECT 1 FROM orders o WHERE o.user_id = u.id)"
                .to_string(),
        ]));
        assert!(tables.contains("users"));
        assert!(tables.contains("orders"));
    }

    #[test]
    fn test_cte() {
        let tables = unwrap_tables(extract_tables_from_sql(&[
            "WITH active AS (SELECT * FROM users WHERE active = true) SELECT * FROM active JOIN projects ON active.id = projects.user_id".to_string()
        ]));
        assert!(tables.contains("users"));
        assert!(tables.contains("projects"));
    }

    #[test]
    fn test_multiple_joins() {
        let tables = unwrap_tables(extract_tables_from_sql(&[
            "SELECT * FROM users u INNER JOIN projects p ON u.id = p.user_id LEFT JOIN tasks t ON p.id = t.project_id".to_string()
        ]));
        assert!(tables.contains("users"));
        assert!(tables.contains("projects"));
        assert!(tables.contains("tasks"));
    }

    #[test]
    fn test_insert() {
        let tables = unwrap_tables(extract_tables_from_sql(&[
            "INSERT INTO users (name) VALUES ('test')".to_string(),
        ]));
        assert!(tables.contains("users"));
    }

    #[test]
    fn test_insert_select() {
        let tables = unwrap_tables(extract_tables_from_sql(&[
            "INSERT INTO audit_log (user_id) SELECT id FROM users".to_string(),
        ]));
        assert!(tables.contains("audit_log"));
        assert!(tables.contains("users"));
    }

    #[test]
    fn test_update() {
        let tables = unwrap_tables(extract_tables_from_sql(&[
            "UPDATE users SET name = 'test' WHERE id = 1".to_string(),
        ]));
        assert!(tables.contains("users"));
    }

    #[test]
    fn test_delete() {
        let tables = unwrap_tables(extract_tables_from_sql(&[
            "DELETE FROM users WHERE id = 1".to_string()
        ]));
        assert!(tables.contains("users"));
    }

    #[test]
    fn test_union() {
        let tables = unwrap_tables(extract_tables_from_sql(&[
            "SELECT id FROM users UNION SELECT id FROM admins".to_string(),
        ]));
        assert!(tables.contains("users"));
        assert!(tables.contains("admins"));
    }

    #[test]
    fn test_subquery_in_from() {
        let tables = unwrap_tables(extract_tables_from_sql(&[
            "SELECT * FROM (SELECT * FROM users WHERE active = true) AS active_users".to_string(),
        ]));
        assert!(tables.contains("users"));
    }

    #[test]
    fn test_normalize_quoted() {
        assert_eq!(normalize_table_name("\"Users\""), "Users");
        assert_eq!(normalize_table_name("'users'"), "users");
    }

    #[test]
    fn test_normalize_schema() {
        assert_eq!(normalize_table_name("public.users"), "users");
        assert_eq!(normalize_table_name("schema.\"Table\""), "Table");
    }

    #[test]
    fn test_multiple_sql_strings() {
        let tables = unwrap_tables(extract_tables_from_sql(&[
            "SELECT * FROM users".to_string(),
            "SELECT * FROM projects".to_string(),
        ]));
        assert!(tables.contains("users"));
        assert!(tables.contains("projects"));
    }

    #[test]
    fn test_sql_with_placeholders() {
        let tables = unwrap_tables(extract_tables_from_sql(&[
            "SELECT * FROM users WHERE id = $1 AND name = $2".to_string(),
        ]));
        assert!(tables.contains("users"));
    }

    #[test]
    fn test_complex_query_with_placeholders() {
        let tables = unwrap_tables(extract_tables_from_sql(&[
            "SELECT r.*, s.current_streak FROM rituals r LEFT JOIN streaks s ON r.id = s.ritual_id WHERE r.user_id = $1".to_string()
        ]));
        assert!(tables.contains("rituals"));
        assert!(tables.contains("streaks"));
    }

    #[test]
    fn test_extract_string_content_regular() {
        assert_eq!(
            SqlStringExtractor::extract_string_content(r#""SELECT * FROM users""#),
            Some("SELECT * FROM users".to_string())
        );
    }

    #[test]
    fn test_extract_string_content_raw() {
        assert_eq!(
            SqlStringExtractor::extract_string_content(r###"r#"SELECT * FROM users"#"###),
            Some("SELECT * FROM users".to_string())
        );
    }

    #[test]
    fn test_scope_check_where_user_id() {
        assert!(matches!(
            sql_references_identity_scope(&["SELECT * FROM tasks WHERE user_id = $1".to_string()]),
            ScopeCheckResult::Scoped
        ));
    }

    #[test]
    fn test_scope_check_and_user_id() {
        assert!(matches!(
            sql_references_identity_scope(&[
                "SELECT * FROM tasks WHERE id = $1 AND user_id = $2".to_string()
            ]),
            ScopeCheckResult::Scoped
        ));
    }

    #[test]
    fn test_scope_check_owner_id() {
        assert!(matches!(
            sql_references_identity_scope(&["DELETE FROM posts WHERE owner_id = $1".to_string()]),
            ScopeCheckResult::Scoped
        ));
    }

    #[test]
    fn test_scope_check_join_on_without_where_is_unscoped() {
        assert!(matches!(
            sql_references_identity_scope(&[
                "SELECT t.* FROM tasks t JOIN users u ON t.user_id = u.id".to_string()
            ]),
            ScopeCheckResult::Unscoped
        ));
    }

    #[test]
    fn test_scope_check_select_only_no_where() {
        assert!(matches!(
            sql_references_identity_scope(&["SELECT user_id, name FROM tasks".to_string()]),
            ScopeCheckResult::Unscoped
        ));
    }

    #[test]
    fn test_scope_check_no_scope_column() {
        assert!(matches!(
            sql_references_identity_scope(&["SELECT * FROM tasks WHERE id = $1".to_string()]),
            ScopeCheckResult::Unscoped
        ));
    }

    #[test]
    fn test_scope_check_empty() {
        assert!(matches!(
            sql_references_identity_scope(&[]),
            ScopeCheckResult::Unscoped
        ));
    }

    #[test]
    fn test_scope_check_multiple_sql_one_unscoped_fails() {
        assert!(matches!(
            sql_references_identity_scope(&[
                "SELECT count(*) FROM tasks".to_string(),
                "SELECT * FROM tasks WHERE user_id = $1".to_string(),
            ]),
            ScopeCheckResult::Unscoped
        ));
    }

    #[test]
    fn test_scope_check_multiple_sql_all_scoped() {
        assert!(matches!(
            sql_references_identity_scope(&[
                "SELECT * FROM tasks WHERE user_id = $1".to_string(),
                "SELECT * FROM orders WHERE owner_id = $2".to_string(),
            ]),
            ScopeCheckResult::Scoped
        ));
    }

    #[test]
    fn scope_check_tenant_id() {
        assert!(matches!(
            sql_references_identity_scope(
                &["SELECT * FROM tasks WHERE tenant_id = $1".to_string()]
            ),
            ScopeCheckResult::Scoped
        ));
    }

    #[test]
    fn scope_check_cte_body_scoped() {
        assert!(matches!(
            sql_references_identity_scope(&[
                "WITH t AS (SELECT * FROM tasks WHERE user_id = $1) SELECT * FROM t".to_string()
            ]),
            ScopeCheckResult::Scoped
        ));
    }

    #[test]
    fn scope_check_subquery_in_from_scoped() {
        assert!(matches!(
            sql_references_identity_scope(&[
                "SELECT * FROM (SELECT * FROM tasks WHERE owner_id = $1) sub".to_string()
            ]),
            ScopeCheckResult::Scoped
        ));
    }

    #[test]
    fn scope_check_cte_body_unscoped_outer_where_rejected() {
        assert!(matches!(
            sql_references_identity_scope(&[
                "WITH all_t AS (SELECT * FROM tasks) SELECT * FROM all_t WHERE user_id = $1"
                    .to_string()
            ]),
            ScopeCheckResult::Unscoped
        ));
    }

    #[test]
    fn scope_check_cte_body_scoped_outer_where_passes() {
        assert!(matches!(
            sql_references_identity_scope(&[
                "WITH scoped_t AS (SELECT * FROM tasks WHERE user_id = $1) \
                 SELECT * FROM scoped_t WHERE status = 'active'"
                    .to_string()
            ]),
            ScopeCheckResult::Scoped
        ));
    }

    #[test]
    fn scope_check_no_scope_anywhere_fails() {
        assert!(matches!(
            sql_references_identity_scope(&[
                "WITH all_t AS (SELECT * FROM tasks) SELECT * FROM all_t".to_string()
            ]),
            ScopeCheckResult::Unscoped
        ));
    }

    #[test]
    fn scope_check_bare_cte_without_scope_fails() {
        // CTE body reads a real table without scope, outer query doesn't scope either.
        assert!(matches!(
            sql_references_identity_scope(&[
                "WITH leaked AS (SELECT * FROM secrets) SELECT * FROM leaked".to_string()
            ]),
            ScopeCheckResult::Unscoped
        ));
    }

    #[test]
    fn scope_check_nested_subquery_without_scope_fails() {
        // Derived subquery reads a real table without scope.
        assert!(matches!(
            sql_references_identity_scope(&[
                "SELECT * FROM (SELECT * FROM secrets) sub".to_string()
            ]),
            ScopeCheckResult::Unscoped
        ));
    }

    #[test]
    fn scope_check_cte_scoped_propagates_to_later_cte() {
        // First CTE is scoped; second CTE reads from the first (inherits scope);
        // outer query reads from the second. Should pass.
        assert!(matches!(
            sql_references_identity_scope(&[
                "WITH scoped AS (SELECT * FROM tasks WHERE user_id = $1), \
                 derived AS (SELECT * FROM scoped) \
                 SELECT * FROM derived"
                    .to_string()
            ]),
            ScopeCheckResult::Scoped
        ));
    }

    #[test]
    fn scope_check_mixed_cte_one_unscoped_with_real_table_fails() {
        // First CTE is scoped, second reads an unscoped real table.
        // Outer query reads from both, but joins don't carry scope.
        // The outer query has no WHERE/JOIN ON with scope, and `leaked`
        // is not a scoped source, so it should fail.
        assert!(matches!(
            sql_references_identity_scope(&[
                "WITH scoped AS (SELECT * FROM tasks WHERE user_id = $1), \
                 leaked AS (SELECT * FROM secrets) \
                 SELECT * FROM scoped JOIN leaked ON scoped.id = leaked.task_id"
                    .to_string()
            ]),
            ScopeCheckResult::Unscoped
        ));
    }

    #[test]
    fn scope_check_subquery_in_where_scoped() {
        // Main query has WHERE with IN-subquery that is scoped. The outer
        // WHERE references the subquery result which carries scope.
        assert!(matches!(
            sql_references_identity_scope(&[
                "SELECT * FROM tasks WHERE user_id IN (SELECT user_id FROM team_members WHERE tenant_id = $1)"
                    .to_string()
            ]),
            ScopeCheckResult::Scoped
        ));
    }

    #[test]
    fn scope_check_in_unscoped_subquery_rejected() {
        assert!(matches!(
            sql_references_identity_scope(&[
                "SELECT * FROM tasks WHERE user_id IN (SELECT user_id FROM other_users)"
                    .to_string()
            ]),
            ScopeCheckResult::Unscoped
        ));
    }

    #[test]
    fn scope_check_in_scoped_subquery_non_scope_lhs_passes() {
        assert!(matches!(
            sql_references_identity_scope(&[
                "SELECT * FROM tasks WHERE id IN (SELECT task_id FROM assignments WHERE user_id = $1)"
                    .to_string()
            ]),
            ScopeCheckResult::Scoped
        ));
    }

    #[test]
    fn scope_check_exists_subquery_scoped() {
        assert!(matches!(
            sql_references_identity_scope(&[
                "SELECT * FROM tasks t WHERE EXISTS (SELECT 1 FROM users u WHERE u.id = t.user_id AND u.tenant_id = $1)"
                    .to_string()
            ]),
            ScopeCheckResult::Scoped
        ));
    }

    #[test]
    fn scope_check_union_both_scoped_passes() {
        assert!(matches!(
            sql_references_identity_scope(&[
                "SELECT * FROM tasks WHERE user_id = $1 UNION ALL SELECT * FROM archived_tasks WHERE user_id = $1"
                    .to_string()
            ]),
            ScopeCheckResult::Scoped
        ));
    }

    #[test]
    fn scope_check_union_one_unscoped_fails() {
        // One branch of a UNION is scoped, the other is not.
        assert!(matches!(
            sql_references_identity_scope(&[
                "SELECT * FROM tasks WHERE user_id = $1 UNION ALL SELECT * FROM public_notices"
                    .to_string()
            ]),
            ScopeCheckResult::Unscoped
        ));
    }

    #[test]
    fn scope_check_join_on_scope_col_without_where_is_unscoped() {
        assert!(matches!(
            sql_references_identity_scope(&[
                "SELECT t.*, p.name FROM tasks t INNER JOIN projects p ON t.user_id = p.owner_id"
                    .to_string()
            ]),
            ScopeCheckResult::Unscoped
        ));
    }

    #[test]
    fn scope_check_join_with_where_scope_passes() {
        assert!(matches!(
            sql_references_identity_scope(&[
                "SELECT t.*, p.name FROM tasks t JOIN projects p ON t.project_id = p.id WHERE t.user_id = $1"
                    .to_string()
            ]),
            ScopeCheckResult::Scoped
        ));
    }

    #[test]
    fn scope_check_join_on_scope_leaks_other_table() {
        assert!(matches!(
            sql_references_identity_scope(&[
                "SELECT s.* FROM secrets s JOIN users u ON u.user_id = $1".to_string()
            ]),
            ScopeCheckResult::Unscoped
        ));
    }

    #[test]
    fn scope_check_deeply_nested_subquery_scoped() {
        assert!(matches!(
            sql_references_identity_scope(&[
                "SELECT * FROM (SELECT * FROM (SELECT * FROM tasks WHERE owner_id = $1) a) b"
                    .to_string()
            ]),
            ScopeCheckResult::Scoped
        ));
    }

    #[test]
    fn scope_check_deeply_nested_subquery_unscoped_fails() {
        assert!(matches!(
            sql_references_identity_scope(&[
                "SELECT * FROM (SELECT * FROM (SELECT * FROM tasks) a) b".to_string()
            ]),
            ScopeCheckResult::Unscoped
        ));
    }

    #[test]
    fn scope_check_rejects_literal_uuid_binding() {
        assert!(matches!(
            sql_references_identity_scope(&[
                "SELECT * FROM tasks WHERE owner_id = '00000000-0000-0000-0000-000000000000'"
                    .to_string()
            ]),
            ScopeCheckResult::Unscoped
        ));
    }

    #[test]
    fn scope_check_rejects_literal_integer_binding() {
        assert!(matches!(
            sql_references_identity_scope(&["SELECT * FROM tasks WHERE user_id = 1".to_string()]),
            ScopeCheckResult::Unscoped
        ));
    }

    #[test]
    fn scope_check_rejects_literal_with_cast() {
        assert!(matches!(
            sql_references_identity_scope(&[
                "SELECT * FROM tasks WHERE user_id = '00000000-0000-0000-0000-000000000000'::uuid"
                    .to_string()
            ]),
            ScopeCheckResult::Unscoped
        ));
    }

    #[test]
    fn scope_check_accepts_placeholder_binding() {
        assert!(matches!(
            sql_references_identity_scope(&["SELECT * FROM tasks WHERE user_id = $1".to_string()]),
            ScopeCheckResult::Scoped
        ));
    }

    #[test]
    fn scope_check_accepts_cast_placeholder_binding() {
        assert!(matches!(
            sql_references_identity_scope(&[
                "SELECT * FROM tasks WHERE user_id = $1::uuid".to_string()
            ]),
            ScopeCheckResult::Scoped
        ));
    }

    #[test]
    fn test_stillpoint_query() {
        let sql = r#"
        SELECT
            r.id,
            r.user_id,
            r.emoji,
            r.title,
            r.description,
            r.sort_order,
            r.is_active,
            r.created_at,
            r.updated_at,
            COALESCE(s.current_streak, 0) as "current_streak!",
            COALESCE(s.longest_streak, 0) as "longest_streak!",
            COALESCE(s.streak_status, 'none') as "streak_status!",
            COALESCE(s.status_emoji, '') as "status_emoji!",
            s.last_completed_at,
            EXISTS(
                SELECT 1 FROM completions c
                WHERE c.ritual_id = r.id AND c.completed_date = $2
            ) as "completed_today!"
        FROM rituals r
        LEFT JOIN streaks s ON s.ritual_id = r.id
        WHERE r.user_id = $1 AND r.is_active = true
        ORDER BY r.sort_order ASC, r.created_at ASC
        "#;
        let tables = unwrap_tables(extract_tables_from_sql(&[sql.to_string()]));
        assert!(tables.contains("rituals"), "Should contain rituals");
        assert!(tables.contains("streaks"), "Should contain streaks");
        assert!(tables.contains("completions"), "Should contain completions");
    }
}
