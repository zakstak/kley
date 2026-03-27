use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use kley::tools::editing::hashline::HashlineEditEngine;
use kley::tools::editing::{
    EditEngine, EditFailureKind, EditOperation, EditOutcome, EditRequest,
    EDIT_ALLOW_PATCH_FALLBACK, EDIT_APPLY_IS_ATOMIC, EDIT_SINGLE_FILE_ONLY,
    EDIT_TOOL_SUMMARY_MAX_CHARS,
};
use kley::tools::hashline_edit::{HashlineEditRequest, HashlineEditTool};
use kley::tools::patch::PatchEditEngine;
use kley::tools::Tool;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const _: () = {
    assert!(EDIT_SINGLE_FILE_ONLY);
    assert!(EDIT_APPLY_IS_ATOMIC);
    assert!(!EDIT_ALLOW_PATCH_FALLBACK);
};

#[derive(Debug, Deserialize)]
struct ContractScenario {
    name: String,
    request: EditRequest,
    outcome: EditOutcome,
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hashline")
}

fn load_scenarios() -> Vec<ContractScenario> {
    let mut files = fs::read_dir(fixtures_dir())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    files.sort();

    files
        .into_iter()
        .map(|path| {
            let raw = fs::read_to_string(&path).unwrap();
            serde_json::from_str::<ContractScenario>(&raw)
                .unwrap_or_else(|err| panic!("failed to parse fixture {}: {err}", path.display()))
        })
        .collect()
}

fn hashline_validation_dir() -> PathBuf {
    fixtures_dir().join("validation")
}

fn hashline_snapshots_dir() -> PathBuf {
    fixtures_dir().join("snapshots")
}

fn load_hashline_request(name: &str) -> HashlineEditRequest {
    let path = hashline_validation_dir().join(name);
    let raw = fs::read_to_string(&path).unwrap();
    serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("failed to parse hashline fixture {}: {err}", path.display()))
}

fn load_hashline_snapshot(name: &str) -> String {
    fs::read_to_string(hashline_snapshots_dir().join(name)).unwrap()
}

fn anchor_for_line(snapshot: &str, line_number: usize) -> String {
    let line = snapshot
        .lines()
        .nth(line_number - 1)
        .unwrap_or_else(|| panic!("line {} missing in snapshot", line_number));
    format!("{}#{}", line_number, hash_line(line))
}

fn hash_line(line: &str) -> String {
    let digest = Sha256::digest(line.as_bytes());
    hex::encode(&digest[..4])
}

#[test]
fn contract_suite_freezes_single_file_atomic_semantics() {
    for scenario in load_scenarios() {
        let validation = scenario.request.validate_contract();
        match scenario.outcome.failure_kind() {
            Some(EditFailureKind::InvalidRequest) => {
                assert_eq!(validation, Err(EditFailureKind::InvalidRequest));
            }
            _ => assert!(
                validation.is_ok(),
                "request should be valid: {}",
                scenario.name
            ),
        }

        let summary = scenario.outcome.tool_summary();
        assert!(summary.chars().count() <= EDIT_TOOL_SUMMARY_MAX_CHARS + 3);
        assert!(!summary.contains('\n'));
    }
}

#[test]
fn failure_taxonomy_includes_noop_and_stale_reference() {
    let frozen = EditFailureKind::ALL
        .iter()
        .map(|kind| kind.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        frozen,
        vec![
            "stale_reference",
            "ambiguous_anchor",
            "no_op",
            "invalid_request",
            "io_error",
            "non_text_file",
            "telemetry_unavailable",
        ]
    );
    assert!(frozen.contains(&"no_op"));
    assert!(frozen.contains(&"stale_reference"));
}

