use std::fs;
use std::path::PathBuf;
use vybrid::client::groq::{FunctionCall, Message, ToolCall};
use vybrid::conversation::{Conversation, ToolResultWindow};
use vybrid::tools::{cargo, executor, file_ops, grep, output, shell};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("vybrid-regression-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, text).unwrap();
        path
    }
    fn store(&self) -> output::ToolOutputStore {
        output::ToolOutputStore::new(&self.0)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn call(id: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        call_type: "function".into(),
        function: FunctionCall {
            name: "read_file".into(),
            arguments: "{\"file_path\":\"src/lib.rs\"}".into(),
        },
    }
}

#[test]
fn oversized_latest_batch_preserves_every_call_and_result() {
    let mut c = Conversation::new("system");
    c.add_task_message("Preserve public API compatibility.");
    c.add_assistant_message(Message {
        role: "assistant".into(),
        content: None,
        tool_calls: Some((0..12).map(|i| call(&format!("call_{i}"))).collect()),
        tool_call_id: None,
    });
    for i in 0..12 {
        c.add_tool_result(&format!("call_{i}"), &"x".repeat(12_000));
    }
    let request = c.messages_for_request_with_budget(36_000);
    let ids: Vec<_> = request
        .iter()
        .filter_map(|m| m.tool_calls.as_ref())
        .flatten()
        .map(|call| call.id.as_str())
        .collect();
    assert_eq!(ids.len(), 12);
    assert_eq!(request.iter().filter(|m| m.role == "tool").count(), 12);
    for message in request.iter().filter(|m| m.role == "tool") {
        assert!(ids.contains(&message.tool_call_id.as_deref().unwrap()));
    }
    assert!(request
        .iter()
        .any(|m| m.content.as_deref() == Some("Preserve public API compatibility.")));
}

#[test]
fn oversized_file_batch_fits_both_budgets_and_reuses_exact_archives() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let mut window = ToolResultWindow::default();
    let mut c = Conversation::new("system");
    c.add_task_message("Keep both files.");
    c.add_assistant_message(Message {
        role: "assistant".into(),
        content: None,
        tool_call_id: None,
        tool_calls: Some(vec![call("a"), call("b")]),
    });
    let originals = ["α", "β"].map(|ch| ch.repeat(64 * 1024));
    c.add_tool_result("a", &originals[0]);
    c.add_tool_result("b", &originals[1]);
    let mut previous = usize::MAX;
    for budget in [36_000, 18_000] {
        let mut request = c.messages_for_request_with_budget(budget);
        assert!(window.fit(&mut request, budget, &store).unwrap() <= budget);
        let bytes = serde_json::to_vec(&request).unwrap().len();
        assert!(bytes < previous);
        previous = bytes;
        assert!(request
            .iter()
            .any(|m| m.content.as_deref() == Some("Keep both files.")));
        assert_eq!(
            request
                .iter()
                .filter_map(|m| m.tool_calls.as_ref())
                .flatten()
                .count(),
            2
        );
        let results: Vec<_> = request.iter().filter(|m| m.role == "tool").collect();
        assert_eq!(results.len(), 2);
        for (i, result) in results.into_iter().enumerate() {
            assert_eq!(result.tool_call_id.as_deref(), Some(["a", "b"][i]));
            let preview = result.content.as_deref().unwrap();
            let path = preview.split('`').nth(1).unwrap();
            assert_eq!(fs::read_to_string(path).unwrap(), originals[i]);
        }
    }
    assert_eq!(
        fs::read_dir(fixture.0.join("tool-results"))
            .unwrap()
            .count(),
        2
    );
    assert_eq!(
        c.messages.last().unwrap().content.as_ref(),
        Some(&originals[1])
    );
    let mut spacious = c.messages_for_request_with_budget(80_000);
    window.fit(&mut spacious, 80_000, &store).unwrap();
    assert_eq!(
        spacious.last().unwrap().content.as_ref(),
        Some(&originals[1])
    );
}

