//! 5-field cron expression is normalised to 6-field internally.

#![allow(unused_imports)]

use forge::prelude::*;

#[forge::cron(schedule = "0 * * * *")]
pub async fn hourly(_ctx: &CronContext) -> Result<()> {
    Ok(())
}

fn main() {}
