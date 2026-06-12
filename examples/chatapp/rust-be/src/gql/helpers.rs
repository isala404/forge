use async_graphql::dataloader::DataLoader;
use async_graphql::{Context, Error, ErrorExtensions, ID, Result};
use uuid::Uuid;

use crate::context::{Ctx, CurrentUser};
use crate::db::{self, UserRow};
use crate::loaders::{AppLoader, UserId};

pub(crate) type Loader = DataLoader<AppLoader>;

pub(crate) fn app<'a>(ctx: &'a Context<'_>) -> Result<&'a Ctx> {
    ctx.data::<Ctx>()
}

pub(crate) fn loader<'a>(ctx: &'a Context<'_>) -> Result<&'a Loader> {
    ctx.data::<Loader>()
}

pub(crate) fn me(ctx: &Context<'_>) -> Result<CurrentUser> {
    ctx.data_opt::<CurrentUser>()
        .cloned()
        .ok_or_else(|| err("UNAUTHENTICATED", "not authenticated"))
}

pub(crate) fn err(code: &str, msg: impl Into<String>) -> Error {
    let code = code.to_string();
    Error::new(msg.into()).extend_with(move |_, e| e.set("code", code.clone()))
}

pub(crate) fn forge_error_code(e: &forge::ForgeError) -> &'static str {
    use forge::ForgeError as F;
    match e {
        F::NotFound => "NOT_FOUND",
        F::Invalid(_) => "INVALID",
        F::Limit(_) => "LIMIT",
        F::Precondition(_) => "PRECONDITION",
        F::Unavailable(_) => "UNAVAILABLE",
        F::Config(_) => "CONFIG",
        F::Backend { .. } => "BACKEND",
        _ => "BACKEND",
    }
}

pub(crate) fn map_forge(e: forge::ForgeError) -> Error {
    err(forge_error_code(&e), e.to_string())
}

pub(crate) fn map_db(e: anyhow::Error) -> Error {
    err("BACKEND", e.to_string())
}

pub(crate) fn parse_id(id: &ID) -> Result<Uuid> {
    Uuid::parse_str(id.as_str()).map_err(|_| err("INVALID", "malformed id"))
}

pub(crate) fn validate_credentials(
    username: &str,
    password: &str,
) -> std::result::Result<(), &'static str> {
    if username.trim().len() < 3 {
        return Err("username must be at least 3 characters");
    }
    if password.len() < 6 {
        return Err("password must be at least 6 characters");
    }
    Ok(())
}

pub(crate) async fn require_member(c: &Ctx, chat_id: Uuid, user_id: Uuid) -> Result<()> {
    if db::is_member(&c.pool, chat_id, user_id)
        .await
        .map_err(map_db)?
    {
        Ok(())
    } else {
        Err(err("NOT_FOUND", "chat not found or not a member"))
    }
}

pub(crate) async fn load_user(ctx: &Context<'_>, id: Uuid) -> Result<UserRow> {
    loader(ctx)?
        .load_one(UserId(id))
        .await
        .map_err(|e| err("BACKEND", e.to_string()))?
        .ok_or_else(|| err("NOT_FOUND", "user not found"))
}