#[tokio::test]
async fn write_tools_require_explicit_text_and_accept_explicit_empty_strings() {
    use serde_json::{json, Value};
    let fixture = Fixture::new();
    let path = fixture.file("data.txt", "keep me");
    for (tool, key) in [
        ("create_file", "content"),
        ("edit_file", "new_snippet"),
        ("edit_file", "new_string"),
    ] {
        for bad in [None, Some(Value::Null), Some(json!(123))] {
            let mut args = json!({"file_path": path, "old_string": "keep"});
            if let Some(bad) = bad {
                args[key] = bad;
            }
            assert!(executor::execute_tool(tool, &args.to_string())
                .await
                .is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), "keep me");
        }
    }
    executor::execute_tool(
        "edit_file",
        &json!({"path":path, "original_snippet":"keep", "new_string":""}).to_string(),
    )
    .await
    .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), " me");
    executor::execute_tool(
        "create_file",
        &json!({"file_path":path, "content":""}).to_string(),
    )
    .await
    .unwrap();
    assert_eq!(fs::metadata(path).unwrap().len(), 0);
}

#[test]
fn grep_distinguishes_unsearched_inputs_from_a_genuine_miss() {
    let fixture = Fixture::new();
    let path = fixture.file("data.txt", "haystack");
    let missing = fixture.0.join("missing");
    let empty_glob = fixture.0.join("*.rs");
    for (input, hint) in [
        (&fixture.0, "**/*.rs"),
        (&missing, "missing"),
        (&empty_glob, "No files matched"),
    ] {
        let error = grep::enhanced_grep("needle", &[input.to_str().unwrap()], 0, true, 10)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("No files searched") && error.contains(hint),
            "{error}"
        );
    }
    let mixed = grep::enhanced_grep(
        "needle",
        &[path.to_str().unwrap(), missing.to_str().unwrap()],
        0,
        true,
        10,
    )
    .unwrap();
    assert!(
        mixed.contains("No matches found")
            && mixed.contains("in 1 file")
            && mixed.contains("missing")
    );
}

#[test]
fn cached_and_uncached_reads_resume_on_the_same_unicode_boundary() {
    let fixture = Fixture::new();
    let source = format!("start\n{}🦀{}", "a".repeat(249), "z".repeat(54));
    let path = fixture.file("data.txt", &source);
    let cache = file_ops::FileReadCache::default();
    for cache in [None, Some(&cache), Some(&cache)] {
        let first = file_ops::read_file_with_options_cached(
            path.to_str().unwrap(),
            Some(1),
            Some(2),
            Some(256),
            cache,
        )
        .unwrap();
        let (header, body) = first.split_once("\n\n").unwrap();
        let offset: usize = header
            .split("next_byte: ")
            .nth(1)
            .unwrap()
            .split(',')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(body.len(), offset);
        assert!(
            header.contains("returned_bytes: 255")
                && header.contains("returned_lines: 1-2")
                && header.contains("more: true")
        );
        let second = file_ops::read_streamed_range(
            path.to_str().unwrap(),
            1,
            None,
            Some(256),
            Some(offset as u64),
        )
        .unwrap();
        assert!(second.contains("more: false"));
        assert_eq!(
            format!("{body}{}", second.split_once("\n\n").unwrap().1),
            source
        );
    }
}

#[tokio::test]
async fn command_outcomes_use_the_real_exit_status_and_keep_output() {
    use serde_json::json;
    let fixture = Fixture::new();
    let runtime = executor::ToolRuntime {
        output_store: fixture.store(),
        ..Default::default()
    };
    for (cmd, code) in [
        ("echo 'Command failed (exit code 5)'", 0),
        ("echo diagnostic >&2; exit 7", 7),
    ] {
        let outcome = executor::execute_tool_outcome(
            "execute_bash_command",
            &json!({"command": cmd}).to_string(),
            &runtime,
        )
        .await
        .unwrap();
        assert_eq!(outcome.success(), code == 0);
        assert_eq!(outcome.exit_status.unwrap().code(), Some(code));
        assert!(outcome.output.contains(if code == 0 {
            "exit code 5"
        } else {
            "diagnostic"
        }));
    }
    let failed = executor::execute_tool_outcome(
        "run_cargo",
        &json!({"subcommand":"check", "manifest_path": fixture.0.join("missing.toml")}).to_string(),
        &runtime,
    )
    .await
    .unwrap();
    assert!(!failed.success());
    assert!(failed.output.contains("missing.toml") && failed.output.contains("exit code"));
}

