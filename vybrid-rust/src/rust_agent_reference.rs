//! Canonical Rust/Cargo guidance for the agent system prompt and human reference.

/// Common Cargo invocations for Rust projects (prefer `run_cargo` tool over ad-hoc shell).
pub const RUST_CARGO_QUICKREF: &str = r#"Quick reference (invoke via `run_cargo` with matching `subcommand` and `extra_args` as needed):
- `check` — Fast typecheck without producing a final binary; use for tight edit/compile loops.
- `build` / `build` + release — Full compile; `--release` for optimized artifacts.
- `test` — Run tests; filter with extra args e.g. a test name or `-- --nocapture` for println.
- `clippy` — Lints; often `clippy -- -D warnings` in CI (pass extra_args accordingly).
- `fmt` — `fmt` with `--check` in extra_args for CI-style verification without writing files.
- `doc` — `doc` / `doc --open` is interactive; prefer `doc --no-deps` in extra_args when appropriate.
- Workspaces: pass `package` for `-p <crate>`; use `manifest_path` for `--manifest-path` to a specific Cargo.toml.
- Examples: `subcommand: "check"`; `subcommand: "test"`, `extra_args: ["my_module"]`; `subcommand: "clippy"`, `extra_args: ["--", "-D", "warnings"]`."#;

/// Iterative workflow after editing Rust code.
pub const RUST_COMPILE_FIX_LOOP: &str = r#"Compile / fix loop (Rust):
1. Prefer `cargo check` first — fastest feedback while fixing errors.
2. Fix compiler **errors** (not warnings) from top to bottom; rustc order matters when errors cascade.
3. After it type-checks, run `cargo test` (and `cargo clippy` when appropriate).
4. For iteration speed, use `check` repeatedly; use full `build` when you need linked artifacts or release mode.
5. Read stderr carefully: error codes (E0xxx), file paths, and line numbers are authoritative."#;

/// Compact hints for frequent rustc themes (not a substitute for the Rust book).
pub const RUST_DIAGNOSTICS_HINTS: &str = r#"Common rustc themes:
- **Borrowing (E0499, E0502, E0505, …):** One mutable xor many immutable refs; shorten scopes, clone(), or use interior mutability (`RefCell`) when appropriate.
- **Moved value (E0382):** Value used after move; clone, borrow, or restructure ownership.
- **Lifetimes:** Elision covers many cases; add named lifetimes when references in signatures must relate; `'static` only when truly required.
- **Async / .await:** Holds across await points must be `Send` where required; watch `Rc<RefCell<>>` vs `Arc<Mutex<>>` for shared state across tasks.
- **Error handling:** Prefer `?` with `From` / `context()`; map errors at boundaries.
- **Traits / dyn:** Object safety for `dyn Trait`; use generics or `impl Trait` when possible for simpler bounds."#;
