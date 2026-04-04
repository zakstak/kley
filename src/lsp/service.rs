use std::collections::HashMap;
use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use serde_json::{Value, json};

use super::builtin_catalog;
use crate::events::{AgentEvent, EventEmitter};
use crate::tools::lsp::path_to_file_uri;

pub trait LspService: Send + Sync {
    fn request(
        &self,
        session_id: &str,
        server_id: &str,
        workspace_root: &Path,
        method: &str,
        params: Value,
    ) -> Result<Value, LspManagerError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspManagerError {
    UnknownServer {
        server_id: String,
    },
    StartupFailed {
        session_id: String,
        server_id: String,
        workspace_root: PathBuf,
        reason: String,
    },
    Failed {
        session_id: String,
        server_id: String,
        workspace_root: PathBuf,
        reason: String,
    },
    RequestFailed {
        session_id: String,
        server_id: String,
        workspace_root: PathBuf,
        reason: String,
    },
    MissingBinary {
        binary: String,
    },
}

impl fmt::Display for LspManagerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownServer { server_id } => {
                write!(f, "unknown builtin lsp server '{server_id}'")
            }
            Self::StartupFailed {
                session_id,
                server_id,
                workspace_root,
                reason,
            } => write!(
                f,
                "lsp startup failed for session={session_id} server={server_id} root={}: {reason}",
                workspace_root.display()
            ),
            Self::Failed {
                session_id,
                server_id,
                workspace_root,
                reason,
            } => write!(
                f,
                "lsp is terminally failed for session={session_id} server={server_id} root={}: {reason}",
                workspace_root.display()
            ),
            Self::RequestFailed {
                session_id,
                server_id,
                workspace_root,
                reason,
            } => write!(
                f,
                "lsp request failed for session={session_id} server={server_id} root={}: {reason}",
                workspace_root.display()
            ),
            Self::MissingBinary { binary } => {
                write!(f, "required lsp binary not found on PATH: {binary}")
            }
        }
    }
}

impl std::error::Error for LspManagerError {}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ServerKey {
    session_id: String,
    server_id: String,
    workspace_root: PathBuf,
}

enum ServerState {
    Idle,
    Starting,
    Ready(Arc<dyn LspClient>),
    Failed(String),
}

struct ServerSlot {
    state: Mutex<ServerState>,
    changed: Condvar,
}

impl ServerSlot {
    fn new() -> Self {
        Self {
            state: Mutex::new(ServerState::Idle),
            changed: Condvar::new(),
        }
    }
}

pub trait LspClient: Send + Sync {
    fn request(&self, method: &str, params: Value) -> Result<Value, LspClientError>;

