use anyhow::{Context, Result};
use console::style;
use forge_core::util::to_pascal_case;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::ui;
use crate::template_vars;

/// Kinds of handlers `forge new` can scaffold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandlerKind {
    Query,
    Mutation,
    Job,
    Cron,
    Workflow,
    Daemon,
    Webhook,
    McpTool,
    Model,
    Enum,
}

impl HandlerKind {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "query" => Some(Self::Query),
            "mutation" => Some(Self::Mutation),
            "job" => Some(Self::Job),
            "cron" => Some(Self::Cron),
            "workflow" => Some(Self::Workflow),
            "daemon" => Some(Self::Daemon),
            "webhook" => Some(Self::Webhook),
            "mcp_tool" | "mcp-tool" => Some(Self::McpTool),
            "model" => Some(Self::Model),
            "enum" => Some(Self::Enum),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Mutation => "mutation",
            Self::Job => "job",
            Self::Cron => "cron",
            Self::Workflow => "workflow",
            Self::Daemon => "daemon",
            Self::Webhook => "webhook",
            Self::McpTool => "mcp_tool",
            Self::Model => "model",
            Self::Enum => "enum",
        }
    }

    fn template_source(self) -> &'static str {
        match self {
            Self::Query => include_str!("handler_templates/query.rs.tmpl"),
            Self::Mutation => include_str!("handler_templates/mutation.rs.tmpl"),
            Self::Job => include_str!("handler_templates/job.rs.tmpl"),
            Self::Cron => include_str!("handler_templates/cron.rs.tmpl"),
            Self::Workflow => include_str!("handler_templates/workflow.rs.tmpl"),
            Self::Daemon => include_str!("handler_templates/daemon.rs.tmpl"),
            Self::Webhook => include_str!("handler_templates/webhook.rs.tmpl"),
            Self::McpTool => include_str!("handler_templates/mcp_tool.rs.tmpl"),
            Self::Model => include_str!("handler_templates/model.rs.tmpl"),
            Self::Enum => include_str!("handler_templates/enum.rs.tmpl"),
        }
    }

    fn target_dir(self) -> &'static str {
        match self {
            Self::Model | Self::Enum => "src/schema",
            _ => "src/functions",
        }
    }

    fn next_steps_hint(self) -> &'static str {
        match self {
            Self::Query | Self::Mutation => {
                "Update the SQL, then run `forge check` and `forge generate`."
            }
            Self::Job => "Wire dispatches from a `transactional` mutation; run `forge check`.",
            Self::Cron => "Adjust the schedule; run `forge check`.",
            Self::Workflow => {
                "Define your steps and compensation handlers; run `forge check` and `forge generate`."
            }
            Self::Daemon => "Implement the run loop; honour `ctx.is_shutdown_requested()`.",
            Self::Webhook => "Set the webhook secret env var; run `forge check`.",
            Self::McpTool => "Document the tool's purpose; run `forge check` and `forge generate`.",
            Self::Model | Self::Enum => "Run `forge migrate prepare` then `forge generate`.",
        }
    }
}

const RESERVED_RUST_KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try",
];

fn validate_handler_name(name: &str) -> Result<()> {
    if name.is_empty() {
        anyhow::bail!("handler name cannot be empty");
    }
    let Some(first) = name.chars().next() else {
        anyhow::bail!("handler name cannot be empty");
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        anyhow::bail!("invalid handler name '{name}': must start with a letter or underscore");
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        anyhow::bail!(
            "invalid handler name '{name}': only letters, digits, and underscore allowed (snake_case recommended)"
        );
    }
    if RESERVED_RUST_KEYWORDS.contains(&name) {
        anyhow::bail!(
            "'{name}' is a reserved Rust keyword. Try '{name}_' or a more descriptive name."
        );
    }
    Ok(())
}

pub async fn scaffold_handler(kind: HandlerKind, raw_name: &str) -> Result<()> {
    validate_handler_name(raw_name)?;

    let snake = to_snake_case(raw_name);
    let pascal = to_pascal_case(&snake);
    let upper = snake.to_uppercase();

    let root = super::project_root::enter_project_root()?;
    ui::section("FORGE Scaffold");
    println!(
        "  {} Project root: {}",
        ui::info(),
        style(root.display()).cyan()
    );

    let target_dir = PathBuf::from(kind.target_dir());
    fs::create_dir_all(&target_dir)
        .with_context(|| format!("failed to create {}", target_dir.display()))?;

    let target_file = target_dir.join(format!("{snake}.rs"));
    let vars = template_vars!("name" => snake.as_str(), "Name" => pascal.as_str(), "NAME" => upper.as_str());
    let rendered = super::template::render(kind.template_source(), &vars);

    let mut f = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target_file)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => anyhow::bail!(
            "{} already exists. Pick a different name or delete the file first.",
            target_file.display()
        ),
        Err(e) => {
            return Err(anyhow::Error::new(e))
                .with_context(|| format!("failed to write {}", target_file.display()));
        }
    };
    f.write_all(rendered.as_bytes())
        .with_context(|| format!("failed to write {}", target_file.display()))?;
    println!(
        "  {} Created {}",
        ui::ok(),
        style(target_file.display()).cyan()
    );

    ensure_mod_entry(&target_dir, &snake)?;
    if kind.target_dir() == "src/functions" {
        ensure_main_includes_functions()?;
    }

    rustfmt_files(&[target_file.as_path(), &target_dir.join("mod.rs")]);

    println!();
    println!("  {} {}", ui::info(), kind.next_steps_hint());
    Ok(())
}

