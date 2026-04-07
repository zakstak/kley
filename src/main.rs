use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use kley::agent::RunMode;
use kley::compact::CompactConfig;
use kley::events::{AgentEvent, event_channel};
use kley::runtime::{RuntimeHooks, RuntimeManager, ToolCall};
use kley::store::{
    Store, TaskAttemptRecord, TaskEdgeRecord, TaskEventRecord, TaskLifecycleState, TaskRecord,
};

#[derive(Debug, Parser)]
#[command(name = "kley")]
#[command(about = "Minimal coding agent — learning-focused, stripped to the basics")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Authenticate with a provider
    Login {
        #[command(subcommand)]
        provider: LoginProvider,
    },
    /// Start an interactive chat session
    Chat {
        /// How tool calls are approved
        #[arg(long, value_enum)]
        tool_approval: Option<ToolApprovalMode>,

        /// Model to use (e.g. "gpt-4.1" or "glm-4.7")
        #[arg(long, short)]
        model: Option<String>,

        /// Resume the most recent session
        #[arg(long)]
        last: bool,

        /// Resume a specific session by ID
        #[arg(long)]
        resume: Option<String>,

        /// Auto-approve all tool executions without confirmation
        #[arg(long, conflicts_with = "tool_approval")]
        yolo: bool,

        /// Run autonomously — the agent works continuously, checking in
        /// via report_status, without waiting for user input between turns.
        #[arg(long)]
        autonomous: bool,

        /// Maximum number of autonomous turns before stopping (safety valve).
        #[arg(long, default_value = "50")]
        max_turns: usize,

        /// Initial prompt (required for --autonomous mode).
        #[arg(long, required_if_eq("autonomous", "true"))]
        prompt: Option<String>,

        /// Character budget for context-window compaction. When history
        /// exceeds this, older items are summarized. (~4 chars/token,
        /// so 800000 ≈ 200k tokens.)
        #[arg(long, default_value = "800000")]
        compact_threshold: usize,

        /// Reasoning effort level for the model (e.g. "low", "medium", "high").
        /// When set, the model will spend more compute on internal reasoning
        /// before responding. Only supported by OpenAI reasoning models.
        #[arg(long)]
        reasoning_effort: Option<String>,
    },
    Web {
        #[arg(long)]
        bind: Option<String>,

        #[arg(long)]
        public_origin: Option<String>,
    },
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    Version,
    Preflight,
}

#[derive(Debug, Subcommand)]
enum TaskCommand {
    List,
    Inspect {
        task_id: String,
    },
    Watch {
        task_id: String,

        #[arg(long, default_value_t = 0)]
        after_sequence: i64,

        #[arg(long, default_value_t = 250)]
        poll_ms: u64,
    },
    Control {
        #[command(subcommand)]
        action: TaskControlCommand,
    },
}

