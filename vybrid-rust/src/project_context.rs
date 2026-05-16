use std::path::{Component, Path, PathBuf};

/// Project-level filesystem context used by tools that accept paths from the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectContext {
    root: PathBuf,
}

impl ProjectContext {
    pub fn discover() -> Self {
        if let Some(root) = std::env::var("VYBRID_PROJECT_ROOT")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            return Self {
                root: canonicalize_existing_or_self(PathBuf::from(root)),
            };
        }

        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            root: discover_project_root_from(&cwd),
        }
    }

    #[allow(dead_code)]
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        Self {
            root: canonicalize_existing_or_self(root.into()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve user/model input as an absolute path. Relative paths are root-relative.
    pub fn resolve_path(&self, input: &str) -> PathBuf {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return self.root.clone();
        }

        if let Some(stripped) = trimmed.strip_prefix("~/") {
            if let Some(home) = dirs::home_dir() {
                return home.join(stripped);
            }
        }

        let path = PathBuf::from(trimmed);
        if path.is_absolute() {
            return path;
        }

        let candidate = self.root.join(&path);
        if candidate.exists() {
            return candidate;
        }

        if let Some(stripped) = strip_repeated_root_prefix(&self.root, &path) {
            return self.root.join(stripped);
        }

        candidate
    }

    pub fn root_relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.display().to_string())
    }

    pub fn not_found_message(&self, requested: &str, resolved: &Path) -> String {
        let mut msg = format!(
            "Path not found: requested `{}` resolved to `{}` (project root: `{}`).",
            requested,
            resolved.display(),
            self.root.display()
        );

        let requested_path = PathBuf::from(requested.trim());
        if !requested_path.is_absolute() {
            if let Some(stripped) = strip_repeated_root_prefix(&self.root, &requested_path) {
                msg.push_str(&format!(
                    " The path appears to repeat the project root; try `{}`.",
                    stripped.display()
                ));
            }
        }

        msg
    }
}

impl Default for ProjectContext {
    fn default() -> Self {
        Self::discover()
    }
}

pub fn current_project_root() -> PathBuf {
    ProjectContext::discover().root
}

pub fn resolve_path(input: &str) -> PathBuf {
    ProjectContext::discover().resolve_path(input)
}

pub fn root_relative(path: &Path) -> String {
    ProjectContext::discover().root_relative(path)
}

pub fn path_not_found_message(requested: &str, resolved: &Path) -> String {
    ProjectContext::discover().not_found_message(requested, resolved)
}

fn discover_project_root_from(start: &Path) -> PathBuf {
    let start = canonicalize_existing_or_self(start.to_path_buf());
    for ancestor in start.ancestors() {
        if ancestor.join(".git").exists() || ancestor.join("Cargo.toml").exists() {
            return ancestor.to_path_buf();
        }
    }
    start
}

fn canonicalize_existing_or_self(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

fn strip_repeated_root_prefix(root: &Path, relative: &Path) -> Option<PathBuf> {
    let root_components = normal_components(root);
    let rel_components = normal_components(relative);
    if root_components.is_empty() || rel_components.is_empty() {
        return None;
    }

    let max = root_components.len().min(rel_components.len());
    for prefix_len in (1..=max).rev() {
        let root_suffix = &root_components[root_components.len() - prefix_len..];
        if root_suffix == &rel_components[..prefix_len] {
            return Some(rel_components[prefix_len..].iter().collect());
        }
    }

    None
}

fn normal_components(path: &Path) -> Vec<std::ffi::OsString> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_os_string()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_root_relative_paths() {
        let root = std::env::temp_dir().join(format!("vybrid-root-{}", std::process::id()));
        let ctx = ProjectContext::from_root(&root);
        assert_eq!(ctx.resolve_path("src/main.rs"), root.join("src/main.rs"));
    }

    #[test]
    fn leaves_absolute_paths_stable() {
        let root = std::env::temp_dir().join(format!("vybrid-root-{}", std::process::id()));
        let ctx = ProjectContext::from_root(&root);
        let absolute = root.join("Cargo.toml");
        assert_eq!(ctx.resolve_path(absolute.to_str().unwrap()), absolute);
    }

    #[test]
    fn strips_repeated_root_suffix_from_relative_paths() {
        let root = PathBuf::from("/tmp/work/boltr_view/boltr-view");
        let ctx = ProjectContext::from_root(&root);
        assert_eq!(
            ctx.resolve_path("boltr_view/boltr-view/Cargo.toml"),
            root.join("Cargo.toml")
        );
    }
}
