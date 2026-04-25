# Rust Agent Evaluation Scenarios

Use these scenarios to manually or automatically score Vybrid's Rust assistance.

## Scoring Rubric

- Tool sequence: inspects crate shape, reads relevant files, runs `run_cargo`, and verifies after editing.
- Root cause: identifies the primary rustc/clippy diagnostic instead of chasing cascaded errors.
- Rust quality: uses idiomatic ownership, trait, enum, lifetime, async, and error-handling patterns.
- Verification: reaches `cargo check` or `cargo test` success when a code fix is requested.
- Explanation: explains the Rust-specific trade-off clearly at the requested depth.

## Scenarios

1. Moved value (`E0382`): a `String` is moved into a helper and used again. Expected behavior: explain ownership transfer and choose borrow, clone, or ownership restructuring based on API intent.
2. Overlapping borrows (`E0499`/`E0502`): a mutable borrow is attempted while an immutable borrow is live. Expected behavior: shorten the immutable borrow scope before introducing cloning or interior mutability.
3. Lifetime relation: a function returns one of two input references without a named lifetime. Expected behavior: describe reference relationships and add the minimal lifetime annotation.
4. Trait bound failure: a generic helper calls `.to_string()` without `Display`/`ToString` bounds. Expected behavior: add the correct bound at the behavior boundary.
5. Enum exhaustiveness: a new enum variant breaks a `match`. Expected behavior: handle the variant explicitly unless a catch-all is intentionally part of the API.
6. Async `Send`: a non-`Send` value is held across `.await` in a spawned task. Expected behavior: shorten the scope or switch to `Arc`/async-aware synchronization as appropriate.
7. Error conversion: a function using `?` lacks a `From` conversion into its public error type. Expected behavior: add contextual conversion at the boundary, not stringly typed errors everywhere.
8. Clippy refactor: clippy reports needless clones or manual combinators. Expected behavior: apply the lint only when it preserves readability and ownership intent.
