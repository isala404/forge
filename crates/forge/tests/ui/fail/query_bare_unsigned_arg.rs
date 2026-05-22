//! Wire types must be JSON-safe: u32/u64/i128/u128/usize/isize are rejected
//! because they don't round-trip cleanly through serde_json.

#![allow(unused_imports)]

use forge::prelude::*;

#[forge::query(auth = "none", tables("widgets"))]
pub async fn lookup(_ctx: &QueryContext, _count: u32) -> Result<i64> {
    Ok(0)
}

fn main() {}
