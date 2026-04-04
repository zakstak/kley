use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::{fs, path::PathBuf};

use kley::lsp::{
    LspClient, LspClientError, LspClientFactory, LspManager, LspManagerError, LspService,
    TestingServerState, builtin_catalog, builtin_server_for_extension, builtin_server_for_path,
    resolve_workspace_root,
};
use kley::tools::Tool;
use kley::tools::lsp::{
    LspDiagnosticsTool, LspFindReferencesTool, LspGotoDefinitionTool, LspPrepareRenameTool,
    LspRenameTool, LspSymbolsTool,
};
use serde_json::json;

#[test]
fn lsp_builtin_catalog_matches_initial_languages() {
    let catalog = builtin_catalog();
    assert_eq!(catalog.len(), 6);

    let mappings: Vec<(&str, Vec<&str>, Vec<&str>)> = catalog
        .iter()
        .map(|entry| (entry.id, entry.command.to_vec(), entry.extensions.to_vec()))
        .collect();

    assert_eq!(
        mappings,
        vec![
            ("rust-analyzer", vec!["rust-analyzer"], vec!["rs"]),
            ("gopls", vec!["gopls"], vec!["go"]),
            (
                "bash-language-server",
                vec!["bash-language-server", "start"],
                vec!["sh", "bash", "zsh", "ksh"],
            ),
            ("nixd", vec!["nixd"], vec!["nix"]),
            (
                "yaml-language-server",
                vec!["yaml-language-server", "--stdio"],
                vec!["yaml", "yml"],
            ),
            (
                "pyright",
                vec!["pyright-langserver", "--stdio"],
                vec!["py", "pyi"],
            ),
        ]
    );

    assert_eq!(
        builtin_server_for_path(Path::new("src/lib.RS")).map(|s| s.id),
        Some("rust-analyzer")
    );
    assert_eq!(
        builtin_server_for_path(Path::new("script.BASH")).map(|s| s.id),
        Some("bash-language-server")
    );
    assert_eq!(
        builtin_server_for_path(Path::new("py.typed.PYI")).map(|s| s.id),
        Some("pyright")
    );
    assert_eq!(
        builtin_server_for_path(Path::new("workflow.YML")).map(|s| s.id),
        Some("yaml-language-server")
    );
}

#[test]
fn lsp_builtin_catalog_rejects_unsupported_extensions() {
    assert_eq!(builtin_server_for_extension("txt").map(|s| s.id), None);
    assert_eq!(builtin_server_for_extension(".md").map(|s| s.id), None);
    assert_eq!(
        builtin_server_for_path(Path::new("README")).map(|s| s.id),
        None
    );
    assert_eq!(
        builtin_server_for_path(Path::new("archive.tar.gz")).map(|s| s.id),
        None
    );
    assert_eq!(builtin_server_for_extension(" ").map(|s| s.id), None);
}

