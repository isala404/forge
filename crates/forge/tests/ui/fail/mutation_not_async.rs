//! Mutations must be async.

#![allow(unused_imports)]

use forge::prelude::*;

#[forge::mutation]
pub fn sync_mutation(_ctx: &MutationContext) -> Result<()> {
    Ok(())
}

fn main() {}
