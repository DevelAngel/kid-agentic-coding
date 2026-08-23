//! Entry point for the interactive ACP terminal UI.

mod ui;

use clap::Parser;
use kid_agentic_coding::PromptRunner;

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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let agent = PromptRunner::parse_agent_args(&args.agent_args)?;

    ui::run(agent).await?;

    Ok(())
}
