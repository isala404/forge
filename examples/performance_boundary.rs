use forgelib::interop::CloudEvent;
use serde::Serialize;
use std::time::{Duration, Instant};

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
    language: &'static str,
    iterations: usize,
    metrics: [Metric; 1],
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut iterations = 1_000usize;
    let mut output = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--iterations" => iterations = args.next().ok_or("missing iteration count")?.parse()?,
            "--output" => output = Some(args.next().ok_or("missing output path")?),
            other => return Err(format!("unknown argument {other:?}").into()),
        }
    }
    let mut event = CloudEvent::new("benchmark", "urn:forge:performance", "forge.benchmark");
    event.data = Some(b"boundary".to_vec());
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let started = Instant::now();
        let encoded = event.encode()?;
        let _ = CloudEvent::decode(&encoded)?;
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    let rank = samples
        .len()
        .saturating_mul(95)
        .div_ceil(100)
        .saturating_sub(1);
    let p95 = samples
        .get(rank)
        .copied()
        .unwrap_or(Duration::ZERO)
        .as_secs_f64()
        * 1000.0;
    let report = Report {
        schema_version: 1,
        kind: "language_boundary",
        language: "rust",
        iterations,
        metrics: [Metric {
            name: "cloudevent_roundtrip_p95_ms",
            value: p95,
            unit: "ms",
        }],
    };
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(output) = output {
        std::fs::write(output, format!("{json}\n"))?;
    } else {
        println!("{json}");
    }
    Ok(())
}