    fn notify(&self, _method: &str, _params: Value) -> Result<(), LspClientError> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspClientError {
    Retryable(String),
    Terminal(String),
}

pub trait LspClientFactory: Send + Sync {
    fn create(
        &self,
        command: &[String],
        workspace_root: &Path,
    ) -> Result<Arc<dyn LspClient>, String>;
}

pub struct LspManager {
    entries: Mutex<HashMap<ServerKey, Arc<ServerSlot>>>,
    factory: Arc<dyn LspClientFactory>,
    events: Mutex<Option<EventEmitter>>,
}

impl Default for LspManager {
    fn default() -> Self {
        Self::new()
    }
}

impl LspManager {
    const MISSING_BINARY_PREFIX: &str = "missing binary: ";
    const POSITION_ENCODING_UTF16: &str = "utf-16";

    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            factory: Arc::new(StdioLspClientFactory),
            events: Mutex::new(None),
        }
    }

    pub fn with_test_factory(factory: Arc<dyn LspClientFactory>) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            factory,
            events: Mutex::new(None),
        }
    }

    pub fn with_event_emitter(events: EventEmitter) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            factory: Arc::new(StdioLspClientFactory),
            events: Mutex::new(Some(events)),
        }
    }

    pub fn with_test_factory_and_event_emitter(
        factory: Arc<dyn LspClientFactory>,
        events: EventEmitter,
    ) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            factory,
            events: Mutex::new(Some(events)),
        }
    }

    pub fn set_event_emitter(&self, events: EventEmitter) {
        let mut slot = self.events.lock().unwrap();
        *slot = Some(events);
    }

    pub fn clear_event_emitter(&self) {
        let mut slot = self.events.lock().unwrap();
        *slot = None;
    }

    fn get_slot(&self, key: &ServerKey) -> Arc<ServerSlot> {
        let mut entries = self.entries.lock().unwrap();
        entries
            .entry(key.clone())
            .or_insert_with(|| Arc::new(ServerSlot::new()))
            .clone()
    }

    fn acquire_client(
        &self,
        key: &ServerKey,
        command: &[String],
    ) -> Result<Arc<dyn LspClient>, LspManagerError> {
        let slot = self.get_slot(key);

        loop {
            let mut state = slot.state.lock().unwrap();
            match &*state {
                ServerState::Ready(client) => return Ok(client.clone()),
                ServerState::Failed(reason) => {
                    return Err(Self::classify_failed_reason(key, reason));
                }
                ServerState::Starting => {
                    state = slot.changed.wait(state).unwrap();
                    drop(state);
                }
                ServerState::Idle => {
                    *state = ServerState::Starting;
                    drop(state);

                    self.emit_status(
                        key,
                        "lsp.starting",
                        format!(
                            "starting {} in {}",
                            command.join(" "),
                            key.workspace_root.display()
                        ),
                        Some(command.to_vec()),
                        None,
                    );

                    let started = self.factory.create(command, &key.workspace_root);
                    let mut update = slot.state.lock().unwrap();
                    match started {
                        Ok(client) => {
                            if let Err(error) = self.initialize_client(key, client.as_ref()) {
                                let failure_reason = error.to_string();
                                *update = ServerState::Failed(failure_reason.clone());
                                slot.changed.notify_all();
                                self.emit_status(
                                    key,
                                    "lsp.failed",
                                    failure_reason.clone(),
                                    Some(command.to_vec()),
                                    Some(failure_reason.clone()),
                                );
                                return Err(error);
                            }
                            *update = ServerState::Ready(client.clone());
                            slot.changed.notify_all();
                            self.emit_status(
                                key,
                                "lsp.ready",
                                format!(
                                    "{} ready for {}",
                                    key.server_id,
                                    key.workspace_root.display()
                                ),
                                Some(command.to_vec()),
                                None,
                            );
                            return Ok(client);
                        }
                        Err(reason) => {
                            *update = ServerState::Failed(reason.clone());
                            slot.changed.notify_all();
                            self.emit_status(
                                key,
                                "lsp.failed",
                                reason.clone(),
                                Some(command.to_vec()),
                                Some(reason.clone()),
                            );
                            if let Some(binary) = Self::missing_binary_from_reason(&reason) {
                                return Err(LspManagerError::MissingBinary {
                                    binary: binary.to_string(),
                                });
                            }
                            return Err(LspManagerError::StartupFailed {
                                session_id: key.session_id.clone(),
                                server_id: key.server_id.clone(),
                                workspace_root: key.workspace_root.clone(),
                                reason,
                            });
                        }
                    }
                }
            }
        }
    }

    fn mark_failed(&self, key: &ServerKey, reason: String) -> bool {
        let slot = self.get_slot(key);
        let mut state = slot.state.lock().unwrap();
        if matches!(&*state, ServerState::Failed(_)) {
            return false;
        }
        *state = ServerState::Failed(reason);
        slot.changed.notify_all();
        true
    }

    fn initialize_client(
        &self,
        key: &ServerKey,
        client: &dyn LspClient,
    ) -> Result<(), LspManagerError> {
        let root_uri = path_to_file_uri(&key.workspace_root).map_err(|error| {
            LspManagerError::StartupFailed {
                session_id: key.session_id.clone(),
                server_id: key.server_id.clone(),
                workspace_root: key.workspace_root.clone(),
                reason: format!(
                    "invalid workspace root '{}': {error}",
                    key.workspace_root.display()
                ),
            }
        })?;
        let initialize_result = client
            .request(
                "initialize",
                json!({
                    "processId": null,
                    "clientInfo": {
                        "name": "kley"
                    },
                    "rootUri": root_uri,
                    "capabilities": {
                        "general": {
                            "positionEncodings": [Self::POSITION_ENCODING_UTF16]
                        }
                    }
                }),
            )
            .map_err(|error| LspManagerError::StartupFailed {
                session_id: key.session_id.clone(),
                server_id: key.server_id.clone(),
                workspace_root: key.workspace_root.clone(),
                reason: format!("initialize failed: {error:?}"),
            })?;

        self.validate_position_encoding(key, &initialize_result)?;
        let _ = client.notify("initialized", json!({}));
        Ok(())
    }

    fn validate_position_encoding(
        &self,
        key: &ServerKey,
        initialize_result: &Value,
    ) -> Result<(), LspManagerError> {
        let Some(position_encoding) = initialize_result
            .get("capabilities")
            .and_then(|capabilities| capabilities.get("positionEncoding"))
            .and_then(Value::as_str)
        else {
            return Ok(());
        };

        if position_encoding == Self::POSITION_ENCODING_UTF16 {
            return Ok(());
        }

        Err(LspManagerError::StartupFailed {
            session_id: key.session_id.clone(),
            server_id: key.server_id.clone(),
            workspace_root: key.workspace_root.clone(),
            reason: format!(
                "unsupported lsp position encoding '{position_encoding}' (expected {})",
                Self::POSITION_ENCODING_UTF16
            ),
        })
    }

    fn emit_status(
        &self,
        key: &ServerKey,
        status: &str,
        detail: String,
        command: Option<Vec<String>>,
        last_error: Option<String>,
    ) {
        let events = self.events.lock().unwrap().clone();
        let Some(events) = events else {
            return;
        };

        events.emit(AgentEvent::StatusReport {
            session_id: Some(key.session_id.clone()),
            turn_id: None,
            status: status.to_string(),
            detail,
            turn_number: 0,
            server_id: Some(key.server_id.clone()),
            command,
            workspace_root: Some(key.workspace_root.display().to_string()),
            last_file: None,
            last_error,
        });
    }

    fn missing_binary_from_reason(reason: &str) -> Option<&str> {
        reason.strip_prefix(Self::MISSING_BINARY_PREFIX)
    }

    fn classify_failed_reason(key: &ServerKey, reason: &str) -> LspManagerError {
        if let Some(binary) = Self::missing_binary_from_reason(reason) {
            return LspManagerError::MissingBinary {
                binary: binary.to_string(),
            };
        }

        LspManagerError::Failed {
            session_id: key.session_id.clone(),
            server_id: key.server_id.clone(),
            workspace_root: key.workspace_root.clone(),
            reason: reason.to_string(),
        }
    }

    fn command_for_server(server_id: &str) -> Option<Vec<String>> {
        builtin_catalog()
            .iter()
            .find(|server| server.id == server_id)
            .map(|server| {
                server
                    .command
                    .iter()
                    .map(|part| (*part).to_string())
                    .collect::<Vec<_>>()
            })
    }

    pub fn lifecycle_state(
        &self,
        session_id: &str,
        server_id: &str,
        workspace_root: &Path,
    ) -> Option<TestingServerState> {
        let key = ServerKey {
            session_id: session_id.to_string(),
            server_id: server_id.to_string(),
            workspace_root: workspace_root.to_path_buf(),
        };
        let entries = self.entries.lock().unwrap();
        let slot = entries.get(&key)?;
        let state = slot.state.lock().unwrap();
        Some(match &*state {
            ServerState::Idle => TestingServerState::Idle,
            ServerState::Starting => TestingServerState::Starting,
            ServerState::Ready(_) => TestingServerState::Ready,
            ServerState::Failed(_) => TestingServerState::Failed,
        })
    }
}

