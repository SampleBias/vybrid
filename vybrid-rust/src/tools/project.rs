use anyhow::{Context, Result};
use chrono::Local;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use super::file_ops::{append_to_file, create_file, file_exists, normalize_path};

/// Create the mandatory project structure files
pub fn create_structure(project_name: Option<&str>, overwrite_existing: bool) -> Result<String> {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M").to_string();
    let project_title = project_name.unwrap_or("Project");

    let mut created_files = Vec::new();
    let mut updated_files = Vec::new();

    // Handle tasks/todo.md
    let todo_path = "tasks/todo.md";
    let todo_exists = file_exists(todo_path);

    if !todo_exists || overwrite_existing {
        let todo_content = format!(
            r#"# {} Todo List

## Project Setup
- [x] Create project structure files
- [x] Initialize todo.md, activity.md, and PROJECT_README.md
- [ ] Define project requirements
- [ ] Plan implementation approach

## Development Tasks
- [ ] Analyze existing codebase
- [ ] Implement core functionality
- [ ] Add error handling
- [ ] Write tests
- [ ] Update documentation

## Testing & Quality
- [ ] Unit tests
- [ ] Integration tests
- [ ] Code review
- [ ] Performance testing

## Deployment
- [ ] Build verification
- [ ] Deployment preparation
- [ ] Production deployment
- [ ] Post-deployment verification

## Review Section
*This section will be updated upon completion with a summary of all changes made during the session.*

---
*Created: {}*
*Last Updated: {}*
"#,
            project_title, timestamp, timestamp
        );

        create_file(todo_path, &todo_content)?;
        created_files.push(todo_path);
    } else {
        let append_content = format!(
            r#"

---

## New Session - {}
- [ ] Review existing todo items
- [ ] Identify new requirements
- [ ] Update task priorities
- [ ] Add session-specific tasks

*Session started: {}*
"#,
            timestamp, timestamp
        );

        append_to_file(todo_path, &append_content)?;
        updated_files.push(todo_path);
    }

    // Handle docs/activity.md
    let activity_path = "docs/activity.md";
    let activity_exists = file_exists(activity_path);

    if !activity_exists || overwrite_existing {
        let activity_content = format!(
            r#"# {} Activity Log

## {} - Project Initialization
- Created project structure files
- Initialized todo.md with project template
- Initialized activity.md for logging
- Generated PROJECT_README.md for context tracking

---
*Activity logging format:*
*## YYYY-MM-DD HH:MM - Action Description*
*- Detailed description of what was done*
*- Files created/modified*
*- Commands executed*
*- Any important notes or decisions*
"#,
            project_title, timestamp
        );

        create_file(activity_path, &activity_content)?;
        created_files.push(activity_path);
    } else {
        let append_content = format!(
            r#"

## {} - Session Started
- Project structure files verified
- Resumed work on existing project
- Todo.md updated with new session section
- PROJECT_README.md context checked
- Ready for continued development

"#,
            timestamp
        );

        append_to_file(activity_path, &append_content)?;
        updated_files.push(activity_path);
    }

    // Handle docs/PROJECT_README.md
    let readme_path = "docs/PROJECT_README.md";
    let readme_exists = file_exists(readme_path);

    if !readme_exists || overwrite_existing {
        let readme_content = format!(
            r#"# {} - Development Context

## Project Purpose
This project is currently under active development. This document maintains context for AI agents and developers working on the project.

## Architecture Overview
- **Project Type**: Software Project
- **Development Status**: Active Development
- **Context Tracking**: Integrated with Vybrid development workflow

## Technology Stack
- *Technology stack will be detected as project develops*

## Project Structure
- *Project structure will be documented as development progresses*

## Getting Started

### Prerequisites
- *Prerequisites will be documented as project requirements are identified*

### Installation
- *Installation instructions will be added as the build process is defined*

### Running the Project
- *Startup instructions will be documented as entry points are established*

## Development Status
- **Task Tracking**: See `tasks/todo.md` for current development tasks and progress
- **Activity History**: See `docs/activity.md` for detailed development timeline
- **Context Updates**: This file is automatically updated when major project changes are detected

## Key Context for AI Agents

### Development Workflow
- This project follows the Vybrid development methodology
- Three mandatory files are maintained: `tasks/todo.md`, `docs/activity.md`, and this `docs/PROJECT_README.md`
- All development activities are tracked and documented systematically

### Project Evolution
- **Initial Analysis**: {}
- **Last Context Update**: {}
- **Major Changes**: *Tracked automatically when significant project modifications occur*

## Documentation Links
- [Task List](../tasks/todo.md) - Current development tasks and progress
- [Activity Log](activity.md) - Detailed timeline of all development activities

---
*Auto-generated by Vybrid*
*Created: {}*
*Last Updated: {}*
*Context Version: 1.0*
"#,
            project_title, timestamp, timestamp, timestamp, timestamp
        );

        create_file(readme_path, &readme_content)?;
        created_files.push(readme_path);
    } else {
        let append_content = format!(
            r#"

---

## Session Update - {}
- **Session Started**: {}
- **Context Status**: Verified and up-to-date

*Context automatically updated for new development session*
"#,
            timestamp, timestamp
        );

        append_to_file(readme_path, &append_content)?;
        updated_files.push(readme_path);
    }

    // Build result message
    let mut result = String::new();

    if !created_files.is_empty() {
        result.push_str(&format!("Created files: {}\n", created_files.join(", ")));
    }

    if !updated_files.is_empty() {
        result.push_str(&format!("Updated files: {}\n", updated_files.join(", ")));
    }

    result.push_str("Project structure ready:\n");
    result.push_str("  - tasks/todo.md (task tracking)\n");
    result.push_str("  - docs/activity.md (activity logging)\n");
    result.push_str("  - docs/PROJECT_README.md (project context)");

    Ok(result)
}