#[test]
fn tasks_and_context_survive_windowing_without_duplicate_snapshots() {
    let mut c = Conversation::new("system");
    assert!(c.set_context_snapshot("project", "Original project docs"));
    for _ in 0..10 {
        assert!(!c.set_context_snapshot("project", "Original project docs"));
    }
    c.add_task_message("TASK: preserve the parser API");
    c.add_task_message("Keep the existing timeout behavior too.");
    for i in 0..50 {
        c.add_assistant_message(Message {
            role: "assistant".into(),
            content: Some(format!("round {i}: {}", "x".repeat(4_000))),
            tool_calls: None,
            tool_call_id: None,
        });
    }
    c.add_task_message("Continue; also preserve Unicode.");
    assert!(c.set_context_snapshot("project", "Updated project docs"));
    let request = c.messages_for_request_with_budget(8_000);
    for text in [
        "TASK: preserve the parser API",
        "Continue; also preserve Unicode.",
        "Updated project docs",
        "Keep the existing timeout behavior too.",
    ] {
        assert_eq!(
            request
                .iter()
                .filter(|m| m.content.as_deref() == Some(text))
                .count(),
            1
        );
    }
    assert!(!request
        .iter()
        .any(|m| m.content.as_deref() == Some("Original project docs")));
}

#[test]
fn manual_summary_never_deletes_unrepresented_middle_history() {
    let mut c = Conversation::new("system");
    for i in 0..50 {
        c.add_user_message(&format!("DECISION_{i} {}", "x".repeat(1_900)));
    }
    let (first_kept, transcript) = c.compactable_transcript(8).unwrap();
    assert!(first_kept < 43);
    for message in &c.messages[1..first_kept] {
        assert!(transcript.contains(message.content.as_deref().unwrap()));
    }
    assert!(c.apply_manual_compaction("Accurate summary of the supplied range", first_kept));
    assert!(c
        .messages
        .iter()
        .any(|m| m.content.as_deref().unwrap_or("").contains("DECISION_35")));
}

#[test]
fn empty_or_expanding_summaries_leave_history_unchanged() {
    let mut c = Conversation::new("system");
    for i in 0..30 {
        c.add_user_message(&format!("message {i}"));
    }
    let before = serde_json::to_string(&c.messages).unwrap();
    assert!(!c.apply_manual_compaction("", 10));
    assert!(!c.apply_manual_compaction(&"x".repeat(8_000), 10));
    assert_eq!(before, serde_json::to_string(&c.messages).unwrap());
}

#[test]
fn compaction_headroom_preserves_subsequent_request_prefixes() {
    let mut c = Conversation::new("system");
    for i in 0..100 {
        c.add_user_message(&format!("message {i}: {}", "x".repeat(1_000)));
    }
    let before = c.messages_for_request_with_budget(10_000).into_owned();
    for i in 0..5 {
        c.add_user_message(&format!("new {i}: {}", "y".repeat(1_000)));
    }
    let after = c.messages_for_request_with_budget(10_000).into_owned();
    assert_eq!(
        serde_json::to_string(&before).unwrap(),
        serde_json::to_string(&after[..before.len()]).unwrap()
    );
}

#[test]
fn grep_counts_hits_separately_from_context() {
    let fixture = Fixture::new();
    let spaced = (0..20)
        .map(|i| format!("needle {i}\na\nb\nc\nd\n"))
        .collect::<String>();
    let path = fixture.file("spaced.txt", &spaced);
    let result = grep::enhanced_grep("needle", &[path.to_str().unwrap()], 1, true, 10).unwrap();
    assert!(result.starts_with("Found 10 match"));
    let path = fixture.file("dense.txt", &"needle\n".repeat(20));
    let result = grep::enhanced_grep("needle", &[path.to_str().unwrap()], 5, true, 2).unwrap();
    assert!(result.starts_with("Found 2 match"));
    assert_eq!(
        result.lines().filter(|line| line.starts_with('>')).count(),
        2
    );
}

#[test]
fn grep_deduplicates_overlapping_paths_and_globs() {
    let fixture = Fixture::new();
    let path = fixture.file("search.txt", "needle\n");
    let glob = fixture.0.join("*.txt");
    let result = grep::enhanced_grep(
        "needle",
        &[path.to_str().unwrap(), glob.to_str().unwrap()],
        0,
        true,
        10,
    )
    .unwrap();
    assert!(result.starts_with("Found 1 match"));
}

