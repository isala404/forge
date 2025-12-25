use proc_macro::TokenStream;

mod action;
mod enum_type;
mod model;
mod mutation;
mod query;

/// Marks a struct as a FORGE model, generating schema metadata and SQL.
///
/// # Example
/// ```ignore
/// #[forge::model]
/// #[table(name = "users")]
/// pub struct User {
///     #[id]
///     pub id: Uuid,
///
///     #[indexed]
///     #[unique]
///     pub email: String,
///
///     pub name: String,
/// }
/// ```
#[proc_macro_attribute]
pub fn model(attr: TokenStream, item: TokenStream) -> TokenStream {
    model::expand_model(attr, item)
}

/// Marks an enum for database storage as a PostgreSQL ENUM type.
///
/// # Example
/// ```ignore
/// #[forge::forge_enum]
/// pub enum ProjectStatus {
///     Draft,
///     Active,
///     Paused,
///     Completed,
/// }
/// ```
#[proc_macro_attribute]
pub fn forge_enum(attr: TokenStream, item: TokenStream) -> TokenStream {
    enum_type::expand_enum(attr, item)
}

/// Marks a function as a query (read-only, cacheable, subscribable).
///
/// Queries can only read from the database and are automatically cached.
/// They can be subscribed to for real-time updates.
///
/// # Attributes
/// - `cache = "5m"` - Cache TTL (duration like "30s", "5m", "1h")
/// - `public` - No authentication required
/// - `require_auth` - Require authentication
/// - `timeout = 30` - Timeout in seconds
///
/// # Example
/// ```ignore
/// #[forge::query]
/// pub async fn get_user(ctx: &QueryContext, user_id: Uuid) -> Result<User> {
///     ctx.db().query::<User>().filter(|u| u.id == user_id).fetch_one().await
/// }
///
/// #[forge::query(cache = "5m", require_auth)]
/// pub async fn get_profile(ctx: &QueryContext) -> Result<Profile> {
///     let user_id = ctx.require_user_id()?;
///     // ...
/// }
/// ```
#[proc_macro_attribute]
pub fn query(attr: TokenStream, item: TokenStream) -> TokenStream {
    query::expand_query(attr, item)
}

/// Marks a function as a mutation (transactional write).
///
/// Mutations run within a database transaction and can read and write data.
/// All changes either commit together or roll back on error.
///
/// # Attributes
/// - `require_auth` - Require authentication
/// - `require_role("admin")` - Require specific role
/// - `timeout = 30` - Timeout in seconds
///
/// # Example
/// ```ignore
/// #[forge::mutation]
/// pub async fn create_project(
///     ctx: &MutationContext,
///     input: CreateProjectInput,
/// ) -> Result<Project> {
///     let user_id = ctx.require_user_id()?;
///     // All operations in a transaction
///     let project = ctx.db().insert(Project { ... }).await?;
///     Ok(project)
/// }
/// ```
#[proc_macro_attribute]
pub fn mutation(attr: TokenStream, item: TokenStream) -> TokenStream {
    mutation::expand_mutation(attr, item)
}

/// Marks a function as an action (side effects, external APIs).
///
/// Actions can call external APIs and perform side effects.
/// They are NOT transactional by default but can call queries and mutations.
///
/// # Attributes
/// - `require_auth` - Require authentication
/// - `require_role("admin")` - Require specific role
/// - `timeout = 60` - Timeout in seconds
///
/// # Example
/// ```ignore
/// #[forge::action(timeout = 60)]
/// pub async fn sync_with_stripe(
///     ctx: &ActionContext,
///     user_id: Uuid,
/// ) -> Result<SyncResult> {
///     // Can call external APIs
///     let customer = stripe::Customer::retrieve(...).await?;
///     Ok(SyncResult::success())
/// }
/// ```
#[proc_macro_attribute]
pub fn action(attr: TokenStream, item: TokenStream) -> TokenStream {
    action::expand_action(attr, item)
}

/// Marks a function as a background job.
#[proc_macro_attribute]
pub fn job(_attr: TokenStream, item: TokenStream) -> TokenStream {
    // TODO: Implement in Phase 5
    item
}

/// Marks a function as a cron task.
#[proc_macro_attribute]
pub fn cron(_attr: TokenStream, item: TokenStream) -> TokenStream {
    // TODO: Implement in Phase 6
    item
}

/// Marks a function as a workflow.
#[proc_macro_attribute]
pub fn workflow(_attr: TokenStream, item: TokenStream) -> TokenStream {
    // TODO: Implement in Phase 7
    item
}
