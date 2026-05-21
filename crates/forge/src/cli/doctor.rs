use anyhow::Result;
use clap::Parser;
use console::style;
use std::path::Path;
use std::time::Duration;

use super::frontend_target::FrontendTarget;
use super::ui;

/// One-shot environment check for Forge development.
///
/// Verifies the dev environment is set up correctly and reports any
/// remediation steps. Returns a non-zero exit code if any check fails.
#[derive(Parser)]
pub struct DoctorCommand {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckStatus {
    Ok,
    Warn,
    Fail,
    Skip,
}

struct Report {
    failures: usize,
    warnings: usize,
}

impl Report {
    fn new() -> Self {
        Self {
            failures: 0,
            warnings: 0,
        }
    }

    fn record(&mut self, status: CheckStatus, label: &str, detail: &str, remedy: Option<&str>) {
        let prefix = match status {
            CheckStatus::Ok => format!("{}", ui::ok()),
            CheckStatus::Warn => {
                self.warnings += 1;
                format!("{}", ui::warn())
            }
            CheckStatus::Fail => {
                self.failures += 1;
                format!("{}", ui::error())
            }
            CheckStatus::Skip => format!("{}", ui::info()),
        };
        println!("  {} {}: {}", prefix, style(label).bold(), detail);
        if let Some(r) = remedy
            && (status == CheckStatus::Warn || status == CheckStatus::Fail)
        {
            println!("      fix: {}", style(r).cyan());
        }
    }
}

impl DoctorCommand {
    pub async fn execute(self) -> Result<()> {
        let root = super::project_root::enter_project_root().ok();

        ui::section("FORGE Doctor");
        if let Some(ref r) = root {
            println!(
                "  {} Project root: {}",
                ui::info(),
                style(r.display()).cyan()
            );
        } else {
            println!(
                "  {} Not inside a Forge project — running environment-only checks",
                ui::warn()
            );
        }

        let mut report = Report::new();

        check_rust_toolchain(&mut report, root.as_deref()).await;
        check_cargo_sqlx(&mut report);
        check_sqlx_offline(&mut report);
        check_database_url(&mut report).await;
        check_docker(&mut report).await;
        check_frontend_tooling(&mut report).await;

        if let Some(ref root) = root {
            check_forge_toml(&mut report, root);
            check_sqlx_cache_freshness(&mut report, root);
            check_latest_migration_markers(&mut report, root);
        }

        println!();
        if report.failures == 0 && report.warnings == 0 {
            println!("{} Environment looks healthy.", ui::ok());
            Ok(())
        } else if report.failures == 0 {
            println!(
                "{} Environment is usable with {} warning(s).",
                ui::warn(),
                report.warnings
            );
            Ok(())
        } else {
            println!(
                "{} {} check(s) failed, {} warning(s).",
                ui::error(),
                report.failures,
                report.warnings
            );
            Err(anyhow::anyhow!(
                "forge doctor: {} failure(s)",
                report.failures
            ))
        }
    }
}

async fn check_rust_toolchain(report: &mut Report, root: Option<&Path>) {
    let required = required_rust_version(root);
    let output = tokio::process::Command::new("rustc")
        .arg("--version")
        .output()
        .await;
    match output {
        Ok(out) if out.status.success() => {
            let banner = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let version = banner.split_whitespace().nth(1).unwrap_or("");
            if version_meets(version, &required) {
                report.record(CheckStatus::Ok, "rustc", &banner, None);
            } else {
                report.record(
                    CheckStatus::Fail,
                    "rustc",
                    &format!("found {version}, need >= {required}"),
                    Some("rustup update stable"),
                );
            }
        }
        _ => report.record(
            CheckStatus::Fail,
            "rustc",
            "not found on PATH",
            Some("Install Rust from https://rustup.rs"),
        ),
    }
}

fn check_cargo_sqlx(report: &mut Report) {
    let ok = super::project_root::cargo_sqlx_available();
    if ok {
        report.record(CheckStatus::Ok, "cargo-sqlx", "installed", None);
    } else {
        report.record(
            CheckStatus::Fail,
            "cargo-sqlx",
            "missing",
            Some("cargo install sqlx-cli --no-default-features --features postgres"),
        );
    }
}

fn check_sqlx_offline(report: &mut Report) {
    match std::env::var("SQLX_OFFLINE").as_deref() {
        Ok("true" | "1") => report.record(CheckStatus::Ok, "SQLX_OFFLINE", "true", None),
        Ok(other) => report.record(
            CheckStatus::Warn,
            "SQLX_OFFLINE",
            &format!("set to {other:?}, expected \"true\""),
            Some("export SQLX_OFFLINE=true"),
        ),
        Err(_) => report.record(
            CheckStatus::Warn,
            "SQLX_OFFLINE",
            "not set — raw `cargo check` will misbehave",
            Some("export SQLX_OFFLINE=true (or eval \"$(forge env)\")"),
        ),
    }
}

async fn check_database_url(report: &mut Report) {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            report.record(
                CheckStatus::Warn,
                "DATABASE_URL",
                "not set",
                Some("export DATABASE_URL=postgres://user:pass@host:port/db"),
            );
            return;
        }
    };
    let (host, port) = match parse_pg_host_port(&url) {
        Some(hp) => hp,
        None => {
            report.record(
                CheckStatus::Fail,
                "DATABASE_URL",
                "not parseable",
                Some("Use postgres://user:pass@host:port/db"),
            );
            return;
        }
    };

    let connect = tokio::time::timeout(
        Duration::from_secs(1),
        tokio::net::TcpStream::connect((host.as_str(), port)),
    )
    .await;
    match connect {
        Ok(Ok(_)) => report.record(
            CheckStatus::Ok,
            "DATABASE_URL",
            &format!("reachable at {host}:{port}"),
            None,
        ),
        Ok(Err(e)) => report.record(
            CheckStatus::Fail,
            "DATABASE_URL",
            &format!("{host}:{port} refused: {e}"),
            Some("Start your PG instance or update DATABASE_URL"),
        ),
        Err(_) => report.record(
            CheckStatus::Fail,
            "DATABASE_URL",
            &format!("{host}:{port} timed out after 1s"),
            Some("Check the host is reachable and DATABASE_URL is correct"),
        ),
    }
}