/// Get current incomplete todo items
pub fn get_current_todo_items() -> Result<String> {
    read_incomplete_tasks(Path::new(&normalize_path("tasks/todo.md")))
}

fn read_incomplete_tasks(path: &Path) -> Result<String> {
    let content = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(
                "No todo.md file found. Use create_project_structure to initialize.".to_string(),
            )
        }
        Err(e) => return Err(e).with_context(|| format!("Reading {}", path.display())),
    };
    let incomplete: Vec<&str> = content
        .lines()
        .filter(|line| line.trim().starts_with("- [ ]"))
        .collect();

    if incomplete.is_empty() {
        Ok("All tasks are complete!".to_string())
    } else {
        let mut result = format!("Incomplete tasks ({}):\n", incomplete.len());
        for task in incomplete {
            result.push_str(&format!("{}\n", task.trim()));
        }
        Ok(result)
    }
}

/// Mark a todo item as complete
pub fn mark_todo_complete(task_description: &str) -> Result<String> {
    mark_task_in_file(
        Path::new(&normalize_path("tasks/todo.md")),
        task_description,
    )
}

fn mark_task_in_file(path: &Path, task_description: &str) -> Result<String> {
    let task_description = task_description.trim();
    anyhow::ensure!(!task_description.is_empty(), "Provide a task description");
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("Opening {}", path.display()))?;
    file.lock()?;
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    let mut offset = 0;
    let mut matches = Vec::new();
    for line in content.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if let Some(description) = trimmed.strip_prefix("- [ ]") {
            if trimmed.trim_end().contains(task_description) {
                let exact = description.trim() == task_description
                    || trimmed.trim_end() == task_description;
                matches.push((offset + line.len() - trimmed.len() + 3, exact));
            }
        }
        offset += line.len();
    }
    if matches.iter().any(|(_, exact)| *exact) {
        matches.retain(|(_, exact)| *exact);
    }
    let position = match matches.as_slice() {
        [(position, _)] => *position,
        [] => anyhow::bail!("Task '{task_description}' not found in todo.md"),
        _ => anyhow::bail!(
            "Multiple incomplete tasks match '{task_description}'. Provide a unique description."
        ),
    };
    file.seek(SeekFrom::Start(position as u64))?;
    file.write_all(b"x")?;

    Ok(format!("Marked task as complete: {}", task_description))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completing_a_late_task_changes_only_its_checkbox() {
        let path = std::env::temp_dir().join(format!("vybrid-todo-{}", uuid::Uuid::new_v4()));
        for ending in ["", "\r\n"] {
            let content = format!(
                "{}\n- [ ] Ship tests\r\n  - [ ] Ship\r\n- [ ] Café 🦀{ending}",
                "# notes\r\n".repeat(3_000)
            );
            fs::write(&path, &content).unwrap();
            let incomplete = read_incomplete_tasks(&path).unwrap();
            assert!(incomplete.contains("Incomplete tasks (3)") && incomplete.contains("Café 🦀"));
            for bad in ["", "Sh", "nonexistent"] {
                assert!(mark_task_in_file(&path, bad).is_err());
                assert_eq!(fs::read_to_string(&path).unwrap(), content);
            }
            mark_task_in_file(&path, "Ship").unwrap();
            let expected = content.replace("- [ ] Ship\r", "- [x] Ship\r");
            assert_eq!(fs::read_to_string(&path).unwrap(), expected);
            mark_task_in_file(&path, "🦀").unwrap();
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                expected.replace("- [ ] Café", "- [x] Café")
            );
        }
        fs::remove_file(path).unwrap();
    }
}
