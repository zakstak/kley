use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::Write;

use kley::agent::RunMode;
use kley::compact::CompactConfig;
use kley::events::{AgentEvent, event_channel};
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
        #[arg(long)]
        yolo: bool,

        /// Run autonomously — the agent works continuously, checking in
        /// via report_status, without waiting for user input between turns.
        #[arg(long)]
        autonomous: bool,

        /// Maximum number of autonomous turns before stopping (safety valve).
        #[arg(long, default_value = "50")]
        max_turns: usize,

        /// Initial prompt (required for --autonomous mode).
        #[arg(long)]
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
            model,
            last,
            resume,
            yolo: _yolo,
            autonomous,
            max_turns,
            prompt,
            compact_threshold,
            reasoning_effort,
        } => {
            // Resolve run mode
            let run_mode = if autonomous {
                let initial_prompt = prompt.unwrap_or_else(|| {
                    eprintln!("error: --autonomous requires --prompt \"<your prompt>\"");
                    std::process::exit(1);
                });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preflight_subcommand_parses() {
        let cli = Cli::try_parse_from(["kley", "preflight"]).unwrap();
        assert!(matches!(cli.command, Command::Preflight));
    }
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
