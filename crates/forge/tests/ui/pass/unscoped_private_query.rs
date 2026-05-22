//! `scope = "global"` opts out of the user_id filter requirement.

use forge::prelude::*;

#[forge::query(scope = "global", tables("posts"))]
pub async fn list_all_posts(_ctx: &QueryContext) -> Result<Vec<String>> {
    Ok(vec![])
}

fn main() {}
