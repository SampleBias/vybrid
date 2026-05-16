use anyhow::Result;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

const DEFAULT_READ_FILE_BYTES: usize = 64 * 1024;
const MIN_READ_FILE_BYTES: usize = 4 * 1024;
const MAX_READ_FILE_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Default)]
pub struct FileReadCache {
    entries: Arc<Mutex<HashMap<PathBuf, CachedFile>>>,
}

#[derive(Debug, Clone)]
struct CachedFile {
    modified: Option<SystemTime>,
    len: u64,
    content: String,
}

impl FileReadCache {
    fn read_or_load(
        &self,
        path: &Path,
        modified: Option<SystemTime>,
        len: u64,
    ) -> Result<(String, String)> {
        let cached = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("file read cache lock poisoned"))?
            .get(path)
            .cloned();

        if let Some(cached) = cached {
            if cached.modified == modified && cached.len == len {
                return Ok((cached.content, "hit".to_string()));
            }
        }

        let content = fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", path.display(), e))?;
        self.entries
            .lock()
            .map_err(|_| anyhow::anyhow!("file read cache lock poisoned"))?
            .insert(
                path.to_path_buf(),
                CachedFile {
                    modified,
                    len,
                    content: content.clone(),
                },
            );
        Ok((content, "miss".to_string()))
    }
}

/// Normalize a file path (expand ~ and make absolute if needed)
pub fn normalize_path(path: &str) -> String {
    crate::project_context::resolve_path(path)
        .to_string_lossy()
        .to_string()
}

/// Read a single file
pub fn read_file(path: &str) -> Result<String> {
    read_file_with_options(path, None, None, None)
}

/// Read a single file with optional 1-based line range and byte cap.
pub fn read_file_with_options(
    path: &str,
    start_line: Option<usize>,
    line_count: Option<usize>,
    max_bytes: Option<usize>,
) -> Result<String> {
    read_file_with_options_cached(path, start_line, line_count, max_bytes, None)
}

/// Read a single file using a metadata-aware session cache when available.
pub fn read_file_with_options_cached(
    path: &str,
    start_line: Option<usize>,
    line_count: Option<usize>,
    max_bytes: Option<usize>,
    cache: Option<&FileReadCache>,
) -> Result<String> {
    let normalized = normalize_path(path);
    let file_path = Path::new(&normalized);
    if !file_path.exists() {
        return Err(anyhow::anyhow!(
            "{}",
            crate::project_context::path_not_found_message(path, file_path)
        ));
    }

    let metadata = fs::metadata(file_path)
        .map_err(|e| anyhow::anyhow!("Failed to stat '{}': {}", normalized, e))?;
    let modified = metadata.modified().ok();
    let len = metadata.len();
    let (content, cache_status) = if let Some(cache) = cache {
        cache.read_or_load(file_path, modified, len)?
    } else {
        (
            fs::read_to_string(&normalized)
                .map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", normalized, e))?,
            "uncached".to_string(),
        )
    };

    let total_lines = content.lines().count();
    let total_bytes = content.len();
    let start = start_line.unwrap_or(1).max(1);
    let count = line_count.unwrap_or(usize::MAX);
    let selected = content
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let line_no = idx + 1;
            (line_no >= start && line_no < start.saturating_add(count)).then_some(line)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let limit = max_bytes
        .unwrap_or(DEFAULT_READ_FILE_BYTES)
        .clamp(MIN_READ_FILE_BYTES, MAX_READ_FILE_BYTES);
    let (body, truncated) = truncate_utf8(&selected, limit);
    let display_path = crate::project_context::root_relative(file_path);

    let mut header = format!(
        "Content of '{}' (resolved: '{}', total_lines: {}, total_bytes: {}, returned_bytes: {}, cache: {}",
        path,
        display_path,
        total_lines,
        total_bytes,
        body.len(),
        cache_status
    );
    if start_line.is_some() || line_count.is_some() {
        let end = start
            .saturating_add(count)
            .saturating_sub(1)
            .min(total_lines);
        header.push_str(&format!(", returned_lines: {}-{}", start, end));
    }
    if truncated {
        header.push_str(", truncated: true");
    }
    header.push_str("):\n\n");

    Ok(format!("{header}{body}"))
}

