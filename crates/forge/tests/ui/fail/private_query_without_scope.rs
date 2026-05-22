//! Private query selecting from a table without a user/owner/tenant filter
//! must fail the structural scope lint.

#![allow(unused_imports)]

use forge::prelude::*;

#[forge::query]
pub async fn list_everything(_ctx: &QueryContext) -> Result<Vec<String>> {
    let _ = sqlx::query!("SELECT id FROM secrets");
    Ok(vec![])
}

fn main() {}
