//! Golden snapshot tests for the code generator.
//!
//! Each fixture in `tests/fixtures/*.rs.txt` is parsed into a `SchemaRegistry`
//! and run through both TypeScript and Dioxus generators. The full file set
//! is concatenated into one snapshot per fixture. Drift in any emitted file
//! shows up as a diff.

use std::fs;
use std::path::{Path, PathBuf};

use forge_codegen::parser::parse_project;
use forge_codegen::{DioxusGenerator, GenerateOptions, TypeScriptGenerator};
use tempfile::TempDir;

#[test]
fn snapshot_primitives() {
    insta::assert_snapshot!(
        "primitives",
        run_fixture(include_str!("fixtures/primitives.rs.txt"), false)
    );
}

#[test]
fn snapshot_models_and_enums() {
    insta::assert_snapshot!(
        "models_and_enums",
        run_fixture(include_str!("fixtures/models_and_enums.rs.txt"), false)
    );
}

#[test]
fn snapshot_custom_args() {
    insta::assert_snapshot!(
        "custom_args",
        run_fixture(include_str!("fixtures/custom_args.rs.txt"), false)
    );
}

#[test]
fn snapshot_jobs_and_workflows() {
    insta::assert_snapshot!(
        "jobs_and_workflows",
        run_fixture(include_str!("fixtures/jobs_and_workflows.rs.txt"), false)
    );
}

#[test]
fn snapshot_upload() {
    insta::assert_snapshot!(
        "upload",
        run_fixture(include_str!("fixtures/upload.rs.txt"), false)
    );
}

#[test]
fn snapshot_full_app() {
    insta::assert_snapshot!(
        "full_app",
        run_fixture(include_str!("fixtures/full_app.rs.txt"), false)
    );
}

#[test]
fn snapshot_full_app_with_auth() {
    insta::assert_snapshot!(
        "full_app_with_auth",
        run_fixture(include_str!("fixtures/full_app.rs.txt"), true)
    );
}

/// Generate output is deterministic: running the same fixture twice yields
/// byte-identical output. Catches non-determinism in sort/dedup paths.
#[test]
fn output_is_deterministic() {
    let src = include_str!("fixtures/full_app.rs.txt");
    let first = run_fixture(src, true);
    let second = run_fixture(src, true);
    assert_eq!(first, second, "codegen output must be deterministic");
}

/// Negative coverage: a `#[forge::model]` on a tuple struct must produce a
/// parser diagnostic with a clear message, not silently emit an empty
/// interface. A regression that drops the named-fields guard would otherwise
/// let downstream tests pass with an empty `Wrapper {}` interface.
#[test]
fn tuple_struct_model_is_rejected_with_clear_diagnostic() {
    let src = r#"
        #[forge::model]
        pub struct Wrapper(pub String);
    "#;
    let src_dir = TempDir::new().expect("tempdir");
    fs::write(src_dir.path().join("handlers.rs"), src).expect("write fixture");

    let outcome = parse_project(src_dir.path()).expect("parse_project");
    assert!(
        !outcome.parse_failures.is_empty(),
        "tuple struct must be rejected by the parser, got no failures"
    );
    let (_, msg) = outcome
        .parse_failures
        .first()
        .expect("at least one failure");
    assert!(
        msg.contains("named fields"),
        "diagnostic must explain the constraint, got: {msg}"
    );
}

/// Same guard for unit structs marked as DTOs (serde derive).
#[test]
fn unit_struct_dto_is_rejected_with_clear_diagnostic() {
    let src = r#"
        #[derive(serde::Serialize, serde::Deserialize)]
        pub struct Marker;
    "#;
    let src_dir = TempDir::new().expect("tempdir");
    fs::write(src_dir.path().join("handlers.rs"), src).expect("write fixture");

    let outcome = parse_project(src_dir.path()).expect("parse_project");
    assert!(
        !outcome.parse_failures.is_empty(),
        "unit struct DTO must be rejected by the parser, got no failures"
    );
    let (_, msg) = outcome
        .parse_failures
        .first()
        .expect("at least one failure");
    assert!(
        msg.contains("named fields"),
        "diagnostic must explain the constraint, got: {msg}"
    );
}

fn run_fixture(source: &str, generate_auth: bool) -> String {
    let src_dir = TempDir::new().expect("tempdir");
    fs::write(src_dir.path().join("handlers.rs"), source).expect("write fixture");

    let outcome = parse_project(src_dir.path()).expect("parse_project");

    assert!(
        outcome.parse_failures.is_empty(),
        "parse failures: {:?}",
        outcome.parse_failures
    );

    let ts_out = TempDir::new().expect("ts tempdir");
    let opts = GenerateOptions {
        generate_auth_store: generate_auth,
    };
    TypeScriptGenerator::with_options(ts_out.path(), opts)
        .generate(&outcome.registry)
        .expect("ts generate");

    let dx_out = TempDir::new().expect("dioxus tempdir");
    DioxusGenerator::new(dx_out.path())
        .generate(&outcome.registry)
        .expect("dioxus generate");

    let mut snap = String::new();
    append_tree(&mut snap, "ts", ts_out.path());
    append_tree(&mut snap, "dioxus", dx_out.path());
    snap
}

fn append_tree(out: &mut String, label: &str, dir: &Path) {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .expect("read_dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    for path in files {
        let rel = path.strip_prefix(dir).expect("strip_prefix");
        let body = fs::read_to_string(&path).expect("read file");
        out.push_str(&format!("\n=== {}/{} ===\n", label, rel.display()));
        out.push_str(&body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
    }
}