/// Read multiple files
#[allow(dead_code)]
pub fn read_multiple_files(paths: &[&str]) -> Result<String> {
    read_multiple_files_cached(paths, None)
}

/// Read multiple files through the same metadata-aware cache.
pub fn read_multiple_files_cached(paths: &[&str], cache: Option<&FileReadCache>) -> Result<String> {
    let mut results = Vec::new();

    for path in paths {
        match read_file_with_options_cached(
            path,
            None,
            None,
            Some(DEFAULT_READ_FILE_BYTES / 2),
            cache,
        ) {
            Ok(content) => results.push(content),
            Err(e) => results.push(format!("Error reading '{}': {}", path, e)),
        }
    }

    let separator = format!("\n\n{}\n\n", "=".repeat(50));
    Ok(results.join(&separator))
}

/// Create or overwrite a file
pub fn create_file(path: &str, content: &str) -> Result<String> {
    let normalized = normalize_path(path);
    let file_path = Path::new(&normalized);

    // Create parent directories if they don't exist
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("Failed to create directories for '{}': {}", path, e))?;
    }

    // Write the file
    fs::write(&normalized, content)
        .map_err(|e| anyhow::anyhow!("Failed to write '{}': {}", path, e))?;

    Ok(format!("Created/updated file '{}'", path))
}

/// Create multiple files
pub fn create_multiple_files(files: &[(String, String)]) -> Result<String> {
    let mut created = Vec::new();
    let mut errors = Vec::new();

    for (path, content) in files {
        match create_file(path, content) {
            Ok(_) => created.push(path.clone()),
            Err(e) => errors.push(format!("{}: {}", path, e)),
        }
    }

    let mut result = String::new();

    if !created.is_empty() {
        result.push_str(&format!(
            "Created {} files: {}",
            created.len(),
            created.join(", ")
        ));
    }

    if !errors.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(&format!("Errors: {}", errors.join("; ")));
    }

    Ok(result)
}

/// Edit a file by replacing a snippet
#[allow(dead_code)]
pub fn edit_file(path: &str, original_snippet: &str, new_snippet: &str) -> Result<String> {
    edit_file_with_options(path, original_snippet, new_snippet, false)
}

/// Edit a file by replacing a snippet, optionally returning a preview without writing.
pub fn edit_file_with_options(
    path: &str,
    original_snippet: &str,
    new_snippet: &str,
    dry_run: bool,
) -> Result<String> {
    edit_file_with_context_options(path, original_snippet, new_snippet, dry_run, None, None)
}

/// Edit a file by replacing a snippet, optionally disambiguating repeated matches with context.
pub fn edit_file_with_context_options(
    path: &str,
    original_snippet: &str,
    new_snippet: &str,
    dry_run: bool,
    context_before: Option<&str>,
    context_after: Option<&str>,
) -> Result<String> {
    let normalized = normalize_path(path);

    // Read the file
    let content = fs::read_to_string(&normalized)
        .map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", path, e))?;

    // Check for the snippet
    let matches: Vec<usize> = content
        .match_indices(original_snippet)
        .map(|(idx, _)| idx)
        .collect();
    let count = matches.len();

    if count == 0 {
        return Err(anyhow::anyhow!(
            "Original snippet not found in '{}'. The file may have changed or the snippet doesn't match exactly.",
            path
        ));
    }

    let candidate_matches = filter_matches_by_context(
        &content,
        original_snippet,
        &matches,
        context_before,
        context_after,
    );

    if candidate_matches.len() != 1 {
        let context_hint = if context_before.is_some() || context_after.is_some() {
            " No occurrence matched the supplied context uniquely."
        } else {
            " Provide `context_before` and/or `context_after`, or include more surrounding lines in `original_snippet`."
        };
        return Err(anyhow::anyhow!(
            "Found {} occurrences of the snippet in '{}'.{} Candidate locations:\n{}",
            count,
            path,
            context_hint,
            occurrence_previews(&content, &candidate_matches, 3)
        ));
    }
    let match_start = candidate_matches[0];

    let preview = format!(
        "Edit preview for '{}':\n--- before\n{}\n--- after\n{}",
        path, original_snippet, new_snippet
    );

    if dry_run {
        return Ok(format!("Dry run: no file written.\n{}", preview));
    }

    // Write the updated content
    let mut updated_content = content;
    updated_content.replace_range(
        match_start..match_start + original_snippet.len(),
        new_snippet,
    );
    fs::write(&normalized, &updated_content)
        .map_err(|e| anyhow::anyhow!("Failed to write '{}': {}", path, e))?;

    Ok(format!("Successfully edited '{}'\n{}", path, preview))
}