#[test]
fn fixture_suite_covers_required_hashline_contract_cases() {
    let scenarios = load_scenarios();
    let names = scenarios
        .iter()
        .map(|scenario| scenario.name.as_str())
        .collect::<BTreeSet<_>>();

    let expected = BTreeSet::from([
        "success",
        "stale_reference",
        "ambiguous_anchor",
        "no_op",
        "invalid_request",
        "io_error",
        "non_text_file",
        "telemetry_unavailable",
    ]);

    assert_eq!(names, expected);

    for scenario in scenarios {
        match scenario.name.as_str() {
            "success" => assert!(matches!(scenario.outcome, EditOutcome::Applied { .. })),
            "stale_reference" => assert!(matches!(
                scenario.outcome,
                EditOutcome::Failed {
                    kind: EditFailureKind::StaleReference,
                    ..
                }
            )),
            "ambiguous_anchor" => assert!(matches!(
                scenario.outcome,
                EditOutcome::Failed {
                    kind: EditFailureKind::AmbiguousAnchor,
                    ..
                }
            )),
            "no_op" => assert!(matches!(
                scenario.outcome,
                EditOutcome::Failed {
                    kind: EditFailureKind::NoOp,
                    ..
                }
            )),
            "invalid_request" => assert!(matches!(
                scenario.outcome,
                EditOutcome::Failed {
                    kind: EditFailureKind::InvalidRequest,
                    ..
                }
            )),
            "io_error" => assert!(matches!(
                scenario.outcome,
                EditOutcome::Failed {
                    kind: EditFailureKind::IoError,
                    ..
                }
            )),
            "non_text_file" => assert!(matches!(
                scenario.outcome,
                EditOutcome::Failed {
                    kind: EditFailureKind::NonTextFile,
                    ..
                }
            )),
            "telemetry_unavailable" => assert!(matches!(
                scenario.outcome,
                EditOutcome::Failed {
                    kind: EditFailureKind::TelemetryUnavailable,
                    ..
                }
            )),
            other => panic!("unexpected scenario fixture: {other}"),
        }
    }
}

#[test]
fn patch_engine_matches_contract_baseline() {
    let engine = PatchEditEngine;

    let invalid_path = engine.apply(&EditRequest {
        path: "".to_string(),
        operations: vec![EditOperation {
            kind: "replace_exact".to_string(),
            anchor: "needle".to_string(),
            end_anchor: None,
            lines: vec!["replacement".to_string()],
        }],
    });
    assert!(matches!(
        invalid_path,
        EditOutcome::Failed {
            kind: EditFailureKind::InvalidRequest,
            ..
        }
    ));

    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"alpha\nbeta\nalpha\n").unwrap();
    let path = file.path().to_string_lossy().to_string();

    let stale_reference = engine.apply(&EditRequest {
        path: path.clone(),
        operations: vec![EditOperation {
            kind: "replace_exact".to_string(),
            anchor: "missing".to_string(),
            end_anchor: None,
            lines: vec!["x".to_string()],
        }],
    });
    assert!(matches!(
        stale_reference,
        EditOutcome::Failed {
            kind: EditFailureKind::StaleReference,
            ..
        }
    ));

    let ambiguous = engine.apply(&EditRequest {
        path: path.clone(),
        operations: vec![EditOperation {
            kind: "replace_exact".to_string(),
            anchor: "alpha".to_string(),
            end_anchor: None,
            lines: vec!["zeta".to_string()],
        }],
    });
    assert!(matches!(
        ambiguous,
        EditOutcome::Failed {
            kind: EditFailureKind::AmbiguousAnchor,
            ..
        }
    ));

    let mut single_match_file = tempfile::NamedTempFile::new().unwrap();
    single_match_file
        .write_all(b"fn main() {\n    println!(\"hello\");\n}\n")
        .unwrap();
    let single_match_path = single_match_file.path().to_string_lossy().to_string();
    let applied = engine.apply(&EditRequest {
        path: single_match_path.clone(),
        operations: vec![EditOperation {
            kind: "replace_exact".to_string(),
            anchor: "    println!(\"hello\");".to_string(),
            end_anchor: None,
            lines: vec!["    println!(\"world\");".to_string()],
        }],
    });
    assert!(matches!(applied, EditOutcome::Applied { .. }));

    let io_error = engine.apply(&EditRequest {
        path: "/definitely/nonexistent/path.rs".to_string(),
        operations: vec![EditOperation {
            kind: "replace_exact".to_string(),
            anchor: "needle".to_string(),
            end_anchor: None,
            lines: vec!["replacement".to_string()],
        }],
    });
    assert!(matches!(
        io_error,
        EditOutcome::Failed {
            kind: EditFailureKind::IoError,
            ..
        }
    ));
}

