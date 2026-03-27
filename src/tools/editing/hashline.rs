use std::time::Instant;

use super::hashline_anchor::{parse_hashline_anchor, HashlineSnapshot};
use super::io::atomic_replace;
use super::{
    EditEngine, EditFailureKind, EditObservation, EditOperation, EditOutcome, EditRequest,
};

pub struct HashlineEditEngine;

impl EditEngine for HashlineEditEngine {
    fn name(&self) -> &str {
        "hashline_edit"
    }

    fn apply(&self, request: &EditRequest) -> EditOutcome {
        let started_at = Instant::now();
        let path = request.path.as_str();
        let edit_count = request.operations.len();

        if let Err(kind) = request.validate_contract() {
            return failure_outcome(
                kind,
                format!("Error: {}", kind.as_str()),
                path,
                edit_count,
                started_at.elapsed().as_millis(),
            );
        }

        let original_bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) => {
                return failure_outcome(
                    EditFailureKind::IoError,
                    format!("Error: failed to read {path}: {err}"),
                    path,
                    edit_count,
                    started_at.elapsed().as_millis(),
                );
            }
        };

        let (original_canonical, fidelity) = match canonicalize_utf8_text(&original_bytes) {
            Ok(text) => text,
            Err(kind) => {
                return failure_outcome(
                    kind,
                    format!("Error: {}", kind.as_str()),
                    path,
                    edit_count,
                    started_at.elapsed().as_millis(),
                );
            }
        };

        let snapshot = HashlineSnapshot::from_text(&original_canonical);
        let mut resolved = Vec::with_capacity(request.operations.len());
        for (index, operation) in request.operations.iter().enumerate() {
            match resolve_operation(operation, &snapshot, index) {
                Ok(value) => resolved.push(value),
                Err(kind) => {
                    return failure_outcome(
                        kind,
                        format!("Error: {}", kind.as_str()),
                        path,
                        edit_count,
                        started_at.elapsed().as_millis(),
                    );
                }
            }
        }

        if let Err(kind) = validate_overlaps(&resolved) {
            return failure_outcome(
                kind,
                format!("Error: {}", kind.as_str()),
                path,
                edit_count,
                started_at.elapsed().as_millis(),
            );
        }

        let mut updated_lines = text_to_lines(&original_canonical);
        resolved.sort_by(|left, right| {
            right
                .start_line
                .cmp(&left.start_line)
                .then_with(|| right.kind.precedence().cmp(&left.kind.precedence()))
                .then_with(|| right.original_index.cmp(&left.original_index))
        });

        for op in &resolved {
            apply_resolved_operation(&mut updated_lines, op);
        }

        let rewritten_canonical = lines_to_text(&updated_lines, fidelity.has_final_newline);
        if rewritten_canonical == original_canonical {
            return failure_outcome(
                EditFailureKind::NoOp,
                "Error: no_op".to_string(),
                path,
                edit_count,
                started_at.elapsed().as_millis(),
            );
        }

        let rewritten_bytes = render_with_fidelity(&rewritten_canonical, &fidelity);
        if let Err(err) = atomic_replace(std::path::Path::new(path), &rewritten_bytes) {
            return failure_outcome(
                EditFailureKind::IoError,
                format!("Error: failed to write {path}: {err}"),
                path,
                edit_count,
                started_at.elapsed().as_millis(),
            );
        }

        EditOutcome::Applied {
            summary: format!("Applied {edit_count} hashline edit(s) to {path}"),
            observations: vec![success_observation(
                path,
                edit_count,
                started_at.elapsed().as_millis(),
            )],
        }
    }
}

fn resolve_operation(
    operation: &EditOperation,
    snapshot: &HashlineSnapshot,
    original_index: usize,
) -> Result<ResolvedOperation, EditFailureKind> {
    let kind = HashlineOperationKind::parse(&operation.kind)?;
    let start_anchor = parse_hashline_anchor(&operation.anchor)?;
    let start_line = snapshot.resolve_anchor(&start_anchor)?;

    if !kind.supports_end() && operation.end_anchor.is_some() {
        return Err(EditFailureKind::InvalidRequest);
    }

    let end_line = match operation.end_anchor.as_deref() {
        Some(end_anchor) => snapshot.resolve_anchor(&parse_hashline_anchor(end_anchor)?)?,
        None => start_line,
    };

    if end_line < start_line {
        return Err(EditFailureKind::InvalidRequest);
    }

    let replacement = if kind.requires_replacement() {
        if operation.lines.is_empty() {
            return Err(EditFailureKind::InvalidRequest);
        }
        Some(operation.lines.join("\n"))
    } else {
        if !operation.lines.is_empty() {
            return Err(EditFailureKind::InvalidRequest);
        }
        None
    };

    Ok(ResolvedOperation {
        kind,
        start_line,
        end_line,
        replacement,
        original_index,
    })
}

fn validate_overlaps(operations: &[ResolvedOperation]) -> Result<(), EditFailureKind> {
    let mut ranges = Vec::new();
    let mut insert_targets = Vec::new();

    for op in operations {
        match op.kind {
            HashlineOperationKind::InsertBefore | HashlineOperationKind::InsertAfter => {
                insert_targets.push((op.start_line, op.kind));
            }
            HashlineOperationKind::Replace | HashlineOperationKind::Delete => {
                ranges.push((op.start_line, op.end_line));
            }
        }
    }

    ranges.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    for pair in ranges.windows(2) {
        if pair[1].0 <= pair[0].1 {
            return Err(EditFailureKind::InvalidRequest);
        }
    }

    for &(target, kind) in &insert_targets {
        if ranges.iter().any(|&(start, end)| {
            let inside_span = target > start && target <= end;
            let insert_after_start_of_multiline =
                matches!(kind, HashlineOperationKind::InsertAfter)
                    && target == start
                    && start < end;
            inside_span || insert_after_start_of_multiline
        }) {
            return Err(EditFailureKind::InvalidRequest);
        }
    }

    Ok(())
}

