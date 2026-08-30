//! Milestone-list MCP tool, registered with the agent when it advertises MCP
//! support and the `gh` CLI with the `milestone` extension is available.
//!
//! The Recipe `gh-milestone-list` in `recipes.toml` is the source of truth
//! for the invoked command and its flags; keep both in sync.

use crate::bridge::SessionEvent;
use agent_client_protocol::mcp_server::McpServer;
use agent_client_protocol::tool_fn;
use agent_client_protocol_rmcp::McpServerExt;

use agent_client_protocol::{Agent, Error, ErrorCode, RunWithConnectionTo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::process::Command;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

/// Stable name of the milestone-list tool, as advertised to the agent.
pub const MILESTONE_LIST_TOOL_NAME: &str = "milestone_list";

/// Empty input contract: the milestone-list tool takes no parameters.
#[derive(Debug, Deserialize, JsonSchema)]
struct MilestoneListParams {}

/// An open GitHub milestone as reported by `gh milestone list`.
///
/// `issue_count` is a proxy for how fleshed-out the milestone is: zero
/// issues usually means it is still a raw idea.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Milestone {
    pub number: u64,
    pub title: String,
    pub issue_count: u64,
}

/// Raw shape of one entry in `gh milestone list --json number,title,issues`.
#[derive(Debug, Deserialize)]
struct RawMilestone {
    number: u64,
    title: String,
    issues: RawIssueCount,
}

#[derive(Debug, Deserialize)]
struct RawIssueCount {
    #[serde(rename = "totalCount")]
    total_count: u64,
}

impl From<RawMilestone> for Milestone {
    fn from(raw: RawMilestone) -> Self {
        Self {
            number: raw.number,
            title: raw.title,
            issue_count: raw.issues.total_count,
        }
    }
}

/// Whether the `gh` CLI and its `milestone` extension are ready to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhAvailability {
    Available,
    /// `gh` itself, or the `milestone` extension, is missing.
    /// `reason` and `hint` are shown to the user as-is.
    Unavailable {
        reason: String,
        hint: String,
    },
}

/// Talks to the `gh` CLI on behalf of the milestone-list tool.
///
/// A trait so tests can substitute a fake instead of shelling out.
pub trait MilestoneCli {
    fn check_availability(&self) -> GhAvailability;
    fn list_open(&self) -> Result<Vec<Milestone>, String>;
}

/// Production [`MilestoneCli`] backed by the actual `gh` binary.
pub struct SystemGhCli;

impl MilestoneCli for SystemGhCli {
    fn check_availability(&self) -> GhAvailability {
        if Command::new("gh").arg("--version").output().is_err() {
            return GhAvailability::Unavailable {
                reason: "the gh CLI is not installed".to_owned(),
                hint: "install it from https://cli.github.com".to_owned(),
            };
        }

        match Command::new("gh").args(["extension", "list"]).output() {
            Ok(output) if String::from_utf8_lossy(&output.stdout).contains("milestone") => {
                GhAvailability::Available
            }
            _ => GhAvailability::Unavailable {
                reason: "the gh milestone extension is not installed".to_owned(),
                hint: "install it with: gh extension install valeriobelli/gh-milestone".to_owned(),
            },
        }
    }

    fn list_open(&self) -> Result<Vec<Milestone>, String> {
        let output = Command::new("gh")
            .args([
                "milestone",
                "list",
                "--state",
                "open",
                "--json",
                "number,title,issues",
            ])
            .output()
            .map_err(|err| format!("failed to run gh milestone list: {err}"))?;

        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned());
        }

        let raw: Vec<RawMilestone> = serde_json::from_slice(&output.stdout)
            .map_err(|err| format!("failed to parse gh milestone list output: {err}"))?;

        Ok(raw.into_iter().map(Milestone::from).collect())
    }
}

fn list_open_milestones(
    cli: &dyn MilestoneCli,
    event_tx: &UnboundedSender<SessionEvent>,
) -> Result<String, Error> {
    let milestones = cli
        .list_open()
        .map_err(|_| Error::from(ErrorCode::InternalError))?;
    let json =
        serde_json::to_string(&milestones).map_err(|_| Error::from(ErrorCode::InternalError))?;

    let _ = event_tx; // reserved for future progress events, mirrors confetti's event usage
    Ok(json)
}