#[derive(Debug, Subcommand)]
enum TaskControlCommand {
    Cancel { task_id: String },
    Retry { task_id: String },
    Resume { task_id: String },
    Reprioritize { task_id: String, priority: i64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ToolApprovalMode {
    Ask,
    Auto,
    Never,
}

#[derive(Debug, Subcommand)]
enum LoginProvider {
    /// Login via OpenAI ChatGPT Plus/Pro subscription (OAuth)
    Openai,
    /// Store a ZAI (ZhipuAI) API key
    Zai,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    if version_flag_requested() {
        println!("{}", format_version_string());
        return Ok(());
    }

    let cli = Cli::parse();

    match cli.command {
        Command::Login { provider } => match provider {
            LoginProvider::Openai => kley::auth::openai::login_interactive().await?,
            LoginProvider::Zai => kley::auth::zai::login_interactive()?,
        },
        Command::Chat {
            tool_approval: approval_mode,
            model,
            last,
            resume,
            yolo,
            autonomous,
            max_turns,
            prompt,
            compact_threshold,
            reasoning_effort,
        } => {
            let approval_mode = resolve_tool_approval_mode(approval_mode, autonomous, yolo)?;
            let auto_approve_tools = yolo || matches!(approval_mode, ToolApprovalMode::Auto);
            let deny_tools = matches!(approval_mode, ToolApprovalMode::Never);
            let _ = io::stdout().flush();

            if matches!(approval_mode, ToolApprovalMode::Auto) {
                eprintln!("  ℹ️  Tool approval mode: auto");
            }
            if matches!(approval_mode, ToolApprovalMode::Never) {
                eprintln!("  ℹ️  Tool approval mode: never");
            }
            if autonomous && deny_tools {
                eprintln!("  ℹ️  Autonomous mode is non-interactive: tool calls will be denied.");
            }

            // Resolve run mode
            let tool_approval: fn(&ToolCall) -> bool = if auto_approve_tools {
                |_| true
            } else if deny_tools {
                |_| false
            } else {
                |tool| request_tool_approval(tool)
            };

            let run_mode = if autonomous {
                let initial_prompt = match prompt {
                    Some(prompt) => prompt,
                    None => bail!("--autonomous requires --prompt"),
                };
                RunMode::Autonomous {
                    initial_prompt,
                    max_turns,
                }
            } else {
                RunMode::Interactive
            };

            let shared_store = Arc::new(Mutex::new(Store::open()?));
            let runtime_manager = Arc::new(RuntimeManager::new());
            runtime_manager.bind_shared_store(Arc::clone(&shared_store));
            runtime_manager.recover_bound_store_on_startup().await?;
            let (emitter, receiver) = event_channel();

            // Spawn a thread to print events as they arrive.
            let event_thread = std::thread::spawn(move || {
                while let Ok(event) = receiver.recv_blocking() {
                    print_event(&event);
                }
            });

            // Determine which session to use
            let session_id = if let Some(id) = resume {
                Some(id)
            } else if last {
                let store = shared_store
                    .lock()
                    .map_err(|error| anyhow::anyhow!("store mutex poisoned: {error}"))?;
                kley::store::Session::get_latest(&store)?.map(|s| s.id)
            } else {
                None
            };

            let compact_config = CompactConfig {
                threshold_chars: compact_threshold,
                ..CompactConfig::default()
            };

            kley::agent::chat_loop(
                model.as_deref(),
                session_id.as_deref(),
                Arc::clone(&shared_store),
                Arc::clone(&runtime_manager),
                emitter,
                run_mode,
                compact_config,
                reasoning_effort,
                RuntimeHooks {
                    on_tool_approval: Some(std::sync::Arc::new(tool_approval)),
                    ..RuntimeHooks::default()
                },
            )
            .await?;

            let _ = event_thread.join();
        }
        Command::Web {
            bind,
            public_origin,
        } => {
            let config =
                kley::web::config::WebConfig::from_args(bind.as_deref(), public_origin.as_deref())?;
            kley::web::serve(config).await?;
        }
        Command::Task { command } => run_task_command(command).await?,
        Command::Version => {
            println!("{}", format_version_string());
        }
        Command::Preflight => {
            if !kley::preflight::run(&mut std::io::stdout())? {
                std::process::exit(1);
            }
        }
    }

    Ok(())
}

fn version_flag_requested() -> bool {
    let mut args = std::env::args_os();
    let _binary = args.next();
    match (args.next(), args.next()) {
        (Some(flag), None) => {
            let flag = flag.to_string_lossy();
            flag == "--version" || flag == "-V"
        }
        _ => false,
    }
}

fn format_version_string() -> String {
    match current_system_generation(Path::new("/nix/var/nix/profiles/system")) {
        Some(generation) => format!("kley {}+gen.{}", env!("CARGO_PKG_VERSION"), generation),
        None => format!("kley {}", env!("CARGO_PKG_VERSION")),
    }
}

fn current_system_generation(profile_link: &Path) -> Option<u64> {
    let target = std::fs::read_link(profile_link).ok()?;
    let file_name = target.file_name()?.to_str()?;
    let trimmed = file_name.strip_prefix("system-")?.strip_suffix("-link")?;
    trimmed.parse::<u64>().ok()
}

fn resolve_tool_approval_mode(
    approval_mode: Option<ToolApprovalMode>,
    autonomous: bool,
    yolo: bool,
) -> Result<ToolApprovalMode> {
    let env_mode = match std::env::var("KLEY_TOOL_APPROVAL") {
        Ok(raw) => Some(parse_tool_approval_mode(&raw)?),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            bail!("invalid tool approval mode: KLEY_TOOL_APPROVAL must be valid unicode")
        }
    };

    resolve_tool_approval_mode_with_env(approval_mode, autonomous, yolo, env_mode)
}

fn resolve_tool_approval_mode_with_env(
    approval_mode: Option<ToolApprovalMode>,
    autonomous: bool,
    yolo: bool,
    env_mode: Option<ToolApprovalMode>,
) -> Result<ToolApprovalMode> {
    if yolo {
        return Ok(ToolApprovalMode::Auto);
    }

    let mode = approval_mode.or(env_mode).unwrap_or({
        if autonomous {
            ToolApprovalMode::Auto
        } else {
            ToolApprovalMode::Ask
        }
    });

    if autonomous && matches!(mode, ToolApprovalMode::Ask) {
        bail!("autonomous mode cannot use ask tool approval; set KLEY_TOOL_APPROVAL=auto or never")
    }

    Ok(mode)
}

fn parse_tool_approval_mode(raw: &str) -> Result<ToolApprovalMode> {
    match raw.to_ascii_lowercase().as_str() {
        "ask" => Ok(ToolApprovalMode::Ask),
        "auto" => Ok(ToolApprovalMode::Auto),
        "never" => Ok(ToolApprovalMode::Never),
        _ => bail!("invalid tool approval mode: {raw}"),
    }
}

fn request_tool_approval(tool: &ToolCall) -> bool {
    eprintln!("  ⇓ tool call: {} {}", tool.name, tool.call_id);
    if !tool.arguments.is_empty() {
        eprintln!("     args: {}", preview_tool_arguments(&tool.arguments));
    }
    loop {
        eprint!("  Allow tool execution? [y/N]: ");
        let _ = std::io::stderr().flush();

        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_err() {
            return false;
        }

        match answer.trim().to_lowercase().as_str() {
            "y" | "yes" => return true,
            "n" | "no" | "" => return false,
            _ => {
                eprintln!("  Please type y or n.");
            }
        }
    }
}

fn preview_tool_arguments(arguments: &str) -> String {
    let max_chars = 220usize;
    let trimmed = arguments.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }

