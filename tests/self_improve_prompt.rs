use std::fs;
use std::path::PathBuf;

fn self_improve_script() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("self-improve.sh");
    fs::read_to_string(path).expect("self-improve.sh should be readable")
}

fn assert_ordered_markers(script: &str, markers: &[&str], context: &str) {
    let mut cursor = 0;
    for marker in markers {
        let relative_index = script[cursor..]
            .find(marker)
            .unwrap_or_else(|| panic!("expected {context} to contain {marker:?}"));
        cursor += relative_index + marker.len();
    }
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

#[test]
fn self_improve_prompt_requires_explicit_base_branch_guidance() {
    let script = self_improve_script();

    for required in [
        "BASE_BRANCH=\"${BASE_BRANCH:-main}\"",
        "git fetch \"$REMOTE\" \"$BASE_BRANCH:$BASE_BRANCH\"",
        "git pull --ff-only \"$REMOTE\" \"$BASE_BRANCH\"",
        "git config branch.\"$(git branch --show-current)\".gh-merge-base \"$BASE_BRANCH\"",
        "gh pr create --repo zakstak/kley --base \"$BASE_BRANCH\" --head improve/<slug> --title \"<title>\" --body \"<body>\"",
    ] {
        assert!(
            script.contains(required),
            "expected self-improve prompt to contain {required:?}"
        );
    }

    assert_ordered_markers(
        &script,
        &[
            "git ls-remote origin HEAD",
            "git ls-remote upstream HEAD",
            "git pull --ff-only \"$REMOTE\" \"$BASE_BRANCH\"",
        ],
        "self-improve base branch flow",
    );
}
