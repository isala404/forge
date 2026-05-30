use anyhow::Result;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SqlxCacheCheck {
    Missing,
    Empty,
    Ready(usize),
}

/// Decide if `.sqlx/` is stale relative to source. Returns `Some(reason)` if so.
pub(super) fn sqlx_cache_staleness(sqlx_dir: &Path, src_dir: &Path) -> Result<Option<String>> {
    if !sqlx_dir.exists() {
        return Ok(Some(".sqlx/ missing".to_string()));
    }

    let entries: Vec<_> = match std::fs::read_dir(sqlx_dir) {
        Ok(it) => it.flatten().collect(),
        Err(e) => return Ok(Some(format!(".sqlx/ unreadable: {e}"))),
    };
    if entries.is_empty() {
        return Ok(Some(".sqlx/ empty".to_string()));
    }

    let cache_oldest = entries
        .iter()
        .filter(|e| e.file_name().to_string_lossy().starts_with("query-"))
        .filter_map(|e| e.metadata().ok())
        .filter_map(|m| m.modified().ok())
        .min();

    if cache_oldest.is_none() {
        return Ok(Some(".sqlx/ has no query entries".to_string()));
    }

    let mut newest_src: Option<std::time::SystemTime> = None;
    let mut stack = vec![src_dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if let Ok(it) = std::fs::read_dir(&path) {
                for e in it.flatten() {
                    stack.push(e.path());
                }
            }
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs")
            && let Ok(modified) = meta.modified()
        {
            newest_src = Some(newest_src.map(|n| n.max(modified)).unwrap_or(modified));
        }
    }

    if let (Some(src), Some(cache)) = (newest_src, cache_oldest)
        && src > cache
    {
        return Ok(Some("Rust source newer than .sqlx/".to_string()));
    }

    Ok(None)
}

/// Resolve `DATABASE_URL`, preferring the env var, then `forge.toml [database].url`
/// (with `${VAR}` substitution applied).
pub(super) fn resolve_database_url(config_path: &str) -> Result<String> {
    if let Ok(url) = std::env::var("DATABASE_URL")
        && !url.is_empty()
    {
        return Ok(url);
    }
    let path = Path::new(config_path);
    if !path.exists() {
        anyhow::bail!("DATABASE_URL not set and {} not found", config_path);
    }
    let cfg = forge_core::config::ForgeConfig::from_file(config_path)
        .map_err(|e| anyhow::anyhow!("failed to load {config_path}: {e}"))?;
    Ok(cfg.database.url().to_string())
}

pub(super) fn project_uses_compile_time_sqlx_macros(src_dir: &Path) -> Result<bool> {
    if !src_dir.exists() {
        return Ok(false);
    }

    for entry in std::fs::read_dir(src_dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            if project_uses_compile_time_sqlx_macros(&path)? {
                return Ok(true);
            }
            continue;
        }

        if !file_type.is_file() || path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }

        let content = std::fs::read_to_string(&path)?;
        if file_uses_sqlx_macros(&content) {
            return Ok(true);
        }
    }

    Ok(false)
}

pub(super) fn file_uses_sqlx_macros(content: &str) -> bool {
    const NEEDLES: &[&str] = &[
        "sqlx::query!(",
        "sqlx::query_as!(",
        "sqlx::query_scalar!(",
        "sqlx::query_file!(",
        "sqlx::query_file_as!(",
    ];
    let stripped = strip_comments(content);
    stripped.lines().any(|line| {
        let code = match line.split_once("//") {
            Some((before, _)) => before,
            None => line,
        };
        NEEDLES.iter().any(|needle| code.contains(needle))
    })
}

/// Replace `/* ... */` block comments with whitespace of the same length so
/// line/column offsets are preserved. Nested comments are not supported (Rust
/// allows them but they are vanishingly rare and don't affect the heuristic).
#[allow(clippy::indexing_slicing)]
fn strip_comments(content: &str) -> String {
    let bytes = content.as_bytes();
    let mut out = String::with_capacity(content.len());
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            // Skip block comment, preserving newlines so line-comment stripping later still works.
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i = (i + 2).min(bytes.len());
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

pub(super) fn inspect_sqlx_cache(sqlx_dir: &Path) -> Result<SqlxCacheCheck> {
    if !sqlx_dir.exists() {
        return Ok(SqlxCacheCheck::Missing);
    }

    let query_file_count = std::fs::read_dir(sqlx_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("query-"))
        .count();

    if query_file_count == 0 {
        Ok(SqlxCacheCheck::Empty)
    } else {
        Ok(SqlxCacheCheck::Ready(query_file_count))
    }
}
