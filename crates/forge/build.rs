use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR")
            .map_err(|e| io::Error::other(format!("CARGO_MANIFEST_DIR: {e}")))?,
    );
    let out_dir =
        PathBuf::from(env::var("OUT_DIR").map_err(|e| io::Error::other(format!("OUT_DIR: {e}")))?);

    let embedded_examples_dir = out_dir.join("examples");

    if embedded_examples_dir.exists() {
        fs::remove_dir_all(&embedded_examples_dir)?;
    }
    fs::create_dir_all(&embedded_examples_dir)?;

    if let Some(examples_dir) = find_examples_dir(&manifest_dir)? {
        build_bundle_from_examples(&examples_dir, &embedded_examples_dir)?;
        return Ok(());
    }

    let archive_path = manifest_dir.join("generated/examples.tar");
    println!("cargo:rerun-if-changed={}", archive_path.display());

    if archive_path.exists() {
        let archive = fs::File::open(&archive_path)?;
        let mut archive = tar::Archive::new(archive);
        archive.unpack(&embedded_examples_dir)?;
        return Ok(());
    }

    Err(io::Error::other(
        "could not find examples directory or generated examples archive",
    ))
}

fn build_bundle_from_examples(examples_dir: &Path, bundle_dir: &Path) -> io::Result<()> {
    for framework_dir in fs::read_dir(examples_dir)? {
        let framework_dir = framework_dir?;
        let framework_path = framework_dir.path();
        if !framework_path.is_dir() {
            continue;
        }

        let Some(framework_name) = framework_path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        if !framework_name.starts_with("with-") {
            continue;
        }

        for template_dir in fs::read_dir(&framework_path)? {
            let template_dir = template_dir?;
            let template_path = template_dir.path();
            if !template_path.is_dir() {
                continue;
            }

            let Some(template_name) = template_path.file_name() else {
                continue;
            };

            copy_template_tree(
                &template_path,
                &bundle_dir.join(framework_name).join(template_name),
            )?;
        }
    }
    Ok(())
}

fn find_examples_dir(manifest_dir: &Path) -> io::Result<Option<PathBuf>> {
    let candidates = [
        manifest_dir.join("../../examples"),
        manifest_dir.join("examples"),
    ];

    for candidate in candidates {
        if candidate.is_dir() {
            register_rerun_paths(&candidate)?;
            return Ok(Some(candidate));
        }
    }

    Ok(None)
}

fn register_rerun_paths(root: &Path) -> io::Result<()> {
    println!("cargo:rerun-if-changed={}", root.display());

    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            println!("cargo:rerun-if-changed={}", path.display());
            if path.is_dir() {
                register_rerun_paths(&path)?;
            }
        }
    }
    Ok(())
}

fn copy_template_tree(src: &Path, dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    copy_dir_contents(src, dest, Path::new(""))
}

fn copy_dir_contents(src: &Path, dest: &Path, relative: &Path) -> io::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let entry_path = entry.path();
        let entry_name = entry.file_name();
        let relative_path = relative.join(&entry_name);

        if should_exclude(&relative_path) {
            continue;
        }

        let dest_path = dest.join(&entry_name);
        if entry_path.is_dir() {
            fs::create_dir_all(&dest_path)?;
            copy_dir_contents(&entry_path, &dest_path, &relative_path)?;
        } else {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&entry_path, &dest_path)?;
        }
    }
    Ok(())
}

fn should_exclude(relative_path: &Path) -> bool {
    const EXACT_FILES: &[&str] = &[".forge-dev-integration.log"];
    const PATH_COMPONENTS: &[&str] = &[
        ".git",
        "pg_data",
        "target",
        "node_modules",
        ".svelte-kit",
        "build",
        "dist",
        "playwright-report",
        "test-results",
        "skills",
    ];

    if let Some(file_name) = relative_path.file_name().and_then(|name| name.to_str())
        && EXACT_FILES.contains(&file_name)
    {
        return true;
    }

    relative_path.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        PATH_COMPONENTS.iter().any(|excluded| *excluded == name)
    })
}
