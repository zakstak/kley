//! Skill and rule discovery for kley's agent loop.
//!
//! Follows the codex-rs convention: skills live in `.agents/skills/` (project-local)
//! and `~/.kley/skills/` (user-global). Rules live in `.agents/rules/` and `~/.kley/rules/`.
//!
//! Three-tier context strategy:
//!   1. Rules — always injected into the system prompt (compact, <30 lines each)
//!   2. Skill index — name + description only, always visible for routing
//!   3. Skill body — full content loaded on demand when relevant

use std::path::{Path, PathBuf};

use anyhow::Result;

// ── Data types ──────────────────────────────────────────────────────────────

/// An always-on rule. Full content is included in every system prompt.
#[derive(Debug, Clone)]
pub struct Rule {
    pub name: String,
    pub content: String,
}

/// A discoverable skill. Only name + description go into the system prompt;
/// the full body is loaded on demand.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: Option<String>,
    pub path: PathBuf,
}

// ── Frontmatter parsing ─────────────────────────────────────────────────────

/// Parse optional YAML-like frontmatter from a SKILL.md file.
/// Mirrors the codex-rs `parse_frontmatter` pattern.
///
/// Returns `(name, description, body_without_frontmatter)`.
pub fn parse_frontmatter(content: &str) -> (Option<String>, Option<String>, String) {
    let mut lines = content.lines().peekable();

    // Check for opening `---`
    match lines.peek() {
        Some(line) if line.trim() == "---" => {
            lines.next();
        }
        _ => return (None, None, content.to_string()),
    }

    let mut name: Option<String> = None;
    let mut desc: Option<String> = None;
    let mut frontmatter_end = false;
    let mut consumed_lines: usize = 1; // the opening `---`

    for line in &mut lines {
        consumed_lines += 1;
        let trimmed = line.trim();

        if trimmed == "---" {
            frontmatter_end = true;
            break;
        }

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some((k, v)) = trimmed.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = strip_quotes(v.trim());
            match key.as_str() {
                "name" => name = Some(val),
                "description" => desc = Some(val),
                _ => {}
            }
        }
    }

    if !frontmatter_end {
        // Unterminated frontmatter — treat entire input as body.
        return (None, None, content.to_string());
    }

    // Body is everything after the closing `---`
    let body: String = content
        .lines()
        .skip(consumed_lines)
        .collect::<Vec<_>>()
        .join("\n");

    (name, desc, body)
}

/// Strip surrounding single or double quotes from a string.
fn strip_quotes(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

// ── Discovery ───────────────────────────────────────────────────────────────

/// Discover rules from project-local and user-global paths.
/// Rules in `.agents/rules/` are read in full (sorted by filename).
pub fn discover_rules(project_dir: &Path) -> Vec<Rule> {
    let mut rules = Vec::new();
    let dirs = rule_dirs(project_dir);

    for dir in dirs {
        if !dir.is_dir() {
            continue;
        }
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path().extension().and_then(|s| s.to_str()) == Some("md") && e.path().is_file()
            })
            .collect();
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let path = entry.path();
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if let Ok(content) = std::fs::read_to_string(&path) {
                rules.push(Rule { name, content });
            }
        }
    }

    rules
}

/// Discover skills from project-local and user-global paths.
/// Only reads frontmatter (name + description). Body is NOT loaded.
/// Project-local skills shadow user-global skills with the same name.
pub fn discover_skills(project_dir: &Path) -> Vec<Skill> {
    let mut skills = Vec::new();
    let dirs = skill_dirs(project_dir);

    for dir in dirs {
        if !dir.is_dir() {
            continue;
        }
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let skill_md = entry.path().join("SKILL.md");
            if !skill_md.is_file() {
                continue;
            }

            let content = match std::fs::read_to_string(&skill_md) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let (parsed_name, desc, _body) = parse_frontmatter(&content);
            let name = parsed_name
                .unwrap_or_else(|| entry.file_name().to_str().unwrap_or("unknown").to_string());

            // Dedup: skip if a skill with this name already exists (project-local wins)
            if skills.iter().any(|s: &Skill| s.name == name) {
                continue;
            }

            skills.push(Skill {
                name,
                description: desc,
                path: skill_md,
            });
        }
    }

    skills
}

/// Load the full body of a skill (everything after frontmatter).
/// Also loads sub-files conditionally (e.g. `rust-checks.md` if `Cargo.toml` exists).
pub fn load_skill(skill: &Skill, project_dir: &Path) -> Result<String> {
    let content = std::fs::read_to_string(&skill.path)?;
    let (_name, _desc, mut body) = parse_frontmatter(&content);

    // Check for sub-files in the same directory
    if let Some(skill_dir) = skill.path.parent() {
        let mut sub_files: Vec<_> = std::fs::read_dir(skill_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let p = e.path();
                p.is_file()
                    && p.extension().and_then(|s| s.to_str()) == Some("md")
                    && p.file_name().and_then(|s| s.to_str()) != Some("SKILL.md")
            })
            .collect();
        sub_files.sort_by_key(|e| e.file_name());

        for sub in sub_files {
            let sub_path = sub.path();
            let sub_name = sub_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");

            // Conditional loading: rust-checks only if Cargo.toml exists
            if sub_name == "rust-checks" && !project_dir.join("Cargo.toml").exists() {
                continue;
            }

            if let Ok(sub_content) = std::fs::read_to_string(&sub_path) {
                body.push_str("\n\n---\n\n");
                body.push_str(&sub_content);
            }
        }
    }

    Ok(body)
}

// ── System prompt building ──────────────────────────────────────────────────

