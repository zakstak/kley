use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, anyhow, bail, ensure};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::auth::{CredentialStore, ResolvedAuth};
use crate::diagnostics::{DiagnosticSeverity, has_error_diagnostics};
use crate::events::event_channel;
use crate::provider::openai::OpenAiProvider;
use crate::provider::test::{CONTROL_BLOCK_END, CONTROL_BLOCK_START, TestProvider};
use crate::provider::zai::ZaiProvider;
use crate::provider::{Provider, SendContext, TokenUsage, TurnResult};
use crate::runtime::SessionRuntime;
use crate::tools::editing::EditObservation;
use crate::tools::editing::artifacts::with_artifact_root_override;
use crate::tools::editing::telemetry::with_metrics_root_override;
use crate::tools::hashline_edit::HashlineEditTool;
use crate::tools::patch::PatchTool;
use crate::tools::{ToolExecutionResult, ToolRegistry};

const HARNESS_SCENARIO_KIND: &str = "hashline_harness_scenario";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum HarnessEngine {
    #[value(name = "patch")]
    Patch,
    #[value(name = "hashline_edit", alias = "hashline-edit")]
    HashlineEdit,
}

impl HarnessEngine {
    pub fn tool_name(self) -> &'static str {
        match self {
            Self::Patch => "patch",
            Self::HashlineEdit => "hashline_edit",
        }
    }
}

impl std::fmt::Display for HarnessEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.tool_name())
    }
}

