use super::{Workflow, WorkflowCompletionReason};
use crate::{Tool, ToolResult};
use std::marker::PhantomData;

pub struct Defined;

#[cfg_attr(test, derive(Debug))]
pub struct Running;

#[cfg_attr(test, derive(Debug))]
pub struct Completed {
    reason: WorkflowCompletionReason,
}

impl<K> Workflow<K, Defined> {
    pub(super) fn from_definition(definition: super::WorkflowDefinition) -> Self {
        Self {
            definition,
            state: Defined,
            kind: PhantomData,
        }
    }

    pub fn start(self) -> Workflow<K, Running> {
        Workflow {
            definition: self.definition,
            state: Running,
            kind: PhantomData,
        }
    }
}

impl<K> Workflow<K, Running> {
    pub fn try_complete(
        self,
        result: &ToolResult,
    ) -> Result<Workflow<K, Completed>, Workflow<K, Running>> {
        let should_complete = result.is_success()
            && self.definition.completion_tool.as_ref().map(Tool::name)
                == Some(result.tool().name());

        if !should_complete {
            return Err(self);
        }

        Ok(Workflow {
            definition: self.definition,
            state: Completed {
                reason: WorkflowCompletionReason::ToolSucceeded {
                    tool: result.tool().clone(),
                },
            },
            kind: PhantomData,
        })
    }
}

impl<K> Workflow<K, Completed> {
    pub fn completion_reason(&self) -> &WorkflowCompletionReason {
        &self.state.reason
    }
}
