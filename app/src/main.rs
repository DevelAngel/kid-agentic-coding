mod log_buffer;
mod ui;

use clap::Parser;
use clap_verbosity_flag::{InfoLevel, Verbosity};
use color_eyre::Result;
use kid_agentic_coding::PromptRunner;
use log_buffer::LogBuffer;
use std::path::PathBuf;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::time::ChronoLocal;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Interactive terminal UI for agentic coding over ACP",
    long_about = None
)]
struct Args {
    /// Skips confetti MCP tool registration even when the agent supports it.
    #[arg(long)]
    disable_confetti: bool,

    /// Uses filesystem sockets under DIR for the MCP bridge instead of
    /// Linux abstract-namespace sockets, which cannot cross a sandbox
    /// boundary. When the agent is sandboxed, DIR must be mounted
    /// writable into the sandbox; the conventional choice is
    /// `$XDG_RUNTIME_DIR/kid-agentic-coding`, e.g.
    /// `/run/user/1000/kid-agentic-coding`. The directory is created
    /// automatically before the agent starts.
    #[arg(long, value_name = "DIR")]
    fs_socket_dir: Option<PathBuf>,

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
        .with_timer(ChronoLocal::new("%Y-%m-%d %H:%M:%S%.6f".to_owned()))
        .with_env_filter(env_filter(&args.verbosity, args.log_baseline))
        .with_writer(log_buffer.clone())
        .init();
    tracing::debug!("logging initialized");

    let agent = PromptRunner::parse_agent_args(&args.agent_args)?;
    ui::run(agent, log_buffer, args.disable_confetti, args.fs_socket_dir).await?;
    Ok(())
}

fn env_filter(verbosity: &Verbosity<InfoLevel>, log_baseline: LevelFilter) -> EnvFilter {
    if let Ok(filter) = EnvFilter::try_from_default_env() {
        return filter;
    }
    let verbosity: LevelFilter = verbosity.tracing_level_filter();
    let baseline = std::cmp::min(verbosity, log_baseline);
    let directive = format!("kid_agentic_coding={verbosity}")
        .parse()
        .expect("crate name and level filter always form a valid directive");
    let agent_stderr = "agent_stderr=debug"
        .parse()
        .expect("static log target is valid");
    EnvFilter::default()
        .add_directive(baseline.into())
        .add_directive(directive)
        .add_directive(agent_stderr)
}

#[cfg(test)]
mod args_tests {
    use super::Args;
    use clap::Parser;
    use std::path::PathBuf;

    #[test]
    fn fs_socket_dir_is_disabled_by_default() {
        let args = Args::parse_from(["kid-agentic-coding", "opencode", "acp"]);

        assert_eq!(args.fs_socket_dir, None);
    }

    #[test]
    fn fs_socket_dir_accepts_an_explicit_directory() {
        let args = Args::parse_from([
            "kid-agentic-coding",
            "--fs-socket-dir",
            "/run/user/1000/kid-agentic-coding",
            "opencode",
        ]);

        assert_eq!(
            args.fs_socket_dir,
            Some(PathBuf::from("/run/user/1000/kid-agentic-coding"))
        );
    }
}
