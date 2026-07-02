use async_graphql::{Context, Object, Result};

use crate::context::CurrentUser;
use crate::gql::helpers::{app, map_db, me, require_admin};
use crate::gql::types::OpsStats;

#[derive(Default)]
pub struct OpsQuery;

#[Object]
impl OpsQuery {
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

#[derive(Default)]
pub struct OpsMutation;

#[Object]
impl OpsMutation {
    async fn set_reactions_rollout(&self, ctx: &Context<'_>, percent: i32) -> Result<bool> {
        let c = app(ctx)?;
        require_admin(ctx)?;
        c.set_reactions_rollout(percent.clamp(0, 100) as u8)
            .await
            .map_err(map_db)?;
        Ok(true)
    }

    async fn trigger_failing_job(&self, ctx: &Context<'_>) -> Result<bool> {
        let c = app(ctx)?;
        require_admin(ctx)?;
        c.enqueue_failing().await.map_err(map_db)?;
        Ok(true)
    }
}
