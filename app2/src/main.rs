use agent_client_protocol::AcpAgent;
use anyhow::{Context, Result};
use clap::Parser;
use kid_agentic_coding_app2::{Client, ClientDisconnected};

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
    let agent_args = args.agent_args.join(" ");
    let agent = AcpAgent::from_args(args.agent_args)?;
    Client::<ClientDisconnected>::new(agent)
        .connect()
        .await
        .context(format!("Failed to connect to agent `{agent_args}`"))?
        .initialize()
        .wait()
        .await
        .context(format!("Agent `{agent_args}` stopped with an error"))?;
    Ok(())
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