#[test]
fn hashline_request_schema_is_strict() {
    let tool = HashlineEditTool;
    let schema = tool.parameters_schema();

    assert_eq!(
        schema,
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to one file"
                },
                "edits": {
                    "type": "array",
                    "description": "Edits against one original snapshot",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "kind": {
                                "type": "string",
                                "enum": ["replace", "insert_before", "insert_after", "delete"],
                                "description": "Edit kind"
                            },
                            "start": {
                                "type": "string",
                                "description": "Start anchor as LINE#HASH"
                            },
                            "end": {
                                "type": "string",
                                "description": "Optional end anchor as LINE#HASH"
                            },
                            "replacement": {
                                "type": "string",
                                "description": "Replacement text"
                            }
                        },
                        "required": ["kind", "start"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["path", "edits"],
            "additionalProperties": false
        })
    );
    assert!(serde_json::to_string(&schema).unwrap().len() < 1_200);

    let valid_request = load_hashline_request("valid_request.json");
    assert!(valid_request.validate_contract().is_ok());

    let mut with_unknown_top_level = serde_json::to_value(&valid_request).unwrap();
    with_unknown_top_level["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<HashlineEditRequest>(with_unknown_top_level).is_err());

    let mut with_unknown_edit_field = serde_json::to_value(&valid_request).unwrap();
    with_unknown_edit_field["edits"][0]["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<HashlineEditRequest>(with_unknown_edit_field).is_err());

    let invalid_end_usage = load_hashline_request("invalid_end_usage.json");
    assert_eq!(
        invalid_end_usage.validate_contract(),
        Err(EditFailureKind::InvalidRequest)
    );
}

#[test]
fn hashline_validation_rejects_overlapping_or_out_of_order_ranges() {
    let original_snapshot = load_hashline_snapshot("original.txt");

    let valid_request = load_hashline_request("valid_request.json");
    let resolved = valid_request
        .validate_against_snapshot(&original_snapshot)
        .unwrap();
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0].start_line, 2);
    assert_eq!(resolved[0].end_line, 2);
    assert_eq!(resolved[1].start_line, 5);
    assert_eq!(resolved[1].end_line, 6);

    let overlapping = load_hashline_request("overlapping_ranges.json");
    assert_eq!(
        overlapping.validate_against_snapshot(&original_snapshot),
        Err(EditFailureKind::InvalidRequest)
    );

    let out_of_order = load_hashline_request("out_of_order_ranges.json");
    assert_eq!(
        out_of_order.validate_against_snapshot(&original_snapshot),
        Err(EditFailureKind::InvalidRequest)
    );
}

#[test]
fn hashline_validation_rejects_stale_refs_without_fallback() {
    let original_snapshot = load_hashline_snapshot("original.txt");

    let stale = load_hashline_request("stale_reference.json");
    assert!(stale.validate_contract().is_ok());
    assert_eq!(
        stale.validate_against_snapshot(&original_snapshot),
        Err(EditFailureKind::StaleReference)
    );

    let ambiguous = load_hashline_request("ambiguous_anchor.json");
    assert!(ambiguous.validate_contract().is_ok());
    assert_eq!(
        ambiguous.validate_against_snapshot(&original_snapshot),
        Err(EditFailureKind::AmbiguousAnchor)
    );
}

