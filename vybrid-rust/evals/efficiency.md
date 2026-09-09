# Efficiency and capability evaluation

Run `cargo fmt --check`, `cargo test --locked`, and `cargo clippy --all-targets -- -D warnings` before a release build. `tests/efficiency_regressions.rs` covers history boundaries, snapshot deduplication, search limits, file continuation, raw log preservation, and compiler summaries. Unit tests cover transport byte splits, LSP synchronization, shared file buffers, and attempt accounting. `tests/provider_regressions.rs` uses a local loopback HTTP server to check fragmented Unicode tool arguments, unexpected stream EOF, and incomplete summary rejection; its environment must allow loopback sockets. It makes no external API calls.

For a local metadata log, launch with:

```sh
VYBRID_METRICS_FILE=/tmp/vybrid-candidate.jsonl ./target/release/vybrid
```

Request records include `task_id`, model, phase (`generation` or `compaction`), finish reason, status, elapsed/first-chunk milliseconds, estimated tokens, and provider-reported prompt, cached, completion, and total tokens. Missing usage stays null, including failed requests without usage. Tool records report execution timing and presented bytes. A task returning successfully means the agent loop returned normally; it is not an independent verification of patch correctness. The baseline commit did not collect these metrics, so an instrumented baseline or provider usage export is needed for a fair paid-model comparison.

Compare the reviewed baseline (`a1b1346`) and candidate with the same model, reasoning settings, tool-round limit, repository fixture, and task. Run multiple repetitions and keep cold/warm provider-cache cases separate. Include all retries and summary requests. Pricing uses separate rates for uncached input, cached input, and output; the rate-limit helper `billable_tokens` is not a cost formula.

Use the eight [Rust capability scenarios](rust_agent_scenarios.md), plus:

- A task that continues beyond 36k estimated history, with a compatibility constraint introduced early and another in a follow-up.
- Repeated short requests with unchanged 8k-character docs and 4k-character memory context, followed by a docs change and directory switch.
- A multi-file edit with relevant material in middle files and a late failing test diagnostic.
- A large source/log file containing a very long line and multibyte identifiers.
- A compiler run with large JSON artifact output and an actual compiler error.
- A local-tool implementation under mocked rate-limit and interrupted-stream responses.
- An LSP hover/diagnostic query, an edit, then a second query of the same file.

Compare task completion, patch correctness, required tests, total input/output tokens, cache hits, retry/summary costs, repeated reads, tool rounds, first useful output, total elapsed time, and peak process memory. Local deterministic tests do not establish a production speedup or dollar-saving percentage. Paid-model runs require a workload and spending limit chosen by the operator.
