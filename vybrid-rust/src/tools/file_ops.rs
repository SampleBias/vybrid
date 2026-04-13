use anyhow::Result;
use std::fs;
use std::path::Path;

/// Normalize a file path (expand ~ and make absolute if needed)
pub fn normalize_path(path: &str) -> String {
    let path = path.trim();

    // Expand home directory
    if path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(&path[2..]).to_string_lossy().to_string();
        }
    }

    // If relative, make it relative to current dir
    if !path.starts_with('/') {
        if let Ok(cwd) = std::env::current_dir() {
            return cwd.join(path).to_string_lossy().to_string();
        }
    }

    path.to_string()
}

/// Read a single file
pub fn read_file(path: &str) -> Result<String> {
    let normalized = normalize_path(path);
    let content = fs::read_to_string(&normalized)
        .map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", normalized, e))?;

    Ok(format!("Content of '{}':\n\n{}", path, content))
}

/// Read multiple files
pub fn read_multiple_files(paths: &[&str]) -> Result<String> {
    let mut results = Vec::new();

    for path in paths {
        match read_file(path) {
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
            result.push_str("\n");
        }
        result.push_str(&format!("Errors: {}", errors.join("; ")));
    }

    Ok(result)
}

/// Edit a file by replacing a snippet
pub fn edit_file(path: &str, original_snippet: &str, new_snippet: &str) -> Result<String> {
    let normalized = normalize_path(path);

    // Read the file
    let content = fs::read_to_string(&normalized)
        .map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", path, e))?;

    // Check for the snippet
    let count = content.matches(original_snippet).count();

    if count == 0 {
        return Err(anyhow::anyhow!(
            "Original snippet not found in '{}'. The file may have changed or the snippet doesn't match exactly.",
            path
        ));
    }

    if count > 1 {
        return Err(anyhow::anyhow!(
            "Found {} occurrences of the snippet in '{}'. Please provide more context to make the match unique.",
            count, path
        ));
    }

    // Replace the snippet
    let updated_content = content.replacen(original_snippet, new_snippet, 1);

    // Write the updated content
    fs::write(&normalized, &updated_content)
        .map_err(|e| anyhow::anyhow!("Failed to write '{}': {}", path, e))?;

    Ok(format!("Successfully edited '{}'", path))
}

/// Check if a file exists
pub fn file_exists(path: &str) -> bool {
    let normalized = normalize_path(path);
    Path::new(&normalized).exists()
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
