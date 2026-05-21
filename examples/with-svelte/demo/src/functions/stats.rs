use crate::schema::DemoStats;
use forge::prelude::*;

#[forge::query(cache = "10s", auth = "none")]
pub async fn get_demo_stats(ctx: &QueryContext) -> Result<DemoStats> {
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let row = sqlx::query!(
        r#"
        SELECT
            (SELECT COUNT(*) FROM users) as "total_users!",
            (SELECT COUNT(*) FROM trades) as "total_trades!",
            (SELECT COUNT(*) FROM webhook_events) as "total_webhooks!",
            NOW() as "computed_at!"
        "#
    )
    .fetch_one(ctx.db())
    .await?;

    Ok(DemoStats {
        total_users: row.total_users,
        total_trades: row.total_trades,
        total_webhooks: row.total_webhooks,
        computed_at: row.computed_at,
    })
}
