#![allow(dead_code)]

use anyhow::Result;
use glob::glob;
use regex::RegexBuilder;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

const MAX_GREP_OUTPUT_BYTES: usize = 64 * 1024;

fn truncate_output(output: &str, max_bytes: usize) -> String {
    super::output::truncate_utf8_middle(output, max_bytes, "Grep output")
}

/// Enhanced grep functionality with context and formatting
pub fn enhanced_grep(
    pattern: &str,
    file_paths: &[&str],
    context_lines: usize,
    case_sensitive: bool,
    max_matches: usize,
) -> Result<String> {
    // Build the regex
    let regex = RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|e| anyhow::anyhow!("Invalid regex pattern '{}': {}", pattern, e))?;

    let mut results = Vec::new();
    let mut total_matches = 0;
    let mut searched = 0;

    // Traverse globs lazily so the match limit also bounds directory traversal.
    let mut visited = HashSet::new();

    let max_matches = max_matches.max(1);
    'inputs: for input in file_paths {
        for entry in expand_path(input)? {
            let path = match entry {
                Ok(path) => path,
                Err(reason) => {
                    results.push(reason);
                    continue;
                }
            };
            let path_str = path.to_string_lossy();

            if !visited.insert(path.canonicalize().unwrap_or_else(|_| path.clone())) {
                continue;
            }

            // Read file content
            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    results.push(format!("Error reading '{}': {}", path_str, e));
                    continue;
                }
            };

            let lines: Vec<&str> = content.lines().collect();
            searched += 1;
            let mut file_matches = Vec::new();
            let mut matched_lines: std::collections::HashSet<usize> =
                std::collections::HashSet::new();
            let mut actual_matches = std::collections::HashSet::new();

            // Find all matching lines
            for (line_num, line) in lines.iter().enumerate() {
                if regex.is_match(line) {
                    if total_matches + actual_matches.len() >= max_matches {
                        break;
                    }
                    matched_lines.insert(line_num);
                    actual_matches.insert(line_num);

                    // Add context lines
                    let start = line_num.saturating_sub(context_lines);
                    let end = line_num
                        .saturating_add(context_lines)
                        .saturating_add(1)
                        .min(lines.len());

                    for ctx_num in start..end {
                        matched_lines.insert(ctx_num);
                    }
                    if total_matches + actual_matches.len() >= max_matches {
                        break;
                    }
                }
            }

            if !matched_lines.is_empty() {
                let mut sorted_lines: Vec<usize> = matched_lines.into_iter().collect();
                sorted_lines.sort();

                let mut file_result = format!("=== {} ===\n", path_str);
                let mut last_line: Option<usize> = None;

                for line_num in sorted_lines {
                    // Add separator if there's a gap
                    if let Some(last) = last_line {
                        if line_num > last + 1 {
                            file_result.push_str("...\n");
                        }
                    }

                    let line = lines[line_num];
                    let is_match = actual_matches.contains(&line_num);
                    let prefix = if is_match { ">" } else { " " };

                    file_result.push_str(&format!("{} {:4}: {}\n", prefix, line_num + 1, line));

                    if is_match {
                        total_matches += 1;
                    }

                    last_line = Some(line_num);
                }

                file_matches.push(file_result);
            }

            if !file_matches.is_empty() {
                results.extend(file_matches);
            }
            if total_matches >= max_matches {
                break 'inputs;
            }
        }
    }

    let header = if searched == 0 {
        return Err(anyhow::anyhow!(
            "No files searched. {}",
            truncate_output(&results.join("\n"), MAX_GREP_OUTPUT_BYTES)
        ));
    } else if total_matches == 0 {
        format!("No matches found for pattern '{pattern}' in {searched} file(s).")
    } else {
        let limit_note = if total_matches >= max_matches {
            "\n[Match limit reached; narrow the scope or increase max_matches for more.]"
        } else {
            ""
        };
        format!(
            "Found {} match(es) for pattern '{}'{}\n{}\n",
            total_matches,
            pattern,
            limit_note,
            "-".repeat(50)
        )
    };
    Ok(truncate_output(
        &(header + "\n" + &results.join("\n")),
        MAX_GREP_OUTPUT_BYTES,
    ))
}

/// Expand file paths including glob patterns
type SearchPaths = Box<dyn Iterator<Item = Result<PathBuf, String>>>;
fn expand_path(path: &str) -> Result<SearchPaths> {
    let normalized = super::file_ops::normalize_path(path);

    // Check if it contains glob characters
    if normalized.contains('*') || normalized.contains('?') || normalized.contains('[') {
        let mut entries = glob(&normalized)
            .map_err(|e| anyhow::anyhow!("Invalid glob pattern '{}': {}", path, e))?
            .filter_map(|entry| match entry {
                Ok(path) if !path.is_file() => None,
                Ok(path) => Some(Ok(path)),
                Err(e) => Some(Err(format!("Glob error: {e}"))),
            })
            .peekable();
        if entries.peek().is_some() {
            return Ok(Box::new(entries));
        }
        Ok(Box::new(std::iter::once(Err(format!(
            "No files matched '{path}'."
        )))))
    } else {
        let literal = PathBuf::from(normalized);
        let entry = if literal.is_dir() {
            Err(format!(
                "'{path}' is a directory. Use a file glob such as '{}/**/*.rs'.",
                path.trim_end_matches('/')
            ))
        } else if literal.is_file() {
            Ok(literal)
        } else {
            Err(format!("'{path}' is missing or is not a regular file."))
        };
        Ok(Box::new(std::iter::once(entry)))
    }
}

/// Simple grep for quick searches (no context)
pub fn simple_grep(
    pattern: &str,
    file_path: &str,
    case_sensitive: bool,
) -> Result<Vec<(usize, String)>> {
    let regex = RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|e| anyhow::anyhow!("Invalid regex: {}", e))?;

    let normalized = super::file_ops::normalize_path(file_path);
    let content = fs::read_to_string(&normalized)
        .map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", file_path, e))?;

    let matches: Vec<(usize, String)> = content
        .lines()
        .enumerate()
        .filter(|(_, line)| regex.is_match(line))
        .map(|(num, line)| (num + 1, line.to_string()))
        .collect();

    Ok(matches)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enhanced_grep_respects_max_matches() {
        let path = std::env::temp_dir().join(format!("vybrid-grep-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "needle one\nneedle two\nneedle three\n").unwrap();

        let result = enhanced_grep("needle", &[path.to_str().unwrap()], 0, true, 2).unwrap();
        let _ = std::fs::remove_file(&path);

        assert!(result.contains("Found 2 match"));
        assert!(!result.contains("needle three"));
    }
}