impl LspService for LspManager {
    fn request(
        &self,
        session_id: &str,
        server_id: &str,
        workspace_root: &Path,
        method: &str,
        params: Value,
    ) -> Result<Value, LspManagerError> {
        let command =
            Self::command_for_server(server_id).ok_or_else(|| LspManagerError::UnknownServer {
                server_id: server_id.to_string(),
            })?;

        let key = ServerKey {
            session_id: session_id.to_string(),
            server_id: server_id.to_string(),
            workspace_root: workspace_root.to_path_buf(),
        };

        let client = self.acquire_client(&key, &command)?;
        match client.request(method, params) {
            Ok(result) => Ok(result),
            Err(LspClientError::Retryable(reason)) => Err(LspManagerError::RequestFailed {
                session_id: key.session_id,
                server_id: key.server_id,
                workspace_root: key.workspace_root,
                reason,
            }),
            Err(LspClientError::Terminal(reason)) => {
                if self.mark_failed(&key, reason.clone()) {
                    self.emit_status(
                        &key,
                        "lsp.failed",
                        reason.clone(),
                        Some(command),
                        Some(reason.clone()),
                    );
                }
                Err(LspManagerError::Failed {
                    session_id: key.session_id,
                    server_id: key.server_id,
                    workspace_root: key.workspace_root,
                    reason,
                })
            }
        }
    }
}