/// Builds the MCP server exposing the milestone-list tool for attachment to a session.
pub fn milestone_mcp_server(
    cli: Box<dyn MilestoneCli + Send + Sync>,
    event_tx: UnboundedSender<SessionEvent>,
) -> McpServer<Agent, impl RunWithConnectionTo<Agent>> {
    McpServer::builder("milestone-tools")
        .tool_fn(
            MILESTONE_LIST_TOOL_NAME,
            "Lists all open milestones of the repository",
            async move |_params: MilestoneListParams, _cx| {
                tracing::debug!("milestone_list MCP tool invoked");
                list_open_milestones(cli.as_ref(), &event_tx)
            },
            tool_fn!(),
        )
        .build()
}

/// User's decision when the milestone-list tool is unavailable at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolUnavailableChoice {
    /// Abort the session instead of starting without the tool.
    Abort,
    /// Start the session without the milestone-list tool.
    Ignore,
}

/// Asks the user, via the session event channel, how to proceed when the
/// milestone-list tool's prerequisites are not met.
pub async fn ask_user_about_unavailable_tool(
    reason: String,
    hint: String,
    event_tx: UnboundedSender<SessionEvent>,
) -> ToolUnavailableChoice {
    let (reply_tx, reply_rx) = oneshot::channel();
    if event_tx
        .send(SessionEvent::MilestoneToolUnavailable {
            reason,
            hint,
            reply: reply_tx,
        })
        .is_err()
    {
        return ToolUnavailableChoice::Ignore;
    }

    reply_rx.await.unwrap_or(ToolUnavailableChoice::Ignore)
}

#[cfg(test)]
mod milestone_cli_tests {
    use super::*;

    struct FakeCli {
        availability: GhAvailability,
        milestones: Result<Vec<Milestone>, String>,
    }

    impl MilestoneCli for FakeCli {
        fn check_availability(&self) -> GhAvailability {
            self.availability.clone()
        }

        fn list_open(&self) -> Result<Vec<Milestone>, String> {
            self.milestones.clone()
        }
    }

    fn sample_milestone() -> Milestone {
        Milestone {
            number: 9,
            title: "Workflow 1: GitHub Milestones as Story Editor".to_owned(),
            issue_count: 4,
        }
    }

    #[test]
    fn list_open_milestones_returns_formatted_json_on_success() {
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let cli = FakeCli {
            availability: GhAvailability::Available,
            milestones: Ok(vec![sample_milestone()]),
        };

        let json = list_open_milestones(&cli, &event_tx).expect("cli reports success");

        let parsed: Vec<Milestone> = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed, vec![sample_milestone()]);
    }

    #[test]
    fn list_open_milestones_fails_when_cli_call_fails() {
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let cli = FakeCli {
            availability: GhAvailability::Available,
            milestones: Err("gh exploded".to_owned()),
        };

        assert!(list_open_milestones(&cli, &event_tx).is_err());
    }

    #[test]
    fn raw_milestone_maps_issue_total_count() {
        let raw = RawMilestone {
            number: 9,
            title: "Workflow 1".to_owned(),
            issues: RawIssueCount { total_count: 4 },
        };

        let milestone: Milestone = raw.into();

        assert_eq!(milestone.issue_count, 4);
    }
}

#[cfg(test)]
mod ask_user_about_unavailable_tool_tests {
    use super::*;

    #[tokio::test]
    async fn sends_event_and_returns_replied_choice() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

        let ask = tokio::spawn(ask_user_about_unavailable_tool(
            "gh not found".to_owned(),
            "install it".to_owned(),
            event_tx,
        ));

        match event_rx.recv().await.expect("event was sent") {
            SessionEvent::MilestoneToolUnavailable { reply, .. } => {
                let _ = reply.send(ToolUnavailableChoice::Abort);
            }
            other => panic!("unexpected event: {other:?}"),
        }

        assert_eq!(
            ask.await.expect("task completes"),
            ToolUnavailableChoice::Abort
        );
    }

    #[tokio::test]
    async fn defaults_to_ignore_when_event_channel_is_closed() {
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        drop(event_rx);

        let choice = ask_user_about_unavailable_tool(
            "gh not found".to_owned(),
            "install it".to_owned(),
            event_tx,
        )
        .await;

        assert_eq!(choice, ToolUnavailableChoice::Ignore);
    }
}
