use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use kley::harness::hashline::{
    HarnessConfig, HarnessEngine, default_model, default_output_dir, default_provider,
    default_scenario_dir, run,
};

#[derive(Debug, Parser)]
#[command(name = "hashline-harness")]
#[command(about = "Run isolated hashline edit scenarios against one engine/provider/model")]
struct Cli {
    #[arg(long, default_value = default_provider())]
    provider: String,

    #[arg(long)]
    model: Option<String>,

    #[arg(long, value_enum, default_value_t = HarnessEngine::HashlineEdit)]
    engine: HarnessEngine,

    #[arg(long, default_value_os_t = default_scenario_dir())]
    scenario_dir: PathBuf,

    #[arg(long, default_value_os_t = default_output_dir())]
    output_dir: PathBuf,
}

#[tokio::main]
async fn main() {
    if let Err(err) = real_main().await {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

async fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let model = cli.model.unwrap_or_else(|| default_model(&cli.provider));
    let summary = run(HarnessConfig {
        model,
        provider: cli.provider,
        engine: cli.engine,
        scenario_dir: cli.scenario_dir,
        output_dir: cli.output_dir,
    })
    .await?;

    println!(
        "hashline harness complete: runs={} success={} correctness_failure={} edit_failure={} provider_failure={} telemetry_failure={} runs_jsonl={}",
        summary.runs,
        summary.success,
        summary.correctness_failure,
        summary.edit_failure,
        summary.provider_failure,
        summary.telemetry_failure,
        summary.runs_jsonl.display(),
    );
    Ok(())
}