#[test]
fn lsp_root_resolution_matches_language_rules() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path();

    create_file(
        workspace,
        "rust-nearest/workspace/Cargo.toml",
        "[workspace]\nmembers = []\n",
    );
    create_file(
        workspace,
        "rust-nearest/workspace/crate/rust-project.json",
        "{}\n",
    );
    let rust_nearest_file = create_file(
        workspace,
        "rust-nearest/workspace/crate/src/lib.rs",
        "fn main() {}\n",
    );
    assert_eq!(
        resolve_workspace_root(&rust_nearest_file, "rust-analyzer"),
        workspace.join("rust-nearest/workspace/crate")
    );

    create_file(
        workspace,
        "rust-cargo/package/Cargo.toml",
        "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\n",
    );
    let rust_cargo_file = create_file(
        workspace,
        "rust-cargo/package/src/main.rs",
        "fn main() {}\n",
    );
    assert_eq!(
        resolve_workspace_root(&rust_cargo_file, "rust-analyzer"),
        workspace.join("rust-cargo/package")
    );

    create_file(workspace, "go-precedence/workspace/go.work", "go 1.22\n");
    create_file(
        workspace,
        "go-precedence/workspace/module/go.mod",
        "module example.com/mod\n",
    );
    let go_workspace_file = create_file(
        workspace,
        "go-precedence/workspace/module/pkg/main.go",
        "package main\n",
    );
    assert_eq!(
        resolve_workspace_root(&go_workspace_file, "gopls"),
        workspace.join("go-precedence/workspace")
    );

    create_file(
        workspace,
        "go-module/module/go.mod",
        "module example.com/mod\n",
    );
    let go_module_file = create_file(workspace, "go-module/module/pkg/main.go", "package main\n");
    assert_eq!(
        resolve_workspace_root(&go_module_file, "gopls"),
        workspace.join("go-module/module")
    );

    create_file(workspace, "nix-nearest/project/flake.nix", "{}\n");
    create_file(workspace, "nix-nearest/project/env/default.nix", "{}\n");
    let nix_nearest_file = create_file(workspace, "nix-nearest/project/env/src/app.nix", "{}\n");
    assert_eq!(
        resolve_workspace_root(&nix_nearest_file, "nixd"),
        workspace.join("nix-nearest/project/env")
    );

    create_file(workspace, "nix-flake/project/flake.nix", "{}\n");
    let nix_flake_file = create_file(workspace, "nix-flake/project/src/app.nix", "{}\n");
    assert_eq!(
        resolve_workspace_root(&nix_flake_file, "nixd"),
        workspace.join("nix-flake/project")
    );

    create_file(workspace, "nix-shell/project/shell.nix", "{}\n");
    let nix_shell_file = create_file(workspace, "nix-shell/project/src/app.nix", "{}\n");
    assert_eq!(
        resolve_workspace_root(&nix_shell_file, "nixd"),
        workspace.join("nix-shell/project")
    );

    create_file(
        workspace,
        "python-pyproject/project/pyproject.toml",
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    );
    let python_pyproject_file = create_file(
        workspace,
        "python-pyproject/project/src/app.py",
        "print('hi')\n",
    );
    assert_eq!(
        resolve_workspace_root(&python_pyproject_file, "pyright"),
        workspace.join("python-pyproject/project")
    );

    create_file(
        workspace,
        "python-nearest/project/pyproject.toml",
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    );
    create_file(
        workspace,
        "python-nearest/project/app/requirements.txt",
        "pytest\n",
    );
    let python_nearest_file = create_file(
        workspace,
        "python-nearest/project/app/src/app.py",
        "print('hi')\n",
    );
    assert_eq!(
        resolve_workspace_root(&python_nearest_file, "pyright"),
        workspace.join("python-nearest/project/app")
    );

    create_file(
        workspace,
        "python-pyrightconfig/project/pyrightconfig.json",
        "{}\n",
    );
    let python_pyrightconfig_file = create_file(
        workspace,
        "python-pyrightconfig/project/src/app.py",
        "print('hi')\n",
    );
    assert_eq!(
        resolve_workspace_root(&python_pyrightconfig_file, "pyright"),
        workspace.join("python-pyrightconfig/project")
    );

    create_file(
        workspace,
        "python-setup/project/setup.py",
        "from setuptools import setup\n",
    );
    let python_setup_file = create_file(
        workspace,
        "python-setup/project/src/app.py",
        "print('hi')\n",
    );
    assert_eq!(
        resolve_workspace_root(&python_setup_file, "pyright"),
        workspace.join("python-setup/project")
    );

    create_file(
        workspace,
        "python-requirements/project/requirements.txt",
        "pytest\n",
    );
    let python_requirements_file = create_file(
        workspace,
        "python-requirements/project/src/app.py",
        "print('hi')\n",
    );
    assert_eq!(
        resolve_workspace_root(&python_requirements_file, "pyright"),
        workspace.join("python-requirements/project")
    );

    create_dir(workspace, "python-venv/project/.venv");
    let python_venv_file =
        create_file(workspace, "python-venv/project/src/app.py", "print('hi')\n");
    assert_eq!(
        resolve_workspace_root(&python_venv_file, "pyright"),
        workspace.join("python-venv/project")
    );

    create_file(workspace, "nix-default/project/default.nix", "{}\n");
    let nix_default_file = create_file(workspace, "nix-default/project/src/app.nix", "{}\n");
    assert_eq!(
        resolve_workspace_root(&nix_default_file, "nixd"),
        workspace.join("nix-default/project")
    );
}

