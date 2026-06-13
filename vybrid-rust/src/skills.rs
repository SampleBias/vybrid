#![allow(dead_code)]

use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const MAX_NAME_LEN: usize = 64;
const MAX_DESCRIPTION_LEN: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub disable_model_invocation: bool,
}

#[derive(Debug, Default)]
pub struct SkillRegistry {
    skills: Vec<SkillMeta>,
}

impl SkillRegistry {
    pub fn discover() -> Self {
        let mut registry = Self::default();
        registry.rescan();
        registry
    }

    pub fn rescan(&mut self) {
        self.skills.clear();
        let mut by_name: HashMap<String, SkillMeta> = HashMap::new();

        for (root, allow_root_md) in discovery_roots() {
            if !root.exists() {
                continue;
            }
            scan_skill_root(&root, allow_root_md, &mut by_name);
        }

        let mut names: Vec<_> = by_name.keys().cloned().collect();
        names.sort();
        for name in names {
            if let Some(skill) = by_name.remove(&name) {
                self.skills.push(skill);
            }
        }
    }

    pub fn skills(&self) -> &[SkillMeta] {
        &self.skills
    }

    pub fn get(&self, name: &str) -> Option<&SkillMeta> {
        self.skills.iter().find(|s| s.name == name)
    }

    pub fn discovery_paths() -> Vec<PathBuf> {
        discovery_roots()
            .into_iter()
            .map(|(path, _)| path)
            .collect()
    }

    pub fn prompt_block(&self) -> String {
        let visible: Vec<_> = self
            .skills
            .iter()
            .filter(|s| !s.disable_model_invocation)
            .collect();
        if visible.is_empty() {
            return String::new();
        }

        let mut out = String::from("<available_skills>\n");
        for skill in visible {
            out.push_str("  <skill>\n");
            out.push_str(&format!("    <name>{}</name>\n", xml_escape(&skill.name)));
            out.push_str(&format!(
                "    <description>{}</description>\n",
                xml_escape(&skill.description)
            ));
            out.push_str(&format!(
                "    <location>{}</location>\n",
                xml_escape(&skill.path.display().to_string())
            ));
            out.push_str("  </skill>\n");
        }
        out.push_str("</available_skills>");
        out
    }

    pub fn load_body(&self, name: &str, user_args: Option<&str>) -> Result<String> {
        let skill = self
            .get(name)
            .ok_or_else(|| anyhow!("Skill '{name}' not found. Use /skills to list available skills."))?;
        let raw = fs::read_to_string(&skill.path)
            .with_context(|| format!("Failed to read skill at {}", skill.path.display()))?;
        let body = skill_body_from_content(&raw);
        match user_args {
            Some(args) if !args.trim().is_empty() => Ok(format!("{body}\n\nUser: {args}")),
            _ => Ok(body),
        }
    }
}

fn discovery_roots() -> Vec<(PathBuf, bool)> {
    let mut roots = Vec::new();
    let project_root = crate::project_context::current_project_root();
    let cwd = std::env::current_dir().unwrap_or_else(|_| project_root.clone());

    if let Some(home) = dirs::home_dir() {
        roots.push((home.join(".vybrid").join("skills"), true));
        roots.push((home.join(".agents").join("skills"), false));
    }

    roots.push((project_root.join(".vybrid").join("skills"), true));

    let mut dir = cwd.clone();
    loop {
        roots.push((dir.join(".agents").join("skills"), false));
        if dir == project_root {
            break;
        }
        if !dir.pop() {
            break;
        }
    }

    roots
}

fn scan_skill_root(root: &Path, allow_root_md: bool, by_name: &mut HashMap<String, SkillMeta>) {
    if allow_root_md {
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("md") {
                    if let Some(skill) = parse_skill_file(&path, None) {
                        insert_skill(by_name, skill);
                    }
                }
            }
        }
    }

    scan_skill_dirs_recursive(root, by_name);
}

fn scan_skill_dirs_recursive(dir: &Path, by_name: &mut HashMap<String, SkillMeta>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let skill_md = path.join("SKILL.md");
            if skill_md.is_file() {
                if let Some(skill) = parse_skill_file(&skill_md, Some(&path)) {
                    insert_skill(by_name, skill);
                }
            }
            scan_skill_dirs_recursive(&path, by_name);
        }
    }
}

fn insert_skill(by_name: &mut HashMap<String, SkillMeta>, skill: SkillMeta) {
    if let Some(existing) = by_name.get(&skill.name) {
        eprintln!(
            "Warning: skill name collision for '{}'; keeping {} and ignoring {}",
            skill.name,
            existing.path.display(),
            skill.path.display()
        );
        return;
    }
    by_name.insert(skill.name.clone(), skill);
}

fn parse_skill_file(path: &Path, skill_dir: Option<&Path>) -> Option<SkillMeta> {
    let raw = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Warning: failed to read skill {}: {e}", path.display());
            return None;
        }
    };

    let parsed = parse_frontmatter(&raw);
    let fallback_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("skill")
        .to_string();

    let name = parsed
        .name
        .unwrap_or_else(|| fallback_name.clone());
    let description = parsed.description?;

    if !is_valid_skill_name(&name) {
        eprintln!(
            "Warning: skill at {} has invalid name '{}'; loading anyway",
            path.display(),
            name
        );
    }

    if description.chars().count() > MAX_DESCRIPTION_LEN {
        eprintln!(
            "Warning: skill '{}' description exceeds {} chars",
            name, MAX_DESCRIPTION_LEN
        );
    }

    let _ = skill_dir;

    Some(SkillMeta {
        name,
        description,
        path: path.to_path_buf(),
        disable_model_invocation: parsed.disable_model_invocation,
    })
}

