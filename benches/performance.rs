// CREATE/DROP DATABASE use a generated identifier and cannot be expressed by sqlx's
// compile-time macros. The identifier is a Forge-generated UUID, never caller input.
#![allow(clippy::disallowed_methods)]

use forgelib::{Bytes, DequeueOpts, EnqueueOpts, FailMode, Forge, Limit, PutOpts};
use serde::Serialize;
use sqlx::{Connection, PgConnection};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::alloc::System;
use std::env;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};
use uuid::Uuid;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

type BenchResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Copy)]
struct Workload {
    iterations: usize,
    concurrency: usize,
    processes: usize,
    blob_bytes: usize,
}

impl Workload {
    fn named(name: &str) -> BenchResult<Self> {
        match name {
            "smoke" => Ok(Self {
                iterations: 20,
                concurrency: 4,
                processes: 2,
                blob_bytes: 4 * 1024,
            }),
            "full" => Ok(Self {
                iterations: 500,
                concurrency: 16,
                processes: 4,
                blob_bytes: 1024 * 1024,
            }),
            _ => Err(format!("profile must be smoke or full, got {name:?}").into()),
        }
    }
}

#[derive(Serialize)]
struct Metric {
    name: &'static str,
    value: f64,
    unit: &'static str,
}

#[derive(Serialize)]
struct Report {
    schema_version: u8,
    kind: &'static str,
    backend: &'static str,
    profile: String,
    iterations: usize,
    concurrency: usize,
    processes: usize,
    metrics: Vec<Metric>,
}

struct Args {
    backend: String,
    profile: String,
    output: Option<String>,
    child: bool,
}