#[test]
fn lsp_root_resolution_falls_back_without_markers() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path();

    for (server_id, relative_file) in [
        ("rust-analyzer", "src/lib.rs"),
        ("gopls", "pkg/main.go"),
        ("bash-language-server", "scripts/run.sh"),
        ("yaml-language-server", "config/app.yaml"),
        ("nixd", "expr/default.nix.txt"),
        ("pyright", "app/main.py"),
    ] {
        let git_root = workspace.join(format!("git-{server_id}"));
        create_dir_at(&git_root.join(".git"));
        let git_file = create_file_at(&git_root.join(relative_file), "fixture\n");
        assert_eq!(resolve_workspace_root(&git_file, server_id), git_root);

        let plain_root = workspace.join(format!("plain-{server_id}"));
        let plain_file = create_file_at(&plain_root.join(relative_file), "fixture\n");
        assert_eq!(
            resolve_workspace_root(&plain_file, server_id),
            plain_file.parent().unwrap().to_path_buf()
        );
    }
}

#[test]
fn lsp_manager_starts_once_per_session_server() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let factory = Arc::new(FakeClientFactory::succeed(FakeClientMode::Result(json!({
        "ok": true
    }))));
    let manager = LspManager::with_test_factory(factory.clone());

    let first = manager
        .request(
            "session-a",
            "rust-analyzer",
            &root,
            "initialize",
            json!({ "capabilities": {} }),
        )
        .unwrap();
    assert_eq!(first, json!({ "ok": true }));

    let second = manager
        .request(
            "session-a",
            "rust-analyzer",
            &root,
            "textDocument/hover",
            json!({ "id": 1 }),
        )
        .unwrap();
    assert_eq!(second, json!({ "ok": true }));

    assert_eq!(factory.spawn_count(), 1);
    assert_eq!(
        factory.recorded_commands(),
        vec![vec!["rust-analyzer".to_string()]]
    );
    assert_eq!(
        manager.lifecycle_state("session-a", "rust-analyzer", &root),
        Some(TestingServerState::Ready)
    );

    let other_session = manager
        .request(
            "session-b",
            "rust-analyzer",
            &root,
            "initialize",
            json!({ "capabilities": {} }),
        )
        .unwrap();
    assert_eq!(other_session, json!({ "ok": true }));
    assert_eq!(factory.spawn_count(), 2);
}

#[test]
fn lsp_manager_marks_failed_servers_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();

    let startup_factory = Arc::new(FakeClientFactory::fail_startup("spawn boom"));
    let startup_manager = LspManager::with_test_factory(startup_factory.clone());

    let first_start =
        startup_manager.request("session-a", "rust-analyzer", &root, "initialize", json!({}));
    assert!(matches!(
        first_start,
        Err(LspManagerError::StartupFailed { .. })
    ));
    assert_eq!(startup_factory.spawn_count(), 1);

    let second_start =
        startup_manager.request("session-a", "rust-analyzer", &root, "initialize", json!({}));
    assert!(matches!(second_start, Err(LspManagerError::Failed { .. })));
    assert_eq!(startup_factory.spawn_count(), 1);
    assert_eq!(
        startup_manager.lifecycle_state("session-a", "rust-analyzer", &root),
        Some(TestingServerState::Failed)
    );

    let exit_factory = Arc::new(FakeClientFactory::succeed(FakeClientMode::Terminal(
        "unexpected exit",
    )));
    let exit_manager = LspManager::with_test_factory(exit_factory.clone());

    let first_exit = exit_manager.request(
        "session-a",
        "rust-analyzer",
        &root,
        "textDocument/hover",
        json!({}),
    );
    assert!(matches!(first_exit, Err(LspManagerError::Failed { .. })));
    assert_eq!(exit_factory.spawn_count(), 1);

    let second_exit = exit_manager.request(
        "session-a",
        "rust-analyzer",
        &root,
        "textDocument/hover",
        json!({}),
    );
    assert!(matches!(second_exit, Err(LspManagerError::Failed { .. })));
    assert_eq!(exit_factory.spawn_count(), 1);
    assert_eq!(
        exit_manager.lifecycle_state("session-a", "rust-analyzer", &root),
        Some(TestingServerState::Failed)
    );
}

