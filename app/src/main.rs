mod log_buffer;
mod ui;

use clap::{Parser, Subcommand};
use clap_verbosity_flag::{InfoLevel, Verbosity};
use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use kid_agentic_coding::PromptRunner;
use log_buffer::LogBuffer;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::time::ChronoLocal;

use std::ffi::OsString;
use std::process::{self, Command as ProcessCommand};

#[derive(Subcommand, Debug)]
enum Command {
    /// Run interactive ACP terminal UI
    Tui {
        /// Skips confetti MCP tool registration even when the agent supports it.
        #[arg(long)]
        disable_confetti: bool,

        /// Runs the agent inside a Linux namespace sandbox exposing the
        /// current directory at /projects
        #[arg(long)]
        sandbox: bool,

        /// Agent command and arguments, or a single JSON configuration
        #[arg(required = true, num_args = 1..)]
        agent_args: Vec<String>,
    },

    /// Internal: enters the namespace sandbox and execs the given command.
    #[command(hide = true)]
    SandboxExec {
        /// Host path exposed inside the sandbox at /projects
        project_path: OsString,

        /// Command and arguments to exec inside the sandbox
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },

    /// Dispatches unknown commands to `kid-agentic-coding-*` binaries.
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Interactive terminal UI for agentic coding over ACP",
    long_about = None
)]
struct Args {
    #[command(subcommand)]
    command: Command,

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

    match args.command {
        Command::Tui {
            disable_confetti,
            sandbox,
            agent_args,
        } => {
            let agent_args = if sandbox {
                sandboxed_agent_args(&agent_args)?
            } else {
                agent_args
            };
            let agent = PromptRunner::parse_agent_args(&agent_args)?;
            ui::run(agent, log_buffer, disable_confetti).await?;
        }
        Command::SandboxExec {
            project_path,
            command,
        } => {
            kid_sandbox::enter_and_exec(project_path.as_ref(), &command)
                .wrap_err("sandbox setup failed")?;
        }
        Command::External(mut args) => {
            let command = args.remove(0);
            let binary = format!("kid-agentic-coding-{}", command.to_string_lossy());
            let status = ProcessCommand::new(&binary)
                .args(args)
                .status()
                .wrap_err(format!("failed to start binary {}", binary))?;
            process::exit(status.code().unwrap_or(1));
        }
    }
    Ok(())
}

/// Rewrites `agent_args` into a command line that re-invokes this binary's
/// `sandbox-exec` subcommand, which enters the namespace sandbox before
/// exec'ing the original agent command.
fn sandboxed_agent_args(agent_args: &[String]) -> Result<Vec<String>> {
    let current_exe =
        std::env::current_exe().wrap_err("failed to resolve current executable path")?;
    let project_path =
        std::env::current_dir().wrap_err("failed to resolve current project directory")?;

    let mut args = vec![
        current_exe.to_string_lossy().into_owned(),
        "sandbox-exec".to_owned(),
        project_path.to_string_lossy().into_owned(),
    ];
    args.extend(agent_args.iter().cloned());
    Ok(args)
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
