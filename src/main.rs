use anyhow::{Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use std::io::{self, Write};

use kley::agent::RunMode;
use kley::compact::CompactConfig;
use kley::events::{AgentEvent, event_channel};
use kley::runtime::{RuntimeHooks, ToolCall};
use kley::store::Store;

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
        #[arg(long, conflicts_with = "tool_approval")]
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
    },
    Preflight,
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

            let store = Store::open()?;
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
                &store,
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
        Command::Web { bind } => {
            let config = kley::web::config::WebConfig::from_bind_arg(bind.as_deref())?;
            kley::web::serve(config).await?;
        }
        Command::Preflight => {
            if !kley::preflight::run(&mut std::io::stdout())? {
                std::process::exit(1);
            }
        }
    }

    Ok(())
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
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

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
    fn preflight_subcommand_parses() {
        let cli = Cli::try_parse_from(["kley", "preflight"]).unwrap();
        assert!(matches!(cli.command, Command::Preflight));
    }
}