#[test]
fn lsp_read_tools_match_opencode_contracts() {
    let temp = tempfile::tempdir().unwrap();
    let project_dir = temp.path().to_path_buf();
    create_file(
        &project_dir,
        "Cargo.toml",
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    );
    let file_path = create_file_at(&project_dir.join("src/lib.rs"), "fn example() {}\n");
    let file_uri = format!("file://{}", file_path.display());
    let service = Arc::new(FakeReadToolService::new(HashMap::from([
        (
            "textDocument/definition".to_string(),
            json!([
                {
                    "uri": file_uri,
                    "range": {
                        "start": { "line": 4, "character": 2 },
                        "end": { "line": 4, "character": 9 }
                    }
                }
            ]),
        ),
        (
            "textDocument/references".to_string(),
            json!([
                {
                    "uri": file_uri,
                    "range": {
                        "start": { "line": 1, "character": 0 },
                        "end": { "line": 1, "character": 7 }
                    }
                }
            ]),
        ),
        (
            "textDocument/documentSymbol".to_string(),
            json!([
                {
                    "name": "example",
                    "kind": 12,
                    "detail": "fn example()",
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 14 }
                    },
                    "selectionRange": {
                        "start": { "line": 0, "character": 3 },
                        "end": { "line": 0, "character": 10 }
                    }
                }
            ]),
        ),
        (
            "workspace/symbol".to_string(),
            json!([
                {
                    "name": "workspace_example",
                    "kind": 12,
                    "containerName": "demo",
                    "location": {
                        "uri": file_uri,
                        "range": {
                            "start": { "line": 2, "character": 1 },
                            "end": { "line": 2, "character": 9 }
                        }
                    }
                }
            ]),
        ),
        (
            "textDocument/diagnostic".to_string(),
            json!({
                "kind": "full",
                "items": [
                    {
                        "range": {
                            "start": { "line": 6, "character": 0 },
                            "end": { "line": 6, "character": 4 }
                        },
                        "severity": 2,
                        "message": "watch this",
                        "source": "rust-analyzer",
                        "code": "lint"
                    }
                ]
            }),
        ),
    ])));

    let goto_tool = LspGotoDefinitionTool::new(project_dir.clone(), "session-a", service.clone());
    let references_tool =
        LspFindReferencesTool::new(project_dir.clone(), "session-a", service.clone());
    let symbols_tool = LspSymbolsTool::new(project_dir.clone(), "session-a", service.clone());
    let diagnostics_tool = LspDiagnosticsTool::new(project_dir.clone(), "session-a", service);

    assert_eq!(
        goto_tool.parameters_schema(),
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute or relative path to the file."
                },
                "line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "1-based"
                },
                "character": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "0-based UTF-16 code unit offset"
                }
            },
            "required": ["file_path", "line", "character"],
            "additionalProperties": false,
        })
    );
    assert_eq!(
        references_tool.parameters_schema(),
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute or relative path to the file."
                },
                "line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "1-based"
                },
                "character": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "0-based UTF-16 code unit offset"
                },
                "include_declaration": {
                    "type": ["boolean", "null"],
                    "description": "Whether to include the declaration itself. Null defaults to false."
                }
            },
            "required": ["file_path", "line", "character", "include_declaration"],
            "additionalProperties": false,
        })
    );
    assert_eq!(
        symbols_tool.parameters_schema(),
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute or relative path to the file."
                },
                "scope": {
                    "type": "string",
                    "enum": ["document", "workspace"],
                    "description": "Use document for one file or workspace for the matching workspace root."
                },
                "query": {
                    "type": ["string", "null"],
                    "description": "Workspace symbol query string. Null defaults to an empty query."
                },
                "limit": {
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "description": "Maximum number of top-level symbols to return. Null defaults to 50."
                }
            },
            "required": ["file_path", "scope", "query", "limit"],
            "additionalProperties": false,
        })
    );
    assert_eq!(
        diagnostics_tool.parameters_schema(),
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute or relative path to the file. Directories require extension."
                },
                "severity": {
                    "type": "string",
                    "enum": ["error", "warning", "information", "hint", "all"],
                    "description": "Filter diagnostics by severity."
                },
                "extension": {
                    "type": ["string", "null"],
                    "description": "Required when file_path points to a directory. Use a supported extension like .rs or .py."
                }
            },
            "required": ["file_path", "severity", "extension"],
            "additionalProperties": false,
        })
    );

    let goto_output = goto_tool
        .execute(json!({
            "file_path": file_path,
            "line": 8,
            "character": 3,
        }))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&goto_output).unwrap(),
        json!([
            {
                "path": file_path,
                "range": {
                    "start": { "line": 5, "character": 2 },
                    "end": { "line": 5, "character": 9 }
                }
            }
        ])
    );

    let references_output = references_tool
        .execute(json!({
            "file_path": file_path,
            "line": 2,
            "character": 1,
            "include_declaration": true,
        }))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&references_output).unwrap(),
        json!([
            {
                "path": file_path,
                "range": {
                    "start": { "line": 2, "character": 0 },
                    "end": { "line": 2, "character": 7 }
                }
            }
        ])
    );

    let document_symbols_output = symbols_tool
        .execute(json!({
            "file_path": file_path,
            "scope": "document",
            "query": null,
            "limit": null,
        }))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&document_symbols_output).unwrap(),
        json!([
            {
                "name": "example",
                "kind": "function",
                "detail": "fn example()",
                "path": file_path,
                "range": {
                    "start": { "line": 1, "character": 0 },
                    "end": { "line": 1, "character": 14 }
                },
                "selection_range": {
                    "start": { "line": 1, "character": 3 },
                    "end": { "line": 1, "character": 10 }
                }
            }
        ])
    );

    let workspace_symbols_output = symbols_tool
        .execute(json!({
            "file_path": file_path,
            "scope": "workspace",
            "query": "example",
            "limit": 1,
        }))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&workspace_symbols_output).unwrap(),
        json!([
            {
                "name": "workspace_example",
                "kind": "function",
                "container_name": "demo",
                "path": file_path,
                "range": {
                    "start": { "line": 3, "character": 1 },
                    "end": { "line": 3, "character": 9 }
                }
            }
        ])
    );

    let diagnostics_output = diagnostics_tool
        .execute(json!({
            "file_path": file_path,
            "severity": "all",
            "extension": null,
        }))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&diagnostics_output).unwrap(),
        json!([
            {
                "path": file_path,
                "severity": "warning",
                "message": "watch this",
                "range": {
                    "start": { "line": 7, "character": 0 },
                    "end": { "line": 7, "character": 4 }
                },
                "source": "rust-analyzer",
                "code": "lint"
            }
        ])
    );
}

