//! Default transactional mutation dispatching a job — happy path.

use forge::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct ExportArgs {
    pub user_id: uuid::Uuid,
}

#[forge::job]
pub async fn export_user(_ctx: &JobContext, _args: ExportArgs) -> Result<()> {
    Ok(())
}

#[forge::mutation]
pub async fn schedule_export(ctx: &MutationContext, user_id: uuid::Uuid) -> Result<()> {
    ctx.dispatch_job("export_user", ExportArgs { user_id }).await?;
    Ok(())
}

fn main() {}