fn apply_resolved_operation(lines: &mut Vec<String>, op: &ResolvedOperation) {
    match op.kind {
        HashlineOperationKind::Replace => {
            lines.splice(
                (op.start_line - 1)..op.end_line,
                replacement_to_lines(op.replacement.as_deref().unwrap_or_default()),
            );
        }
        HashlineOperationKind::Delete => {
            lines.splice((op.start_line - 1)..op.end_line, std::iter::empty());
        }
        HashlineOperationKind::InsertBefore => {
            lines.splice(
                (op.start_line - 1)..(op.start_line - 1),
                replacement_to_lines(op.replacement.as_deref().unwrap_or_default()),
            );
        }
        HashlineOperationKind::InsertAfter => {
            lines.splice(
                op.start_line..op.start_line,
                replacement_to_lines(op.replacement.as_deref().unwrap_or_default()),
            );
        }
    }
}

fn replacement_to_lines(replacement: &str) -> Vec<String> {
    if replacement.is_empty() {
        return vec![String::new()];
    }

    let mut lines = replacement
        .split('\n')
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if replacement.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn text_to_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut lines = text
        .split('\n')
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn lines_to_text(lines: &[String], has_final_newline: bool) -> String {
    let mut text = lines.join("\n");
    if has_final_newline {
        text.push('\n');
    }
    text
}

fn canonicalize_utf8_text(bytes: &[u8]) -> Result<(String, TextFidelity), EditFailureKind> {
    let (has_bom, body) = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        (true, &bytes[3..])
    } else {
        (false, bytes)
    };

    let text = String::from_utf8(body.to_vec()).map_err(|_| EditFailureKind::NonTextFile)?;
    let line_ending = if text.contains("\r\n") {
        LineEndingStyle::Crlf
    } else {
        LineEndingStyle::Lf
    };

    let canonical = match line_ending {
        LineEndingStyle::Lf => text,
        LineEndingStyle::Crlf => text.replace("\r\n", "\n"),
    };

    Ok((
        canonical.clone(),
        TextFidelity {
            has_bom,
            line_ending,
            has_final_newline: canonical.ends_with('\n'),
        },
    ))
}

fn render_with_fidelity(canonical_text: &str, fidelity: &TextFidelity) -> Vec<u8> {
    let mut text = match fidelity.line_ending {
        LineEndingStyle::Lf => canonical_text.to_string(),
        LineEndingStyle::Crlf => canonical_text.replace('\n', "\r\n"),
    };

    if fidelity.has_bom {
        text.insert(0, '\u{FEFF}');
    }

    text.into_bytes()
}

fn failure_outcome(
    kind: EditFailureKind,
    summary: String,
    path: &str,
    edit_count: usize,
    duration_ms: u128,
) -> EditOutcome {
    EditOutcome::Failed {
        kind,
        summary,
        observations: vec![failure_observation(kind, path, edit_count, duration_ms)],
    }
}

fn success_observation(path: &str, edit_count: usize, duration_ms: u128) -> EditObservation {
    let mut observation = EditObservation::new(
        "hashline_edit",
        "hashline_edit",
        path,
        edit_count,
        duration_ms,
    );
    observation.applied_count = edit_count;
    observation
}

fn failure_observation(
    kind: EditFailureKind,
    path: &str,
    edit_count: usize,
    duration_ms: u128,
) -> EditObservation {
    let mut observation = EditObservation::new(
        "hashline_edit",
        "hashline_edit",
        path,
        edit_count,
        duration_ms,
    );
    observation.failure_kind = Some(kind.as_str().to_string());
    observation.stale_reference_count = usize::from(kind == EditFailureKind::StaleReference);
    observation.noop_count = usize::from(kind == EditFailureKind::NoOp);
    observation
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum HashlineOperationKind {
    Replace,
    InsertBefore,
    InsertAfter,
    Delete,
}

impl HashlineOperationKind {
    fn parse(raw: &str) -> Result<Self, EditFailureKind> {
        match raw {
            "replace" => Ok(Self::Replace),
            "insert_before" => Ok(Self::InsertBefore),
            "insert_after" => Ok(Self::InsertAfter),
            "delete" => Ok(Self::Delete),
            _ => Err(EditFailureKind::InvalidRequest),
        }
    }

    const fn supports_end(self) -> bool {
        matches!(self, Self::Replace | Self::Delete)
    }

    const fn requires_replacement(self) -> bool {
        matches!(self, Self::Replace | Self::InsertBefore | Self::InsertAfter)
    }

    const fn precedence(self) -> u8 {
        match self {
            Self::InsertAfter => 3,
            Self::Replace => 2,
            Self::Delete => 1,
            Self::InsertBefore => 0,
        }
    }
}

#[derive(Debug, Clone)]
struct ResolvedOperation {
    kind: HashlineOperationKind,
    start_line: usize,
    end_line: usize,
    replacement: Option<String>,
    original_index: usize,
}

#[derive(Debug, Copy, Clone)]
enum LineEndingStyle {
    Lf,
    Crlf,
}

#[derive(Debug, Copy, Clone)]
struct TextFidelity {
    has_bom: bool,
    line_ending: LineEndingStyle,
    has_final_newline: bool,
}
