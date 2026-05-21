use forge::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationInput {
    pub account_id: String,
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationOutput {
    pub verified: bool,
    pub token: String,
}

/// Multi-step account verification with durable sleep and event wait.
///
/// Demonstrates:
/// - Step tracking with is_step_completed/record_step_*
/// - Durable sleep that survives server restarts
/// - wait_for_event with a manual confirmation trigger
/// - Resumption detection with is_resumed()
/// - Workflow versioning (this is the active version)
#[forge::workflow(
    name = "account_verification",
    version = "2026-03",
    active,
    timeout = "24h",
    auth = "none"
)]
pub async fn account_verification(
    ctx: &WorkflowContext,
    input: VerificationInput,
) -> Result<VerificationOutput> {
    if ctx.is_resumed() {
        tracing::info!(workflow_id = %ctx.run_id, "Resuming verification workflow");
    }

    // Step 1: Generate token
    let token = if ctx.is_step_completed("generate_token") {
        ctx.get_step_result::<String>("generate_token")
            .unwrap_or_else(|| format!("verify_{}", Uuid::new_v4()))
    } else {
        ctx.record_step_start("generate_token").await?;
        tracing::info!("Generating verification token");
        tokio::time::sleep(Duration::from_millis(600)).await;
        let token = format!("verify_{}", Uuid::new_v4());
        ctx.record_step_complete("generate_token", serde_json::json!(token))
            .await?;
        token
    };

    // Step 2: Store token
    if !ctx.is_step_completed("store_token") {
        ctx.record_step_start("store_token").await?;
        tracing::info!("Storing verification token");
        tokio::time::sleep(Duration::from_millis(600)).await;
        ctx.record_step_complete("store_token", serde_json::json!({"stored": true}))
            .await?;
    }

    // Step 3: Send verification email
    if !ctx.is_step_completed("send_email") {
        ctx.record_step_start("send_email").await?;
        tracing::info!(email = %input.email, "Sending verification email");
        tokio::time::sleep(Duration::from_millis(600)).await;
        ctx.record_step_complete("send_email", serde_json::json!({"sent": true}))
            .await?;
    }

    // Step 4: Wait for user confirmation (event-driven, user clicks a button in the UI)
    if !ctx.is_step_completed("await_confirmation") {
        ctx.record_step_start("await_confirmation").await?;
        tracing::info!(workflow_id = %ctx.run_id, "Waiting for user confirmation");
        let _confirmation: serde_json::Value = ctx
            .wait_for_event("verification_confirmed", Some(Duration::from_secs(300)))
            .await?;
        ctx.record_step_complete("await_confirmation", serde_json::json!({"confirmed": true}))
            .await?;
    }

    // Step 5: Short wait period (durable sleep, survives restarts)
    if !ctx.is_step_completed("wait_period") {
        ctx.record_step_start("wait_period").await?;
        tracing::info!("Entering brief wait period");
        ctx.sleep(Duration::from_secs(2)).await?;
        ctx.record_step_complete("wait_period", serde_json::json!({"waited": true}))
            .await?;
    }

    // Step 6: Mark verified
    if !ctx.is_step_completed("mark_verified") {
        ctx.record_step_start("mark_verified").await?;
        tracing::info!(account_id = %input.account_id, "Marking account verified");
        tokio::time::sleep(Duration::from_millis(600)).await;
        ctx.record_step_complete("mark_verified", serde_json::json!(true))
            .await?;
    }

    tracing::info!(workflow_id = %ctx.run_id, "Verification complete");

    Ok(VerificationOutput {
        verified: true,
        token,
    })
}

/// Confirm a pending verification workflow.
/// Called when the user clicks the "Confirm" button in the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfirmVerificationInput {
    pub workflow_id: String,
}

#[forge::mutation(auth = "none")]
// forge_workflow_events is owned by the runtime, so the framework user's .sqlx
// cache doesn't see it. Runtime sqlx::query is the right tool here.
#[allow(clippy::disallowed_methods)]
pub async fn confirm_verification(
    ctx: &MutationContext,
    input: ConfirmVerificationInput,
) -> Result<bool> {
    // Insert the confirmation event into the workflow events table.
    // The scheduler's NOTIFY trigger will wake the waiting workflow.
    sqlx::query(
        r#"
        INSERT INTO forge_workflow_events (id, event_name, correlation_id, payload)
        VALUES (gen_random_uuid(), 'verification_confirmed', $1, '{"confirmed": true}'::jsonb)
        "#,
    )
    .bind(&input.workflow_id)
    .execute(ctx.conn().await?.as_mut())
    .await
    .map_err(ForgeError::Database)?;

    Ok(true)
}