async fn check_docker(report: &mut Report) {
    let needs_docker = Path::new("docker-compose.yml").exists()
        || Path::new("compose.yml").exists()
        || Path::new("compose.yaml").exists();
    if !needs_docker {
        report.record(
            CheckStatus::Skip,
            "docker",
            "not used by this project",
            None,
        );
        return;
    }
    if super::project_root::docker_available().await {
        report.record(CheckStatus::Ok, "docker", "daemon reachable", None);
    } else {
        report.record(
            CheckStatus::Fail,
            "docker",
            "daemon not reachable",
            Some("Start Docker Desktop or `colima start`"),
        );
    }
}

async fn check_frontend_tooling(report: &mut Report) {
    let frontend = Path::new("frontend");
    if !frontend.exists() {
        report.record(
            CheckStatus::Skip,
            "frontend",
            "no frontend/ directory",
            None,
        );
        return;
    }
    match FrontendTarget::detect(frontend) {
        Some(FrontendTarget::SvelteKit) => {
            probe_version(report, "bun", "Install bun from https://bun.sh").await;
        }
        Some(FrontendTarget::Dioxus) => {
            probe_version(report, "dx", "cargo install dioxus-cli").await;
        }
        None => report.record(
            CheckStatus::Skip,
            "frontend",
            "no recognised frontend target in frontend/",
            None,
        ),
    }
}

async fn probe_version(report: &mut Report, bin: &str, remedy: &str) {
    match tokio::process::Command::new(bin)
        .arg("--version")
        .output()
        .await
    {
        Ok(out) if out.status.success() => {
            let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let label = if bin == "dx" { "dioxus-cli" } else { bin };
            report.record(CheckStatus::Ok, label, &v, None);
        }
        _ => {
            let label = if bin == "dx" { "dioxus-cli" } else { bin };
            report.record(CheckStatus::Fail, label, "not found on PATH", Some(remedy));
        }
    }
}

fn check_forge_toml(report: &mut Report, root: &Path) {
    let path = root.join("forge.toml");
    match forge_core::config::ForgeConfig::from_file(&path) {
        Ok(_) => report.record(CheckStatus::Ok, "forge.toml", "parses cleanly", None),
        Err(e) => report.record(
            CheckStatus::Fail,
            "forge.toml",
            &format!("invalid: {e}"),
            Some("Fix the reported field or syntax error"),
        ),
    }
}

fn check_sqlx_cache_freshness(report: &mut Report, root: &Path) {
    let cache = root.join(".sqlx");
    if !cache.exists() {
        report.record(
            CheckStatus::Warn,
            ".sqlx/",
            "missing",
            Some("forge migrate prepare"),
        );
        return;
    }
    let cache_oldest = std::fs::read_dir(&cache)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("query-"))
        .filter_map(|e| e.metadata().ok().and_then(|m| m.modified().ok()))
        .min();
    let src_mtime = newest_mtime_under(&root.join("src"), |p| {
        p.extension().and_then(|s| s.to_str()) == Some("rs")
    });
    match (cache_oldest, src_mtime) {
        (Some(cm), Some(sm)) if sm > cm => report.record(
            CheckStatus::Warn,
            ".sqlx/",
            "stale (Rust sources newer than cache)",
            Some("forge migrate prepare"),
        ),
        (Some(_), _) => report.record(CheckStatus::Ok, ".sqlx/", "fresh", None),
        _ => report.record(CheckStatus::Skip, ".sqlx/", "could not stat", None),
    }
}

