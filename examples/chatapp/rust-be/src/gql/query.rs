use async_graphql::{Context, ID, Object, Result};
use chrono::{DateTime, Utc};

use crate::context::CurrentUser;
use crate::db;
use crate::gql::helpers::{app, load_user, map_db, me, parse_id, require_member};
use crate::gql::types::{GqlChat, GqlMessage, GqlUser, OpsStats};

pub struct Query;

#[Object]
impl Query {
    async fn me(&self, ctx: &Context<'_>) -> Result<Option<GqlUser>> {
        let Some(user) = ctx.data_opt::<CurrentUser>() else {
            return Ok(None);
        };
        Ok(load_user(ctx, user.id).await.ok().map(GqlUser))
    }

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
        Ok(db::chat(&c.pool, chat_id)
            .await
            .map_err(map_db)?
            .map(GqlChat))
    }

    async fn messages(
        &self,
        ctx: &Context<'_>,
        chat_id: ID,
        before: Option<DateTime<Utc>>,
        #[graphql(default = 50)] limit: i32,
    ) -> Result<Vec<GqlMessage>> {
        let c = app(ctx)?;
        let user = me(ctx)?;
        let cid = parse_id(&chat_id)?;
        require_member(c, cid, user.id).await?;
        let limit = limit.clamp(1, 200) as i64;
        Ok(db::list_messages(&c.pool, cid, before, limit)
            .await
            .map_err(map_db)?
            .into_iter()
            .map(GqlMessage)
            .collect())
    }

    async fn presence(&self, ctx: &Context<'_>, user_ids: Vec<ID>) -> Result<Vec<GqlUser>> {
        let c = app(ctx)?;
        me(ctx)?;
        let mut ids = Vec::with_capacity(user_ids.len());
        for id in &user_ids {
            ids.push(parse_id(id)?);
        }
        Ok(db::users_by_ids(&c.pool, &ids)
            .await
            .map_err(map_db)?
            .into_iter()
            .map(GqlUser)
            .collect())
    }

    async fn reactions_enabled(&self, ctx: &Context<'_>) -> Result<bool> {
        let Some(user) = ctx.data_opt::<CurrentUser>() else {
            return Ok(false);
        };
        Ok(app(ctx)?.reactions_enabled(user.id).await)
    }

    async fn ops_stats(&self, ctx: &Context<'_>) -> Result<OpsStats> {
        let c = app(ctx)?;
        me(ctx)?;
        Ok(OpsStats {
            online_count: c.online_count().await as i32,
            dlq_count: c.dlq_count().await as i32,
        })
    }
}