#[test]
fn large_file_reads_are_bounded_and_support_utf8_byte_continuation() {
    let fixture = Fixture::new();
    let prefix = "x".repeat(2 * 1024 * 1024);
    let path = fixture.file("long.txt", &format!("{prefix}café 🦀"));
    let first =
        file_ops::read_file_with_options(path.to_str().unwrap(), None, None, Some(1024)).unwrap();
    assert!(first.contains("cache: streamed"));
    assert!(first.len() < 1400);
    let last = file_ops::read_streamed_range(
        path.to_str().unwrap(),
        1,
        None,
        Some(1024),
        Some(prefix.len() as u64),
    )
    .unwrap();
    assert!(last.ends_with("café 🦀"));
    assert!(last.contains("more: false"));
}

#[tokio::test]
async fn explicit_file_ranges_are_not_offloaded_or_shortened_again() {
    let fixture = Fixture::new();
    let source = "line\n".repeat(14_000);
    let path = fixture.file("source.txt", &source);
    let runtime = executor::ToolRuntime {
        output_store: fixture.store(),
        ..Default::default()
    };
    let result = executor::execute_tool_with_context("read_file", &serde_json::json!({"file_path": path, "start_line": 1, "line_count": 14_000, "max_bytes": 80_000}).to_string(), &runtime).await.unwrap();
    assert!(result.ends_with(&source));
    assert!(!result.contains("offloaded"));
    let mut c = Conversation::new("system");
    c.add_tool_result("call", &result);
    assert_eq!(
        c.messages.last().unwrap().content.as_deref(),
        Some(result.as_str())
    );
}

#[tokio::test]
async fn noisy_process_capture_retains_raw_bytes_and_bounded_preview() {
    use tokio::io::AsyncReadExt;
    let fixture = Fixture::new();
    let capture = output::capture_pipe(
        tokio::io::repeat(b'x').take(4 * 1024 * 1024),
        fixture.store(),
        "test-output",
        false,
    )
    .await
    .unwrap();
    assert_eq!(capture.bytes, 4 * 1024 * 1024);
    assert!(capture.text.len() < 9000);
    let artifact = capture.artifact.unwrap();
    assert_eq!(fs::metadata(artifact).unwrap().len(), capture.bytes);
}

#[tokio::test]
async fn small_archived_output_preserves_the_entire_unicode_preview() {
    let fixture = Fixture::new();
    for repeats in [100, 1500, 2000, 2728] {
        let source = format!("BEGIN{}END", "€".repeat(repeats));
        let capture =
            output::capture_pipe(source.as_bytes(), fixture.store(), "small-output", true)
                .await
                .unwrap();
        assert!(capture.text.ends_with(&source));
        assert_eq!(
            fs::read(capture.artifact.unwrap()).unwrap(),
            source.as_bytes()
        );
    }
}

#[tokio::test]
async fn unicode_shell_output_and_failure_tail_remain_visible() {
    let fixture = Fixture::new();
    let result = shell::execute_bash_with_store(
        "printf '€%.0s' {1..40000}; printf 'FAILURE_SENTINEL\\n' >&2; exit 7",
        None,
        Some(fixture.0.to_str().unwrap()),
        &fixture.store(),
    )
    .await
    .unwrap();
    assert!(result.contains("exit code 7"));
    assert!(result.contains("FAILURE_SENTINEL"));
    assert!(!result.contains('�'));
    assert!(result.len() < 12_000);
}

#[tokio::test]
async fn cargo_json_returns_diagnostics_and_archives_raw_events() {
    let fixture = Fixture::new();
    fixture.file(
        "Cargo.toml",
        "[package]\nname='regression_fixture'\nversion='0.1.0'\nedition='2021'\n",
    );
    fixture.file(
        "src/lib.rs",
        "pub fn broken() { let s = String::new(); drop(s); println!(\"{}\", s); }\n",
    );
    let result = cargo::run_cargo_with_store(
        "check",
        false,
        None,
        None,
        &[],
        Some(fixture.0.to_str().unwrap()),
        cargo::DiagnosticFormat::Json,
        &fixture.store(),
    )
    .await
    .unwrap();
    assert!(result.contains("E0382"));
    assert!(result.contains("Raw Cargo output:"));
    assert!(!result.contains("\"reason\":\"compiler-message\""));
    let raw = fs::read_dir(fixture.0.join("tool-results"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| fs::read_to_string(entry.path()).unwrap())
        .collect::<String>();
    assert!(raw.contains("compiler-message"));
}