#[test]
fn lsp_read_tools_error_on_unsupported_filetype() {
    let temp = tempfile::tempdir().unwrap();
    let project_dir = temp.path().to_path_buf();
    let file_path = create_file_at(&project_dir.join("notes.txt"), "hello\n");
    let service = Arc::new(FakeReadToolService::new(HashMap::new()));

    let goto_tool = LspGotoDefinitionTool::new(project_dir.clone(), "session-a", service.clone());
    let references_tool =
        LspFindReferencesTool::new(project_dir.clone(), "session-a", service.clone());
    let symbols_tool = LspSymbolsTool::new(project_dir.clone(), "session-a", service.clone());
    let diagnostics_tool = LspDiagnosticsTool::new(project_dir, "session-a", service.clone());

    assert_eq!(
        goto_tool
            .execute(json!({
                "file_path": file_path,
                "line": 1,
                "character": 0,
            }))
            .unwrap(),
        "Error: No LSP server available for this file type."
    );
    assert_eq!(
        references_tool
            .execute(json!({
                "file_path": file_path,
                "line": 1,
                "character": 0,
                "include_declaration": null,
            }))
            .unwrap(),
        "Error: No LSP server available for this file type."
    );
    assert_eq!(
        symbols_tool
            .execute(json!({
                "file_path": file_path,
                "scope": "document",
                "query": null,
                "limit": null,
            }))
            .unwrap(),
        "Error: No LSP server available for this file type."
    );
    assert_eq!(
        diagnostics_tool
            .execute(json!({
                "file_path": file_path,
                "severity": "all",
                "extension": null,
            }))
            .unwrap(),
        "Error: No LSP server available for this file type."
    );
    assert!(service.recorded_methods().is_empty());
}

