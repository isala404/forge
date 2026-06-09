//! A guided tour of every Forge primitive in one runnable program.
//!
//! It signs up a user, mints a session and an API key, gates a feature with a flag,
//! rate-limits an action, stores and presigns a file, and schedules a one-shot job —
//! all on a single Postgres connection. Each step asserts its outcome, so the example
//! doubles as a smoke test of the whole surface.
//!
//! Run it:
//!   docker compose up -d db
//!   FORGE_POSTGRES_URL=postgres://postgres:forge@localhost:5432/forge_dev \
//!     cargo run --example full_tour

use forge::{
    Algo, Bytes, ConfigExt, DequeueOpts, EvalCtx, FlagRule, Forge, ForgeConfig, Limit, PutOpts,
    SessionOpts,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[tokio::main]
async fn main() -> forge::Result<()> {
    let url = std::env::var("FORGE_POSTGRES_URL")
        .unwrap_or_else(|_| "postgres://postgres:forge@localhost:5432/forge_dev".to_string());

    // A signing secret enables presigned blob URLs; everything else needs nothing.
    let forge =
        Forge::init(ForgeConfig::new(url).with_blob_signing_secret("tour-secret-change-me"))
            .await?;

    let run = unique_token();
    let user_id = format!("user:{run}");

    // ---- auth: password, session, API key ------------------------------------
    let hash = forge.auth().hash_password("hunter2-correct-horse").await?;
    assert!(
        forge
            .auth()
            .verify_password("hunter2-correct-horse", &hash)
            .await?
    );
    assert!(!forge.auth().verify_password("wrong", &hash).await?);

    let token = forge
        .auth()
        .create_session(&user_id, SessionOpts::new())
        .await?;
    let session = forge.auth().validate_session(token.as_str()).await?;
    assert_eq!(
        session.map(|s| s.user_id).as_deref(),
        Some(user_id.as_str())
    );

    let api_key = forge.auth().create_api_key(&user_id, "cli").await?;
    let info = forge.auth().verify_api_key(api_key.secret.as_str()).await?;
    assert_eq!(info.map(|i| i.owner_id).as_deref(), Some(user_id.as_str()));
    println!(
        "auth: password verified, session + API key ({}) minted",
        api_key.id
    );

    // ---- config + flags ------------------------------------------------------
    // get_raw returns the stored string as-is; ConfigExt::get<T> parses it as JSON.
    forge
        .config()
        .set_raw(&format!("plan:{run}"), "pro")
        .await?;
    assert_eq!(
        forge.config().get_raw(&format!("plan:{run}")).await?,
        Some("pro".to_string())
    );
    forge
        .config()
        .set_raw(&format!("max_uploads:{run}"), "10")
        .await?;
    assert_eq!(
        forge
            .config()
            .get::<u32>(&format!("max_uploads:{run}"))
            .await?,
        Some(10)
    );
    let flag = format!("new_ui:{run}");
    forge
        .config()
        .set_flag(&flag, FlagRule::Percent(100))
        .await?;
    let on = forge
        .config()
        .flag(&flag, false, &EvalCtx::user(&user_id))
        .await;
    assert!(on, "Percent(100) is on for everyone");
    println!("config: plan=pro stored; flag {flag} resolved to {on}");

    // ---- ratelimit: 3 per minute, the 4th is throttled -----------------------
    let limit = Limit::per_duration(3, Duration::from_secs(60)).with_algo(Algo::TokenBucket);
    let mut allowed = 0;
    for _ in 0..4 {
        if forge
            .ratelimit()
            .check("login", &user_id, limit)
            .await?
            .allowed
        {
            allowed += 1;
        }
    }
    assert_eq!(allowed, 3, "3 admitted, the 4th throttled");
    println!("ratelimit: {allowed}/4 login attempts admitted (limit 3/min)");

    // ---- blob: store, read back, presign -------------------------------------
    let key = format!("exports/{run}/report.txt");
    forge
        .blob()
        .put(
            &key,
            Bytes::from_static(b"hello,world\n1,2\n"),
            PutOpts::new().with_content_type("text/csv"),
        )
        .await?;
    let head = forge.blob().head(&key).await?.expect("object exists");
    assert_eq!(head.content_type, "text/csv");
    let download_url = forge
        .blob()
        .presign_download(&key, Duration::from_secs(300))
        .await?;
    println!(
        "blob: stored {} bytes; presigned URL {download_url}",
        head.size
    );

    // ---- schedule: a one-shot due now, fired into the queue ------------------
    let report_queue = format!("reports_{run}");
    let job_id = forge
        .schedule()
        .at(
            SystemTime::now(),
            &report_queue,
            Bytes::from_static(b"generate-report"),
        )
        .await?;
    let fired = forge.run_scheduler_once().await?;
    assert!(fired >= 1, "the due one-shot fired");
    let job = forge
        .queue()
        .dequeue(&report_queue, DequeueOpts::new().with_wait(Duration::ZERO))
        .await?
        .expect("scheduled job landed in the queue");
    assert_eq!(
        job.id, job_id,
        "the queued job carries the JobId `at` returned"
    );
    forge.queue().ack(&job).await?;
    println!("schedule: one-shot {job_id} fired and was consumed from `{report_queue}`");

    // ---- kv: a counter, for good measure -------------------------------------
    let hits = forge.kv().incr(&format!("hits:{run}"), 1).await?;
    assert_eq!(hits, 1);

    println!("\nOK — every primitive worked end to end.");
    Ok(())
}

/// A short unique-ish token so reruns don't collide; avoids a uuid dependency.
fn unique_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}