struct StdioLspClientFactory;

impl LspClientFactory for StdioLspClientFactory {
    fn create(
        &self,
        command: &[String],
        workspace_root: &Path,
    ) -> Result<Arc<dyn LspClient>, String> {
        if command.is_empty() {
            return Err("empty lsp command".to_string());
        }

        let mut process = Command::new(&command[0]);
        process
            .args(command.iter().skip(1))
            .current_dir(workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let mut child = process.spawn().map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                return format!("{}{}", LspManager::MISSING_BINARY_PREFIX, command[0]);
            }
            format!(
                "failed to spawn '{}' in {}: {error}",
                command.join(" "),
                workspace_root.display()
            )
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "failed to capture lsp stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "failed to capture lsp stdout".to_string())?;

        Ok(Arc::new(StdioLspClient::new(child, stdin, stdout)))
    }
}

struct StdioLspClient {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<ChildStdout>>,
    next_id: AtomicU64,
    io_lock: Mutex<()>,
}

impl StdioLspClient {
    fn new(child: Child, stdin: ChildStdin, stdout: ChildStdout) -> Self {
        Self {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            next_id: AtomicU64::new(1),
            io_lock: Mutex::new(()),
        }
    }

    fn ensure_running(&self) -> Result<(), LspClientError> {
        let mut child = self.child.lock().unwrap();
        match child.try_wait() {
            Ok(Some(status)) => Err(LspClientError::Terminal(format!(
                "lsp server exited unexpectedly with status {status}"
            ))),
            Ok(None) => Ok(()),
            Err(error) => Err(LspClientError::Retryable(format!(
                "failed to query lsp process status: {error}"
            ))),
        }
    }

    fn write_json_rpc(&self, body: &str) -> Result<(), LspClientError> {
        let mut stdin = self.stdin.lock().unwrap();
        let framed = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        stdin.write_all(framed.as_bytes()).map_err(|error| {
            LspClientError::Terminal(format!("failed writing lsp stdin: {error}"))
        })?;
        stdin.flush().map_err(|error| {
            LspClientError::Terminal(format!("failed flushing lsp stdin: {error}"))
        })
    }

    fn read_json_rpc_response_for_id(&self, expected_id: u64) -> Result<Value, LspClientError> {
        let mut stdout = self.stdout.lock().unwrap();
        read_json_rpc_response_for_id_from_reader(&mut *stdout, expected_id)
    }
}

impl LspClient for StdioLspClient {
    fn request(&self, method: &str, params: Value) -> Result<Value, LspClientError> {
        self.ensure_running()?;

        let _guard = self.io_lock.lock().unwrap();
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let body = serde_json::to_string(&request).map_err(|error| {
            LspClientError::Retryable(format!("failed serializing request: {error}"))
        })?;

        self.write_json_rpc(&body)?;
        let response = self.read_json_rpc_response_for_id(id)?;

        if let Some(error) = response.get("error") {
            return Err(LspClientError::Retryable(format!(
                "lsp error response: {error}"
            )));
        }

        response
            .get("result")
            .cloned()
            .ok_or_else(|| LspClientError::Retryable("missing json-rpc result field".to_string()))
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), LspClientError> {
        self.ensure_running()?;

        let _guard = self.io_lock.lock().unwrap();
        let request = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let body = serde_json::to_string(&request).map_err(|error| {
            LspClientError::Retryable(format!("failed serializing notification: {error}"))
        })?;
        self.write_json_rpc(&body)
    }
}

impl Drop for StdioLspClient {
    fn drop(&mut self) {
        let mut child = self.child.lock().unwrap();
        terminate_child_process(&mut child);
    }
}

fn terminate_child_process(child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }

    let _ = child.kill();
    let _ = child.wait();
}

fn read_lsp_frame(reader: &mut dyn BufRead) -> io::Result<Vec<u8>> {
    let mut content_length: Option<usize> = None;

    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "eof while reading json-rpc headers",
            ));
        }

        if line == "\r\n" {
            break;
        }

        if let Some(value) = line.strip_prefix("Content-Length:") {
            let parsed = value.trim().parse::<usize>().map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid content-length header: {error}"),
                )
            })?;
            content_length = Some(parsed);
        }
    }

    let content_length = content_length.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing content-length header")
    })?;
    let mut payload = vec![0_u8; content_length];
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