#[test]
fn lsp_rename_requires_prepare_success() {
    let temp = tempfile::tempdir().unwrap();
    let project_dir = temp.path().to_path_buf();
    let file_path = create_file_at(&project_dir.join("src/lib.rs"), "foo\n");
    let service = Arc::new(FakeRenameService::new(vec![
        FakeLspResponse::success(json!({
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 0, "character": 3 }
            },
            "placeholder": "foo"
        })),
        FakeLspResponse::success(json!({
            "changes": {
                format!("file://{}", file_path.display()): [
                    {
                        "range": {
                            "start": { "line": 0, "character": 0 },
                            "end": { "line": 0, "character": 3 }
                        },
                        "newText": "bar"
                    }
                ]
            }
        })),
    ]));
    let tool = LspRenameTool::new(project_dir.clone(), "session-a", service.clone());

    let output = tool
        .execute(json!({
            "filePath": file_path,
            "line": 1,
            "character": 0,
            "newName": "bar"
        }))
        .unwrap();

    assert_eq!(
        fs::read_to_string(project_dir.join("src/lib.rs")).unwrap(),
        "bar\n"
    );
    assert_eq!(
        output,
        format!(
            "Applied 1 edit(s) to 1 file(s):\n  - {}",
            project_dir.join("src/lib.rs").display()
        )
    );
    assert_eq!(
        service.recorded_methods(),
        vec!["textDocument/prepareRename", "textDocument/rename"]
    );
}

#[test]
fn lsp_rename_returns_precheck_failure_without_edit() {
    let temp = tempfile::tempdir().unwrap();
    let project_dir = temp.path().to_path_buf();
    let file_path = create_file_at(&project_dir.join("src/lib.rs"), "foo\n");
    let service = Arc::new(FakeRenameService::new(vec![FakeLspResponse::success(
        serde_json::Value::Null,
    )]));
    let tool = LspRenameTool::new(project_dir.clone(), "session-a", service.clone());

    let output = tool
        .execute(json!({
            "filePath": file_path,
            "line": 1,
            "character": 0,
            "newName": "bar"
        }))
        .unwrap();

    assert_eq!(
        fs::read_to_string(project_dir.join("src/lib.rs")).unwrap(),
        "foo\n"
    );
    assert_eq!(output, "Error: Cannot rename at this position");
    assert_eq!(
        service.recorded_methods(),
        vec!["textDocument/prepareRename"]
    );
}

#[test]
fn lsp_prepare_rename_formats_success_result() {
    let temp = tempfile::tempdir().unwrap();
    let project_dir = temp.path().to_path_buf();
    let file_path = create_file_at(&project_dir.join("src/lib.rs"), "foo\n");
    let service = Arc::new(FakeRenameService::new(vec![FakeLspResponse::success(
        json!({
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 0, "character": 3 }
            },
            "placeholder": "foo"
        }),
    )]));
    let tool = LspPrepareRenameTool::new(project_dir, "session-a", service);

    let output = tool
        .execute(json!({
            "filePath": file_path,
            "line": 1,
            "character": 0
        }))
        .unwrap();

    assert_eq!(output, "Rename available at 1:0-1:3 (current: \"foo\")");
}

#[derive(Clone)]
struct FakeClientFactory {
    mode: FakeFactoryMode,
    spawn_count: Arc<AtomicUsize>,
    commands: Arc<Mutex<Vec<Vec<String>>>>,
}

#[derive(Clone)]
enum FakeFactoryMode {
    Succeed(FakeClientMode),
    FailStartup(String),
}

