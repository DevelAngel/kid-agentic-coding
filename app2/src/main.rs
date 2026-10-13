use anyhow::Result;
use clap::Parser;
use kid_agentic_coding_app2::Application;
use tokio::io::{self, BufReader};

#[derive(Debug, Parser)]
#[command(about = "ACP client for an agent program")]
struct Args {
    /// Agent command and arguments.
    #[arg(required = true, num_args = 1..)]
    agent_args: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt().init();
    tracing::debug!("logging initialized");

    Application::new(args.agent_args)
        .run(BufReader::new(io::stdin()))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_agent_program_and_arguments() {
        let args = Args::parse_from(["app2", "opencode", "acp"]);

        assert_eq!(args.agent_args, ["opencode", "acp"]);
    }
}