    trimmed.chars().take(max_chars).chain("…".chars()).collect()
}

async fn run_task_command(command: TaskCommand) -> Result<()> {
    match command {
        TaskCommand::List => {
            let store = Store::open()?;
            print!("{}", render_task_list(&store)?);
            io::stdout().flush().ok();
        }
        TaskCommand::Inspect { task_id } => {
            let store = Store::open()?;
            print!("{}", render_task_detail(&store, &task_id, 0)?.0);
            io::stdout().flush().ok();
        }
        TaskCommand::Watch {
            task_id,
            after_sequence,
            poll_ms,
        } => {
            let store = Store::open()?;
            let (output, mut last_sequence) = render_task_detail(&store, &task_id, after_sequence)?;
            print!("{output}");
            io::stdout().flush().ok();

            loop {
                let store = Store::open()?;
                let events = TaskEventRecord::list_for_task(&store, &task_id, last_sequence)?;
                for event in &events {
                    print!("{}", render_task_event_line(event)?);
                    last_sequence = event.sequence;
                }
                io::stdout().flush().ok();

                tokio::select! {
                    result = tokio::signal::ctrl_c() => {
                        result.context("failed to listen for ctrl-c")?;
                        break;
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(poll_ms)) => {}
                }
            }
        }
        TaskCommand::Control { action } => {
            let shared_store = Arc::new(Mutex::new(Store::open()?));
            let manager = RuntimeManager::new();
            match action {
                TaskControlCommand::Cancel { task_id } => {
                    {
                        let store = lock_cli_store(&shared_store)?;
                        validate_cancelable_task(&store, &task_id)?;
                    }
                    let affected_task_ids = manager.cancel_task_graph(&shared_store, &task_id)?;
                    let store = lock_cli_store(&shared_store)?;
                    let task = TaskRecord::get(&store, &task_id)?;
                    let state = TaskRecord::current_state(&store, &task_id)?;
                    println!(
                        "action=cancel task_id={} task_state={} affected_task_ids={}",
                        task.task_id,
                        state,
                        affected_task_ids.join(",")
                    );
                }
                TaskControlCommand::Retry { task_id } => {
                    let new_attempt_id = manager.retry_task(&shared_store, &task_id)?;
                    let store = lock_cli_store(&shared_store)?;
                    let state = TaskRecord::current_state(&store, &task_id)?;
                    println!(
                        "action=retry task_id={} task_state={} new_attempt_id={}",
                        task_id, state, new_attempt_id
                    );
                }
                TaskControlCommand::Resume { task_id } => {
                    let new_attempt_id = manager.resume_task(&shared_store, &task_id)?;
                    let store = lock_cli_store(&shared_store)?;
                    let state = TaskRecord::current_state(&store, &task_id)?;
                    println!(
                        "action=resume task_id={} task_state={} new_attempt_id={}",
                        task_id, state, new_attempt_id
                    );
                }
                TaskControlCommand::Reprioritize { task_id, priority } => {
                    manager.reprioritize_task(&shared_store, &task_id, priority)?;
                    let store = lock_cli_store(&shared_store)?;
                    let task = TaskRecord::get(&store, &task_id)?;
                    let state = TaskRecord::current_state(&store, &task_id)?;
                    println!(
                        "action=reprioritize task_id={} task_state={} priority={}",
                        task.task_id, state, task.priority
                    );
                }
            }
        }
    }

    Ok(())
}

