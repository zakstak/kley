use std::path::Path;

mod root;
mod service;

pub use root::resolve_workspace_root;
#[doc(hidden)]
pub use service::{LspClient, LspClientError, LspClientFactory, TestingServerState};
#[doc(hidden)]
pub use service::{LspManager, LspManagerError, LspService};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinLspServer {
    pub id: &'static str,
    pub command: &'static [&'static str],
    pub extensions: &'static [&'static str],
}

const BUILTIN_LSP_CATALOG: [BuiltinLspServer; 6] = [
    BuiltinLspServer {
        id: "rust-analyzer",
        command: &["rust-analyzer"],
        extensions: &["rs"],
    },
    BuiltinLspServer {
        id: "gopls",
        command: &["gopls"],
        extensions: &["go"],
    },
    BuiltinLspServer {
        id: "bash-language-server",
        command: &["bash-language-server", "start"],
        extensions: &["sh", "bash", "zsh", "ksh"],
    },
    BuiltinLspServer {
        id: "nixd",
        command: &["nixd"],
        extensions: &["nix"],
    },
    BuiltinLspServer {
        id: "yaml-language-server",
        command: &["yaml-language-server", "--stdio"],
        extensions: &["yaml", "yml"],
    },
    BuiltinLspServer {
        id: "pyright",
        command: &["pyright-langserver", "--stdio"],
        extensions: &["py", "pyi"],
    },
];

pub fn builtin_catalog() -> &'static [BuiltinLspServer] {
    &BUILTIN_LSP_CATALOG
}

pub fn builtin_server_for_extension(extension: &str) -> Option<&'static BuiltinLspServer> {
    let normalized = normalize_extension(extension)?;
    BUILTIN_LSP_CATALOG
        .iter()
        .find(|server| server.extensions.contains(&normalized.as_str()))
}

pub fn builtin_server_for_path(path: &Path) -> Option<&'static BuiltinLspServer> {
    let extension = path.extension()?.to_str()?;
    builtin_server_for_extension(extension)
}

fn normalize_extension(extension: &str) -> Option<String> {
    let trimmed = extension.trim().trim_start_matches('.');
    if trimmed.is_empty() {
        return None;
    }

    Some(trimmed.to_lowercase())
}
