use super::{
    Completed, Defined, Running, Started, Workflow, WorkflowCompletionReason, WorkflowDefinition,
    WorkflowPrompt,
};
use crate::{Tool, ToolCall, ToolSet};

const COMMIT_FIX_ENTRY_TOOL: &str = "git_commit_with_fix";

#[derive(Default)]
#[cfg_attr(test, derive(Debug))]
pub struct Programming;

#[derive(Default)]
pub struct Planning;

#[cfg_attr(test, derive(Debug))]
pub struct CommitFix {
    caller: Workflow<Programming, Running>,
    request: CommitFixRequest,
}

impl CommitFix {
    fn definition() -> WorkflowDefinition {
        let commit = Tool::new("git_commit");
        let tools = ToolSet::default()
            .with(Tool::new("git_status"))
            .with(Tool::new("git_diff"))
            .with(commit.clone());

        WorkflowDefinition::default()
            .with_tools(tools)
            .completes_on_successful_tool(commit)
    }
}

#[cfg_attr(test, derive(Debug))]
struct CommitFixRequest {
    tldr: String,
    why: String,
    what: String,
}

impl CommitFixRequest {
    fn from_input(input: &serde_json::Value) -> Result<Self, EntryError> {
        let field = |name: &str| {
            input
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| {
                    EntryError::InvalidParameters(format!("`{name}` must be a non-empty string"))
                })
        };
        Ok(Self {
            tldr: field("tldr")?,
            why: field("why")?,
            what: field("what")?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EntryError {
    NotEntryTool,
    InvalidParameters(String),
}

pub trait InitialWorkflow {
    fn initial() -> Self;
}

pub trait WorkflowPromptProvider {
    fn prompt(&self, prompt: String) -> WorkflowPrompt;
}

impl WorkflowPromptProvider for Workflow<Programming, Started> {
    fn prompt(&self, prompt: String) -> WorkflowPrompt {
        WorkflowPrompt::new(format!(
            "Handle programming tasks within the Programming workflow.\n\n{prompt}"
        ))
    }
}

impl WorkflowPromptProvider for Workflow<Programming, Running> {
    fn prompt(&self, prompt: String) -> WorkflowPrompt {
        WorkflowPrompt::new(prompt)
    }
}

impl InitialWorkflow for Workflow<Programming, Defined> {
    fn initial() -> Self {
        Workflow::from_definition(WorkflowDefinition::default())
    }
}

impl InitialWorkflow for Workflow<Planning, Defined> {
    fn initial() -> Self {
        Workflow::from_definition(WorkflowDefinition::default())
    }
}

impl Workflow<Programming, Running> {
    pub fn enter(
        self,
        call: &ToolCall,
    ) -> Result<Workflow<CommitFix, Started>, (Self, EntryError)> {
        if call.tool().name() != COMMIT_FIX_ENTRY_TOOL {
            return Err((self, EntryError::NotEntryTool));
        }
        match CommitFixRequest::from_input(call.input()) {
            Ok(request) => {
                let kind = CommitFix {
                    caller: self,
                    request,
                };
                Ok(Workflow::from_parts(kind, CommitFix::definition()).start())
            }
            Err(error) => Err((self, error)),
        }
    }
}

impl Workflow<CommitFix, Started> {
    pub fn initial_prompt(&self) -> WorkflowPrompt {
        let CommitFixRequest { tldr, why, what } = &self.kind.request;
        WorkflowPrompt::new(format!(
            "Handle this task within the CommitFix workflow: commit the changes.\n\n\
             TL;DR: {tldr}\nWhy: {why}\nWhat: {what}"
        ))
    }
}

impl Workflow<CommitFix, Completed> {
    pub fn resume(self) -> (Workflow<Programming, Running>, WorkflowCompletionReason) {
        let (kind, reason) = self.into_parts();
        (kind.caller, reason)
    }
}
