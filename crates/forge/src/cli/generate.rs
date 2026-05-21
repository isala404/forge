use anyhow::Result;
use clap::Parser;
use console::style;
use std::path::Path;

use super::frontend_codegen::BindingGeneratorInput;
use super::frontend_target::FrontendTarget;
const FORGE_VERSION: &str = env!("CARGO_PKG_VERSION");
use super::ui;

/// Generate frontend bindings from backend source.
#[derive(Parser)]
pub struct GenerateCommand {
    /// Force regeneration even if files exist.
    #[arg(long)]
    pub force: bool,

    /// Output directory (defaults to frontend/src/lib/forge).
    #[arg(short, long)]
    pub output: Option<String>,

    /// Frontend target (`sveltekit` or `dioxus`). Defaults to auto-detection.
    #[arg(long)]
    pub target: Option<FrontendTarget>,

    /// Source directory to scan for models (defaults to src).
    #[arg(short, long)]
    pub src: Option<String>,

    /// Auto-accept prompts (useful for CI).
    #[arg(short = 'y', long)]
    pub yes: bool,
}

impl GenerateCommand {
    pub async fn execute(self) -> Result<()> {
        let root = super::project_root::enter_project_root()?;
        eprintln!(
            "  {} Project root: {}",
            ui::info(),
            style(root.display()).cyan()
        );

        let src_dir = self.src.unwrap_or_else(|| "src".to_string());
        let src_path = Path::new(&src_dir);

        let detected_target = self
            .target
            .or_else(|| FrontendTarget::detect(Path::new("frontend")))
            .unwrap_or(FrontendTarget::SvelteKit);
        let output_dir = self
            .output
            .unwrap_or_else(|| detected_target.default_output_dir().to_string());
        let output_path = Path::new(&output_dir);

        eprint!("  Scanning Rust source files...");
        let registry = if src_path.exists() {
            let outcome = forge_codegen::parse_project(src_path)?;
            eprintln!(" done");
            if !outcome.parse_failures.is_empty() {
                eprintln!();
                eprintln!(
                    "  {} {} source file(s) failed to parse; handlers in those files will be missing from bindings:",
                    ui::warn(),
                    outcome.parse_failures.len()
                );
                for (path, msg) in &outcome.parse_failures {
                    eprintln!("    - {}: {}", path.display(), msg);
                }
            }
            outcome.registry
        } else {
            eprintln!(" done");
            forge_core::schema::SchemaRegistry::new()
        };

        if let Err(errors) = forge_codegen::validate_registry(&registry) {
            eprintln!();
            eprintln!("  {} Unsupported types in handler signatures:", ui::error());
            for msg in &errors {
                eprintln!("    - {}", msg);
            }
            return Err(anyhow::anyhow!(
                "Cannot generate bindings: {} unsupported type(s) found",
                errors.len()
            ));
        }

        let has_schema = !registry.all_tables().is_empty()
            || !registry.all_enums().is_empty()
            || !registry.all_functions().is_empty();

        let schema_path = Path::new("forge.schema.json");
        let schema_json = forge_codegen::emit_schema_json(&registry)
            .map_err(|e| anyhow::anyhow!("Failed to serialize schema: {}", e))?;
        std::fs::write(schema_path, &schema_json)?;

        eprint!(
            "  Generating {} bindings...",
            detected_target.display_name()
        );
        detected_target.generate_bindings(&BindingGeneratorInput {
            output_dir: &output_dir,
            output_path,
            registry: &registry,
            has_schema,
            force: self.force,
        })?;
        eprintln!(" done");

        println!();
        if has_schema {
            let table_count = registry.all_tables().len();
            let enum_count = registry.all_enums().len();
            let function_count = registry.all_functions().len();
            println!(
                "  {} Generated bindings from {} models, {} enums, {} functions (v{})",
                ui::ok(),
                style(table_count).cyan(),
                style(enum_count).cyan(),
                style(function_count).cyan(),
                FORGE_VERSION
            );
        }
        println!("  {} Output: {}", ui::info(), style(&output_dir).cyan());
        println!();

        Ok(())
    }
}
