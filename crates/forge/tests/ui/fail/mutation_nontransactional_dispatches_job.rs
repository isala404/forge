//! `transactional = false` + dispatch_job() is unsafe: the job may execute
//! even if the surrounding logic later errors.

#![allow(unused_imports)]

use forge::prelude::*;

#[forge::mutation(transactional = false)]
pub async fn fire_and_forget(ctx: &MutationContext) -> Result<()> {
    ctx.dispatch_job("noop", ()).await?;
    Ok(())
}

fn main() {}
