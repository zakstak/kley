//! Read-skill tool — loads full skill content on demand.
//!
//! The system prompt includes only skill names and descriptions (Tier 2).
//! When the model needs full instructions, it calls this tool to load
//! the skill body (Tier 3).

use std::path::PathBuf;

use anyhow::Result;
use serde_json::Value;

use super::Tool;
use crate::skills;

pub struct ReadSkillTool {
    project_dir: PathBuf,
}

impl ReadSkillTool {
    pub fn new(project_dir: PathBuf) -> Self {
        Self { project_dir }
    }
}

impl Tool for ReadSkillTool {
    fn name(&self) -> &str {
        "read_skill"
    }

    fn description(&self) -> &str {
        "Load the full instructions for a skill by name. Use when you need detailed guidance for a task that matches a skill in the Available Skills list."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Name of the skill to load (from the Available Skills list)"
                }
            },
            "required": ["name"],
            "additionalProperties": false,
        })
    }

    fn execute(&self, args: Value) -> Result<String> {
        let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");

        if name.is_empty() {
            return Ok("Error: skill name is required".into());
        }

        // Discover skills and find the requested one
        let discovered = skills::discover_skills(&self.project_dir);
        let skill = match discovered.iter().find(|s| s.name == name) {
            Some(s) => s,
            None => {
                let available: Vec<&str> = discovered.iter().map(|s| s.name.as_str()).collect();
                return Ok(format!(
                    "Error: skill '{}' not found. Available: {}",
                    name,
                    available.join(", ")
                ));
            }
        };

        match skills::load_skill(skill, &self.project_dir) {
            Ok(body) => Ok(body),
            Err(e) => Ok(format!("Error loading skill '{}': {e}", name)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn read_skill_loads_body() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join(".agents/skills/test-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: test-skill\ndescription: A test\n---\n\n# Skill Body\n\nDo the thing.\n",
        )
        .unwrap();

        let tool = ReadSkillTool::new(tmp.path().to_path_buf());
        let result = tool
            .execute(serde_json::json!({"name": "test-skill"}))
            .unwrap();
        assert!(result.contains("Skill Body"));
        assert!(result.contains("Do the thing"));
    }

    #[test]
    fn read_skill_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = ReadSkillTool::new(tmp.path().to_path_buf());
        let result = tool
            .execute(serde_json::json!({"name": "nonexistent"}))
            .unwrap();
        assert!(result.contains("not found"));
    }

    #[test]
    fn read_skill_empty_name() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = ReadSkillTool::new(tmp.path().to_path_buf());
        let result = tool.execute(serde_json::json!({"name": ""})).unwrap();
        assert!(result.contains("Error: skill name is required"));
    }
}
