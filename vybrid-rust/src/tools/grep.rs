#![allow(dead_code)]

use anyhow::Result;
use glob::glob;
use regex::RegexBuilder;
use std::fs;
use std::path::PathBuf;

/// Enhanced grep functionality with context and formatting
pub fn enhanced_grep(
    pattern: &str,
    file_paths: &[&str],
    context_lines: usize,
    case_sensitive: bool,
) -> Result<String> {
    // Build the regex
    let regex = RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|e| anyhow::anyhow!("Invalid regex pattern '{}': {}", pattern, e))?;

    let mut results = Vec::new();
    let mut total_matches = 0;

    // Expand all file paths (including globs)
    let expanded_paths = expand_paths(file_paths)?;

    for path in expanded_paths {
        let path_str = path.to_string_lossy();
        
        // Skip non-files
        if !path.is_file() {
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
        let mut file_matches = Vec::new();
        let mut matched_lines: std::collections::HashSet<usize> = std::collections::HashSet::new();

        // Find all matching lines
        for (line_num, line) in lines.iter().enumerate() {
            if regex.is_match(line) {
                matched_lines.insert(line_num);
                
                // Add context lines
                let start = line_num.saturating_sub(context_lines);
                let end = (line_num + context_lines + 1).min(lines.len());
                
                for ctx_num in start..end {
                    matched_lines.insert(ctx_num);
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
                let is_match = regex.is_match(line);
                let prefix = if is_match { ">" } else { " " };
                
                file_result.push_str(&format!(
                    "{} {:4}: {}\n",
                    prefix,
                    line_num + 1,
                    line
                ));

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
    }

    if results.is_empty() {
        Ok(format!("No matches found for pattern '{}'", pattern))
    } else {
        let header = format!(
            "Found {} match(es) for pattern '{}'\n{}\n",
            total_matches,
            pattern,
            "-".repeat(50)
        );
        Ok(header + &results.join("\n"))
    }
}

/// Expand file paths including glob patterns
fn expand_paths(paths: &[&str]) -> Result<Vec<PathBuf>> {
    let mut expanded = Vec::new();

    for path in paths {
        let normalized = super::file_ops::normalize_path(path);
        
        // Check if it contains glob characters
        if normalized.contains('*') || normalized.contains('?') || normalized.contains('[') {
            for entry in glob(&normalized)
                .map_err(|e| anyhow::anyhow!("Invalid glob pattern '{}': {}", path, e))?
            {
                match entry {
                    Ok(p) => expanded.push(p),
                    Err(e) => eprintln!("Glob error for '{}': {}", path, e),
                }
            }
        } else {
            expanded.push(PathBuf::from(normalized));
        }
    }

    Ok(expanded)
}

/// Simple grep for quick searches (no context)
pub fn simple_grep(pattern: &str, file_path: &str, case_sensitive: bool) -> Result<Vec<(usize, String)>> {
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
