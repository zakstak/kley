use serde_json::json;

use kley::tools::{ToolRegistry, default_registry};

fn with_registry<R>(f: impl FnOnce(&ToolRegistry) -> R) -> R {
    let reg = default_registry(std::env::temp_dir());
    f(&reg)
}

#[test]
fn default_registry_has_builtins() {
    with_registry(|reg| {
        assert!(reg.get("shell").is_some());
        assert!(reg.get("read_file").is_some());
        assert!(reg.get("patch").is_some());
        assert!(reg.get("hashline_edit").is_some());
        assert!(reg.get("lsp_diagnostics").is_some());
        assert!(reg.get("lsp_symbols").is_some());
        assert!(reg.get("lsp_goto_definition").is_some());
        assert!(reg.get("lsp_find_references").is_some());
        assert!(reg.get("lsp_prepare_rename").is_some());
        assert!(reg.get("lsp_rename").is_some());
        assert!(reg.get("read_skill").is_some());
        assert!(reg.get("delegate_task").is_some());
        assert!(reg.get("report_status").is_some());
        assert!(reg.get("web_search").is_some());
    });
}

#[test]
fn default_registry_tool_schemas_match_strict_mode_requirements() {
    with_registry(|reg| {
        for tool in reg.to_api_tools() {
            assert_eq!(tool["strict"], true);

            let parameters = tool["parameters"].as_object().unwrap();
            assert_eq!(parameters.get("additionalProperties"), Some(&json!(false)),);

            let properties = parameters
                .get("properties")
                .and_then(|value| value.as_object())
                .unwrap();
            let required = parameters
                .get("required")
                .and_then(|value| value.as_array())
                .unwrap();

            for property_name in properties.keys() {
                assert!(
                    required.iter().any(|value| value == property_name),
                    "tool '{}' is missing '{}' from required",
                    tool["name"],
                    property_name,
                );
            }
        }
    });
}

#[test]
fn default_registry_to_api_tools_contains_web_search_schema() {
    with_registry(|reg| {
        let api_tools = reg.to_api_tools();
        let web_search_tool = api_tools
            .iter()
            .find(|tool| tool["name"] == "web_search")
            .expect("web_search should be serialized to API tools");

        assert_eq!(web_search_tool["strict"], true);

        let parameters = web_search_tool["parameters"].as_object().unwrap();
        assert_eq!(parameters["additionalProperties"], json!(false));

        let properties = parameters["properties"].as_object().unwrap();
        assert_eq!(properties["query"]["type"], json!("string"));
        assert_eq!(
            properties["max_results"]["type"],
            json!(["integer", "null"]),
        );

        let required = parameters["required"].as_array().unwrap();
        assert!(
            required.iter().any(|value| value == &json!("query")),
            "web_search schema must require 'query'",
        );
        assert!(
            required.iter().any(|value| value == &json!("max_results")),
            "web_search schema must require 'max_results'",
        );
    });
}