fn lock_cli_store(shared_store: &Arc<Mutex<Store>>) -> Result<std::sync::MutexGuard<'_, Store>> {
    shared_store
        .lock()
        .map_err(|error| anyhow::anyhow!("store mutex poisoned: {error}"))
}

fn validate_cancelable_task(store: &Store, task_id: &str) -> Result<()> {
    let state = TaskRecord::current_state(store, task_id)?;
    if matches!(
        state,
        TaskLifecycleState::Completed | TaskLifecycleState::Cancelled
    ) {
        bail!("cancel is only allowed for nonterminal tasks: task {task_id} is {state}");
    }
    if state == TaskLifecycleState::CancelRequested {
        bail!("cancel is already requested for task {task_id}");
    }
    if state == TaskLifecycleState::Failed {
        bail!("cancel is only allowed before terminal failure: task {task_id} is {state}");
    }
    Ok(())
}

fn render_task_list(store: &Store) -> Result<String> {
    let tasks = TaskRecord::list(store)?;
    let mut output = String::new();
    for task in tasks {
        let attempts = TaskAttemptRecord::list_for_task(store, &task.task_id)?;
        push_task_summary(store, &mut output, &task, &attempts, false)?;
        output.push('\n');
    }
    Ok(output)
}

fn render_task_detail(store: &Store, task_id: &str, after_sequence: i64) -> Result<(String, i64)> {
    use std::fmt::Write as _;

    let selected_task = TaskRecord::get(store, task_id)?;
    let all_tasks = TaskRecord::list(store)?;
    let all_edges = TaskEdgeRecord::list(store)?;
    let related_task_ids = collect_related_task_ids(task_id, &all_tasks, &all_edges);
    let related_tasks = all_tasks
        .into_iter()
        .filter(|task| related_task_ids.contains(&task.task_id))
        .collect::<Vec<_>>();
    let related_edges = all_edges
        .into_iter()
        .filter(|edge| {
            related_task_ids.contains(&edge.task_id)
                && related_task_ids.contains(&edge.depends_on_task_id)
        })
        .collect::<Vec<_>>();
    let attempts = TaskAttemptRecord::list_for_task(store, task_id)?;
    let replay_events = TaskEventRecord::list_for_task(store, task_id, after_sequence)?;
    let latest_sequence = TaskEventRecord::list_for_task(store, task_id, 0)?
        .last()
        .map(|event| event.sequence)
        .unwrap_or(0);

    let mut output = String::new();
    push_task_summary(store, &mut output, &selected_task, &attempts, true)?;
    writeln!(&mut output, "cursor_after_sequence={after_sequence}")?;
    writeln!(&mut output, "cursor_latest_sequence={latest_sequence}")?;
    writeln!(&mut output, "graph_nodes:")?;
    for task in related_tasks {
        let task_attempts = TaskAttemptRecord::list_for_task(store, &task.task_id)?;
        push_graph_node_summary(store, &mut output, &task, &task_attempts)?;
    }
    writeln!(&mut output, "graph_edges:")?;
    for edge in related_edges {
        writeln!(
            &mut output,
            "  - task_id={} depends_on_task_id={}",
            edge.task_id, edge.depends_on_task_id
        )?;
    }
    writeln!(&mut output, "attempts:")?;
    for attempt in &attempts {
        push_attempt_summary(&mut output, attempt)?;
    }
    writeln!(&mut output, "events:")?;
    for event in &replay_events {
        output.push_str(&render_task_event_line(event)?);
    }
    Ok((output, latest_sequence))
}

