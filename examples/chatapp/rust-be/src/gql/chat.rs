use async_graphql::{Context, ID, Object, Result};

use crate::db;
use crate::gql::helpers::{app, err, map_db, me, parse_id, require_member};
use crate::gql::types::{ChatKind, GqlChat};

#[derive(Default)]
pub struct ChatQuery;

#[Object]
impl ChatQuery {
    async fn chats(&self, ctx: &Context<'_>) -> Result<Vec<GqlChat>> {
        let c = app(ctx)?;
        let user = me(ctx)?;
        Ok(db::chats_for_user(&c.pool, user.id)
            .await
            .map_err(map_db)?
            .into_iter()
            .map(GqlChat)
            .collect())
    }

    async fn chat(&self, ctx: &Context<'_>, id: ID) -> Result<Option<GqlChat>> {
        let c = app(ctx)?;
        let user = me(ctx)?;
        let chat_id = parse_id(&id)?;
        require_member(c, chat_id, user.id).await?;
        Ok(db::chat(&c.pool, chat_id).await.map_err(map_db)?.map(GqlChat))
    }
}

#[derive(Default)]
pub struct ChatMutation;

#[Object]
impl ChatMutation {
    async fn create_chat(
        &self,
        ctx: &Context<'_>,
        kind: ChatKind,
        title: Option<String>,
        member_usernames: Vec<String>,
    ) -> Result<GqlChat> {
        let c = app(ctx)?;
        let user = me(ctx)?;
        let mut ids = vec![user.id];
        for uname in &member_usernames {
            let u = db::user_by_username(&c.pool, uname)
                .await
                .map_err(map_db)?
                .ok_or_else(|| err("NOT_FOUND", format!("no such user: {uname}")))?;
            if !ids.contains(&u.id) {
                ids.push(u.id);
            }
        }
        let kind_str = match kind {
            ChatKind::Direct => "direct",
            ChatKind::Group => "group",
        };
        if kind == ChatKind::Direct && ids.len() != 2 {
            return Err(err("INVALID", "a direct chat needs exactly one other member"));
        }
        let chat_id = db::create_chat(&c.pool, kind_str, title.as_deref(), user.id, &ids)
            .await
            .map_err(map_db)?;
        db::chat(&c.pool, chat_id)
            .await
            .map_err(map_db)?
            .map(GqlChat)
            .ok_or_else(|| err("BACKEND", "chat vanished after create"))
    }

    async fn add_member(
        &self,
        ctx: &Context<'_>,
        chat_id: ID,
        username: String,
    ) -> Result<GqlChat> {
        let c = app(ctx)?;
        let user = me(ctx)?;
        let cid = parse_id(&chat_id)?;
        require_member(c, cid, user.id).await?;
        let u = db::user_by_username(&c.pool, &username)
            .await
            .map_err(map_db)?
            .ok_or_else(|| err("NOT_FOUND", format!("no such user: {username}")))?;
        db::add_member(&c.pool, cid, u.id).await.map_err(map_db)?;
        db::chat(&c.pool, cid)
            .await
            .map_err(map_db)?
            .map(GqlChat)
            .ok_or_else(|| err("NOT_FOUND", "chat not found"))
    }
}
