use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::Serialize;

#[derive(Debug, Serialize)]
struct RetrospectiveRecord {
    cycle: i64,
    timestamp: String,
    status: String,
    run_exit: i64,
    log_file: String,
    branch: String,
    commit: String,
    pr: String,
    helpful_feature_ideas: Vec<String>,
    struggle: String,
    preventable: Option<bool>,
    preventable_raw: Option<String>,
    prevention_notes: Vec<String>,
}

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let log_path = PathBuf::from(next_arg(&mut args, "log_file")?);
    let cycle: i64 = next_arg(&mut args, "cycle")?
        .parse()
        .context("cycle must be an integer")?;
    let timestamp = next_arg(&mut args, "timestamp")?;
    let run_exit: i64 = next_arg(&mut args, "run_exit")?
        .parse()
        .context("run_exit must be an integer")?;
    let status = next_arg(&mut args, "status")?;
    let output_path = PathBuf::from(next_arg(&mut args, "output_file")?);

    if args.next().is_some() {
        return Err(anyhow!("unexpected extra arguments"));
    }

    let log_content = fs::read_to_string(&log_path)
        .with_context(|| format!("failed to read log file: {}", log_path.display()))?;
    let record = parse_record(&log_content, cycle, timestamp, run_exit, status, &log_path)?;
    append_record(&output_path, &record)
}

fn next_arg(args: &mut impl Iterator<Item = String>, name: &str) -> Result<String> {
    args.next()
        .ok_or_else(|| anyhow!("missing required argument: {name}"))
}

fn parse_record(
    log_content: &str,
    cycle: i64,
    timestamp: String,
    run_exit: i64,
    status: String,
    log_path: &Path,
) -> Result<RetrospectiveRecord> {
    let lines: Vec<&str> = log_content.lines().collect();
    let status_start = lines
        .iter()
        .rposition(|line| line.starts_with("STATUS: "))
        .ok_or_else(|| anyhow!("no final status block found"))?;

    let block = &lines[status_start..];

    let mut branch = String::from("none");
    let mut commit = String::from("none");
    let mut pr = String::from("none");

    let mut sections: HashMap<&'static str, Vec<String>> = HashMap::from([
        ("helpful_feature_ideas", Vec::new()),
        ("struggle_lines", Vec::new()),
        ("preventable_lines", Vec::new()),
        ("prevention_notes", Vec::new()),
    ]);
    let mut current_section: Option<&'static str> = None;

    for line in block {
        if let Some(value) = line.strip_prefix("BRANCH: ") {
            branch = value.trim().to_owned();
            current_section = None;
            continue;
        }
        if let Some(value) = line.strip_prefix("COMMIT: ") {
            commit = value.trim().to_owned();
            current_section = None;
            continue;
        }
        if let Some(value) = line.strip_prefix("PR: ") {
            pr = value.trim().to_owned();
            current_section = None;
            continue;
        }

        let stripped = line.trim();
        if let Some(section_name) = stripped.strip_suffix(':') {
            current_section = match section_name {
                "HELPFUL FEATURE IDEAS" => Some("helpful_feature_ideas"),
                "STRUGGLE" => Some("struggle_lines"),
                "PREVENTABLE" => Some("preventable_lines"),
                "PREVENTION NOTES" => Some("prevention_notes"),
                _ => None,
            };
            continue;
        }

        if stripped.is_empty() {
            continue;
        }

        let Some(section_key) = current_section else {
            continue;
        };

        let entry = sections
            .get_mut(section_key)
            .ok_or_else(|| anyhow!("missing section: {section_key}"))?;
        if let Some(value) = stripped.strip_prefix("- ") {
            entry.push(value.trim().to_owned());
        } else if let Some(last) = entry.last_mut() {
            last.push(' ');
            last.push_str(stripped);
        } else {
            entry.push(stripped.to_owned());
        }
    }

    let helpful_feature_ideas = sections.remove("helpful_feature_ideas").unwrap_or_default();
    let struggle_lines = sections.remove("struggle_lines").unwrap_or_default();
    let preventable_lines = sections.remove("preventable_lines").unwrap_or_default();
    let prevention_notes = sections.remove("prevention_notes").unwrap_or_default();

    let preventable_raw = join_and_trim_lower(&preventable_lines);
    let preventable = match preventable_raw.as_deref() {
        Some("yes") => Some(true),
        Some("no") => Some(false),
        Some(_) => None,
        None => None,
    };

    Ok(RetrospectiveRecord {
        cycle,
        timestamp,
        status,
        run_exit,
        log_file: log_path.to_string_lossy().to_string(),
        branch,
        commit,
        pr,
        helpful_feature_ideas,
        struggle: struggle_lines.join(" ").trim().to_owned(),
        preventable,
        preventable_raw,
        prevention_notes,
    })
}

fn join_and_trim_lower(lines: &[String]) -> Option<String> {
    let value = lines.join(" ");
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_lowercase())
    }
}

fn append_record(output_path: &Path, record: &RetrospectiveRecord) -> Result<()> {
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory: {}", parent.display()))?;
    }

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(output_path)
        .with_context(|| format!("failed to open output file: {}", output_path.display()))?;
    serde_json::to_writer(&mut file, record).context("failed to serialize retrospective record")?;
    file.write_all(b"\n")
        .context("failed to write trailing newline")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_record_extracts_status_block_and_sections() {
        let log_content = r#"noise
STATUS: success
BRANCH: improve/rust
COMMIT: abc123
PR: https://example/pr/1

HELPFUL FEATURE IDEAS:
- One
- Two
continued details

STRUGGLE:
- Hard thing

PREVENTABLE:
- yes

PREVENTION NOTES:
- Add richer diagnostics
"#;

        let record = parse_record(
            log_content,
            2,
            "20260101T000000".to_string(),
            0,
            "success".to_string(),
            Path::new("/tmp/cycle.log"),
        )
        .expect("record should parse");

        assert_eq!(record.cycle, 2);
        assert_eq!(record.branch, "improve/rust");
        assert_eq!(record.commit, "abc123");
        assert_eq!(record.pr, "https://example/pr/1");
        assert_eq!(
            record.helpful_feature_ideas,
            vec!["One".to_string(), "Two continued details".to_string()]
        );
        assert_eq!(record.struggle, "Hard thing");
        assert_eq!(record.preventable, Some(true));
        assert_eq!(record.preventable_raw, Some("yes".to_string()));
        assert_eq!(
            record.prevention_notes,
            vec!["Add richer diagnostics".to_string()]
        );
    }

    #[test]
    fn parse_record_handles_unknown_preventable_value() {
        let log_content = r#"STATUS: blocked
PREVENTABLE:
- maybe
"#;

        let record = parse_record(
            log_content,
            1,
            "20260101T000001".to_string(),
            1,
            "blocked".to_string(),
            Path::new("/tmp/cycle.log"),
        )
        .expect("record should parse");

        assert_eq!(record.preventable, None);
        assert_eq!(record.preventable_raw, Some("maybe".to_string()));
    }
}