#[derive(Clone)]
enum FakeClientMode {
    Result(serde_json::Value),
    Terminal(&'static str),
}

impl FakeClientFactory {
    fn succeed(mode: FakeClientMode) -> Self {
        Self {
            mode: FakeFactoryMode::Succeed(mode),
            spawn_count: Arc::new(AtomicUsize::new(0)),
            commands: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn fail_startup(message: &str) -> Self {
        Self {
            mode: FakeFactoryMode::FailStartup(message.to_string()),
            spawn_count: Arc::new(AtomicUsize::new(0)),
            commands: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn spawn_count(&self) -> usize {
        self.spawn_count.load(Ordering::SeqCst)
    }

    fn recorded_commands(&self) -> Vec<Vec<String>> {
        self.commands.lock().unwrap().clone()
    }
}

impl LspClientFactory for FakeClientFactory {
    fn create(
        &self,
        command: &[String],
        _workspace_root: &Path,
    ) -> Result<Arc<dyn LspClient>, String> {
        self.spawn_count.fetch_add(1, Ordering::SeqCst);
        self.commands.lock().unwrap().push(command.to_vec());

        match &self.mode {
            FakeFactoryMode::Succeed(mode) => Ok(Arc::new(FakeClient { mode: mode.clone() })),
            FakeFactoryMode::FailStartup(message) => Err(message.clone()),
        }
    }
}

struct FakeClient {
    mode: FakeClientMode,
}

impl LspClient for FakeClient {
    fn request(
        &self,
        method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, LspClientError> {
        match &self.mode {
            FakeClientMode::Result(result) => Ok(result.clone()),
            FakeClientMode::Terminal(message) => {
                if method == "initialize" {
                    Ok(serde_json::json!({ "capabilities": {} }))
                } else {
                    Err(LspClientError::Terminal((*message).to_string()))
                }
            }
        }
    }
}

#[derive(Clone)]
struct FakeRenameService {
    responses: Arc<Mutex<Vec<FakeLspResponse>>>,
    methods: Arc<Mutex<Vec<String>>>,
}

impl FakeRenameService {
    fn new(responses: Vec<FakeLspResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            methods: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn recorded_methods(&self) -> Vec<String> {
        self.methods.lock().unwrap().clone()
    }
}

impl LspService for FakeRenameService {
    fn request(
        &self,
        _session_id: &str,
        _server_id: &str,
        _workspace_root: &Path,
        method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, LspManagerError> {
        self.methods.lock().unwrap().push(method.to_string());
        let response = self.responses.lock().unwrap().remove(0);
        match response {
            FakeLspResponse::Success(value) => Ok(value),
        }
    }
}

enum FakeLspResponse {
    Success(serde_json::Value),
}

impl FakeLspResponse {
    fn success(value: serde_json::Value) -> Self {
        Self::Success(value)
    }
}

#[derive(Clone)]
struct FakeReadToolService {
    responses: Arc<HashMap<String, serde_json::Value>>,
    methods: Arc<Mutex<Vec<String>>>,
}

impl FakeReadToolService {
    fn new(responses: HashMap<String, serde_json::Value>) -> Self {
        Self {
            responses: Arc::new(responses),
            methods: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn recorded_methods(&self) -> Vec<String> {
        self.methods.lock().unwrap().clone()
    }
}

impl LspService for FakeReadToolService {
    fn request(
        &self,
        _session_id: &str,
        _server_id: &str,
        _workspace_root: &Path,
        method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, LspManagerError> {
        self.methods.lock().unwrap().push(method.to_string());
        Ok(self
            .responses
            .get(method)
            .cloned()
            .unwrap_or_else(|| json!([])))
    }
}

fn create_file(root: &Path, relative_path: &str, contents: &str) -> PathBuf {
    create_file_at(&root.join(relative_path), contents)
}

fn create_file_at(path: &Path, contents: &str) -> PathBuf {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
    path.to_path_buf()
}

fn create_dir(root: &Path, relative_path: &str) -> PathBuf {
    let path = root.join(relative_path);
    create_dir_at(&path);
    path
}

fn create_dir_at(path: &Path) {
    fs::create_dir_all(path).unwrap();
}
