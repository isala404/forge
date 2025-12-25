use proc_macro::TokenStream;

mod action;
mod cron;
mod enum_type;
mod job;
mod model;
mod mutation;
mod query;
mod workflow;

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
///
/// Jobs are durable background tasks that survive server restarts,
/// automatically retry on failure, and can be scheduled for the future.
///
/// # Attributes
/// - `timeout = "30m"` - Job timeout (duration like "30s", "5m", "1h")
/// - `priority = "normal"` - Priority: background, low, normal, high, critical
/// - `max_attempts = 3` - Maximum retry attempts
/// - `worker_capability = "general"` - Required worker capability
/// - `idempotent` - Enable deduplication by key
///
/// # Example
/// ```ignore
/// #[forge::job]
/// #[timeout = "30m"]
/// #[priority = "high"]
/// #[max_attempts = 5]
/// pub async fn send_welcome_email(
///     ctx: &JobContext,
///     input: SendEmailInput,
/// ) -> Result<()> {
///     email::send(&input).await
/// }
/// ```
#[proc_macro_attribute]
pub fn job(attr: TokenStream, item: TokenStream) -> TokenStream {
    job::job_impl(attr, item)
}

/// Marks a function as a scheduled cron task.
///
/// Cron jobs run on a schedule and are guaranteed to run exactly once
/// per scheduled time across the entire cluster.
///
/// # Arguments
/// The cron expression is passed as the first argument:
/// - `"0 * * * *"` - Every hour
/// - `"*/5 * * * *"` - Every 5 minutes
/// - `"0 0 * * *"` - Every day at midnight
///
/// # Attributes
/// - `timezone = "UTC"` - Timezone for the schedule (default: UTC)
/// - `catch_up` - Run missed executions after downtime
/// - `catch_up_limit = 10` - Maximum missed runs to catch up
/// - `timeout = "1h"` - Execution timeout
///
/// # Example
/// ```ignore
/// #[forge::cron("0 0 * * *")]
/// #[timezone = "America/New_York"]
/// #[catch_up]
/// pub async fn daily_cleanup(ctx: &CronContext) -> Result<()> {
///     ctx.log.info("Starting cleanup", json!({}));
///     // Cleanup logic...
///     Ok(())
/// }
/// ```
#[proc_macro_attribute]
pub fn cron(attr: TokenStream, item: TokenStream) -> TokenStream {
    cron::cron_impl(attr, item)
}

/// Marks a function as a durable workflow.
///
/// Workflows are multi-step processes that:
/// - Survive server restarts
/// - Handle failures with compensation
/// - Track progress and state
/// - Can run for hours, days, or longer
///
/// # Attributes
/// - `version = 1` - Workflow version (increment for breaking changes)
/// - `timeout = "24h"` - Maximum workflow execution time
/// - `deprecated` - Mark as deprecated
///
/// # Example
/// ```ignore
/// #[forge::workflow]
/// #[version = 1]
/// pub async fn user_onboarding(
///     ctx: &WorkflowContext,
///     input: OnboardingInput,
/// ) -> Result<OnboardingResult> {
///     let user = ctx.step("create_user")
///         .run(|| ctx.mutate(create_user, input.clone()))
///         .compensate(|user| ctx.mutate(delete_user, user.id))
///         .await?;
///
///     ctx.step("send_welcome")
///         .run(|| send_email(&user.email))
///         .optional()
///         .await;
///
///     Ok(OnboardingResult { user })
/// }
/// ```
#[proc_macro_attribute]
pub fn workflow(attr: TokenStream, item: TokenStream) -> TokenStream {
    workflow::workflow_impl(attr, item)
}
