use std::path::PathBuf;
use std::{env, fs};

use kley::tools;

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

fn prompt_capability_tools(script: &str) -> Vec<String> {
    let block_start = script
        .find("You only have these capabilities in this harness:")
        .expect("capability block should exist");
    let after_header = &script[block_start..];
    let block_end = after_header
        .find("Do not assume any other tools, callbacks, or hidden functions exist.")
        .expect("capability block should end before the no-hidden-tools guardrail");
    let capability_block = &after_header[..block_end];

    capability_block
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("- "))
        .map(|line| {
            line.trim_start_matches("- ")
                .trim()
                .trim_matches('`')
                .to_string()
        })
        .collect()
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
fn self_improve_prompt_tool_list_matches_builtin_registry() {
    let script = self_improve_script();
    let prompt_tools = prompt_capability_tools(&script);
    let registry =
        tools::default_registry(env::current_dir().expect("test cwd should be readable"));
    let builtin_tools: Vec<String> = registry
        .to_api_tools()
        .into_iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .expect("tool name should be present in API schema")
                .to_string()
        })
        .collect();

    assert_eq!(
        prompt_tools, builtin_tools,
        "expected self-improve prompt tool list to stay in sync with built-in runtime tools"
    );
    assert!(
        script.contains("There is no separate `git` or `write` tool."),
        "expected self-improve prompt to explain that git/write are not separate tools"
    );
}

#[test]
fn self_improve_script_enters_repo_root_before_logging_or_launch() {
    let script = self_improve_script();

    assert_ordered_markers(
        &script,
        &[
            "SCRIPT_DIR=",
            "cd \"$SCRIPT_DIR\"",
            "LOG_DIR=",
            "run_kley chat \\",
        ],
        "self-improve launcher flow",
    );
}

#[test]
fn self_improve_prompt_allows_tooling_improvements_for_future_cycles() {
    let script = self_improve_script();

    for required in [
        "These are the tools available to you in the current cycle.",
        "You may still modify the harness, tool registry, prompts, or workflows to implement or wire in a tool/capability for future cycles when that is the highest-value evidence-backed change.",
        "Prompt or registry wording alone does not count unless it lands executable behavior or deterministic validation.",
        "If you add a tool, validate it locally and remember that the new capability only becomes available after a later cycle starts.",
    ] {
        assert!(
            script.contains(required),
            "expected self-improve prompt to preserve future-tooling guidance {required:?}"
        );
    }
}

#[test]
fn self_improve_prompt_treats_evidence_backed_tooling_gaps_as_valid_work() {
    let script = self_improve_script();

    for required in [
        "Hardens a reproducible harness/workflow failure or closes a concrete missing capability (including a new tool) and proves it with deterministic local checks",
        "Harness/workflow/script failures or concrete missing capabilities (including tools) observed in prior runs or reproducible locally, with deterministic local validation",
        "When the evidence points to a concrete missing capability, include a tool/capability improvement among the candidates.",
    ] {
        assert!(
            script.contains(required),
            "expected self-improve prompt to keep evidence-backed tooling candidate guidance {required:?}"
        );
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
