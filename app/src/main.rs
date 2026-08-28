//! Entry point for the interactive ACP terminal UI.

mod log_buffer;
mod ui;

use clap::Parser;
use clap_verbosity_flag::{InfoLevel, Verbosity};
use color_eyre::Result;
use kid_agentic_coding::PromptRunner;
use log_buffer::LogBuffer;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Interactive terminal UI for agentic coding over ACP",
    long_about = None
)]
struct Args {
    /// Agent command and arguments, or a single JSON configuration
    #[arg(required = true, num_args = 1..)]
    agent_args: Vec<String>,

    #[command(flatten)]
    verbosity: Verbosity<InfoLevel>,

    /// Log level for dependencies outside this crate
    #[clap(long, default_value = "warn")]
    log_baseline: LevelFilter,
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let args = Args::parse();
    let log_buffer = LogBuffer::default();
    tracing_subscriber::fmt()
        .with_env_filter(env_filter(&args.verbosity, args.log_baseline))
        .with_writer(log_buffer.clone())
        .init();

    let agent = PromptRunner::parse_agent_args(&args.agent_args)?;

    ui::run(agent, log_buffer).await?;
    Ok(())
}

/// Builds the log filter from `--verbose`/`--quiet` and `--log-baseline`.
/// Everything outside this crate (`agent_client_protocol`, `ratatui`, ...)
/// sits at `log_baseline` (default `warn`), capped at `verbosity` itself so
/// `--quiet` (error level) quiets dependencies down to `error` too, not
/// just this crate. `RUST_LOG`, if set, wins outright.
fn env_filter(verbosity: &Verbosity<InfoLevel>, log_baseline: LevelFilter) -> EnvFilter {
    if let Ok(filter) = EnvFilter::try_from_default_env() {
        return filter;
    }
    let verbosity: LevelFilter = verbosity.tracing_level_filter();
    let baseline = std::cmp::min(verbosity, log_baseline);
    let directive = format!("kid_agentic_coding={verbosity}")
        .parse()
        .expect("crate name and level filter always form a valid directive");
    EnvFilter::default()
        .add_directive(baseline.into())
        .add_directive(directive)
}