#[derive(Debug, Clone)]
pub struct HarnessConfig {
    pub model: String,
    pub provider: String,
    pub engine: HarnessEngine,
    pub scenario_dir: PathBuf,
    pub output_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct HarnessSummary {
    pub runs: usize,
    pub success: usize,
    pub correctness_failure: usize,
    pub edit_failure: usize,
    pub provider_failure: usize,
    pub telemetry_failure: usize,
    pub output_dir: PathBuf,
    pub runs_jsonl: PathBuf,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HarnessStatus {
    Success,
    CorrectnessFailure,
    EditFailure,
    ProviderFailure,
    TelemetryFailure,
}

#[derive(Debug, Serialize)]
pub struct HarnessRunRecord {
    scenario: String,
    scenario_fixture: String,
    engine: HarnessEngine,
    provider: String,
    model: String,
    status: HarnessStatus,
    started_at: String,
    duration_ms: u128,
    prompt_path: String,
    provider_result_path: String,
    tool_result_path: String,
    run_artifact_path: String,
    workspace_snapshot_dir: String,
    artifact_root: String,
    token_usage: Option<SerializableTokenUsage>,
    selected_tool_call: Option<SerializableToolCall>,
    edit_observation: Option<EditObservation>,
    mismatches: Vec<FileMismatch>,
    provider_error: Option<String>,
    tool_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SerializableTokenUsage {
    input_tokens: usize,
    output_tokens: usize,
    total_tokens: usize,
}

impl From<&TokenUsage> for SerializableTokenUsage {
    fn from(value: &TokenUsage) -> Self {
        Self {
            input_tokens: value.input_tokens,
            output_tokens: value.output_tokens,
            total_tokens: value.total_tokens,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct SerializableToolCall {
    name: String,
    arguments: Value,
}

#[derive(Debug, Clone, Serialize)]
struct FileMismatch {
    path: String,
    expected: String,
    actual: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HashlineHarnessScenario {
    kind: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    workspace: WorkspaceFixture,
    engines: ScenarioCases,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceFixture {
    files: Vec<WorkspaceFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceFile {
    path: String,
    content: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioCases {
    #[serde(default)]
    patch: Option<ScenarioCase>,
    #[serde(default, rename = "hashline_edit")]
    hashline_edit: Option<ScenarioCase>,
}

impl ScenarioCases {
    fn select(&self, engine: HarnessEngine) -> Option<&ScenarioCase> {
        match engine {
            HarnessEngine::Patch => self.patch.as_ref(),
            HarnessEngine::HashlineEdit => self.hashline_edit.as_ref(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioCase {
    prompt: String,
    offline_provider_result: OfflineProviderResult,
    #[serde(default)]
    expected_files: Vec<ExpectedFile>,
    #[serde(default)]
    artifact_root_mode: ArtifactRootMode,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OfflineProviderResult {
    ToolCall { arguments: Value },
    Text { content: String },
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ArtifactRootMode {
    #[default]
    Directory,
    ExistingFile,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedFile {
    path: String,
    content: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CapturedProviderResult {
    ToolCalls { calls: Vec<SerializableToolCall> },
    Text { content: String },
    Aborted,
    Error { message: String },
}

#[derive(Debug, Serialize)]
struct CapturedToolResult {
    output: Option<String>,
    observations: Vec<EditObservation>,
    error: Option<String>,
}

#[derive(Debug)]
struct LoadedScenario {
    fixture_path: PathBuf,
    fixture_rel_path: String,
    scenario: HashlineHarnessScenario,
}

struct RunRecordPaths<'a> {
    prompt_path: &'a Path,
    provider_result_path: &'a Path,
    tool_result_path: &'a Path,
    run_artifact_path: &'a Path,
    workspace_snapshot_dir: &'a Path,
    artifact_root: &'a Path,
}

struct FinalizeRunRecord<'a> {
    loaded: &'a LoadedScenario,
    config: &'a HarnessConfig,
    started_at: chrono::DateTime<Utc>,
    duration_ms: u128,
    paths: RunRecordPaths<'a>,
    token_usage: Option<SerializableTokenUsage>,
    selected_tool_call: Option<SerializableToolCall>,
    edit_observation: Option<EditObservation>,
    mismatches: Vec<FileMismatch>,
    provider_error: Option<String>,
    tool_error: Option<String>,
    status: HarnessStatus,
}

pub fn default_provider() -> &'static str {
    "openai"
}

pub fn default_model(provider: &str) -> String {
    if provider == "test" {
        return "test-model".to_string();
    }
    SessionRuntime::default_model(provider)
}

pub fn default_scenario_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hashline")
}

pub fn default_output_dir() -> PathBuf {
    std::env::temp_dir()
        .join("kley")
        .join("hashline-harness")
        .join(format!(
            "{}-{}",
            Utc::now().format("%Y%m%dT%H%M%SZ"),
            Uuid::new_v4()
        ))
}

pub async fn run(config: HarnessConfig) -> Result<HarnessSummary> {
    let scenario_dir = absolutize(&config.scenario_dir)?;
    let output_dir = absolutize_without_existing(&config.output_dir)?;
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    let scenarios = load_scenarios(&scenario_dir)?;
    let runnable = scenarios
        .into_iter()
        .filter(|loaded| loaded.scenario.engines.select(config.engine).is_some())
        .collect::<Vec<_>>();
    ensure!(
        !runnable.is_empty(),
        "no {} scenarios found under {}",
        config.engine,
        scenario_dir.display()
    );

    let auth = resolve_auth(&config.provider).await?;
    let runs_jsonl = output_dir.join("runs.jsonl");
    let mut summary = HarnessSummary {
        runs: 0,
        success: 0,
        correctness_failure: 0,
        edit_failure: 0,
        provider_failure: 0,
        telemetry_failure: 0,
        output_dir: output_dir.clone(),
        runs_jsonl: runs_jsonl.clone(),
    };

    for (index, loaded) in runnable.iter().enumerate() {
        let record = run_scenario(&config, &auth, loaded, index, &output_dir).await?;
        append_jsonl(&runs_jsonl, &record)?;

        summary.runs += 1;
        match record.status {
            HarnessStatus::Success => summary.success += 1,
            HarnessStatus::CorrectnessFailure => summary.correctness_failure += 1,
            HarnessStatus::EditFailure => summary.edit_failure += 1,
            HarnessStatus::ProviderFailure => summary.provider_failure += 1,
            HarnessStatus::TelemetryFailure => summary.telemetry_failure += 1,
        }
    }

    Ok(summary)
}

async fn run_scenario(
    config: &HarnessConfig,
    auth: &ResolvedAuth,
    loaded: &LoadedScenario,
    index: usize,
    output_dir: &Path,
) -> Result<HarnessRunRecord> {
    let started_at = Utc::now();
    let started = std::time::Instant::now();
    let case = loaded
        .scenario
        .engines
        .select(config.engine)
        .ok_or_else(|| {
            anyhow!(
                "missing {} case for {}",
                config.engine,
                loaded.scenario.name
            )
        })?;

    let scenario_output_dir = output_dir.join(format!(
        "{:02}-{}",
        index + 1,
        slugify(&loaded.scenario.name)
    ));
    fs::create_dir_all(&scenario_output_dir)
        .with_context(|| format!("failed to create {}", scenario_output_dir.display()))?;
    fs::copy(
        &loaded.fixture_path,
        scenario_output_dir.join("scenario.json"),
    )
    .with_context(|| format!("failed to copy fixture {}", loaded.fixture_path.display()))?;

    let workspace = TempWorkspace::new(&loaded.scenario.name)?;
    materialize_workspace(&workspace.path, &loaded.scenario.workspace)?;

    let prompt = build_prompt(&loaded.scenario, case, config.engine, &config.provider)?;
    let prompt_path = scenario_output_dir.join("prompt.txt");
    fs::write(&prompt_path, &prompt)
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;

    let artifact_root = scenario_output_dir.join("edit-artifacts");
    let telemetry_root = match case.artifact_root_mode {
        ArtifactRootMode::Directory => artifact_root.clone(),
        ArtifactRootMode::ExistingFile => {
            let blocker = scenario_output_dir.join("metrics-root-blocker");
            fs::write(&blocker, "blocked")
                .with_context(|| format!("failed to write {}", blocker.display()))?;
            blocker
        }
    };

    let mut token_usage = None;
    let provider_result = invoke_provider(config, auth, &prompt, &mut token_usage).await;
    let provider_result_path = scenario_output_dir.join("provider-result.json");
    let tool_result_path = scenario_output_dir.join("tool-result.json");
    let run_artifact_path = scenario_output_dir.join("run.json");
    let workspace_snapshot_dir = scenario_output_dir.join("final-workspace");
    let paths = RunRecordPaths {
        prompt_path: &prompt_path,
        provider_result_path: &provider_result_path,
        tool_result_path: &tool_result_path,
        run_artifact_path: &run_artifact_path,
        workspace_snapshot_dir: &workspace_snapshot_dir,
        artifact_root: &artifact_root,
    };

    let mut selected_tool_call = None;
    let mut edit_observation = None;
    let mut provider_error = None;
    let mut tool_error = None;
    let mut mismatches = Vec::new();
    let status = match provider_result {
        Ok(TurnResult::ToolCalls(calls)) => {
            let serializable_calls = calls
                .iter()
                .map(|call| SerializableToolCall {
                    name: call.name.clone(),
                    arguments: serde_json::from_str(&call.arguments)
                        .unwrap_or(Value::String(call.arguments.clone())),
                })
                .collect::<Vec<_>>();
            write_json(
                &provider_result_path,
                &CapturedProviderResult::ToolCalls {
                    calls: serializable_calls,
                },
            )?;

            let maybe_call = calls
                .into_iter()
                .find(|call| call.name == config.engine.tool_name());

            if let Some(call) = maybe_call {
                let arguments: Value = match serde_json::from_str(&call.arguments) {
                    Ok(arguments) => arguments,
                    Err(err) => {
                        provider_error =
                            Some(format!("provider returned invalid tool arguments: {err}"));
                        write_json(
                            &tool_result_path,
                            &CapturedToolResult {
                                output: None,
                                observations: Vec::new(),
                                error: None,
                            },
                        )?;
                        copy_dir_recursive(&workspace.path, &workspace_snapshot_dir)?;
                        return finalize_run_record(FinalizeRunRecord {
                            loaded,
                            config,
                            started_at,
                            duration_ms: started.elapsed().as_millis(),
                            paths,
                            token_usage: token_usage.as_ref().map(SerializableTokenUsage::from),
                            selected_tool_call: None,
                            edit_observation: None,
                            mismatches: Vec::new(),
                            provider_error,
                            tool_error: None,
                            status: HarnessStatus::ProviderFailure,
                        });
                    }
                };

                selected_tool_call = Some(SerializableToolCall {
                    name: call.name,
                    arguments: arguments.clone(),
                });

                let arguments = match absolutize_tool_path_argument(arguments, &workspace.path) {
                    Ok(value) => value,
                    Err(err) => {
                        provider_error = Some(format!(
                            "provider returned unsupported tool path argument: {err}"
                        ));
                        write_json(
                            &tool_result_path,
                            &CapturedToolResult {
                                output: None,
                                observations: Vec::new(),
                                error: None,
                            },
                        )?;
                        copy_dir_recursive(&workspace.path, &workspace_snapshot_dir)?;
                        return finalize_run_record(FinalizeRunRecord {
                            loaded,
                            config,
                            started_at,
                            duration_ms: started.elapsed().as_millis(),
                            paths,
                            token_usage: token_usage.as_ref().map(SerializableTokenUsage::from),
                            selected_tool_call,
                            edit_observation: None,
                            mismatches: Vec::new(),
                            provider_error,
                            tool_error: None,
                            status: HarnessStatus::ProviderFailure,
                        });
                    }
                };

                let registry = selected_tool_registry(config.engine);
                let tool = registry.get(config.engine.tool_name()).ok_or_else(|| {
                    anyhow!("missing selected tool {}", config.engine.tool_name())
                })?;

                match with_artifact_root_override(&artifact_root, || {
                    with_metrics_root_override(&telemetry_root, || {
                        tool.execute_with_result(arguments)
                    })
                }) {
                    Ok(tool_result) => {
                        edit_observation = tool_result.edit_observations.first().cloned();
                        write_json(
                            &tool_result_path,
                            &CapturedToolResult {
                                output: Some(tool_result.output.clone()),
                                observations: tool_result.edit_observations.clone(),
                                error: None,
                            },
                        )?;
                        mismatches = compare_expected_files(&workspace.path, &case.expected_files)?;
                        classify_tool_result(&tool_result, &mismatches)
                    }
                    Err(err) => {
                        tool_error = Some(err.to_string());
                        write_json(
                            &tool_result_path,
                            &CapturedToolResult {
                                output: None,
                                observations: Vec::new(),
                                error: tool_error.clone(),
                            },
                        )?;
                        HarnessStatus::EditFailure
                    }
                }
            } else {
                provider_error = Some(format!(
                    "provider returned no {} tool call",
                    config.engine.tool_name()
                ));
                write_json(
                    &tool_result_path,
                    &CapturedToolResult {
                        output: None,
                        observations: Vec::new(),
                        error: None,
                    },
                )?;
                HarnessStatus::ProviderFailure
            }
        }
        Ok(TurnResult::Text(content)) => {
            provider_error = Some(format!(
                "provider returned text instead of a {} tool call",
                config.engine.tool_name()
            ));
            write_json(
                &provider_result_path,
                &CapturedProviderResult::Text { content },
            )?;
            write_json(
                &tool_result_path,
                &CapturedToolResult {
                    output: None,
                    observations: Vec::new(),
                    error: None,
                },
            )?;
            HarnessStatus::ProviderFailure
        }
        Ok(TurnResult::Aborted) => {
            provider_error = Some("provider aborted before returning a tool call".to_string());
            write_json(&provider_result_path, &CapturedProviderResult::Aborted)?;
            write_json(
                &tool_result_path,
                &CapturedToolResult {
                    output: None,
                    observations: Vec::new(),
                    error: None,
                },
            )?;
            HarnessStatus::ProviderFailure
        }
        Err(err) => {
            provider_error = Some(err.to_string());
            write_json(
                &provider_result_path,
                &CapturedProviderResult::Error {
                    message: provider_error.clone().unwrap_or_default(),
                },
            )?;
            write_json(
                &tool_result_path,
                &CapturedToolResult {
                    output: None,
                    observations: Vec::new(),
                    error: None,
                },
            )?;
            HarnessStatus::ProviderFailure
        }
    };

    copy_dir_recursive(&workspace.path, &workspace_snapshot_dir)?;
    finalize_run_record(FinalizeRunRecord {
        loaded,
        config,
        started_at,
        duration_ms: started.elapsed().as_millis(),
        paths,
        token_usage: token_usage.as_ref().map(SerializableTokenUsage::from),
        selected_tool_call,
        edit_observation,
        mismatches,
        provider_error,
        tool_error,
        status,
    })
}

fn finalize_run_record(input: FinalizeRunRecord<'_>) -> Result<HarnessRunRecord> {
    let record = HarnessRunRecord {
        scenario: input.loaded.scenario.name.clone(),
        scenario_fixture: input.loaded.fixture_rel_path.clone(),
        engine: input.config.engine,
        provider: input.config.provider.clone(),
        model: input.config.model.clone(),
        status: input.status,
        started_at: input.started_at.to_rfc3339(),
        duration_ms: input.duration_ms,
        prompt_path: input.paths.prompt_path.to_string_lossy().to_string(),
        provider_result_path: input
            .paths
            .provider_result_path
            .to_string_lossy()
            .to_string(),
        tool_result_path: input.paths.tool_result_path.to_string_lossy().to_string(),
        run_artifact_path: input.paths.run_artifact_path.to_string_lossy().to_string(),
        workspace_snapshot_dir: input
            .paths
            .workspace_snapshot_dir
            .to_string_lossy()
            .to_string(),
        artifact_root: input.paths.artifact_root.to_string_lossy().to_string(),
        token_usage: input.token_usage,
        selected_tool_call: input.selected_tool_call,
        edit_observation: input.edit_observation,
        mismatches: input.mismatches,
        provider_error: input.provider_error,
        tool_error: input.tool_error,
    };
    write_json(input.paths.run_artifact_path, &record)?;
    Ok(record)
}

fn classify_tool_result(
    result: &ToolExecutionResult,
    mismatches: &[FileMismatch],
) -> HarnessStatus {
    if result
        .edit_observations
        .iter()
        .any(|observation| observation.failure_kind.as_deref() == Some("telemetry_unavailable"))
        || result.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "tool.edit.telemetry_unavailable"
                || (diagnostic.severity == DiagnosticSeverity::Error
                    && diagnostic.code.contains("telemetry_unavailable"))
        })
        || result.output.contains("telemetry_unavailable")
    {
        return HarnessStatus::TelemetryFailure;
    }

    if result
        .edit_observations
        .iter()
        .any(|observation| observation.failure_kind.is_some())
        || has_error_diagnostics(&result.diagnostics)
        || result.output.trim_start().starts_with("Error:")
    {
        return HarnessStatus::EditFailure;
    }

    if !mismatches.is_empty() {
        return HarnessStatus::CorrectnessFailure;
    }

    HarnessStatus::Success
}

async fn invoke_provider(
    config: &HarnessConfig,
    auth: &ResolvedAuth,
    prompt: &str,
    token_usage: &mut Option<TokenUsage>,
) -> Result<TurnResult> {
    let provider = create_provider(&config.provider)?;
    let history = vec![serde_json::json!({
        "type": "message",
        "role": "user",
        "content": prompt,
    })];
    let session_id = format!("harness-{}", Uuid::new_v4());
    let turn_id = format!("turn-{}", Uuid::new_v4());
    let registry = selected_tool_registry(config.engine);
    let (events, _receiver) = event_channel();
    let abort_signal = AtomicBool::new(false);
    provider
        .send(
            auth,
            SendContext {
                model: &config.model,
                session_id: &session_id,
                turn_id: &turn_id,
                history: &history,
                registry: &registry,
                instructions: "You are running inside the Kley hashline harness. Use the one available edit tool when needed.",
                abort_signal: &abort_signal,
                events: &events,
                output_hook: None,
                reasoning_effort: None,
            },
            token_usage,
        )
        .await
}

fn resolve_relative_path(path: &str) -> Result<&Path> {
    let path = Path::new(path);
    ensure!(
        !path.is_absolute(),
        "fixture path must be relative: {}",
        path.display()
    );
    ensure!(
        path.components()
            .all(|component| !matches!(component, Component::ParentDir)),
        "fixture path must not traverse parents: {}",
        path.display()
    );
    Ok(path)
}

fn materialize_workspace(root: &Path, workspace: &WorkspaceFixture) -> Result<()> {
    for file in &workspace.files {
        let relative = resolve_relative_path(&file.path)?;
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(&path, &file.content)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    Ok(())
}

fn compare_expected_files(
    root: &Path,
    expected_files: &[ExpectedFile],
) -> Result<Vec<FileMismatch>> {
    let mut mismatches = Vec::new();
    for expected in expected_files {
        let relative = resolve_relative_path(&expected.path)?;
        let path = root.join(relative);
        let actual = match fs::read_to_string(&path) {
            Ok(content) => Some(content),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to read expected output {}", path.display()));
            }
        };
        if actual.as_deref() != Some(expected.content.as_str()) {
            mismatches.push(FileMismatch {
                path: expected.path.clone(),
                expected: expected.content.clone(),
                actual,
            });
        }
    }
    Ok(mismatches)
}

fn build_prompt(
    scenario: &HashlineHarnessScenario,
    case: &ScenarioCase,
    engine: HarnessEngine,
    provider: &str,
) -> Result<String> {
    let mut prompt = String::new();
    prompt.push_str(&format!("Scenario: {}\n", scenario.name));
    if let Some(description) = &scenario.description {
        prompt.push_str(&format!("Description: {}\n", description));
    }
    prompt.push_str(&format!("Selected engine: {}\n\n", engine.tool_name()));
    prompt.push_str(case.prompt.trim());
    prompt.push_str("\n\nWorkspace files:\n");

    for file in &scenario.workspace.files {
        prompt.push_str(&format!("\n--- {} ---\n{}\n", file.path, file.content));
    }

    if provider == "test" {
        let control = match &case.offline_provider_result {
            OfflineProviderResult::ToolCall { arguments } => serde_json::json!({
                "type": "tool_call",
                "name": engine.tool_name(),
                "arguments": arguments,
            }),
            OfflineProviderResult::Text { content } => serde_json::json!({
                "type": "text",
                "content": content,
            }),
        };
        prompt.push_str("\n\n");
        prompt.push_str(CONTROL_BLOCK_START);
        prompt.push('\n');
        prompt.push_str(&serde_json::to_string_pretty(&control)?);
        prompt.push('\n');
        prompt.push_str(CONTROL_BLOCK_END);
    }

    Ok(prompt)
}

fn selected_tool_registry(engine: HarnessEngine) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    match engine {
        HarnessEngine::Patch => registry.register(Box::new(PatchTool)),
        HarnessEngine::HashlineEdit => registry.register(Box::new(HashlineEditTool)),
    }
    registry
}

fn create_provider(provider: &str) -> Result<Box<dyn Provider>> {
    match provider {
        "openai" => Ok(Box::new(OpenAiProvider::new())),
        "zai" => Ok(Box::new(ZaiProvider::new())),
        "test" => Ok(Box::new(TestProvider::new())),
        other => bail!("unsupported provider: {other}"),
    }
}

async fn resolve_auth(provider: &str) -> Result<ResolvedAuth> {
    if provider == "test" {
        return Ok(ResolvedAuth {
            provider: "test".to_string(),
            api_key: "test-key".to_string(),
            base_url: "http://unused".to_string(),
            account_id: None,
        });
    }

    let store = CredentialStore::open().context("failed to open credential store")?;
    let (events, _receiver) = event_channel();
    let resolved = crate::auth::resolve_auth(&store, &events)
        .await
        .context("failed to resolve provider auth")?;
    ensure!(
        resolved.provider == provider,
        "requested provider '{}' but resolved credentials for '{}'",
        provider,
        resolved.provider
    );
    Ok(resolved)
}

fn load_scenarios(root: &Path) -> Result<Vec<LoadedScenario>> {
    ensure!(
        root.exists(),
        "scenario directory does not exist: {}",
        root.display()
    );
    let mut files = Vec::new();
    collect_json_files(root, &mut files)?;
    files.sort();

    let mut scenarios = Vec::new();
    for path in files {
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let value: Value = serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        let kind = value
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if kind != HARNESS_SCENARIO_KIND {
            continue;
        }

        let scenario: HashlineHarnessScenario = serde_json::from_value(value)
            .with_context(|| format!("failed to decode harness scenario {}", path.display()))?;
        ensure!(
            scenario.kind == HARNESS_SCENARIO_KIND,
            "unexpected scenario kind in {}: {}",
            path.display(),
            scenario.kind
        );
        for file in &scenario.workspace.files {
            let _ = resolve_relative_path(&file.path)?;
        }
        if let Some(case) = scenario.engines.patch.as_ref() {
            validate_case(case)?;
        }
        if let Some(case) = scenario.engines.hashline_edit.as_ref() {
            validate_case(case)?;
        }
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        scenarios.push(LoadedScenario {
            fixture_path: path,
            fixture_rel_path: relative,
            scenario,
        });
    }

    Ok(scenarios)
}

fn validate_case(case: &ScenarioCase) -> Result<()> {
    for expected in &case.expected_files {
        let _ = resolve_relative_path(&expected.path)?;
    }
    Ok(())
}

fn collect_json_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_json_files(&path, out)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            out.push(path);
        }
    }
    Ok(())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(value)?;
    fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))
}

fn append_jsonl<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    let line = serde_json::to_string(value)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

fn copy_dir_recursive(from: &Path, to: &Path) -> Result<()> {
    if to.exists() {
        fs::remove_dir_all(to).with_context(|| format!("failed to clear {}", to.display()))?;
    }
    fs::create_dir_all(to).with_context(|| format!("failed to create {}", to.display()))?;
    copy_dir_recursive_inner(from, to)
}

fn copy_dir_recursive_inner(from: &Path, to: &Path) -> Result<()> {
    for entry in fs::read_dir(from).with_context(|| format!("failed to read {}", from.display()))? {
        let entry = entry?;
        let source = entry.path();
        let dest = to.join(entry.file_name());
        if source.is_dir() {
            fs::create_dir_all(&dest)
                .with_context(|| format!("failed to create {}", dest.display()))?;
            copy_dir_recursive_inner(&source, &dest)?;
        } else {
            fs::copy(&source, &dest).with_context(|| {
                format!("failed to copy {} to {}", source.display(), dest.display())
            })?;
        }
    }
    Ok(())
}

fn absolutize(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}

fn absolutize_without_existing(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}

fn absolutize_tool_path_argument(mut arguments: Value, workspace: &Path) -> Result<Value> {
    let path_value = arguments
        .get_mut("path")
        .ok_or_else(|| anyhow!("missing tool path field"))?;
    let raw_path = path_value
        .as_str()
        .ok_or_else(|| anyhow!("tool path must be a string"))?;

    ensure!(
        !Path::new(raw_path).is_absolute(),
        "tool path must be relative"
    );
    let normalized = workspace.join(resolve_relative_path(raw_path)?);

    *path_value = Value::String(normalized.to_string_lossy().to_string());
    Ok(arguments)
}

fn slugify(raw: &str) -> String {
    let mut slug = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if matches!(ch, '-' | '_' | ' ') && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

struct TempWorkspace {
    path: PathBuf,
}

impl TempWorkspace {
    fn new(name: &str) -> Result<Self> {
        let path = std::env::temp_dir()
            .join("kley")
            .join("hashline-harness-workspaces")
            .join(format!("{}-{}", slugify(name), Uuid::new_v4()));
        fs::create_dir_all(&path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        Ok(Self { path })
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::absolutize_tool_path_argument;
    use serde_json::json;

    #[test]
    fn absolutize_tool_path_argument_rejects_absolute_paths() {
        let workspace = std::env::temp_dir();
        let args = json!({ "path": "/tmp/escape.txt" });
        let err = absolutize_tool_path_argument(args, &workspace)
            .expect_err("absolute path should be rejected");
        assert!(err.to_string().contains("tool path must be relative"));
    }

    #[test]
    fn absolutize_tool_path_argument_expands_relative_paths_inside_workspace() {
        let workspace = std::env::temp_dir().join("kley-harness-test-workspace");
        let args = json!({ "path": "src/file.txt" });
        let normalized = absolutize_tool_path_argument(args, &workspace).unwrap();
        let normalized_path = normalized["path"].as_str().unwrap();
        assert_eq!(
            std::path::PathBuf::from(normalized_path),
            workspace.join("src/file.txt")
        );
    }
}
