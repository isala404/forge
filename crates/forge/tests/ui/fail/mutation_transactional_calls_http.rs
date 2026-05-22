//! ctx.http() inside a transactional mutation is a footgun (cannot rollback
//! external side-effects, holds the DB connection during the round-trip).

#![allow(unused_imports)]

use forge::prelude::*;

#[forge::mutation]
pub async fn risky_http(ctx: &MutationContext) -> Result<()> {
    let _ = ctx.http();
    Ok(())
}

fn main() {}
