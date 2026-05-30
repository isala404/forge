use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use include_dir::{Dir, File, include_dir};
use serde::Deserialize;

use super::frontend_target::FrontendTarget;

static EMBEDDED_EXAMPLES: Dir<'_> = include_dir!("$OUT_DIR/examples");

const SUPPORTED_TEMPLATE_IDS: &[&str] = &[
    "with-svelte/minimal",
    "with-svelte/demo",
    "with-svelte/realtime-todo-list",
    "with-dioxus/minimal",
    "with-dioxus/demo",
    "with-dioxus/realtime-todo-list",
];

#[derive(Debug, Clone, Deserialize)]
pub struct TemplateMetadata {
    pub template_id: String,
    pub frontend: String,
    pub canonical_internal_slug: String,
    #[serde(default)]
    pub rewrite_paths: Vec<String>,
    #[serde(default)]
    pub copy_exclude: Vec<String>,
    #[serde(default, rename = "replacement")]
    pub replacements: Vec<TemplateReplacement>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TemplateReplacement {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub files: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TemplateDefinition {
    pub id: String,
    pub frontend: FrontendTarget,
    pub canonical_internal_slug: String,
    pub rewrite_paths: Vec<String>,
    pub copy_exclude: Vec<String>,
    pub replacements: Vec<TemplateReplacement>,
}

#[derive(Debug, Clone)]
pub struct BundledTemplateFile<'a> {
    pub relative_path: PathBuf,
    pub file: &'a File<'a>,
}

impl TemplateDefinition {
    pub fn bundled_files(&self) -> Result<Vec<BundledTemplateFile<'static>>> {
        let dir = EMBEDDED_EXAMPLES
            .get_dir(self.id.as_str())
            .ok_or_else(|| anyhow!("embedded examples missing directory '{}'", self.id))?;

        let mut files = Vec::new();
        collect_files(dir, Path::new(""), &mut files);
        Ok(files)
    }

    pub fn bundled_directories(&self) -> Result<Vec<PathBuf>> {
        let dir = EMBEDDED_EXAMPLES
            .get_dir(self.id.as_str())
            .ok_or_else(|| anyhow!("embedded examples missing directory '{}'", self.id))?;

        let mut directories = Vec::new();
        collect_directories(dir, Path::new(""), &mut directories);
        Ok(directories)
    }

    pub fn rewrite_file(&self, relative_path: &Path) -> bool {
        let relative = relative_path.to_string_lossy();
        self.rewrite_paths
            .iter()
            .any(|path| path == relative.as_ref())
    }

    pub fn should_exclude(&self, relative_path: &Path) -> bool {
        if relative_path == Path::new(".forge-template.toml") {
            return true;
        }

        self.copy_exclude
            .iter()
            .any(|entry| path_matches(relative_path, entry))
    }
}

pub fn supported_template_ids() -> &'static [&'static str] {
    SUPPORTED_TEMPLATE_IDS
}

pub fn load_template_definition(id: &str) -> Result<TemplateDefinition> {
    if !SUPPORTED_TEMPLATE_IDS.contains(&id) {
        return Err(anyhow!("unknown template '{}'", id));
    }

    let metadata_file = EMBEDDED_EXAMPLES
        .get_file(format!("{id}/.forge-template.toml").as_str())
        .ok_or_else(|| anyhow!("template metadata missing for '{}'", id))?;

    let metadata: TemplateMetadata = toml::from_str(
        metadata_file
            .contents_utf8()
            .ok_or_else(|| anyhow!("template metadata for '{}' is not valid UTF-8", id))?,
    )
    .with_context(|| format!("failed to parse template metadata for '{id}'"))?;

    if metadata.template_id != id {
        return Err(anyhow!(
            "template metadata mismatch for '{}': found '{}'",
            id,
            metadata.template_id
        ));
    }

    let frontend = metadata
        .frontend
        .parse()
        .map_err(|err: String| anyhow!("invalid frontend target for '{}': {}", id, err))?;

    Ok(TemplateDefinition {
        id: metadata.template_id,
        frontend,
        canonical_internal_slug: metadata.canonical_internal_slug,
        rewrite_paths: metadata.rewrite_paths,
        copy_exclude: metadata.copy_exclude,
        replacements: metadata.replacements,
    })
}

fn collect_files<'a>(dir: &'a Dir<'a>, prefix: &Path, files: &mut Vec<BundledTemplateFile<'a>>) {
    for file in dir.files() {
        let file_name = file
            .path()
            .file_name()
            .expect("bundled file should have a file name");
        files.push(BundledTemplateFile {
            relative_path: prefix.join(file_name),
            file,
        });
    }

    for child in dir.dirs() {
        let dir_name = child
            .path()
            .file_name()
            .expect("bundled directory should have a name");
        collect_files(child, &prefix.join(dir_name), files);
    }
}

fn collect_directories(dir: &Dir<'_>, prefix: &Path, directories: &mut Vec<PathBuf>) {
    for child in dir.dirs() {
        let dir_name = child
            .path()
            .file_name()
            .expect("bundled directory should have a name");
        let child_path = prefix.join(dir_name);
        directories.push(child_path.clone());
        collect_directories(child, &child_path, directories);
    }
}

/// Matches `entry` against `path` as a relative-path prefix.
///
/// Exact match wins. Otherwise, `entry` is treated as a path prefix anchored
/// at the relative root (e.g. `node_modules` matches `node_modules/foo` but
/// not `frontend/node_modules`). Matching any component anywhere in the path
/// was too aggressive and silently excluded unrelated files.
fn path_matches(path: &Path, entry: &str) -> bool {
    let entry_path = Path::new(entry);
    if path == entry_path {
        return true;
    }

    path.starts_with(entry_path)
}
