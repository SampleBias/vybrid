use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

/// Manages project-specific documentation context
pub struct ProjectDocs {
    docs_path: PathBuf,
}

#[allow(dead_code)]

impl ProjectDocs {
    /// Create a new ProjectDocs instance for the current directory
    pub fn new() -> Self {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let docs_path = current_dir.join(".vybrid").join("docs.md");
        Self { docs_path }
    }

    /// Read the project documentation
    pub fn read(&self) -> Result<Option<String>> {
        if !self.docs_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&self.docs_path).context(format!(
            "Failed to read project docs from {:?}",
            self.docs_path
        ))?;

        if content.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(content))
        }
    }

    /// Add or append documentation to the project docs
    pub fn add(&self, content: &str) -> Result<()> {
        // Create .vybrid directory if it doesn't exist
        if let Some(parent) = self.docs_path.parent() {
            fs::create_dir_all(parent)
                .context(format!("Failed to create directory {:?}", parent))?;
        }

        let existing_content = if self.docs_path.exists() {
            fs::read_to_string(&self.docs_path).context("Failed to read existing project docs")?
        } else {
            String::new()
        };

        let new_content = if existing_content.trim().is_empty() {
            content.to_string()
        } else {
            format!("{}\n\n---\n\n{}", existing_content.trim(), content)
        };

        fs::write(&self.docs_path, new_content).context(format!(
            "Failed to write project docs to {:?}",
            self.docs_path
        ))?;

        Ok(())
    }

    /// Clear all project documentation
    pub fn clear(&self) -> Result<()> {
        if self.docs_path.exists() {
            fs::remove_file(&self.docs_path).context(format!(
                "Failed to remove project docs at {:?}",
                self.docs_path
            ))?;
        }
        Ok(())
    }

    /// Check if project docs exist
    pub fn exists(&self) -> bool {
        self.docs_path.exists()
    }

    /// Get the path to the docs file
    pub fn path(&self) -> &PathBuf {
        &self.docs_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_project_docs_operations() {
        // This test would require temp directory setup
        // Skipping for now as tests are not currently set up
    }
}
