//! `transactional = false` mutations are allowed to call ctx.http().

use forge::prelude::*;

#[forge::mutation(transactional = false)]
pub async fn ping_webhook(ctx: &MutationContext) -> Result<()> {
    let _ = ctx.http();
    Ok(())
}

fn main() {}