fn filter_matches_by_context(
    content: &str,
    original_snippet: &str,
    matches: &[usize],
    context_before: Option<&str>,
    context_after: Option<&str>,
) -> Vec<usize> {
    const CONTEXT_WINDOW_BYTES: usize = 8 * 1024;

    matches
        .iter()
        .copied()
        .filter(|match_start| {
            let match_end = match_start + original_snippet.len();
            let before_start = content[..*match_start]
                .char_indices()
                .rev()
                .take_while(|(idx, _)| match_start.saturating_sub(*idx) <= CONTEXT_WINDOW_BYTES)
                .last()
                .map(|(idx, _)| idx)
                .unwrap_or(0);
            let after_end = content[match_end..]
                .char_indices()
                .take_while(|(idx, _)| *idx <= CONTEXT_WINDOW_BYTES)
                .last()
                .map(|(idx, ch)| match_end + idx + ch.len_utf8())
                .unwrap_or(match_end);
            let before_window = &content[before_start..*match_start];
            let after_window = &content[match_end..after_end];

            context_before
                .map(|context| before_window.contains(context))
                .unwrap_or(true)
                && context_after
                    .map(|context| after_window.contains(context))
                    .unwrap_or(true)
        })
        .collect()
}

fn occurrence_previews(content: &str, matches: &[usize], max_previews: usize) -> String {
    if matches.is_empty() {
        return "  (no candidate locations after applying context filters)".to_string();
    }

    let lines: Vec<&str> = content.lines().collect();
    let mut previews = Vec::new();
    for (occurrence_idx, match_start) in matches.iter().take(max_previews).enumerate() {
        let line_no = content[..*match_start]
            .bytes()
            .filter(|b| *b == b'\n')
            .count()
            + 1;
        let start_line = line_no.saturating_sub(2).max(1);
        let end_line = (line_no + 2).min(lines.len().max(1));
        let mut preview = format!("  occurrence {} at line {}:", occurrence_idx + 1, line_no);
        for current_line in start_line..=end_line {
            if let Some(line) = lines.get(current_line - 1) {
                preview.push_str(&format!("\n    {:>4}: {}", current_line, line));
            }
        }
        previews.push(preview);
    }

    if matches.len() > max_previews {
        previews.push(format!(
            "  ... {} more occurrence(s) omitted",
            matches.len() - max_previews
        ));
    }

    previews.join("\n")
}

/// Check if a file exists
pub fn file_exists(path: &str) -> bool {
    let normalized = normalize_path(path);
    Path::new(&normalized).exists()
}

fn truncate_utf8(content: &str, max_bytes: usize) -> (String, bool) {
    if content.len() <= max_bytes {
        return (content.to_string(), false);
    }
    let split = content
        .char_indices()
        .take_while(|(idx, _)| *idx < max_bytes)
        .last()
        .map(|(idx, ch)| idx + ch.len_utf8())
        .unwrap_or(0);
    (
        format!(
            "{}\n\n[File output truncated at {} bytes; read a narrower line range for more.]",
            &content[..split],
            max_bytes
        ),
        true,
    )
}

