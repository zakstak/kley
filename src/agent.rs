use std::io::{self, BufRead, Write};
use std::sync::Arc;

use anyhow::Result;

use crate::auth::{self, CredentialStore};
use crate::compact::CompactConfig;
use crate::events::EventEmitter;
use crate::runtime::{
    AbortResult, RuntimeEvent, RuntimeHooks, SessionRuntime, SubmitResult,
};
use crate::store::Store;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunMode {
    Interactive,
    Autonomous {
        initial_prompt: String,
        max_turns: usize,
    },
}

pub async fn chat_loop(
    model_override: Option<&str>,
    resume_session_id: Option<&str>,
    store: &Store,
    events: EventEmitter,
    run_mode: RunMode,
    compact_config: CompactConfig,
) -> Result<()> {
    let cred_store = CredentialStore::open()?;
    let resolved = auth::resolve_auth(&cred_store, &events).await?;

    let project_dir = std::env::current_dir().unwrap_or_default();
    let registry = crate::tools::default_registry(project_dir.clone());
    let rules = crate::skills::discover_rules(&project_dir);
    let skills = crate::skills::discover_skills(&project_dir);
    let instructions = crate::skills::build_system_prompt(&rules, &skills);

    let hooks = RuntimeHooks {
        on_output_delta: None,
        on_event: Some(Arc::new(|event| match event {
            RuntimeEvent::SessionResumed { session_id } => {
                eprintln!("Resuming session {}", &session_id[..8]);
            }
            RuntimeEvent::SessionCreated { session_id } => {
                eprintln!("Session {}", &session_id[..8]);
            }
            RuntimeEvent::HistoryLoaded { turns } => {
                eprintln!("Loaded {turns} previous turns");
            }
            RuntimeEvent::ToolCallStarted { .. } | RuntimeEvent::ToolCallCompleted { .. } => {}
        })),
    };

    let mut runtime = SessionRuntime::new(
        store,
        resolved,
        model_override,
        resume_session_id,
        events,
        compact_config,
        registry,
        instructions,
        hooks,
    )?;

    eprintln!("kley v0 — {}/{}", runtime.provider(), runtime.model());
    eprintln!("Type a message and press Enter. Ctrl+D to quit.\n");

    let stdin = io::stdin();
    let (is_autonomous, mut remaining_turns) = match &run_mode {
        RunMode::Interactive => (false, usize::MAX),
        RunMode::Autonomous { max_turns, .. } => (true, *max_turns),
    };
    let mut pending_input: Option<String> = match &run_mode {
        RunMode::Autonomous { initial_prompt, .. } => Some(initial_prompt.clone()),
        RunMode::Interactive => None,
    };
    let mut consecutive_errors: usize = 0;

    loop {
        let input = if let Some(queued) = pending_input.take() {
            queued
        } else {
            eprint!("> ");
            io::stderr().flush().ok();

            let mut line = String::new();
            let bytes_read = stdin.lock().read_line(&mut line)?;
            if bytes_read == 0 {
                runtime.mark_completed()?;
                eprintln!("\nGoodbye.");
                break;
            }
            let trimmed = line.trim().to_string();
            if trimmed.is_empty() {
                continue;
            }
            trimmed
        };

        let submit = runtime.submit_prompt(input).await?;

        let turn_ok = match submit {
            SubmitResult::Completed { .. } => true,
            SubmitResult::Failed { error, .. } => {
                eprintln!("Error: {error}");
                false
            }
            SubmitResult::Aborted { result, .. } => {
                if let AbortResult::Aborted { .. } = result {
                    eprintln!("Aborted.");
                }
                false
            }
        };

        println!();

        if is_autonomous {
            if turn_ok {
                consecutive_errors = 0;
            } else {
                consecutive_errors += 1;
                if consecutive_errors >= 3 {
                    eprintln!(
                        "\n🛑 Autonomous mode: {} consecutive errors. Stopping.",
                        consecutive_errors
                    );
                    runtime.mark_completed()?;
                    break;
                }
            }

            remaining_turns = remaining_turns.saturating_sub(1);
            if remaining_turns == 0 {
                eprintln!("\n🛑 Autonomous mode: max turns reached. Stopping.");
                runtime.mark_completed()?;
                break;
            }

            pending_input = Some("Acknowledged. Continue to the next improvement.".to_string());
        }
    }

    Ok(())
}

pub use crate::runtime::Message;
pub use crate::runtime::history_from_turns;
pub use crate::runtime::history_items_from_turns;
pub use crate::runtime::process_openai_sse_block;
pub use crate::runtime::process_zai_sse_line;

#[cfg(any(test, feature = "testing"))]
pub async fn run_cli_adapter_with_runtime_for_test(
    runtime: &mut SessionRuntime<'_>,
    prompts: &[String],
) -> Result<()> {
    for prompt in prompts {
        let _ = runtime.submit_prompt(prompt.clone()).await?;
        println!();
    }
    runtime.mark_completed()?;
    Ok(())
}
