use std::fs;
use std::path::PathBuf;

fn self_improve_script() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("self-improve.sh");
    fs::read_to_string(path).expect("self-improve.sh should be readable")
}

#[test]
fn self_improve_prompt_requires_grounded_retrospective_sections() {
    let script = self_improve_script();

    for required in [
        "## Required retrospective",
        "List up to 3 concrete feature ideas suggested by the actual cycle",
        "say \"none identified\" and explain why.",
        "Record the hardest real struggle you encountered during the cycle.",
        "would likely have prevented or materially reduced that struggle.",
        "HELPFUL FEATURE IDEAS:",
        "STRUGGLE:",
        "PREVENTABLE:",
        "PREVENTION NOTES:",
    ] {
        assert!(
            script.contains(required),
            "expected self-improve prompt contract to contain {required:?}"
        );
    }
}

#[test]
fn self_improve_prompt_keeps_retrospective_fields_in_final_status_order() {
    let script = self_improve_script();
    let block_start = script
        .find("## Required final status block")
        .expect("required final status block should exist");
    let final_block = &script[block_start..];

    let mut cursor = 0;
    for marker in [
        "STATUS: success|blocked|no-safe-change",
        "SUMMARY:",
        "HELPFUL FEATURE IDEAS:",
        "STRUGGLE:",
        "PREVENTABLE:",
        "PREVENTION NOTES:",
        "NEXT:",
    ] {
        let relative_index = final_block[cursor..]
            .find(marker)
            .unwrap_or_else(|| panic!("expected final status block to contain {marker:?}"));
        cursor += relative_index + marker.len();
    }
}

#[test]
fn self_improve_script_appends_structured_retrospective_records() {
    let script = self_improve_script();

    for required in [
        "RETROSPECTIVE_FILE=\"$LOG_DIR/retrospectives.jsonl\"",
        "run_repo_cargo_bin()",
        "append_retrospective_record()",
        "run_repo_cargo_bin self-improve-retrospective \\",
        "\"$RETROSPECTIVE_FILE\"",
        "Retrospective record appended to $RETROSPECTIVE_FILE",
    ] {
        assert!(
            script.contains(required),
            "expected self-improve script to contain {required:?}"
        );
    }
}
