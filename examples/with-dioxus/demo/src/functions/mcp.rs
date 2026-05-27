use crate::schema::{McpUserInfo, UserRole};
use forge::forge_core::mcp::McpToolContext;

#[forge::mcp_tool(
    name = "demo.me",
    title = "My Profile",
    description = "Get the authenticated user's profile information",
    read_only
)]
pub async fn mcp_me(ctx: &McpToolContext) -> forge::forge_core::Result<McpUserInfo> {
    let user_id = ctx.user_id()?;
    let mut conn = ctx.conn().await?;

    let user = sqlx::query_as!(
        McpUserInfo,
        r#"
        SELECT id, email, name, role as "role: UserRole"
        FROM users WHERE id = $1
        "#,
        user_id
    )
    .fetch_optional(&mut conn)
    .await?
    .ok_or_else(|| forge::forge_core::ForgeError::NotFound("User not found".into()))?;

    Ok(user)
}

#[forge::mcp_tool(
    name = "demo.list_users",
    title = "List Users",
    description = "List all users in the demo database with their roles",
    read_only
)]
pub async fn mcp_list_users(ctx: &McpToolContext) -> forge::forge_core::Result<Vec<McpUserInfo>> {
    let _ = ctx.user_id()?;
    let mut conn = ctx.conn().await?;

    let users = sqlx::query_as!(
        McpUserInfo,
        r#"
        SELECT
            id, email, name,
            role as "role: UserRole"
        FROM users
        ORDER BY created_at DESC
        "#
    )
    .fetch_all(&mut conn)
    .await?;

    Ok(users)
}

#[derive(Debug, serde::Deserialize, forge::schemars::JsonSchema)]
#[schemars(crate = "forge::schemars")]
pub struct McpGetUserInput {
    /// The email address of the user to look up
    pub email: String,
}

#[forge::mcp_tool(
    name = "demo.get_user_by_email",
    title = "Get User by Email",
    description = "Look up a single user by their email address",
    read_only
)]
pub async fn mcp_get_user_by_email(
    ctx: &McpToolContext,
    input: McpGetUserInput,
) -> forge::forge_core::Result<Option<McpUserInfo>> {
    let _ = ctx.user_id()?;
    let mut conn = ctx.conn().await?;

    let user = sqlx::query_as!(
        McpUserInfo,
        r#"
        SELECT
            id, email, name,
            role as "role: UserRole"
        FROM users
        WHERE email = $1
        "#,
        &input.email
    )
    .fetch_optional(&mut conn)
    .await?;

    Ok(user)
}
