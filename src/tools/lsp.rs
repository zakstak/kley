use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use urlencoding::{decode, encode};

use super::Tool;
use crate::lsp::{
    LspService, builtin_server_for_extension, builtin_server_for_path, resolve_workspace_root,
};

pub struct LspDiagnosticsTool {
    project_dir: PathBuf,
    session_id: String,
    service: Arc<dyn LspService>,
}

impl LspDiagnosticsTool {
    pub fn new(
        project_dir: PathBuf,
        session_id: impl Into<String>,
        service: Arc<dyn LspService>,
    ) -> Self {
        Self {
            project_dir,
            session_id: session_id.into(),
            service,
        }
    }
}

impl Tool for LspDiagnosticsTool {
    fn name(&self) -> &str {
        "lsp_diagnostics"
    }

    fn description(&self) -> &str {
        "Read diagnostics for a supported file or directory through the built-in LSP service. Returns stable diagnostic summaries instead of raw protocol blobs."
    }

    fn parameters_schema(&self) -> Value {
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
    }

    fn execute(&self, args: Value) -> Result<String> {
        let args: DiagnosticsArgs = match serde_json::from_value(args) {
            Ok(args) => args,
            Err(error) => return Ok(format!("Error: invalid arguments: {error}")),
        };
        if args.file_path.trim().is_empty() {
            return Ok("Error: file_path is required".to_string());
        }

        Ok(
            match diagnostics_context(
                &self.project_dir,
                &self.session_id,
                args.file_path.clone(),
                args.extension.clone(),
            ) {
                Ok(context) => match request_diagnostics(self.service.as_ref(), &context) {
                    Ok(result) => {
                        format_diagnostics_result(&context.file_path, args.severity, &result)?
                    }
                    Err(error) => format!("Error: {error}"),
                },
                Err(error) => format!("Error: {error}"),
            },
        )
    }

    fn bind_session_context(&mut self, session_id: &str) {
        self.session_id = session_id.to_string();
    }
}

pub struct LspSymbolsTool {
    project_dir: PathBuf,
    session_id: String,
    service: Arc<dyn LspService>,
}

impl LspSymbolsTool {
    pub fn new(
        project_dir: PathBuf,
        session_id: impl Into<String>,
        service: Arc<dyn LspService>,
    ) -> Self {
        Self {
            project_dir,
            session_id: session_id.into(),
            service,
        }
    }
}

impl Tool for LspSymbolsTool {
    fn name(&self) -> &str {
        "lsp_symbols"
    }

    fn description(&self) -> &str {
        "Read document or workspace symbols for a supported file through the built-in LSP service. Returns stable symbol summaries instead of raw protocol unions."
    }

    fn parameters_schema(&self) -> Value {
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
    }

    fn execute(&self, args: Value) -> Result<String> {
        let args: SymbolsArgs = match serde_json::from_value(args) {
            Ok(args) => args,
            Err(error) => return Ok(format!("Error: invalid arguments: {error}")),
        };
        if args.file_path.trim().is_empty() {
            return Ok("Error: file_path is required".to_string());
        }
        if matches!(args.limit, Some(0)) {
            return Ok("Error: limit must be >= 1".to_string());
        }

        Ok(
            match read_context(&self.project_dir, &self.session_id, args.file_path.clone()) {
                Ok(context) => match request_symbols(self.service.as_ref(), &context, &args) {
                    Ok(result) => format_symbols_result(
                        &context.file_path,
                        args.limit.unwrap_or(50),
                        &result,
                    )?,
                    Err(error) => format!("Error: {error}"),
                },
                Err(error) => format!("Error: {error}"),
            },
        )
    }

    fn bind_session_context(&mut self, session_id: &str) {
        self.session_id = session_id.to_string();
    }
}

pub struct LspGotoDefinitionTool {
    project_dir: PathBuf,
    session_id: String,
    service: Arc<dyn LspService>,
}

impl LspGotoDefinitionTool {
    pub fn new(
        project_dir: PathBuf,
        session_id: impl Into<String>,
        service: Arc<dyn LspService>,
    ) -> Self {
        Self {
            project_dir,
            session_id: session_id.into(),
            service,
        }
    }
}

impl Tool for LspGotoDefinitionTool {
    fn name(&self) -> &str {
        "lsp_goto_definition"
    }

    fn description(&self) -> &str {
        "Look up definitions for a symbol through the built-in LSP service. line is 1-based and character is a 0-based UTF-16 code unit offset."
    }

    fn parameters_schema(&self) -> Value {
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
    }

    fn execute(&self, args: Value) -> Result<String> {
        let args: PositionArgs = match serde_json::from_value(args) {
            Ok(args) => args,
            Err(error) => return Ok(format!("Error: invalid arguments: {error}")),
        };

        Ok(
            match read_context(&self.project_dir, &self.session_id, args.file_path.clone()) {
                Ok(context) => match request_definition(self.service.as_ref(), &context, &args) {
                    Ok(result) => format_location_result("lsp_goto_definition", &result)?,
                    Err(error) => format!("Error: {error}"),
                },
                Err(error) => format!("Error: {error}"),
            },
        )
    }

    fn bind_session_context(&mut self, session_id: &str) {
        self.session_id = session_id.to_string();
    }
}

pub struct LspFindReferencesTool {
    project_dir: PathBuf,
    session_id: String,
    service: Arc<dyn LspService>,
}