fn args() -> BenchResult<Args> {
    let mut parsed = Args {
        backend: "memory".to_string(),
        profile: "smoke".to_string(),
        output: None,
        child: false,
    };
    let mut values = env::args().skip(1);
    while let Some(arg) = values.next() {
        match arg.as_str() {
            "--backend" => parsed.backend = values.next().ok_or("--backend needs a value")?,
            "--profile" => parsed.profile = values.next().ok_or("--profile needs a value")?,
            "--output" => parsed.output = Some(values.next().ok_or("--output needs a value")?),
            "--child" => parsed.child = true,
            "--bench" => {}
            other => return Err(format!("unknown benchmark argument {other:?}").into()),
        }
    }
    Ok(parsed)
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("performance benchmark failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> BenchResult<()> {
    let args = args()?;
    if args.child {
        return child_workload().await;
    }
    let workload = Workload::named(&args.profile)?;
    let (forge, backend, temp_db, bootstrap) = build_backend(&args.backend).await?;
    let metrics = measure(&forge, backend, workload).await?;
    forge.close(Duration::from_secs(10)).await?;
    drop(temp_db);
    if let Some(bootstrap) = bootstrap {
        bootstrap.close(Duration::from_secs(10)).await?;
    }
    let report = Report {
        schema_version: 1,
        kind: "backend",
        backend,
        profile: args.profile,
        iterations: workload.iterations,
        concurrency: workload.concurrency,
        processes: if backend == "postgres" {
            workload.processes
        } else {
            1
        },
        metrics,
    };
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(output) = args.output {
        std::fs::write(output, format!("{json}\n"))?;
    } else {
        println!("{json}");
    }
    Ok(())
}

async fn build_backend(
    backend: &str,
) -> BenchResult<(Forge, &'static str, Option<TempDatabase>, Option<Forge>)> {
    match backend {
        "memory" => Ok((
            Forge::init_from_str("[forge]\nmode = \"memory\"\nenvironment = \"test\"\n").await?,
            "memory",
            None,
            None,
        )),
        "postgres" => {
            let admin_url = env::var("PERF_DATABASE_URL")
                .map_err(|_| "PERF_DATABASE_URL is required for --backend postgres")?;
            let db = TempDatabase::new(&admin_url).await?;
            let forge = Forge::init_from_str(&postgres_config(db.url(), 16, true)).await?;
            Ok((forge, "postgres", Some(db), None))
        }
        "filesystem" => {
            let admin_url = env::var("PERF_DATABASE_URL")
                .map_err(|_| "PERF_DATABASE_URL is required for --backend filesystem")?;
            let db = TempDatabase::new(&admin_url).await?;
            let root = env::var("FORGE_PERF_BLOB_DIR")
                .unwrap_or_else(|_| "target/forge-perf-blobs".to_string());
            let config = format!(
                "{}\n[blob]\nbackend = \"fs\"\nfs_root = {root:?}\n",
                postgres_config(db.url(), 16, true)
            );
            let forge = Forge::init_from_str(&config).await?;
            Ok((forge, "filesystem", Some(db), None))
        }
        "s3" => {
            let admin_url = env::var("PERF_DATABASE_URL")
                .map_err(|_| "PERF_DATABASE_URL is required for --backend s3")?;
            let db = TempDatabase::new(&admin_url).await?;
            let config = format!(
                "{}{}",
                postgres_config(db.url(), 16, true),
                s3_blob_config()?
            );
            let forge = Forge::init_from_str(&config).await?;
            Ok((forge, "s3", Some(db), None))
        }
        "embedded" => build_embedded("postgres").await,
        "embedded-filesystem" => build_embedded("filesystem").await,
        "embedded-s3" => build_embedded("s3").await,
        other => Err(format!(
            "backend must be memory, postgres, embedded, filesystem, embedded-filesystem, s3, or embedded-s3, got {other:?}"
        )
        .into()),
    }
}

#[cfg(feature = "embedded")]
async fn build_embedded(
    report_backend: &'static str,
) -> BenchResult<(Forge, &'static str, Option<TempDatabase>, Option<Forge>)> {
    let directory = env::var("FORGE_PERF_EMBEDDED_DIR")
        .unwrap_or_else(|_| "target/forge-perf-postgres".to_string());
    let config = format!(
        "[forge]\nnamespace = \"perf_bootstrap\"\n[postgres]\nembedded = true\nembedded_dir = {directory:?}\nmax_connections = 2\n"
    );
    let bootstrap = Forge::init_from_str(&config).await?;
    let db = TempDatabase::new(bootstrap.postgres_url()?).await?;
    let mut config = postgres_config(db.url(), 16, true);
    if report_backend == "filesystem" {
        let root = env::var("FORGE_PERF_BLOB_DIR")
            .unwrap_or_else(|_| "target/forge-perf-blobs".to_string());
        config.push_str(&format!("\n[blob]\nbackend = \"fs\"\nfs_root = {root:?}\n"));
    } else if report_backend == "s3" {
        config.push_str(&s3_blob_config()?);
    }
    let forge = Forge::init_from_str(&config).await?;
    Ok((forge, report_backend, Some(db), Some(bootstrap)))
}

#[cfg(not(feature = "embedded"))]
async fn build_embedded(
    _report_backend: &'static str,
) -> BenchResult<(Forge, &'static str, Option<TempDatabase>, Option<Forge>)> {
    Err("--backend embedded requires --features embedded".into())
}

fn postgres_config(url: &str, connections: u32, migrate: bool) -> String {
    format!(
        "[forge]\nnamespace = \"perf\"\nenvironment = \"test\"\n[postgres]\nurl = {url:?}\nmax_connections = {connections}\nauto_migrate = {migrate}\n"
    )
}

fn s3_blob_config() -> BenchResult<String> {
    let endpoint = env::var("PERF_S3_ENDPOINT")?;
    let bucket = env::var("PERF_S3_BUCKET")?;
    let access_key = env::var("PERF_S3_ACCESS_KEY")?;
    let secret_key = env::var("PERF_S3_SECRET_KEY")?;
    let region = env::var("PERF_S3_REGION").unwrap_or_else(|_| "us-east-1".to_string());
    let prefix = format!("forge-performance/{}", Uuid::new_v4());
    Ok(format!(
        "\n[blob]\nbackend = \"s3\"\nendpoint = {endpoint:?}\nbucket = {bucket:?}\nregion = {region:?}\nprefix = {prefix:?}\naccess_key = {access_key:?}\nsecret_key = {secret_key:?}\npath_style = true\nsigning_secret = \"forge-performance-signing-secret\"\n"
    ))
}

async fn measure(
    forge: &Forge,
    backend: &'static str,
    workload: Workload,
) -> BenchResult<Vec<Metric>> {
    let pool_done = Arc::new(AtomicBool::new(false));
    let pool_peak = Arc::new(AtomicU32::new(0));
    let pool_monitor = if backend != "memory" {
        let client = forge.clone();
        let done = pool_done.clone();
        let peak = pool_peak.clone();
        Some(tokio::spawn(async move {
            while !done.load(Ordering::Acquire) {
                if let Ok(pool) = client.pool() {
                    let idle = u32::try_from(pool.num_idle()).unwrap_or(u32::MAX);
                    let used = pool.size().saturating_sub(idle);
                    peak.fetch_max(used, Ordering::AcqRel);
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }))
    } else {
        None
    };
    let allocation_region = Region::new(GLOBAL);
    let mut queue_samples = Vec::with_capacity(workload.iterations);
    let queue_started = Instant::now();
    let dequeue = DequeueOpts::new().with_wait(Duration::ZERO);
    for _ in 0..workload.iterations {
        let started = Instant::now();
        forge
            .queue()
            .enqueue("bench", Bytes::from_static(b"work"), EnqueueOpts::new())
            .await?;
        let job = forge
            .queue()
            .dequeue("bench", dequeue.clone())
            .await?
            .ok_or("enqueued job was not visible")?;
        forge.queue().ack(&job).await?;
        queue_samples.push(started.elapsed());
    }
    let queue_elapsed = queue_started.elapsed();

    let limiter = Limit::per_duration(2_000_000_000, Duration::from_secs(60));
    let rate_samples = Arc::new(tokio::sync::Mutex::new(Vec::with_capacity(
        workload.iterations * workload.concurrency,
    )));
    let rate_started = Instant::now();
    let mut rate_tasks = tokio::task::JoinSet::new();
    for _ in 0..workload.concurrency {
        let client = forge.clone();
        let samples = rate_samples.clone();
        let per_task = workload.iterations;
        rate_tasks.spawn(async move {
            let mut local = Vec::with_capacity(per_task);
            for _ in 0..per_task {
                let started = Instant::now();
                client
                    .ratelimit()
                    .check_with("bench", "shared", limiter, FailMode::Closed)
                    .await?;
                local.push(started.elapsed());
            }
            samples.lock().await.extend(local);
            forgelib::Result::Ok(())
        });
    }
    while let Some(result) = rate_tasks.join_next().await {
        result??;
    }
    let rate_elapsed = rate_started.elapsed();
    let rate_samples = Arc::try_unwrap(rate_samples)
        .map_err(|_| "rate sample handles remained")?
        .into_inner();

    let body = Bytes::from(vec![0x5a; workload.blob_bytes]);
    let mut blob_samples = Vec::with_capacity(workload.iterations);
    let blob_started = Instant::now();
    for iteration in 0..workload.iterations {
        let key = format!("bench/{iteration}");
        let started = Instant::now();
        forge.blob().put(&key, body.clone(), PutOpts::new()).await?;
        let loaded = forge
            .blob()
            .get(&key)
            .await?
            .ok_or("benchmark blob disappeared")?;
        if loaded.len() != body.len() {
            return Err("benchmark blob length changed".into());
        }
        forge.blob().delete(&key).await?;
        blob_samples.push(started.elapsed());
    }
    let blob_elapsed = blob_started.elapsed();

    let mut maintenance_samples = Vec::with_capacity(workload.iterations);
    for _ in 0..workload.iterations {
        let started = Instant::now();
        forge.maintain().await?;
        maintenance_samples.push(started.elapsed());
    }

    let operation_count = workload.iterations * (3 + workload.concurrency);
    let allocations = allocation_region.change().allocations as f64 / operation_count as f64;
    let mut metrics = vec![
        metric(
            "queue_roundtrip_p95_ms",
            millis(percentile(&queue_samples, 95)),
            "ms",
        ),
        metric(
            "queue_throughput_ops_per_sec",
            workload.iterations as f64 / queue_elapsed.as_secs_f64(),
            "operations/second",
        ),
        metric(
            "rate_limit_contention_p95_ms",
            millis(percentile(&rate_samples, 95)),
            "ms",
        ),
        metric(
            "rate_limit_contention_ops_per_sec",
            (workload.iterations * workload.concurrency) as f64 / rate_elapsed.as_secs_f64(),
            "operations/second",
        ),
        metric(
            "blob_roundtrip_p95_ms",
            millis(percentile(&blob_samples, 95)),
            "ms",
        ),
        metric(
            "blob_transfer_mib_per_sec",
            (2 * workload.blob_bytes * workload.iterations) as f64
                / 1_048_576.0
                / blob_elapsed.as_secs_f64(),
            "MiB/second",
        ),
        metric(
            "maintenance_p95_ms",
            millis(percentile(&maintenance_samples, 95)),
            "ms",
        ),
    ];
    if backend == "memory" {
        metrics.push(metric(
            "allocations_per_operation",
            allocations,
            "allocations/operation",
        ));
    } else {
        pool_done.store(true, Ordering::Release);
        if let Some(monitor) = pool_monitor {
            monitor.await?;
        }
        let pool = forge.pool()?;
        let size = pool.size();
        let used = pool_peak.load(Ordering::Acquire);
        metrics.push(metric(
            "pool_peak_utilization_ratio",
            if size == 0 {
                0.0
            } else {
                used as f64 / size as f64
            },
            "ratio",
        ));
        if backend == "postgres" {
            metrics.extend(multiprocess(forge, workload).await?);
        }
    }
    Ok(metrics)
}

async fn multiprocess(forge: &Forge, workload: Workload) -> BenchResult<Vec<Metric>> {
    let dsn = forge.postgres_url()?;
    let executable = env::current_exe()?;
    let started = Instant::now();
    let mut children = Vec::with_capacity(workload.processes);
    for worker in 0..workload.processes {
        let child = Command::new(&executable)
            .arg("--child")
            .env("FORGE_PERF_DATABASE_URL", dsn)
            .env("FORGE_PERF_ITERATIONS", workload.iterations.to_string())
            .env("FORGE_PERF_WORKER", worker.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()?;
        children.push(child);
    }
    for mut child in children {
        let status = child.wait()?;
        if !status.success() {
            return Err(format!("contention child exited with {status}").into());
        }
    }
    let elapsed = started.elapsed();
    let operations = workload.iterations * workload.processes * 2;
    Ok(vec![
        metric(
            "multiprocess_contention_p95_ms",
            millis(elapsed) / workload.iterations as f64,
            "ms/iteration",
        ),
        metric(
            "multiprocess_contention_ops_per_sec",
            operations as f64 / elapsed.as_secs_f64(),
            "operations/second",
        ),
    ])
}

async fn child_workload() -> BenchResult<()> {
    let url = env::var("FORGE_PERF_DATABASE_URL")?;
    let iterations: usize = env::var("FORGE_PERF_ITERATIONS")?.parse()?;
    let worker = env::var("FORGE_PERF_WORKER")?;
    let forge = Forge::init_from_str(&postgres_config(&url, 2, false)).await?;
    let limit = Limit::per_duration(2_000_000_000, Duration::from_secs(60));
    for iteration in 0..iterations {
        forge
            .ratelimit()
            .check_with("process-contention", "shared", limit, FailMode::Closed)
            .await?;
        let queue = format!("process-contention-{worker}");
        forge
            .queue()
            .enqueue(&queue, Bytes::from_static(b"work"), EnqueueOpts::new())
            .await?;
        let job = forge
            .queue()
            .dequeue(&queue, DequeueOpts::new().with_wait(Duration::ZERO))
            .await?
            .ok_or("child job was not visible")?;
        forge.queue().ack(&job).await?;
        if iteration % 50 == 0 {
            tokio::task::yield_now().await;
        }
    }
    forge.close(Duration::from_secs(10)).await?;
    Ok(())
}

fn percentile(samples: &[Duration], percentile: usize) -> Duration {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank = sorted
        .len()
        .saturating_mul(percentile)
        .div_ceil(100)
        .saturating_sub(1);
    sorted.get(rank).copied().unwrap_or_default()
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
fn metric(name: &'static str, value: f64, unit: &'static str) -> Metric {
    Metric { name, value, unit }
}

struct TempDatabase {
    admin_url: String,
    name: String,
    url: String,
}

impl TempDatabase {
    async fn new(admin_url: &str) -> BenchResult<Self> {
        let name = format!("forge_perf_{}", Uuid::new_v4().simple());
        let url = with_database(admin_url, &name);
        let mut connection = PgConnection::connect(admin_url).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
            .execute(&mut connection)
            .await?;
        connection.close().await?;
        Ok(Self {
            admin_url: admin_url.to_string(),
            name,
            url,
        })
    }
    fn url(&self) -> &str {
        &self.url
    }
}

impl Drop for TempDatabase {
    fn drop(&mut self) {
        let admin_url = self.admin_url.clone();
        let name = self.name.clone();
        let _ = std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                if let Ok(mut connection) = PgConnection::connect(&admin_url).await {
                    let query = sqlx::AssertSqlSafe(format!(
                        "DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"
                    ));
                    let _ = sqlx::query(query).execute(&mut connection).await;
                    let _ = connection.close().await;
                }
            });
        })
        .join();
    }
}

fn with_database(url: &str, database: &str) -> String {
    let (base, query) = url
        .split_once('?')
        .map_or((url, None), |(base, query)| (base, Some(query)));
    let trimmed = base.trim_end_matches('/');
    let prefix = trimmed
        .rfind('/')
        .and_then(|index| trimmed.get(..index))
        .unwrap_or(trimmed);
    match query {
        Some(query) => format!("{prefix}/{database}?{query}"),
        None => format!("{prefix}/{database}"),
    }
}