fn read_json_rpc_response_for_id_from_reader(
    reader: &mut dyn BufRead,
    expected_id: u64,
) -> Result<Value, LspClientError> {
    loop {
        let payload = read_lsp_frame(reader).map_err(|error| {
            LspClientError::Terminal(format!("failed reading lsp stdout: {error}"))
        })?;
        let response = serde_json::from_slice::<Value>(&payload).map_err(|error| {
            LspClientError::Retryable(format!("invalid json-rpc response: {error}"))
        })?;
        if response.get("id").and_then(|value| value.as_u64()) == Some(expected_id) {
            return Ok(response);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestingServerState {
    Idle,
    Starting,
    Ready,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Mutex;

    fn frame(value: Value) -> String {
        let body = value.to_string();
        format!("Content-Length: {}\r\n\r\n{body}", body.len())
    }

    #[test]
    fn reads_past_interleaved_notification_and_non_matching_response() {
        let stream = format!(
            "{}{}{}",
            frame(json!({
                "jsonrpc": "2.0",
                "method": "window/logMessage",
                "params": {"type": 3, "message": "hello"}
            })),
            frame(json!({
                "jsonrpc": "2.0",
                "id": 41,
                "result": {"ignored": true}
            })),
            frame(json!({
                "jsonrpc": "2.0",
                "id": 42,
                "result": {"ok": true}
            }))
        );

        let mut reader = Cursor::new(stream.into_bytes());
        let matching = read_json_rpc_response_for_id_from_reader(&mut reader, 42).unwrap();

        assert_eq!(matching["result"], json!({"ok": true}));
    }

    #[test]
    fn drop_terminates_lsp_child_process() {
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();

        let client = StdioLspClient::new(child, stdin, stdout);
        drop(client);

        let status = Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .status()
            .unwrap();

        assert!(!status.success());
    }

    struct RecordingFactory {
        initialize_params: Arc<Mutex<Vec<Value>>>,
        response_capabilities: Value,
    }

    impl RecordingFactory {
        fn with_response_capabilities(response_capabilities: Value) -> Self {
            Self {
                initialize_params: Arc::new(Mutex::new(Vec::new())),
                response_capabilities,
            }
        }

        fn initialize_params(&self) -> Vec<Value> {
            self.initialize_params.lock().unwrap().clone()
        }
    }

    struct RecordingClient {
        initialize_params: Arc<Mutex<Vec<Value>>>,
        response_capabilities: Value,
    }

    impl LspClient for RecordingClient {
        fn request(&self, method: &str, params: Value) -> Result<Value, LspClientError> {
            if method == "initialize" {
                self.initialize_params.lock().unwrap().push(params);
                return Ok(json!({
                    "capabilities": self.response_capabilities,
                }));
            }

            Ok(json!({ "ok": true }))
        }
    }

    impl LspClientFactory for RecordingFactory {
        fn create(
            &self,
            _command: &[String],
            _workspace_root: &Path,
        ) -> Result<Arc<dyn LspClient>, String> {
            Ok(Arc::new(RecordingClient {
                initialize_params: self.initialize_params.clone(),
                response_capabilities: self.response_capabilities.clone(),
            }))
        }
    }

    #[test]
    fn initialize_negotiates_utf16_position_encoding() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let factory = Arc::new(RecordingFactory::with_response_capabilities(json!({})));
        let manager = LspManager::with_test_factory(factory.clone());

        let response = manager.request(
            "session-a",
            "rust-analyzer",
            &root,
            "textDocument/hover",
            json!({}),
        );

        assert_eq!(response.unwrap(), json!({ "ok": true }));
        let initialize_calls = factory.initialize_params();
        assert_eq!(initialize_calls.len(), 1);
        assert_eq!(
            initialize_calls[0]["capabilities"]["general"]["positionEncodings"],
            json!(["utf-16"])
        );
    }

    #[test]
    fn initialize_rejects_non_utf16_position_encoding() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let factory = Arc::new(RecordingFactory::with_response_capabilities(json!({
            "positionEncoding": "utf-8"
        })));
        let manager = LspManager::with_test_factory(factory);

        let response = manager.request(
            "session-a",
            "rust-analyzer",
            &root,
            "textDocument/hover",
            json!({}),
        );

        assert!(matches!(
            response,
            Err(LspManagerError::StartupFailed { reason, .. })
                if reason.contains("unsupported lsp position encoding 'utf-8'")
        ));
    }
}