/// Append content to a file
pub fn append_to_file(path: &str, content: &str) -> Result<String> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let normalized = normalize_path(path);
    let file_path = Path::new(&normalized);

    // Create parent directories if they don't exist
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("Failed to create directories for '{}': {}", path, e))?;
    }

    // Open file in append mode
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&normalized)
        .map_err(|e| anyhow::anyhow!("Failed to open '{}' for appending: {}", path, e))?;

    file.write_all(content.as_bytes())
        .map_err(|e| anyhow::anyhow!("Failed to append to '{}': {}", path, e))?;

    Ok(format!("Appended content to '{}'", path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_file_dry_run_does_not_write() {
        let path =
            std::env::temp_dir().join(format!("vybrid-edit-dry-run-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "hello world").unwrap();

        let result = edit_file_with_options(path.to_str().unwrap(), "hello", "goodbye", true)
            .expect("dry-run edit should validate");
        let content = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert!(result.contains("Dry run"));
        assert_eq!(content, "hello world");
    }

    #[test]
    fn edit_file_rejects_ambiguous_snippet() {
        let path =
            std::env::temp_dir().join(format!("vybrid-edit-ambiguous-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "same\nsame\n").unwrap();

        let err = edit_file(path.to_str().unwrap(), "same", "new").unwrap_err();
        let _ = std::fs::remove_file(&path);

        assert!(err.to_string().contains("occurrences"));
    }

    #[test]
    fn edit_file_context_disambiguates_repeated_snippet() {
        let path = std::env::temp_dir().join(format!(
            "vybrid-edit-context-disambiguates-{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        std::fs::write(
            &path,
            "fn first() {\n    same\n}\n\nfn second() {\n    same\n}\n",
        )
        .unwrap();

        edit_file_with_context_options(
            path.to_str().unwrap(),
            "    same",
            "    changed",
            false,
            Some("fn second() {"),
            Some("\n}"),
        )
        .unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert!(content.contains("fn first() {\n    same\n}"));
        assert!(content.contains("fn second() {\n    changed\n}"));
    }

    #[test]
    fn read_file_can_return_line_range_with_cap() {
        let path =
            std::env::temp_dir().join(format!("vybrid-read-range-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "one\ntwo\nthree\nfour\n").unwrap();

        let result =
            read_file_with_options(path.to_str().unwrap(), Some(2), Some(2), Some(8)).unwrap();
        let _ = std::fs::remove_file(&path);

        assert!(result.contains("returned_lines: 2-3"));
        assert!(result.contains("two"));
        assert!(result.contains("three"));
        assert!(!result.contains("four"));
    }

    #[test]
    fn read_file_cache_hits_when_metadata_is_unchanged() {
        let path = std::env::temp_dir().join(format!(
            "vybrid-read-cache-hit-{}-{}.txt",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "cached body\n").unwrap();
        let cache = FileReadCache::default();

        let first =
            read_file_with_options_cached(path.to_str().unwrap(), None, None, None, Some(&cache))
                .unwrap();
        let second =
            read_file_with_options_cached(path.to_str().unwrap(), None, None, None, Some(&cache))
                .unwrap();
        let _ = std::fs::remove_file(&path);

        assert!(first.contains("cache: miss"));
        assert!(second.contains("cache: hit"));
        assert!(second.contains("cached body"));
    }

    #[test]
    fn read_file_cache_misses_after_file_changes() {
        let path = std::env::temp_dir().join(format!(
            "vybrid-read-cache-change-{}-{}.txt",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "first\n").unwrap();
        let cache = FileReadCache::default();

        read_file_with_options_cached(path.to_str().unwrap(), None, None, None, Some(&cache))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        std::fs::write(&path, "second and longer\n").unwrap();
        let changed =
            read_file_with_options_cached(path.to_str().unwrap(), None, None, None, Some(&cache))
                .unwrap();
        let _ = std::fs::remove_file(&path);

        assert!(changed.contains("cache: miss"));
        assert!(changed.contains("second and longer"));
    }
}
