//! Public queries don't require user-scoped SQL filters.

use forge::prelude::*;

#[forge::query(auth = "none", tables("posts"))]
pub async fn list_posts(_ctx: &QueryContext) -> Result<Vec<String>> {
    Ok(vec![])
}

fn main() {}