/// Build the system prompt with always-on rules and a skill index.
/// Full skill content is NOT included — only name + description for routing.
pub fn build_system_prompt(rules: &[Rule], skills: &[Skill]) -> String {
    let mut prompt = String::from("You are a helpful coding assistant.\n");

    // Tier 1: Always-on rules
    if !rules.is_empty() {
        prompt.push_str("\n## Rules\n\n");
        for rule in rules {
            prompt.push_str(&rule.content);
            prompt.push('\n');
        }
    }

    // Tier 2: Skill index (name + description only)
    if !skills.is_empty() {
        prompt.push_str("\n## Available Skills\n\n");
        for skill in skills {
            let desc = skill.description.as_deref().unwrap_or("No description");
            prompt.push_str(&format!("- **{}**: {}\n", skill.name, desc));
        }
    }

    prompt
}

// ── Path helpers ────────────────────────────────────────────────────────────

fn kley_home() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".kley"))
}

/// Rule discovery directories, in priority order (project-local first).
fn rule_dirs(project_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![project_dir.join(".agents").join("rules")];
    if let Some(home) = kley_home() {
        dirs.push(home.join("rules"));
    }
    dirs
}

/// Skill discovery directories, in priority order (project-local first).
fn skill_dirs(project_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![project_dir.join(".agents").join("skills")];
    if let Some(home) = kley_home() {
        dirs.push(home.join("skills"));
    }
    dirs
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parse_frontmatter_with_metadata() {
        let input = "---\nname: git\ndescription: \"Git operations\"\n---\n\n# Body here\n";
        let (name, desc, body) = parse_frontmatter(input);
        assert_eq!(name.as_deref(), Some("git"));
        assert_eq!(desc.as_deref(), Some("Git operations"));
        assert!(body.contains("# Body here"));
    }

    #[test]
    fn parse_frontmatter_no_frontmatter() {
        let input = "# Just a heading\n\nSome content.\n";
        let (name, desc, body) = parse_frontmatter(input);
        assert!(name.is_none());
        assert!(desc.is_none());
        assert_eq!(body, input);
    }

    #[test]
    fn parse_frontmatter_empty() {
        let (name, desc, body) = parse_frontmatter("");
        assert!(name.is_none());
        assert!(desc.is_none());
        assert!(body.is_empty());
    }

    #[test]
    fn parse_frontmatter_unterminated() {
        let input = "---\nname: test\nno closing marker\n";
        let (name, desc, body) = parse_frontmatter(input);
        assert!(name.is_none());
        assert!(desc.is_none());
        assert_eq!(body, input);
    }

    #[test]
    fn discover_skills_from_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join(".agents/skills/test-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: test-skill\ndescription: A test\n---\n\nBody.\n",
        )
        .unwrap();

        let skills = discover_skills(tmp.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "test-skill");
        assert_eq!(skills[0].description.as_deref(), Some("A test"));
    }

    #[test]
    fn discover_skills_dedup_project_wins() {
        let tmp = tempfile::tempdir().unwrap();

        // Project-local
        let local_dir = tmp.path().join(".agents/skills/mygit");
        fs::create_dir_all(&local_dir).unwrap();
        fs::write(
            local_dir.join("SKILL.md"),
            "---\nname: mygit\ndescription: local version\n---\n",
        )
        .unwrap();

        let skills = discover_skills(tmp.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description.as_deref(), Some("local version"));
    }

    #[test]
    fn discover_rules_from_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let rules_dir = tmp.path().join(".agents/rules");
        fs::create_dir_all(&rules_dir).unwrap();
        fs::write(rules_dir.join("00-test.md"), "- Rule one\n- Rule two\n").unwrap();

        let rules = discover_rules(tmp.path());
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "00-test");
        assert!(rules[0].content.contains("Rule one"));
    }

    #[test]
    fn build_system_prompt_includes_rules_and_index() {
        let rules = vec![Rule {
            name: "dev".into(),
            content: "- Always test\n".into(),
        }];
        let skills = vec![Skill {
            name: "git".into(),
            description: Some("Git operations".into()),
            path: PathBuf::from("/fake/SKILL.md"),
        }];

        let prompt = build_system_prompt(&rules, &skills);
        assert!(prompt.contains("You are a helpful coding assistant."));
        assert!(prompt.contains("Always test"));
        assert!(prompt.contains("**git**: Git operations"));
        // Full skill body should NOT be in the prompt
        assert!(!prompt.contains("COMMIT MODE"));
    }

    #[test]
    fn load_skill_with_subfiles() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join(".agents/skills/git");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: git\n---\n\n# Main body\n",
        )
        .unwrap();
        fs::write(
            skill_dir.join("rust-checks.md"),
            "# Rust Checks\ncargo test\n",
        )
        .unwrap();

        // With Cargo.toml present, rust-checks should be loaded
        fs::write(tmp.path().join("Cargo.toml"), "[package]\n").unwrap();
        let body = load_skill(
            &Skill {
                name: "git".into(),
                description: None,
                path: skill_dir.join("SKILL.md"),
            },
            tmp.path(),
        )
        .unwrap();
        assert!(body.contains("Main body"));
        assert!(body.contains("Rust Checks"));
    }

    #[test]
    fn load_skill_skips_rust_checks_without_cargo() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join(".agents/skills/git");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: git\n---\n\n# Main body\n",
        )
        .unwrap();
        fs::write(
            skill_dir.join("rust-checks.md"),
            "# Rust Checks\ncargo test\n",
        )
        .unwrap();

        // No Cargo.toml — rust-checks should NOT be loaded
        let body = load_skill(
            &Skill {
                name: "git".into(),
                description: None,
                path: skill_dir.join("SKILL.md"),
            },
            tmp.path(),
        )
        .unwrap();
        assert!(body.contains("Main body"));
        assert!(!body.contains("Rust Checks"));
    }
}
