use std::fs;

use vybrid::tools::cargo::{run_cargo, DiagnosticFormat};
use vybrid::tools::rust::{explain_rust_diagnostic, rust_project_snapshot};

fn temp_crate(name: &str, lib_rs: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
    )
    .unwrap();
    fs::write(root.join("src/lib.rs"), lib_rs).unwrap();
    root
}

#[tokio::test]
async fn run_cargo_json_diagnostics_find_trait_bound_errors() {
    let root = temp_crate(
        "vybrid_trait_bound_eval",
        "pub fn needs_display<T: std::fmt::Display>(value: T) -> String { value.to_string() }\n\
         pub struct NoDisplay;\n\
         pub fn demo() { let _ = needs_display(NoDisplay); }\n",
    );

    let output = run_cargo(
        "check",
        false,
        None,
        None,
        &[],
        Some(root.to_str().unwrap()),
        DiagnosticFormat::Json,
    )
    .await
    .unwrap();
    let _ = fs::remove_dir_all(&root);

    assert!(output.contains("Rust diagnostic summary"));
    assert!(output.contains("Display") || output.contains("trait"));
}

#[tokio::test]
async fn rust_project_snapshot_reports_targets() {
    let root = temp_crate("vybrid_snapshot_eval", "pub fn answer() -> u32 { 42 }\n");

    let snapshot = rust_project_snapshot(None, Some(root.to_str().unwrap()))
        .await
        .unwrap();
    let _ = fs::remove_dir_all(&root);

    assert!(snapshot.contains("Rust project snapshot"));
    assert!(snapshot.contains("vybrid_snapshot_eval"));
    assert!(snapshot.contains("lib"));
}

#[tokio::test]
async fn explain_rust_diagnostic_includes_builtin_hint() {
    let explanation = explain_rust_diagnostic("E0382").await.unwrap();
    assert!(explanation.contains("Moved value") || explanation.contains("use of moved value"));
}
