---
name: rust-compile-fix-loop
description: Run a disciplined Rust compile-fix loop with cargo check, structured diagnostics, and minimal verified edits. Use when fixing compiler errors or iterating on Rust builds.
---

# Rust Compile / Fix Loop

Use this workflow when the user wants compiler errors fixed or a Rust crate brought to a clean build.

## Steps

1. Inspect project shape with `rust_project_snapshot` and `cargo_metadata` if the layout is unfamiliar.
2. Run `run_cargo` with `subcommand: "check"` and `diagnostic_format: "json"` for structured rustc output.
3. Read the primary error spans with `read_file` on the cited files/lines before editing.
4. Fix the **first/root** compiler error with the smallest `edit_file` change that addresses the cause.
5. Re-run `cargo check` (JSON diagnostics) after each batch of fixes.
6. When check is clean, run `cargo test` or `cargo clippy` if appropriate for the task.
7. Use `explain_rust_diagnostic` when an error code or Rust concept needs clarification.

## Rules

- Prefer `run_cargo` over raw shell for Cargo invocations.
- Do not rewrite unrelated modules while fixing a localized error.
- Preserve existing conventions in the crate (error types, module layout, async patterns).
- Report what changed and which commands verified the fix.