fn check_latest_migration_markers(report: &mut Report, root: &Path) {
    let dir = root.join("migrations");
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => {
            report.record(CheckStatus::Skip, "migrations/", "no migrations/ dir", None);
            return;
        }
    };
    let mut latest: Option<std::path::PathBuf> = None;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) == Some("sql")
            && latest.as_ref().map(|l| p > *l).unwrap_or(true)
        {
            latest = Some(p);
        }
    }
    let Some(path) = latest else {
        report.record(CheckStatus::Skip, "migrations/", "empty", None);
        return;
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            report.record(
                CheckStatus::Fail,
                "migrations/",
                &format!("read error: {e}"),
                None,
            );
            return;
        }
    };
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("(unknown)");
    if content.contains("-- @up") || !content.trim().is_empty() {
        report.record(
            CheckStatus::Ok,
            "latest migration",
            &format!("{name} present"),
            None,
        );
    } else {
        report.record(
            CheckStatus::Fail,
            "latest migration",
            &format!("{name} is empty"),
            Some("Migration file must contain SQL"),
        );
    }
}

fn parse_pg_host_port(url: &str) -> Option<(String, u16)> {
    let rest = url
        .strip_prefix("postgres://")
        .or_else(|| url.strip_prefix("postgresql://"))?;
    let rest = rest.rsplit_once('@').map(|(_, r)| r).unwrap_or(rest);
    let host_port = rest.split(['/', '?']).next().unwrap_or(rest);
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(5432)),
        None => (host_port.to_string(), 5432),
    };
    if host.is_empty() {
        return None;
    }
    Some((host, port))
}

fn required_rust_version(root: Option<&Path>) -> String {
    let cargo_toml = root.map_or_else(
        || Path::new("Cargo.toml").to_path_buf(),
        |r| r.join("Cargo.toml"),
    );
    let parsed = std::fs::read_to_string(&cargo_toml)
        .ok()
        .and_then(|c| toml::from_str::<toml::Value>(&c).ok());
    let extract = |v: &toml::Value, keys: &[&str]| -> Option<String> {
        let mut cursor = v;
        for k in keys {
            cursor = cursor.get(*k)?;
        }
        cursor.as_str().map(String::from)
    };
    parsed
        .as_ref()
        .and_then(|v| {
            extract(v, &["workspace", "package", "rust-version"])
                .or_else(|| extract(v, &["package", "rust-version"]))
        })
        .unwrap_or_else(|| "1.92".to_string())
}

fn version_meets(found: &str, required: &str) -> bool {
    fn parts(s: &str) -> Vec<u32> {
        s.split('.').filter_map(|x| x.parse().ok()).collect()
    }
    let f = parts(found);
    let r = parts(required);
    for i in 0..r.len().max(f.len()) {
        let a = f.get(i).copied().unwrap_or(0);
        let b = r.get(i).copied().unwrap_or(0);
        if a > b {
            return true;
        }
        if a < b {
            return false;
        }
    }
    true
}

