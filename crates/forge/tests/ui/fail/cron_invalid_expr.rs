//! Invalid cron expression must be rejected at compile time.

#![allow(unused_imports)]

use forge::prelude::*;

#[forge::cron(schedule = "this is not a cron expression")]
pub async fn broken(_ctx: &CronContext) -> Result<()> {
    Ok(())
}

fn main() {}