fn push_task_summary(
    store: &Store,
    output: &mut String,
    task: &TaskRecord,
    attempts: &[TaskAttemptRecord],
    include_metadata: bool,
) -> Result<()> {
    use std::fmt::Write as _;

    let state = task_current_state(store, task, attempts)?;
    let latest_attempt = attempts.last();
    writeln!(output, "task_id={}", task.task_id)?;
    writeln!(output, "task_state={state}")?;
    writeln!(output, "title={}", display_option(task.title.as_deref()))?;
    writeln!(output, "priority={}", task.priority)?;
    writeln!(
        output,
        "parent_task_id={}",
        display_option(task.parent_task_id.as_deref())
    )?;
    writeln!(
        output,
        "latest_attempt_id={}",
        display_option(latest_attempt.map(|attempt| attempt.attempt_id.as_str()))
    )?;
    writeln!(
        output,
        "latest_attempt_state={}",
        display_option(latest_attempt.map(|attempt| attempt.status.as_str()))
    )?;
    writeln!(
        output,
        "child_session_id={}",
        display_option(latest_attempt.and_then(|attempt| attempt.session_id.as_deref()))
    )?;
    let lease = latest_attempt
        .and_then(|attempt| extract_attempt_lease(attempt).transpose())
        .transpose()?;
    writeln!(
        output,
        "lease_owner={}",
        display_option(lease.as_ref().map(|lease| lease.owner_id.as_str()))
    )?;
    writeln!(
        output,
        "lease_expires_at={}",
        display_option(lease.as_ref().map(|lease| lease.lease_expires_at.as_str()))
    )?;

    if include_metadata {
        writeln!(output, "created_at={}", task.created_at.to_rfc3339())?;
        writeln!(output, "updated_at={}", task.updated_at.to_rfc3339())?;
        writeln!(
            output,
            "policy_snapshot={}",
            render_json_string(&task.policy_snapshot)
        )?;
        writeln!(output, "parent_close_policy={}", task.parent_close_policy)?;
        writeln!(
            output,
            "recovery_checkpoint={}",
            display_option(
                task.recovery_checkpoint
                    .as_deref()
                    .map(render_json_string_ref)
                    .as_deref()
            )
        )?;
    }

    Ok(())
}

fn push_graph_node_summary(
    store: &Store,
    output: &mut String,
    task: &TaskRecord,
    attempts: &[TaskAttemptRecord],
) -> Result<()> {
    use std::fmt::Write as _;

    let state = task_current_state(store, task, attempts)?;
    let latest_attempt = attempts.last();
    let lease = latest_attempt
        .and_then(|attempt| extract_attempt_lease(attempt).transpose())
        .transpose()?;
    writeln!(
        output,
        "  - task_id={} task_state={} latest_attempt_id={} latest_attempt_state={} child_session_id={} lease_owner={}",
        task.task_id,
        state,
        display_option(latest_attempt.map(|attempt| attempt.attempt_id.as_str())),
        display_option(latest_attempt.map(|attempt| attempt.status.as_str())),
        display_option(latest_attempt.and_then(|attempt| attempt.session_id.as_deref())),
        display_option(lease.as_ref().map(|lease| lease.owner_id.as_str())),
    )?;
    Ok(())
}