fn newest_mtime_under(
    path: &Path,
    filter: impl Fn(&Path) -> bool,
) -> Option<std::time::SystemTime> {
    let mut newest: Option<std::time::SystemTime> = None;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(meta) = std::fs::metadata(&p) else {
            continue;
        };
        if meta.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&p) {
                for e in entries.flatten() {
                    stack.push(e.path());
                }
            }
        } else if filter(&p)
            && let Ok(modified) = meta.modified()
        {
            newest = Some(newest.map(|n| n.max(modified)).unwrap_or(modified));
        }
    }
    newest
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn parse_pg_host_port_basic() {
        assert_eq!(
            parse_pg_host_port("postgres://u:p@localhost:5432/db"),
            Some(("localhost".into(), 5432))
        );
        assert_eq!(
            parse_pg_host_port("postgresql://localhost/db"),
            Some(("localhost".into(), 5432))
        );
        assert_eq!(
            parse_pg_host_port("postgres://u:p@db.internal:6543/db?sslmode=require"),
            Some(("db.internal".into(), 6543))
        );
        assert_eq!(parse_pg_host_port("not a url"), None);
    }

    #[test]
    fn parse_pg_host_port_handles_no_userinfo_and_no_db() {
        // No userinfo segment, default port.
        assert_eq!(
            parse_pg_host_port("postgres://host"),
            Some(("host".into(), 5432))
        );
        // No userinfo, explicit port, no db.
        assert_eq!(
            parse_pg_host_port("postgres://host:6432"),
            Some(("host".into(), 6432))
        );
    }

    #[test]
    fn parse_pg_host_port_unparseable_port_falls_back_to_default() {
        // `xyz` is not a valid port, so the parser falls back to 5432.
        assert_eq!(
            parse_pg_host_port("postgres://h:xyz/db"),
            Some(("h".into(), 5432))
        );
    }

    #[test]
    fn parse_pg_host_port_rejects_empty_host() {
        // Empty host after the `://` separator is meaningless — reject it.
        assert_eq!(parse_pg_host_port("postgres:///db"), None);
        assert_eq!(parse_pg_host_port("postgres://@:5432/db"), None);
    }

    #[test]
    fn parse_pg_host_port_strips_query_string() {
        assert_eq!(
            parse_pg_host_port("postgres://h:5433?sslmode=require"),
            Some(("h".into(), 5433))
        );
    }

    #[test]
    fn version_meets_basic() {
        assert!(version_meets("1.92.0", "1.92"));
        assert!(version_meets("1.93.0", "1.92"));
        assert!(version_meets("2.0.0", "1.92"));
        assert!(!version_meets("1.91.0", "1.92"));
        assert!(!version_meets("1.0", "1.92"));
        assert!(version_meets("1.92", "1.92"));
    }

    #[test]
    fn version_meets_handles_trailing_garbage_after_full_match() {
        // Dotted segments that don't parse are dropped, so any trailing tag
        // attached to a later segment gets truncated. As long as enough
        // numeric components match before the bad one, the comparison passes.
        assert!(version_meets("1.92.0-nightly", "1.92"));
        // But if the bad token replaces a required component, the missing
        // component is treated as 0 and the comparison fails.
        assert!(!version_meets("1.93-beta", "1.92"));
    }

    #[test]
    fn version_meets_treats_empty_found_as_zero() {
        // No digits at all -> treated as 0.0.0, which is less than 1.92.
        assert!(!version_meets("", "1.92"));
    }

    #[test]
    fn report_counters_increment_correctly() {
        let mut r = Report::new();
        assert_eq!(r.failures, 0);
        assert_eq!(r.warnings, 0);

        r.record(CheckStatus::Ok, "a", "ok detail", None);
        r.record(CheckStatus::Warn, "b", "warn detail", Some("do x"));
        r.record(CheckStatus::Skip, "c", "skip detail", None);
        r.record(CheckStatus::Fail, "d", "fail detail", Some("do y"));
        r.record(CheckStatus::Fail, "e", "another fail", None);

        // Only Warn and Fail bump counters.
        assert_eq!(r.warnings, 1);
        assert_eq!(r.failures, 2);
    }

    #[test]
    fn required_rust_version_reads_workspace_then_package_then_fallback() {
        use std::fs;
        use tempfile::tempdir;

        // 1. workspace.package.rust-version wins.
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            r#"[workspace.package]
rust-version = "1.99"
"#,
        )
        .unwrap();
        assert_eq!(required_rust_version(Some(dir.path())), "1.99");

        // 2. Falls back to package.rust-version when no workspace section.
        let dir2 = tempdir().unwrap();
        fs::write(
            dir2.path().join("Cargo.toml"),
            r#"[package]
name = "thing"
rust-version = "1.80"
"#,
        )
        .unwrap();
        assert_eq!(required_rust_version(Some(dir2.path())), "1.80");

        // 3. Defaults to 1.92 when nothing is declared.
        let dir3 = tempdir().unwrap();
        fs::write(
            dir3.path().join("Cargo.toml"),
            r#"[package]
name = "thing"
"#,
        )
        .unwrap();
        assert_eq!(required_rust_version(Some(dir3.path())), "1.92");
    }

    #[test]
    fn newest_mtime_under_finds_filtered_files_recursively() {
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let sub = dir.path().join("inner");
        fs::create_dir_all(&sub).unwrap();
        fs::write(dir.path().join("keep.rs"), "fn a(){}").unwrap();
        fs::write(sub.join("also.rs"), "fn b(){}").unwrap();
        // Non-matching extension must be ignored.
        fs::write(sub.join("README.md"), "ignored").unwrap();

        let mtime = newest_mtime_under(dir.path(), |p| {
            p.extension().and_then(|s| s.to_str()) == Some("rs")
        });
        assert!(mtime.is_some(), "should find at least one .rs file");
    }

    #[test]
    fn newest_mtime_under_returns_none_when_nothing_matches() {
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        fs::write(dir.path().join("only.txt"), "x").unwrap();
        let mtime = newest_mtime_under(dir.path(), |p| {
            p.extension().and_then(|s| s.to_str()) == Some("rs")
        });
        assert!(mtime.is_none());
    }

    #[test]
    fn newest_mtime_under_handles_missing_path() {
        let mtime = newest_mtime_under(Path::new("/definitely/not/a/real/path"), |_| true);
        assert!(mtime.is_none());
    }
}