#[test]
fn hashline_engine_applies_bottom_up_against_original_snapshot() {
    let engine = HashlineEditEngine;
    let original = load_hashline_snapshot("bottom_up_original.txt");
    let expected = load_hashline_snapshot("bottom_up_expected.txt");

    let mut target = tempfile::NamedTempFile::new().unwrap();
    target.write_all(original.as_bytes()).unwrap();
    let path = target.path().to_string_lossy().to_string();

    let outcome = engine.apply(&EditRequest {
        path: path.clone(),
        operations: vec![
            EditOperation {
                kind: "replace".to_string(),
                anchor: anchor_for_line(&original, 2),
                end_anchor: Some(anchor_for_line(&original, 2)),
                lines: vec!["beta-1\nbeta-2\n".to_string()],
            },
            EditOperation {
                kind: "replace".to_string(),
                anchor: anchor_for_line(&original, 4),
                end_anchor: Some(anchor_for_line(&original, 4)),
                lines: vec!["DELTA!\n".to_string()],
            },
        ],
    });

    assert!(matches!(outcome, EditOutcome::Applied { .. }));
    let updated = fs::read_to_string(path).unwrap();
    assert_eq!(updated, expected);
}

#[test]
fn hashline_engine_preserves_crlf_and_bom() {
    let engine = HashlineEditEngine;
    let original_bytes = b"\xEF\xBB\xBFalpha\r\nbeta\r\ngamma\r\n".to_vec();
    let canonical_snapshot = "alpha\nbeta\ngamma\n";

    let mut target = tempfile::NamedTempFile::new().unwrap();
    target.write_all(&original_bytes).unwrap();
    let path = target.path().to_string_lossy().to_string();

    let outcome = engine.apply(&EditRequest {
        path: path.clone(),
        operations: vec![EditOperation {
            kind: "replace".to_string(),
            anchor: anchor_for_line(canonical_snapshot, 2),
            end_anchor: Some(anchor_for_line(canonical_snapshot, 2)),
            lines: vec!["BETA!\n".to_string()],
        }],
    });

    assert!(matches!(outcome, EditOutcome::Applied { .. }));
    let updated = fs::read(path).unwrap();
    assert!(updated.starts_with(&[0xEF, 0xBB, 0xBF]));

    let text = String::from_utf8(updated[3..].to_vec()).unwrap();
    assert!(text.contains("\r\n"));
    assert!(!text.contains("\nBETA!\n"));
    assert!(text.ends_with("\r\n"));
}

#[test]
fn hashline_engine_rejects_noop_and_non_utf8_inputs() {
    let engine = HashlineEditEngine;
    let original = load_hashline_snapshot("bottom_up_original.txt");

    let mut noop_target = tempfile::NamedTempFile::new().unwrap();
    noop_target.write_all(original.as_bytes()).unwrap();
    let noop_path = noop_target.path().to_string_lossy().to_string();

    let noop = engine.apply(&EditRequest {
        path: noop_path.clone(),
        operations: vec![EditOperation {
            kind: "replace".to_string(),
            anchor: anchor_for_line(&original, 2),
            end_anchor: Some(anchor_for_line(&original, 2)),
            lines: vec!["beta\n".to_string()],
        }],
    });

    assert!(matches!(
        noop,
        EditOutcome::Failed {
            kind: EditFailureKind::NoOp,
            ..
        }
    ));
    let noop_after = fs::read_to_string(noop_path).unwrap();
    assert_eq!(noop_after, original);

    let mut binary_target = tempfile::NamedTempFile::new().unwrap();
    binary_target.write_all(&[0xFF, 0xFE, 0xF8, 0x00]).unwrap();
    let binary_path = binary_target.path().to_string_lossy().to_string();

    let non_text = engine.apply(&EditRequest {
        path: binary_path,
        operations: vec![EditOperation {
            kind: "replace".to_string(),
            anchor: "1#deadbeef".to_string(),
            end_anchor: Some("1#deadbeef".to_string()),
            lines: vec!["replacement\n".to_string()],
        }],
    });

    assert!(matches!(
        non_text,
        EditOutcome::Failed {
            kind: EditFailureKind::NonTextFile,
            ..
        }
    ));
}