fn push_attempt_summary(output: &mut String, attempt: &TaskAttemptRecord) -> Result<()> {
    use std::fmt::Write as _;

    let lease = extract_attempt_lease(attempt)?;
    writeln!(
        output,
        "  - attempt_id={} attempt_state={} child_session_id={} lease_owner={} lease_expires_at={} recovery_checkpoint={}",
        attempt.attempt_id,
        attempt.status,
        display_option(attempt.session_id.as_deref()),
        display_option(lease.as_ref().map(|lease| lease.owner_id.as_str())),
        display_option(lease.as_ref().map(|lease| lease.lease_expires_at.as_str())),
        display_option(
            attempt
                .recovery_checkpoint
                .as_deref()
                .map(render_json_string_ref)
                .as_deref()
        ),
    )?;
    Ok(())
}

fn render_task_event_line(event: &TaskEventRecord) -> Result<String> {
    use std::fmt::Write as _;

    let payload_value = parse_json_value(&event.payload)?;
    let lease_owner = payload_value
        .get("owner_id")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let mut line = String::new();
    writeln!(
        &mut line,
        "  - sequence={} task_id={} attempt_id={} child_session_id={} event_type={} lease_owner={} recorded_at={} payload={}",
        event.sequence,
        event.task_id,
        event.attempt_id,
        display_option(event.session_id.as_deref()),
        event.event_type,
        lease_owner,
        event.recorded_at.to_rfc3339(),
        serde_json::to_string(&payload_value).context("failed to render task event payload")?
    )?;
    Ok(line)
}

fn collect_related_task_ids(
    selected_task_id: &str,
    tasks: &[TaskRecord],
    edges: &[TaskEdgeRecord],
) -> HashSet<String> {
    let mut by_parent = HashMap::<String, Vec<String>>::new();
    for task in tasks {
        if let Some(parent_task_id) = &task.parent_task_id {
            by_parent
                .entry(parent_task_id.clone())
                .or_default()
                .push(task.task_id.clone());
        }
    }

    let mut related = HashSet::from([selected_task_id.to_string()]);
    let mut queue = VecDeque::from([selected_task_id.to_string()]);

    while let Some(task_id) = queue.pop_front() {
        for edge in edges {
            let adjacent = if edge.task_id == task_id {
                Some(edge.depends_on_task_id.clone())
            } else if edge.depends_on_task_id == task_id {
                Some(edge.task_id.clone())
            } else {
                None
            };

            if let Some(adjacent) = adjacent
                && related.insert(adjacent.clone())
            {
                queue.push_back(adjacent);
            }
        }

        if let Some(task) = tasks.iter().find(|task| task.task_id == task_id)
            && let Some(parent_task_id) = &task.parent_task_id
            && related.insert(parent_task_id.clone())
        {
            queue.push_back(parent_task_id.clone());
        }

        if let Some(children) = by_parent.get(&task_id) {
            for child_task_id in children {
                if related.insert(child_task_id.clone()) {
                    queue.push_back(child_task_id.clone());
                }
            }
        }
    }

    related
}

fn extract_attempt_lease(attempt: &TaskAttemptRecord) -> Result<Option<CliAttemptLease>> {
    let Some(raw_checkpoint) = attempt.recovery_checkpoint.as_deref() else {
        return Ok(None);
    };
    let parsed = parse_json_value(raw_checkpoint)?;
    let Some(lease) = parsed.get("scheduler_lease") else {
        return Ok(None);
    };
    let owner_id = lease
        .get("owner_id")
        .and_then(Value::as_str)
        .context("scheduler_lease.owner_id missing")?;
    let lease_expires_at = lease
        .get("lease_expires_at")
        .and_then(Value::as_str)
        .context("scheduler_lease.lease_expires_at missing")?;
    Ok(Some(CliAttemptLease {
        owner_id: owner_id.to_string(),
        lease_expires_at: lease_expires_at.to_string(),
    }))
}