struct ParsedFrontmatter {
    name: Option<String>,
    description: Option<String>,
    disable_model_invocation: bool,
}

fn parse_frontmatter(content: &str) -> ParsedFrontmatter {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return ParsedFrontmatter {
            name: None,
            description: None,
            disable_model_invocation: false,
        };
    }

    let rest = trimmed.strip_prefix("---").unwrap_or("").trim_start_matches('\n');
    let Some(end_idx) = rest.find("\n---") else {
        return ParsedFrontmatter {
            name: None,
            description: None,
            disable_model_invocation: false,
        };
    };

    let block = &rest[..end_idx];
    let mut name = None;
    let mut description = None;
    let mut disable_model_invocation = false;

    for line in block.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim().trim_matches('"').trim_matches('\'').to_string();
            match key.as_str() {
                "name" if !value.is_empty() => name = Some(value),
                "description" if !value.is_empty() => description = Some(value),
                "disable-model-invocation" => {
                    disable_model_invocation =
                        matches!(value.to_ascii_lowercase().as_str(), "true" | "yes" | "1");
                }
                _ => {}
            }
        }
    }

    ParsedFrontmatter {
        name,
        description,
        disable_model_invocation,
    }
}

fn skill_body_from_content(content: &str) -> String {
    let trimmed = content.trim_start();
    if trimmed.starts_with("---") {
        let rest = trimmed.strip_prefix("---").unwrap_or("").trim_start_matches('\n');
        if let Some(end_idx) = rest.find("\n---") {
            let after = rest[end_idx + 4..].trim_start_matches('\n');
            return after.to_string();
        }
    }
    content.to_string()
}

pub fn is_valid_skill_name(name: &str) -> bool {
    if name.is_empty() || name.chars().count() > MAX_NAME_LEN {
        return false;
    }
    if name.starts_with('-') || name.ends_with('-') || name.contains("--") {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_skill(dir: &Path, name: &str, content: &str) {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{name}.md"));
        fs::File::create(&path)
            .unwrap()
            .write_all(content.as_bytes())
            .unwrap();
    }

    fn write_skill_dir(dir: &Path, content: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("SKILL.md"), content).unwrap();
    }

    #[test]
    fn parses_frontmatter_fields() {
        let content = r#"---
name: test-skill
description: Does testing things
disable-model-invocation: true
---

# Body
"#;
        let parsed = parse_frontmatter(content);
        assert_eq!(parsed.name.as_deref(), Some("test-skill"));
        assert_eq!(parsed.description.as_deref(), Some("Does testing things"));
        assert!(parsed.disable_model_invocation);
        assert_eq!(skill_body_from_content(content).trim(), "# Body");
    }

    #[test]
    fn skips_skill_without_description() {
        let dir = std::env::temp_dir().join(format!("vybrid-skill-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write_skill_dir(
            &dir.join("no-desc"),
            "---\nname: no-desc\n---\n\nbody\n",
        );
        let skill = parse_skill_file(&dir.join("no-desc").join("SKILL.md"), None);
        assert!(skill.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prompt_block_omits_disabled_skills() {
        let registry = SkillRegistry {
            skills: vec![
                SkillMeta {
                    name: "visible".into(),
                    description: "shown".into(),
                    path: PathBuf::from("/tmp/visible/SKILL.md"),
                    disable_model_invocation: false,
                },
                SkillMeta {
                    name: "hidden".into(),
                    description: "hidden".into(),
                    path: PathBuf::from("/tmp/hidden/SKILL.md"),
                    disable_model_invocation: true,
                },
            ],
        };
        let block = registry.prompt_block();
        assert!(block.contains("visible"));
        assert!(!block.contains("hidden"));
        assert!(block.contains("<available_skills>"));
    }

    #[test]
    fn validates_skill_names() {
        assert!(is_valid_skill_name("rust-compile-fix-loop"));
        assert!(!is_valid_skill_name("Bad_Name"));
        assert!(!is_valid_skill_name("-bad"));
        assert!(!is_valid_skill_name("bad--name"));
    }

    #[test]
    fn load_body_appends_user_args() {
        let dir = std::env::temp_dir().join(format!("vybrid-skill-load-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write_skill_dir(
            &dir.join("demo"),
            "---\nname: demo\ndescription: demo skill\n---\n\nDo the thing.\n",
        );
        let registry = SkillRegistry {
            skills: vec![SkillMeta {
                name: "demo".into(),
                description: "demo skill".into(),
                path: dir.join("demo").join("SKILL.md"),
                disable_model_invocation: false,
            }],
        };
        let body = registry.load_body("demo", Some("extra context")).unwrap();
        assert!(body.contains("Do the thing."));
        assert!(body.contains("User: extra context"));
        let _ = fs::remove_dir_all(&dir);
    }
}
