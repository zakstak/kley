use std::path::{Path, PathBuf};

type MarkerGroup = &'static [&'static str];

const RUST_MARKER_GROUPS: &[MarkerGroup] = &[&["Cargo.toml", "rust-project.json"]];
const GO_MARKER_GROUPS: &[MarkerGroup] = &[&["go.work"], &["go.mod"]];
const NIX_MARKER_GROUPS: &[MarkerGroup] = &[&["flake.nix", "shell.nix", "default.nix"]];
const PYTHON_MARKER_GROUPS: &[MarkerGroup] = &[&[
    "pyproject.toml",
    "pyrightconfig.json",
    "setup.py",
    "requirements.txt",
    ".venv",
]];

pub fn resolve_workspace_root(file_path: &Path, server_id: &str) -> PathBuf {
    let file_parent = parent_dir(file_path);

    find_root_by_markers(&file_parent, marker_groups_for_server(server_id))
        .or_else(|| find_git_root(&file_parent))
        .unwrap_or(file_parent)
}

fn marker_groups_for_server(server_id: &str) -> &'static [MarkerGroup] {
    match server_id {
        "rust-analyzer" => RUST_MARKER_GROUPS,
        "gopls" => GO_MARKER_GROUPS,
        "nixd" => NIX_MARKER_GROUPS,
        "pyright" => PYTHON_MARKER_GROUPS,
        "bash-language-server" | "yaml-language-server" => &[],
        _ => &[],
    }
}

fn find_root_by_markers(
    start_dir: &Path,
    marker_groups: &'static [MarkerGroup],
) -> Option<PathBuf> {
    marker_groups
        .iter()
        .find_map(|markers| find_nearest_marker_root(start_dir, markers))
}

fn find_nearest_marker_root(start_dir: &Path, markers: MarkerGroup) -> Option<PathBuf> {
    start_dir.ancestors().find_map(|candidate| {
        markers
            .iter()
            .any(|marker| candidate.join(marker).exists())
            .then(|| candidate.to_path_buf())
    })
}

fn find_git_root(start_dir: &Path) -> Option<PathBuf> {
    start_dir.ancestors().find_map(|candidate| {
        candidate
            .join(".git")
            .exists()
            .then(|| candidate.to_path_buf())
    })
}

fn parent_dir(path: &Path) -> PathBuf {
    path.parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}