fn display_option(value: Option<&str>) -> &str {
    value.unwrap_or("-")
}

fn render_json_string(raw: &str) -> String {
    render_json_string_ref(raw)
}

fn render_json_string_ref(raw: &str) -> String {
    parse_json_value(raw)
        .and_then(|value| serde_json::to_string(&value).context("failed to render json value"))
        .unwrap_or_else(|_| raw.to_string())
}

fn parse_json_value(raw: &str) -> Result<Value> {
    serde_json::from_str(raw).or_else(|_| Ok(Value::String(raw.to_string())))
}

#[derive(Debug, Clone)]
struct CliAttemptLease {
    owner_id: String,
    lease_expires_at: String,
}

fn task_current_state(
    store: &Store,
    task: &TaskRecord,
    attempts: &[TaskAttemptRecord],
) -> Result<TaskLifecycleState> {
    if attempts.is_empty() {
        return Ok(TaskLifecycleState::Queued);
    }
    TaskRecord::current_state(store, &task.task_id)
}

/// Render an event to stderr with visual emphasis appropriate to its severity.
fn print_event(event: &AgentEvent) {
    match event {
        AgentEvent::MessageDelta { delta, .. } => {
            print!("{delta}");
            let _ = std::io::stdout().flush();
        }
        AgentEvent::TransportFallback { .. } | AgentEvent::TurnFailed { .. } => {
            // High-visibility: box the message
            let msg = event.to_string();
            let width = msg.len() + 4;
            eprintln!();
            eprintln!("┌{}┐", "─".repeat(width));
            eprintln!("│  {}  │", msg);
            eprintln!("└{}┘", "─".repeat(width));
            eprintln!();
        }
        AgentEvent::TokenRefreshed { .. } => {
            eprintln!("  ↻ {event}");
        }
        AgentEvent::TransportSelected { .. }
        | AgentEvent::TurnStarted { .. }
        | AgentEvent::TurnCompleted { .. } => {
            eprintln!("  {event}");
        }
        AgentEvent::ToolCallStarted { .. } | AgentEvent::ToolCallCompleted { .. } => {
            eprintln!("{event}");
        }
        AgentEvent::MessageStarted { .. } | AgentEvent::MessageCompleted { .. } => {}
        AgentEvent::StatusReport { .. } => {
            eprintln!("  {event}");
        }
        AgentEvent::HistoryCompacted { .. } => {
            eprintln!("  {event}");
        }
        AgentEvent::TaskLifecycle { .. } => {
            eprintln!("  {event}");
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use std::path::Path;

    #[test]
    fn autonomous_mode_requires_prompt() {
        let err = Cli::try_parse_from(["kley", "chat", "--autonomous"]).unwrap_err();
        assert!(matches!(err.kind(), ErrorKind::MissingRequiredArgument));
    }

    #[test]
    fn autonomous_mode_accepts_prompt() {
        let cli =
            Cli::try_parse_from(["kley", "chat", "--autonomous", "--prompt", "tinker"]).unwrap();
        let Command::Chat {
            autonomous, prompt, ..
        } = cli.command
        else {
            panic!("expected chat command")
        };

        assert!(autonomous);
        assert_eq!(prompt.unwrap(), "tinker");
    }

    #[test]
    fn autonomous_mode_accepts_never_tool_approval_flag() {
        let cli = Cli::try_parse_from([
            "kley",
            "chat",
            "--autonomous",
            "--prompt",
            "tinker",
            "--tool-approval",
            "never",
        ])
        .unwrap();

        let Command::Chat {
            autonomous,
            prompt,
            tool_approval,
            ..
        } = cli.command
        else {
            panic!("expected chat command")
        };

        assert!(autonomous);
        assert_eq!(prompt.unwrap(), "tinker");
        assert_eq!(tool_approval, Some(ToolApprovalMode::Never));
    }

    #[test]
    fn autonomous_mode_accepts_auto_tool_approval_flag() {
        let cli = Cli::try_parse_from([
            "kley",
            "chat",
            "--autonomous",
            "--prompt",
            "tinker",
            "--tool-approval",
            "auto",
        ])
        .unwrap();

        let Command::Chat { tool_approval, .. } = cli.command else {
            panic!("expected chat command")
        };

        assert_eq!(tool_approval, Some(ToolApprovalMode::Auto));
    }

    #[test]
    fn preview_tool_arguments_truncates_long_unicode_input() {
        let long = "界".repeat(230);
        let preview = preview_tool_arguments(&long);
        assert_eq!(preview.chars().count(), 221);
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn preview_tool_arguments_truncates_long_input() {
        let long = "x".repeat(250);
        let preview = preview_tool_arguments(&long);
        assert_eq!(preview, format!("{}{}", "x".repeat(220), "…"));
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn preview_tool_arguments_returns_input_if_short() {
        let short = r#"{"path":"file.txt"}"#;
        let preview = preview_tool_arguments(short);
        assert_eq!(preview, short);
    }

    #[test]
    fn tool_approval_value_must_be_valid() {
        let err =
            Cli::try_parse_from(["kley", "chat", "--tool-approval", "sometimes"]).unwrap_err();
        assert!(matches!(err.kind(), ErrorKind::InvalidValue));
    }

    #[test]
    fn tool_approval_conflicts_with_yolo() {
        let err =
            Cli::try_parse_from(["kley", "chat", "--tool-approval", "auto", "--yolo"]).unwrap_err();
        assert!(matches!(err.kind(), ErrorKind::ArgumentConflict));
    }

    #[test]
    fn autonomous_mode_defaults_to_auto_tool_approval() {
        let mode = resolve_tool_approval_mode_with_env(None, true, false, None).unwrap();
        assert_eq!(mode, ToolApprovalMode::Auto);
    }

    #[test]
    fn autonomous_mode_honors_never_tool_approval_from_env() {
        let mode =
            resolve_tool_approval_mode_with_env(None, true, false, Some(ToolApprovalMode::Never))
                .unwrap();
        assert_eq!(mode, ToolApprovalMode::Never);
    }

    #[test]
    fn autonomous_mode_rejects_ask_tool_approval_from_env() {
        let err =
            resolve_tool_approval_mode_with_env(None, true, false, Some(ToolApprovalMode::Ask))
                .unwrap_err();
        assert!(
            err.to_string()
                .contains("autonomous mode cannot use ask tool approval")
        );
    }

    #[test]
    fn autonomous_mode_rejects_ask_tool_approval_flag() {
        let err =
            resolve_tool_approval_mode_with_env(Some(ToolApprovalMode::Ask), true, false, None)
                .unwrap_err();
        assert!(
            err.to_string()
                .contains("autonomous mode cannot use ask tool approval")
        );
    }

    #[test]
    fn preflight_subcommand_parses() {
        let cli = Cli::try_parse_from(["kley", "preflight"]).unwrap();
        assert!(matches!(cli.command, Command::Preflight));
    }

    #[test]
    fn version_subcommand_parses() {
        let cli = Cli::try_parse_from(["kley", "version"]).unwrap();
        assert!(matches!(cli.command, Command::Version));
    }

    #[test]
    fn extracts_generation_from_profile_link_target() {
        let link_name = Path::new("/nix/var/nix/profiles/system-42-link");
        let file_name = link_name.file_name().unwrap().to_str().unwrap();
        let generation = file_name
            .strip_prefix("system-")
            .and_then(|rest| rest.strip_suffix("-link"))
            .and_then(|value| value.parse::<u64>().ok());
        assert_eq!(generation, Some(42));
    }

    #[test]
    fn current_system_generation_returns_none_for_missing_link() {
        let missing = Path::new("/tmp/kley-does-not-exist-system-link");
        assert_eq!(current_system_generation(missing), None);
    }
}
