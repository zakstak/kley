use sha2::{Digest, Sha256};

use super::EditFailureKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedHashlineAnchor {
    pub line_number: usize,
    pub hash: String,
}

pub fn parse_hashline_anchor(raw: &str) -> Result<ParsedHashlineAnchor, EditFailureKind> {
    let (line, hash) = raw.split_once('#').ok_or(EditFailureKind::InvalidRequest)?;
    let line_number = line
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or(EditFailureKind::InvalidRequest)?;
    let hash = hash.trim();

    if hash.is_empty() || hash.contains('#') || hash.chars().any(char::is_whitespace) {
        return Err(EditFailureKind::InvalidRequest);
    }

    Ok(ParsedHashlineAnchor {
        line_number,
        hash: hash.to_string(),
    })
}

#[derive(Debug)]
pub struct HashlineSnapshot {
    lines: Vec<SnapshotLine>,
}

impl HashlineSnapshot {
    pub fn from_text(text: &str) -> Self {
        let lines = text
            .lines()
            .enumerate()
            .map(|(index, line)| SnapshotLine {
                line_number: index + 1,
                hash: hash_line(line),
            })
            .collect();

        Self { lines }
    }

    pub fn resolve_anchor(&self, anchor: &ParsedHashlineAnchor) -> Result<usize, EditFailureKind> {
        if self
            .lines
            .iter()
            .any(|line| line.line_number == anchor.line_number && line.hash == anchor.hash)
        {
            return Ok(anchor.line_number);
        }

        let matching_lines = self
            .lines
            .iter()
            .filter(|line| line.hash == anchor.hash)
            .count();

        if matching_lines > 1 {
            return Err(EditFailureKind::AmbiguousAnchor);
        }

        Err(EditFailureKind::StaleReference)
    }
}

#[derive(Debug)]
struct SnapshotLine {
    line_number: usize,
    hash: String,
}

pub fn hash_line(line: &str) -> String {
    let digest = Sha256::digest(line.as_bytes());
    hex::encode(&digest[..4])
}