impl LspFindReferencesTool {
    pub fn new(
        project_dir: PathBuf,
        session_id: impl Into<String>,
        service: Arc<dyn LspService>,
    ) -> Self {
        Self {
            project_dir,
            session_id: session_id.into(),
            service,
        }
    }
}

impl Tool for LspFindReferencesTool {
    fn name(&self) -> &str {
        "lsp_find_references"
    }

    fn description(&self) -> &str {
        "Find references for a symbol through the built-in LSP service. line is 1-based and character is a 0-based UTF-16 code unit offset."
    }

    fn parameters_schema(&self) -> Value {
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
    }

    fn execute(&self, args: Value) -> Result<String> {
        let args: FindReferencesArgs = match serde_json::from_value(args) {
            Ok(args) => args,
            Err(error) => return Ok(format!("Error: invalid arguments: {error}")),
        };

        Ok(
            match read_context(&self.project_dir, &self.session_id, args.file_path.clone()) {
                Ok(context) => match request_references(self.service.as_ref(), &context, &args) {
                    Ok(result) => format_location_result("lsp_find_references", &result)?,
                    Err(error) => format!("Error: {error}"),
                },
                Err(error) => format!("Error: {error}"),
            },
        )
    }

    fn bind_session_context(&mut self, session_id: &str) {
        self.session_id = session_id.to_string();
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiagnosticsArgs {
    file_path: String,
    severity: DiagnosticSeverityFilter,
    #[serde(default)]
    extension: Option<String>,
}

#[derive(Debug, Copy, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DiagnosticSeverityFilter {
    Error,
    Warning,
    Information,
    Hint,
    All,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SymbolsArgs {
    file_path: String,
    scope: SymbolsScope,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Copy, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SymbolsScope {
    Document,
    Workspace,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PositionArgs {
    file_path: String,
    line: u32,
    character: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FindReferencesArgs {
    file_path: String,
    line: u32,
    character: u32,
    #[serde(default)]
    include_declaration: Option<bool>,
}

struct ReadContext {
    file_path: PathBuf,
    session_id: String,
    server_id: &'static str,
    workspace_root: PathBuf,
    uri: String,
}

struct DiagnosticsContext {
    file_path: PathBuf,
    session_id: String,
    server_id: &'static str,
    workspace_root: PathBuf,
    uri: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct OutputPosition {
    line: usize,
    character: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct OutputRange {
    start: OutputPosition,
    end: OutputPosition,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct LocationResult {
    path: String,
    range: OutputRange,
    #[serde(skip_serializing_if = "Option::is_none")]
    selection_range: Option<OutputRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin_selection_range: Option<OutputRange>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SymbolResult {
    name: String,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    container_name: Option<String>,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    range: Option<OutputRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selection_range: Option<OutputRange>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deprecated: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    children: Vec<SymbolResult>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DiagnosticResult {
    path: String,
    severity: String,
    message: String,
    range: OutputRange,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    tags: Vec<String>,
}

fn read_context(
    project_dir: &Path,
    session_id: &str,
    file_path: String,
) -> Result<ReadContext, LspToolError> {
    let file_path = resolve_file_path(project_dir, &file_path);
    if !file_path.is_file() {
        return Err(LspToolError::FileNotFound(file_path));
    }

    let Some(server) = builtin_server_for_path(&file_path) else {
        return Err(LspToolError::UnsupportedFileType);
    };

    let workspace_root = resolve_workspace_root(&file_path, server.id);
    let uri = path_to_file_uri(&file_path).map_err(LspToolError::InvalidPath)?;
    Ok(ReadContext {
        file_path,
        session_id: session_id.to_string(),
        server_id: server.id,
        workspace_root,
        uri,
    })
}

fn diagnostics_context(
    project_dir: &Path,
    session_id: &str,
    file_path: String,
    extension: Option<String>,
) -> Result<DiagnosticsContext, LspToolError> {
    let file_path = resolve_file_path(project_dir, &file_path);

    if file_path.is_file() {
        let Some(server) = builtin_server_for_path(&file_path) else {
            return Err(LspToolError::UnsupportedFileType);
        };

        let workspace_root = resolve_workspace_root(&file_path, server.id);
        let uri = path_to_file_uri(&file_path).map_err(LspToolError::InvalidPath)?;
        return Ok(DiagnosticsContext {
            file_path,
            session_id: session_id.to_string(),
            server_id: server.id,
            workspace_root,
            uri: Some(uri),
        });
    }

    if file_path.is_dir() {
        let extension = extension.ok_or(LspToolError::DirectoryExtensionRequired)?;
        let Some(server) = builtin_server_for_extension(&extension) else {
            return Err(LspToolError::UnsupportedFileType);
        };

        return Ok(DiagnosticsContext {
            file_path: file_path.clone(),
            session_id: session_id.to_string(),
            server_id: server.id,
            workspace_root: file_path,
            uri: None,
        });
    }

    Err(LspToolError::FileNotFound(file_path))
}

fn request_definition(
    service: &dyn LspService,
    context: &ReadContext,
    args: &PositionArgs,
) -> Result<Value, LspToolError> {
    request_position_method(
        service,
        context,
        "textDocument/definition",
        args.line,
        args.character,
    )
}

fn request_references(
    service: &dyn LspService,
    context: &ReadContext,
    args: &FindReferencesArgs,
) -> Result<Value, LspToolError> {
    if args.line == 0 {
        return Err(LspToolError::InvalidRequest(
            "line must be >= 1".to_string(),
        ));
    }

    service
        .request(
            &context.session_id,
            context.server_id,
            &context.workspace_root,
            "textDocument/references",
            json!({
                "textDocument": { "uri": context.uri },
                "position": {
                    "line": args.line.saturating_sub(1),
                    "character": args.character,
                },
                "context": {
                    "includeDeclaration": args.include_declaration.unwrap_or(false),
                }
            }),
        )
        .map_err(LspToolError::Server)
}

fn request_position_method(
    service: &dyn LspService,
    context: &ReadContext,
    method: &str,
    line: u32,
    character: u32,
) -> Result<Value, LspToolError> {
    if line == 0 {
        return Err(LspToolError::InvalidRequest(
            "line must be >= 1".to_string(),
        ));
    }

    service
        .request(
            &context.session_id,
            context.server_id,
            &context.workspace_root,
            method,
            json!({
                "textDocument": { "uri": context.uri },
                "position": {
                    "line": line.saturating_sub(1),
                    "character": character,
                }
            }),
        )
        .map_err(LspToolError::Server)
}

fn request_symbols(
    service: &dyn LspService,
    context: &ReadContext,
    args: &SymbolsArgs,
) -> Result<Value, LspToolError> {
    match args.scope {
        SymbolsScope::Document => service
            .request(
                &context.session_id,
                context.server_id,
                &context.workspace_root,
                "textDocument/documentSymbol",
                json!({
                    "textDocument": { "uri": context.uri }
                }),
            )
            .map_err(LspToolError::Server),
        SymbolsScope::Workspace => service
            .request(
                &context.session_id,
                context.server_id,
                &context.workspace_root,
                "workspace/symbol",
                json!({ "query": args.query.clone().unwrap_or_default() }),
            )
            .map_err(LspToolError::Server),
    }
}

fn request_diagnostics(
    service: &dyn LspService,
    context: &DiagnosticsContext,
) -> Result<Value, LspToolError> {
    let (method, params) = match &context.uri {
        Some(uri) => (
            "textDocument/diagnostic",
            json!({
                "textDocument": { "uri": uri }
            }),
        ),
        None => (
            "workspace/diagnostic",
            json!({
                "identifier": context.server_id,
                "previousResultIds": []
            }),
        ),
    };

    service
        .request(
            &context.session_id,
            context.server_id,
            &context.workspace_root,
            method,
            params,
        )
        .map_err(LspToolError::Server)
}

fn format_location_result(tool_name: &str, result: &Value) -> Result<String> {
    let locations = normalize_location_entries(result);
    if locations.is_empty() {
        return Ok(format!("No results found for {tool_name}"));
    }

    serde_json::to_string_pretty(&locations).map_err(Into::into)
}

fn format_symbols_result(file_path: &Path, limit: usize, result: &Value) -> Result<String> {
    let mut symbols = result
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| normalize_symbol_entry(file_path, entry))
        .collect::<Vec<_>>();
    symbols.truncate(limit);

    if symbols.is_empty() {
        return Ok("No results found for lsp_symbols".to_string());
    }

    serde_json::to_string_pretty(&symbols).map_err(Into::into)
}

fn format_diagnostics_result(
    file_path: &Path,
    filter: DiagnosticSeverityFilter,
    result: &Value,
) -> Result<String> {
    let diagnostics = normalize_diagnostic_entries(file_path, filter, result);
    if diagnostics.is_empty() {
        return Ok("No diagnostics found".to_string());
    }

    serde_json::to_string_pretty(&diagnostics).map_err(Into::into)
}

fn normalize_location_entries(result: &Value) -> Vec<LocationResult> {
    match result {
        Value::Array(entries) => entries
            .iter()
            .filter_map(normalize_location_entry)
            .collect(),
        Value::Object(_) => normalize_location_entry(result).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn normalize_location_entry(entry: &Value) -> Option<LocationResult> {
    if let Some(target_uri) = entry.get("targetUri").and_then(Value::as_str) {
        let target_range = parse_range_value(entry.get("targetRange")?)?;
        return Some(LocationResult {
            path: uri_to_path(target_uri).ok()?.display().to_string(),
            range: preview_range_to_output(&target_range),
            selection_range: entry
                .get("targetSelectionRange")
                .and_then(parse_range_value)
                .map(|range| preview_range_to_output(&range)),
            origin_selection_range: entry
                .get("originSelectionRange")
                .and_then(parse_range_value)
                .map(|range| preview_range_to_output(&range)),
        });
    }

    let uri = entry.get("uri")?.as_str()?;
    let range = parse_range_value(entry.get("range")?)?;
    Some(LocationResult {
        path: uri_to_path(uri).ok()?.display().to_string(),
        range: preview_range_to_output(&range),
        selection_range: None,
        origin_selection_range: None,
    })
}

fn normalize_symbol_entry(file_path: &Path, entry: &Value) -> Option<SymbolResult> {
    let name = entry.get("name")?.as_str()?.to_string();
    let kind = symbol_kind_name(entry.get("kind")?.as_u64()?).to_string();
    let children = entry
        .get("children")
        .and_then(Value::as_array)
        .map(|children| {
            children
                .iter()
                .filter_map(|child| normalize_symbol_entry(file_path, child))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let path = entry
        .get("location")
        .and_then(|location| location.get("uri"))
        .and_then(Value::as_str)
        .and_then(|uri| uri_to_path(uri).ok())
        .unwrap_or_else(|| file_path.to_path_buf())
        .display()
        .to_string();
    let range = entry
        .get("range")
        .or_else(|| {
            entry
                .get("location")
                .and_then(|location| location.get("range"))
        })
        .and_then(parse_range_value)
        .map(|range| preview_range_to_output(&range));
    let selection_range = entry
        .get("selectionRange")
        .and_then(parse_range_value)
        .map(|range| preview_range_to_output(&range));

    Some(SymbolResult {
        name,
        kind,
        detail: entry
            .get("detail")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        container_name: entry
            .get("containerName")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        path,
        range,
        selection_range,
        tags: entry
            .get("tags")
            .and_then(Value::as_array)
            .map(|tags| {
                tags.iter()
                    .filter_map(|tag| tag.as_u64())
                    .filter_map(symbol_tag_name)
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        deprecated: entry.get("deprecated").and_then(Value::as_bool),
        children,
    })
}

fn normalize_diagnostic_entries(
    file_path: &Path,
    filter: DiagnosticSeverityFilter,
    result: &Value,
) -> Vec<DiagnosticResult> {
    match result {
        Value::Array(entries) => entries
            .iter()
            .filter_map(|entry| normalize_diagnostic_entry(file_path, filter, entry))
            .collect(),
        Value::Object(object) => {
            let Some(items) = object.get("items").and_then(Value::as_array) else {
                return Vec::new();
            };

            if items.iter().all(|item| item.get("uri").is_some()) {
                items
                    .iter()
                    .flat_map(|report| {
                        let path = report
                            .get("uri")
                            .and_then(Value::as_str)
                            .and_then(|uri| uri_to_path(uri).ok())
                            .unwrap_or_else(|| file_path.to_path_buf());
                        report
                            .get("items")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(move |entry| {
                                normalize_diagnostic_entry(&path, filter, entry)
                            })
                    })
                    .collect()
            } else {
                items
                    .iter()
                    .filter_map(|entry| normalize_diagnostic_entry(file_path, filter, entry))
                    .collect()
            }
        }
        _ => Vec::new(),
    }
}

fn normalize_diagnostic_entry(
    file_path: &Path,
    filter: DiagnosticSeverityFilter,
    entry: &Value,
) -> Option<DiagnosticResult> {
    let severity = diagnostic_severity_name(entry.get("severity")?.as_u64()?)?;
    if !severity_matches_filter(filter, severity) {
        return None;
    }

    Some(DiagnosticResult {
        path: file_path.display().to_string(),
        severity: severity.to_string(),
        message: entry.get("message")?.as_str()?.to_string(),
        range: preview_range_to_output(&parse_range_value(entry.get("range")?)?),
        source: entry
            .get("source")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        code: entry.get("code").and_then(value_to_scalar_string),
        tags: entry
            .get("tags")
            .and_then(Value::as_array)
            .map(|tags| {
                tags.iter()
                    .filter_map(|tag| tag.as_u64())
                    .filter_map(diagnostic_tag_name)
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    })
}

fn preview_range_to_output(range: &RangePreview) -> OutputRange {
    OutputRange {
        start: OutputPosition {
            line: range.start.line + 1,
            character: range.start.character,
        },
        end: OutputPosition {
            line: range.end.line + 1,
            character: range.end.character,
        },
    }
}

fn severity_matches_filter(filter: DiagnosticSeverityFilter, severity: &str) -> bool {
    match filter {
        DiagnosticSeverityFilter::All => true,
        DiagnosticSeverityFilter::Error => severity == "error",
        DiagnosticSeverityFilter::Warning => severity == "warning",
        DiagnosticSeverityFilter::Information => severity == "information",
        DiagnosticSeverityFilter::Hint => severity == "hint",
    }
}

fn diagnostic_severity_name(value: u64) -> Option<&'static str> {
    match value {
        1 => Some("error"),
        2 => Some("warning"),
        3 => Some("information"),
        4 => Some("hint"),
        _ => None,
    }
}

fn diagnostic_tag_name(value: u64) -> Option<&'static str> {
    match value {
        1 => Some("unnecessary"),
        2 => Some("deprecated"),
        _ => None,
    }
}

fn symbol_tag_name(value: u64) -> Option<&'static str> {
    match value {
        1 => Some("deprecated"),
        _ => None,
    }
}

fn symbol_kind_name(value: u64) -> &'static str {
    match value {
        1 => "file",
        2 => "module",
        3 => "namespace",
        4 => "package",
        5 => "class",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "constructor",
        10 => "enum",
        11 => "interface",
        12 => "function",
        13 => "variable",
        14 => "constant",
        15 => "string",
        16 => "number",
        17 => "boolean",
        18 => "array",
        19 => "object",
        20 => "key",
        21 => "null",
        22 => "enum_member",
        23 => "struct",
        24 => "event",
        25 => "operator",
        26 => "type_parameter",
        _ => "unknown",
    }
}

fn value_to_scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(boolean) => Some(boolean.to_string()),
        _ => Some(value.to_string()),
    }
}

pub struct LspPrepareRenameTool {
    project_dir: PathBuf,
    session_id: String,
    service: Arc<dyn LspService>,
}

impl LspPrepareRenameTool {
    pub fn new(
        project_dir: PathBuf,
        session_id: impl Into<String>,
        service: Arc<dyn LspService>,
    ) -> Self {
        Self {
            project_dir,
            session_id: session_id.into(),
            service,
        }
    }
}

impl Tool for LspPrepareRenameTool {
    fn name(&self) -> &str {
        "lsp_prepare_rename"
    }

    fn description(&self) -> &str {
        "Check if rename is valid. Use BEFORE lsp_rename."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "filePath": {
                    "type": "string",
                    "description": "The absolute or relative path to the file"
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
            "required": ["filePath", "line", "character"],
            "additionalProperties": false,
        })
    }

    fn execute(&self, args: Value) -> Result<String> {
        let args: PrepareRenameArgs = match serde_json::from_value(args) {
            Ok(args) => args,
            Err(error) => return Ok(format!("Error: invalid arguments: {error}")),
        };

        Ok(
            match rename_context(&self.project_dir, &self.session_id, args.file_path.clone()) {
                Ok(context) => {
                    match request_prepare_rename(
                        self.service.as_ref(),
                        &context,
                        args.line,
                        args.character,
                    ) {
                        Ok(result) => format_prepare_rename_result(&result),
                        Err(error) => format!("Error: {error}"),
                    }
                }
                Err(error) => format!("Error: {error}"),
            },
        )
    }

    fn bind_session_context(&mut self, session_id: &str) {
        self.session_id = session_id.to_string();
    }
}

pub struct LspRenameTool {
    project_dir: PathBuf,
    session_id: String,
    service: Arc<dyn LspService>,
}

impl LspRenameTool {
    pub fn new(
        project_dir: PathBuf,
        session_id: impl Into<String>,
        service: Arc<dyn LspService>,
    ) -> Self {
        Self {
            project_dir,
            session_id: session_id.into(),
            service,
        }
    }
}

impl Tool for LspRenameTool {
    fn name(&self) -> &str {
        "lsp_rename"
    }

    fn description(&self) -> &str {
        "Rename symbol across entire workspace. APPLIES changes to all files."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "filePath": {
                    "type": "string",
                    "description": "The absolute or relative path to the file"
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
                "newName": {
                    "type": "string",
                    "description": "New symbol name"
                }
            },
            "required": ["filePath", "line", "character", "newName"],
            "additionalProperties": false,
        })
    }

    fn execute(&self, args: Value) -> Result<String> {
        let args: RenameArgs = match serde_json::from_value(args) {
            Ok(args) => args,
            Err(error) => return Ok(format!("Error: invalid arguments: {error}")),
        };

        Ok(
            match rename_context(&self.project_dir, &self.session_id, args.file_path.clone()) {
                Ok(context) => {
                    match request_prepare_rename(
                        self.service.as_ref(),
                        &context,
                        args.line,
                        args.character,
                    ) {
                        Ok(result) => {
                            if !prepare_rename_is_valid(&result) {
                                return Ok("Error: Cannot rename at this position".to_string());
                            }

                            match request_rename(self.service.as_ref(), &context, &args) {
                                Ok(edit) => format_apply_result(apply_workspace_edit(edit)),
                                Err(error) => format!("Error: {error}"),
                            }
                        }
                        Err(error) => format!("Error: {error}"),
                    }
                }
                Err(error) => format!("Error: {error}"),
            },
        )
    }

    fn bind_session_context(&mut self, session_id: &str) {
        self.session_id = session_id.to_string();
    }
}

#[derive(Debug, Deserialize)]
struct PrepareRenameArgs {
    #[serde(rename = "filePath")]
    file_path: String,
    line: u32,
    character: u32,
}

#[derive(Debug, Deserialize)]
struct RenameArgs {
    #[serde(rename = "filePath")]
    file_path: String,
    line: u32,
    character: u32,
    #[serde(rename = "newName")]
    new_name: String,
}

struct RenameContext {
    file_path: PathBuf,
    session_id: String,
    server_id: &'static str,
    workspace_root: PathBuf,
}

#[derive(Debug, Deserialize)]
struct WorkspaceEdit {
    #[serde(default)]
    changes: BTreeMap<String, Vec<TextEdit>>,
    #[serde(default, rename = "documentChanges")]
    document_changes: Vec<DocumentChange>,
}

#[derive(Debug, Deserialize)]
struct TextEdit {
    range: Range,
    #[serde(rename = "newText")]
    new_text: String,
}

#[derive(Debug, Deserialize)]
struct Range {
    start: Position,
    end: Position,
}

#[derive(Debug, Deserialize)]
struct Position {
    line: usize,
    character: usize,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DocumentChange {
    TextDocumentEdit(TextDocumentEdit),
    ResourceOperation(ResourceOperation),
}

#[derive(Debug, Deserialize)]
struct TextDocumentEdit {
    #[serde(rename = "textDocument")]
    text_document: VersionedTextDocumentIdentifier,
    edits: Vec<TextEdit>,
}

#[derive(Debug, Deserialize)]
struct VersionedTextDocumentIdentifier {
    uri: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
enum ResourceOperation {
    #[serde(rename = "create")]
    Create { uri: String },
    #[serde(rename = "rename")]
    Rename {
        #[serde(rename = "oldUri")]
        old_uri: String,
        #[serde(rename = "newUri")]
        new_uri: String,
    },
    #[serde(rename = "delete")]
    Delete { uri: String },
}

struct ApplyResult {
    success: bool,
    files_modified: Vec<String>,
    total_edits: usize,
    errors: Vec<String>,
}

fn rename_context(
    project_dir: &Path,
    session_id: &str,
    file_path: String,
) -> Result<RenameContext, LspToolError> {
    let file_path = resolve_file_path(project_dir, &file_path);
    if !file_path.is_file() {
        return Err(LspToolError::FileNotFound(file_path));
    }

    let Some(server) = builtin_server_for_path(&file_path) else {
        return Err(LspToolError::UnsupportedFileType);
    };

    let workspace_root = resolve_workspace_root(&file_path, server.id);
    Ok(RenameContext {
        file_path,
        session_id: session_id.to_string(),
        server_id: server.id,
        workspace_root,
    })
}

fn request_prepare_rename(
    service: &dyn LspService,
    context: &RenameContext,
    line: u32,
    character: u32,
) -> Result<Value, LspToolError> {
    if line == 0 {
        return Err(LspToolError::InvalidRequest(
            "line must be >= 1".to_string(),
        ));
    }

    let uri = path_to_file_uri(&context.file_path).map_err(LspToolError::InvalidPath)?;
    service
        .request(
            &context.session_id,
            context.server_id,
            &context.workspace_root,
            "textDocument/prepareRename",
            serde_json::json!({
                "textDocument": { "uri": uri },
                "position": { "line": line.saturating_sub(1), "character": character },
            }),
        )
        .map_err(LspToolError::Server)
}

fn request_rename(
    service: &dyn LspService,
    context: &RenameContext,
    args: &RenameArgs,
) -> Result<Option<WorkspaceEdit>, LspToolError> {
    if args.line == 0 {
        return Err(LspToolError::InvalidRequest(
            "line must be >= 1".to_string(),
        ));
    }

    let uri = path_to_file_uri(&context.file_path).map_err(LspToolError::InvalidPath)?;
    let result = service
        .request(
            &context.session_id,
            context.server_id,
            &context.workspace_root,
            "textDocument/rename",
            serde_json::json!({
                "textDocument": { "uri": uri },
                "position": { "line": args.line.saturating_sub(1), "character": args.character },
                "newName": args.new_name,
            }),
        )
        .map_err(LspToolError::Server)?;

    if result.is_null() {
        return Ok(None);
    }

    serde_json::from_value(result).map(Some).map_err(|error| {
        LspToolError::InvalidServerResponse(format!("invalid workspace edit: {error}"))
    })
}

fn prepare_rename_is_valid(result: &Value) -> bool {
    if result.is_null() {
        return false;
    }

    if let Some(default_behavior) = result.get("defaultBehavior").and_then(Value::as_bool) {
        return default_behavior;
    }

    parse_range_value(result.get("range").unwrap_or(result)).is_some()
}

fn format_prepare_rename_result(result: &Value) -> String {
    if result.is_null() {
        return "Cannot rename at this position".to_string();
    }

    if let Some(default_behavior) = result.get("defaultBehavior").and_then(Value::as_bool) {
        return if default_behavior {
            "Rename supported (using default behavior)".to_string()
        } else {
            "Cannot rename at this position".to_string()
        };
    }

    if let Some(range) = parse_range_value(result.get("range").unwrap_or(result)) {
        let placeholder = result
            .get("placeholder")
            .and_then(Value::as_str)
            .map(|value| format!(" (current: \"{value}\")"))
            .unwrap_or_default();
        return format!(
            "Rename available at {}:{}-{}:{}{}",
            range.start.line + 1,
            range.start.character,
            range.end.line + 1,
            range.end.character,
            placeholder,
        );
    }

    "Cannot rename at this position".to_string()
}

fn apply_workspace_edit(edit: Option<WorkspaceEdit>) -> ApplyResult {
    let Some(edit) = edit else {
        return ApplyResult {
            success: false,
            files_modified: Vec::new(),
            total_edits: 0,
            errors: vec!["No edit provided".to_string()],
        };
    };

    let mut result = ApplyResult {
        success: true,
        files_modified: Vec::new(),
        total_edits: 0,
        errors: Vec::new(),
    };

    for (uri, edits) in edit.changes {
        let file_path = match uri_to_path(&uri) {
            Ok(path) => path,
            Err(error) => {
                result.success = false;
                result.errors.push(error);
                continue;
            }
        };

        match apply_text_edits_to_file(&file_path, &edits) {
            Ok(edit_count) => {
                result.files_modified.push(file_path.display().to_string());
                result.total_edits += edit_count;
            }
            Err(error) => {
                result.success = false;
                result
                    .errors
                    .push(format!("{}: {error}", file_path.display()));
            }
        }
    }

    for change in edit.document_changes {
        match change {
            DocumentChange::TextDocumentEdit(change) => {
                let file_path = match uri_to_path(&change.text_document.uri) {
                    Ok(path) => path,
                    Err(error) => {
                        result.success = false;
                        result.errors.push(error);
                        continue;
                    }
                };

                match apply_text_edits_to_file(&file_path, &change.edits) {
                    Ok(edit_count) => {
                        result.files_modified.push(file_path.display().to_string());
                        result.total_edits += edit_count;
                    }
                    Err(error) => {
                        result.success = false;
                        result
                            .errors
                            .push(format!("{}: {error}", file_path.display()));
                    }
                }
            }
            DocumentChange::ResourceOperation(ResourceOperation::Create { uri }) => {
                match uri_to_path(&uri) {
                    Ok(path) => match fs::write(&path, "") {
                        Ok(()) => result.files_modified.push(path.display().to_string()),
                        Err(error) => {
                            result.success = false;
                            result.errors.push(format!("Create {uri}: {error}"));
                        }
                    },
                    Err(error) => {
                        result.success = false;
                        result.errors.push(error);
                    }
                }
            }
            DocumentChange::ResourceOperation(ResourceOperation::Rename { old_uri, new_uri }) => {
                let old_path = match uri_to_path(&old_uri) {
                    Ok(path) => path,
                    Err(error) => {
                        result.success = false;
                        result.errors.push(error);
                        continue;
                    }
                };
                let new_path = match uri_to_path(&new_uri) {
                    Ok(path) => path,
                    Err(error) => {
                        result.success = false;
                        result.errors.push(error);
                        continue;
                    }
                };

                match fs::read_to_string(&old_path).and_then(|content| {
                    fs::write(&new_path, content)?;
                    fs::remove_file(&old_path)
                }) {
                    Ok(()) => result.files_modified.push(new_path.display().to_string()),
                    Err(error) => {
                        result.success = false;
                        result.errors.push(format!("Rename {old_uri}: {error}"));
                    }
                }
            }
            DocumentChange::ResourceOperation(ResourceOperation::Delete { uri }) => {
                match uri_to_path(&uri) {
                    Ok(path) => match fs::remove_file(&path) {
                        Ok(()) => result.files_modified.push(path.display().to_string()),
                        Err(error) => {
                            result.success = false;
                            result.errors.push(format!("Delete {uri}: {error}"));
                        }
                    },
                    Err(error) => {
                        result.success = false;
                        result.errors.push(error);
                    }
                }
            }
        }
    }

    result
}

fn format_apply_result(result: ApplyResult) -> String {
    if result.success {
        let mut lines = vec![format!(
            "Applied {} edit(s) to {} file(s):",
            result.total_edits,
            result.files_modified.len()
        )];
        for file in result.files_modified {
            lines.push(format!("  - {file}"));
        }
        return lines.join("\n");
    }

    let mut lines = vec!["Failed to apply some changes:".to_string()];
    for error in result.errors {
        lines.push(format!("  Error: {error}"));
    }
    if !result.files_modified.is_empty() {
        lines.push(format!(
            "Successfully modified: {}",
            result.files_modified.join(", ")
        ));
    }
    lines.join("\n")
}

fn apply_text_edits_to_file(file_path: &Path, edits: &[TextEdit]) -> Result<usize, String> {
    let content = fs::read_to_string(file_path).map_err(|error| error.to_string())?;
    let mut lines = content
        .split('\n')
        .map(ToOwned::to_owned)
        .collect::<Vec<String>>();

    let mut sorted_edits = edits.iter().collect::<Vec<_>>();
    sorted_edits.sort_by(|left, right| {
        right
            .range
            .start
            .line
            .cmp(&left.range.start.line)
            .then_with(|| right.range.start.character.cmp(&left.range.start.character))
    });

    for edit in sorted_edits {
        let start_line = edit.range.start.line;
        let end_line = edit.range.end.line;
        let start_char = edit.range.start.character;
        let end_char = edit.range.end.character;

        let required_len = start_line.max(end_line) + 1;
        if lines.len() < required_len {
            lines.resize(required_len, String::new());
        }

        let first_line = lines.get(start_line).cloned().unwrap_or_default();
        let last_line = lines.get(end_line).cloned().unwrap_or_default();
        let start_byte = utf16_code_unit_offset_to_byte_offset(&first_line, start_char)?;
        let end_byte = utf16_code_unit_offset_to_byte_offset(&last_line, end_char)?;

        if start_line == end_line {
            lines[start_line] = format!(
                "{}{}{}",
                &first_line[..start_byte],
                edit.new_text,
                &first_line[end_byte..]
            );
            continue;
        }

        let replacement = format!(
            "{}{}{}",
            &first_line[..start_byte],
            edit.new_text,
            &last_line[end_byte..]
        );
        let replacement_lines = replacement
            .split('\n')
            .map(ToOwned::to_owned)
            .collect::<Vec<String>>();
        lines.splice(start_line..=end_line, replacement_lines);
    }

    fs::write(file_path, lines.join("\n")).map_err(|error| error.to_string())?;
    Ok(edits.len())
}

fn utf16_code_unit_offset_to_byte_offset(line: &str, character: usize) -> Result<usize, String> {
    let mut utf16_units = 0;

    for (index, ch) in line.char_indices() {
        if utf16_units == character {
            return Ok(index);
        }

        utf16_units += ch.len_utf16();
        if utf16_units > character {
            return Err(format!(
                "invalid UTF-16 character offset {character} within line segment"
            ));
        }
    }

    if utf16_units == character {
        return Ok(line.len());
    }

    Ok(line.len())
}

fn resolve_file_path(project_dir: &Path, file_path: &str) -> PathBuf {
    let file_path = Path::new(file_path);
    if file_path.is_absolute() {
        file_path.to_path_buf()
    } else {
        project_dir.join(file_path)
    }
}

pub(crate) fn path_to_file_uri(path: &Path) -> Result<String, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(path)
    };
    let encoded = absolute
        .to_string_lossy()
        .split('/')
        .map(|segment| encode(segment).into_owned())
        .collect::<Vec<_>>()
        .join("/");
    Ok(format!("file://{encoded}"))
}

fn uri_to_path(uri: &str) -> Result<PathBuf, String> {
    let raw = uri
        .strip_prefix("file://")
        .ok_or_else(|| format!("Unsupported URI: {uri}"))?;
    let raw = raw.strip_prefix("localhost/").unwrap_or(raw);
    let decoded = decode(raw).map_err(|error| format!("Unsupported URI: {uri} ({error})"))?;
    Ok(PathBuf::from(decoded.into_owned()))
}

fn parse_range_value(value: &Value) -> Option<RangePreview> {
    Some(RangePreview {
        start: PositionPreview {
            line: value.get("start")?.get("line")?.as_u64()? as usize,
            character: value.get("start")?.get("character")?.as_u64()? as usize,
        },
        end: PositionPreview {
            line: value.get("end")?.get("line")?.as_u64()? as usize,
            character: value.get("end")?.get("character")?.as_u64()? as usize,
        },
    })
}

struct RangePreview {
    start: PositionPreview,
    end: PositionPreview,
}

struct PositionPreview {
    line: usize,
    character: usize,
}

#[derive(Debug)]
enum LspToolError {
    FileNotFound(PathBuf),
    UnsupportedFileType,
    DirectoryExtensionRequired,
    InvalidRequest(String),
    InvalidPath(String),
    InvalidServerResponse(String),
    Server(crate::lsp::LspManagerError),
}

impl std::fmt::Display for LspToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FileNotFound(path) => write!(f, "File not found: {}", path.display()),
            Self::UnsupportedFileType => write!(f, "No LSP server available for this file type."),
            Self::DirectoryExtensionRequired => {
                write!(
                    f,
                    "extension is required when file_path points to a directory"
                )
            }
            Self::InvalidRequest(message) => write!(f, "{message}"),
            Self::InvalidPath(message) => write!(f, "{message}"),
            Self::InvalidServerResponse(message) => write!(f, "{message}"),
            Self::Server(error) => write!(f, "{error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use serde_json::json;

    struct CountingService {
        calls: Mutex<usize>,
    }

    impl CountingService {
        fn new() -> Self {
            Self {
                calls: Mutex::new(0),
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    impl LspService for CountingService {
        fn request(
            &self,
            _session_id: &str,
            _server_id: &str,
            _workspace_root: &Path,
            _method: &str,
            _params: Value,
        ) -> Result<Value, crate::lsp::LspManagerError> {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            Ok(Value::Null)
        }
    }

    fn rename_context_fixture() -> (tempfile::TempDir, RenameContext) {
        let fixture = tempfile::tempdir().unwrap();
        let file_path = fixture.path().join("sample.rs");
        fs::write(&file_path, "fn sample() {}\n").unwrap();
        let context = rename_context(
            fixture.path(),
            "lsp-rename-line-validation",
            file_path.display().to_string(),
        )
        .unwrap();
        (fixture, context)
    }

    #[test]
    fn prepare_rename_rejects_zero_line_without_service_request() {
        let (_fixture, context) = rename_context_fixture();
        let service = CountingService::new();

        let result = request_prepare_rename(&service, &context, 0, 0);

        match result {
            Err(LspToolError::InvalidRequest(message)) => {
                assert_eq!(message, "line must be >= 1");
            }
            _ => panic!("expected invalid request"),
        }
        assert_eq!(service.calls(), 0);
    }

    #[test]
    fn rename_rejects_zero_line_without_service_request() {
        let (_fixture, context) = rename_context_fixture();
        let service = CountingService::new();
        let args = RenameArgs {
            file_path: context.file_path.display().to_string(),
            line: 0,
            character: 0,
            new_name: "renamed".to_string(),
        };

        let result = request_rename(&service, &context, &args);

        match result {
            Err(LspToolError::InvalidRequest(message)) => {
                assert_eq!(message, "line must be >= 1");
            }
            _ => panic!("expected invalid request"),
        }
        assert_eq!(service.calls(), 0);
    }

    #[test]
    fn rename_tool_reports_invalid_zero_line() {
        let fixture = tempfile::tempdir().unwrap();
        let file_path = fixture.path().join("sample.rs");
        fs::write(&file_path, "fn sample() {}\n").unwrap();
        let tool = LspRenameTool::new(
            fixture.path().to_path_buf(),
            "lsp-rename-line-validation",
            Arc::new(CountingService::new()),
        );

        let output = tool
            .execute(json!({
                "filePath": file_path.display().to_string(),
                "line": 0,
                "character": 0,
                "newName": "renamed"
            }))
            .unwrap();

        assert_eq!(output, "Error: line must be >= 1");
    }

    #[test]
    fn apply_text_edits_uses_utf16_offsets_for_non_bmp_text() {
        let fixture = tempfile::tempdir().unwrap();
        let file_path = fixture.path().join("sample.rs");
        fs::write(&file_path, "a😀b\n").unwrap();

        let edits = vec![TextEdit {
            range: Range {
                start: Position {
                    line: 0,
                    character: 3,
                },
                end: Position {
                    line: 0,
                    character: 4,
                },
            },
            new_text: "z".to_string(),
        }];

        let applied = apply_text_edits_to_file(&file_path, &edits);

        assert_eq!(applied.unwrap(), 1);
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "a😀z\n");
    }
}