/// Best-effort rustfmt pass; formatting is a polish step, not a blocker.
fn rustfmt_files(paths: &[&Path]) {
    let existing: Vec<&Path> = paths.iter().copied().filter(|p| p.exists()).collect();
    if existing.is_empty() {
        return;
    }
    let _ = std::process::Command::new("rustfmt")
        .arg("--edition")
        .arg("2024")
        .args(existing)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

use forge_core::util::to_snake_case;

fn ensure_mod_entry(dir: &Path, name: &str) -> Result<()> {
    let mod_path = dir.join("mod.rs");
    let line = format!("pub mod {name};");
    if mod_path.exists() {
        let content = fs::read_to_string(&mod_path)?;
        if content.lines().any(|l| l.trim() == line) {
            println!(
                "  {} {} already lists `{}`",
                ui::info(),
                mod_path.display(),
                line
            );
            return Ok(());
        }
        let mut updated = content;
        if !updated.ends_with('\n') {
            updated.push('\n');
        }
        updated.push_str(&line);
        updated.push('\n');
        fs::write(&mod_path, updated)?;
        println!(
            "  {} Appended `{}` to {}",
            ui::ok(),
            line,
            mod_path.display()
        );
    } else {
        fs::write(&mod_path, format!("{line}\n"))?;
        println!(
            "  {} Created {} with `{}`",
            ui::ok(),
            mod_path.display(),
            line
        );
    }
    Ok(())
}

/// Make sure `src/main.rs` exposes the relevant module so auto-registration finds the handler.
fn ensure_main_includes_functions() -> Result<()> {
    let main = Path::new("src/main.rs");
    if !main.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(main)?;
    let already = content.lines().any(|l| {
        let l = l.trim();
        l == "mod functions;" || l == "pub mod functions;"
    });
    if already {
        return Ok(());
    }
    let mut updated = String::with_capacity(content.len() + 32);
    updated.push_str("mod functions;\n");
    updated.push_str(&content);
    fs::write(main, updated)?;
    println!(
        "  {} Inserted `mod functions;` into src/main.rs (required for auto-registration)",
        ui::warn()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::Mutex;

    // The process has a single cwd. Tests that mutate it must hold this lock,
    // otherwise parallel test runners interleave their cd's and trash each
    // other's reads.
    static CWD_LOCK: Mutex<()> = Mutex::new(());

    fn with_cwd<R>(dir: &Path, f: impl FnOnce() -> R) -> R {
        let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let original = env::current_dir().expect("cwd");
        env::set_current_dir(dir).expect("chdir");
        let result = f();
        env::set_current_dir(original).expect("restore cwd");
        result
    }

    #[test]
    fn validate_rejects_keywords() {
        assert!(validate_handler_name("type").is_err());
        assert!(validate_handler_name("move").is_err());
        assert!(validate_handler_name("list_items").is_ok());
    }

    #[test]
    fn validate_rejects_bad_chars() {
        assert!(validate_handler_name("").is_err());
        assert!(validate_handler_name("1abc").is_err());
        assert!(validate_handler_name("kebab-case").is_err());
        assert!(validate_handler_name("ok_name").is_ok());
    }

    #[test]
    fn pascal_case() {
        assert_eq!(to_pascal_case("list_items"), "ListItems");
        assert_eq!(to_pascal_case("foo"), "Foo");
        assert_eq!(to_pascal_case("a_b_c"), "ABC");
    }

    #[test]
    fn snake_case_normalises_inputs() {
        assert_eq!(to_snake_case("Invoice"), "invoice");
        assert_eq!(to_snake_case("InvoiceStatus"), "invoice_status");
        assert_eq!(to_snake_case("listInvoices"), "list_invoices");
        assert_eq!(to_snake_case("HTTPServer"), "http_server");
        assert_eq!(to_snake_case("foo2Bar"), "foo2_bar");
        assert_eq!(to_snake_case("already_snake"), "already_snake");
    }

    #[test]
    fn ensure_mod_entry_creates_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("src/functions");
        fs::create_dir_all(&dir).expect("mkdir");
        ensure_mod_entry(&dir, "foo").expect("create");
        let content = fs::read_to_string(dir.join("mod.rs")).expect("read mod.rs");
        assert!(content.contains("pub mod foo;"));
    }

    #[test]
    fn ensure_mod_entry_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("src/functions");
        fs::create_dir_all(&dir).expect("mkdir");
        fs::write(dir.join("mod.rs"), "pub mod foo;\n").expect("seed mod.rs");
        ensure_mod_entry(&dir, "foo").expect("idempotent");
        let content = fs::read_to_string(dir.join("mod.rs")).expect("read mod.rs");
        assert_eq!(content.matches("pub mod foo;").count(), 1);
    }

    #[test]
    fn ensure_main_inserts_mod_functions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).expect("mkdir");
        fs::write(src.join("main.rs"), "fn main() {}\n").expect("seed main.rs");
        with_cwd(tmp.path(), || {
            ensure_main_includes_functions().expect("insert");
        });
        let content = fs::read_to_string(src.join("main.rs")).expect("read main.rs");
        assert!(content.starts_with("mod functions;\n"));
    }

    #[test]
    fn ensure_main_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).expect("mkdir");
        fs::write(src.join("main.rs"), "mod functions;\nfn main() {}\n").expect("seed main.rs");
        with_cwd(tmp.path(), || {
            ensure_main_includes_functions().expect("idempotent");
        });
        let content = fs::read_to_string(src.join("main.rs")).expect("read main.rs");
        assert_eq!(content.matches("mod functions;").count(), 1);
    }
}
