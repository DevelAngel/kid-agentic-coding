use anyhow::Context as _;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use tokio::sync::{mpsc, oneshot};

use super::{Defined, Running, Started, Workflow, WorkflowPromptProvider};
use crate::channel::{PromptRequest, WorkflowEnds};
use crate::{ToolEvent, ToolResult};

pub(crate) struct Runner<K> {
    workflow: Workflow<K, Defined>,
    ends: WorkflowEnds,
}

impl<K> Runner<K>
where
    Workflow<K, Started>: WorkflowPromptProvider,
    Workflow<K, Running>: WorkflowPromptProvider,
{
    pub(crate) fn new(workflow: Workflow<K, Defined>, ends: WorkflowEnds) -> Self {
        Self { workflow, ends }
    }

    pub(crate) async fn run(self, mut input: impl AsyncBufRead + Unpin) -> anyhow::Result<()> {
        let WorkflowEnds {
            prompts,
            mut events,
        } = self.ends;

        let workflow = self.workflow.start();
        let Some(first) = Self::read_prompt(&mut input).await? else {
            return Ok(());
        };
        let first = workflow.prompt(first).into_inner();
        let running = Outcome::Running(workflow.run());
        let mut outcome = Self::submit(&prompts, &mut events, running, first).await?;

        while let Outcome::Running(workflow) = outcome {
            let Some(next) = Self::read_prompt(&mut input).await? else {
                return Ok(());
            };
            let next = workflow.prompt(next).into_inner();
            let running = Outcome::Running(workflow);
            outcome = Self::submit(&prompts, &mut events, running, next).await?;
        }
        Ok(())
    }

    async fn read_prompt(
        input: &mut (impl AsyncBufRead + Unpin),
    ) -> anyhow::Result<Option<String>> {
        println!("Enter a prompt:");
        let mut line = String::new();
        if input.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        let line = line.trim().to_owned();
        Ok(Some(line))
    }

    async fn submit(
        prompts: &mpsc::Sender<PromptRequest>,
        events: &mut mpsc::Receiver<ToolEvent>,
        mut outcome: Outcome<K>,
        text: String,
    ) -> anyhow::Result<Outcome<K>> {
        let (reply, mut reply_rx) = oneshot::channel();
        prompts
            .send(PromptRequest { text, reply })
            .await
            .map_err(|_| anyhow::anyhow!("ACP module stopped before accepting the prompt"))?;

        loop {
            // Events are delivered before the reply, so they must be observed first.
            tokio::select! {
                biased;
                Some(event) = events.recv() => {
                    tracing::info!(?event, "tool event");
                    if let ToolEvent::Result(result) = event {
                        outcome = outcome.observe(&result);
                    }
                }
                response = &mut reply_rx => {
                    let response = response.context("ACP module dropped the prompt")?;
                    response?;
                    return Ok(outcome);
                }
            }
        }
    }
}

enum Outcome<K> {
    Running(Workflow<K, Running>),
    Completed,
}

impl<K> Outcome<K> {
    fn observe(self, result: &ToolResult) -> Self {
        match self {
            Self::Running(workflow) => match workflow.try_complete(result) {
                Ok(completed) => {
                    tracing::info!(reason = ?completed.completion_reason(), "workflow completed");
                    Self::Completed
                }
                Err(running) => Self::Running(running),
            },
            Self::Completed => Self::Completed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InitialWorkflow;
    use crate::channel::Channels;
    use crate::workflow::WorkflowPrompt;
    use crate::{Programming, Tool, WorkflowDefinition};
    use agent_client_protocol::Error;
    use agent_client_protocol::schema::v1::{PromptResponse, StopReason};

    fn runner(workflow_ends: WorkflowEnds) -> Runner<Programming> {
        Runner::new(Workflow::<Programming, Defined>::initial(), workflow_ends)
    }

    #[tokio::test]
    async fn sends_context_with_first_prompt_then_bare_prompts_until_input_ends() {
        let Channels { workflow, mut acp } = Channels::open();
        let run = tokio::spawn(runner(workflow).run(&b"first\nsecond\n"[..]));

        let first = acp.prompts.recv().await.unwrap();
        assert_eq!(
            first.text,
            "Handle programming tasks within the Programming workflow.\n\nfirst"
        );
        first
            .reply
            .send(Ok(PromptResponse::new(StopReason::EndTurn)))
            .unwrap();

        let second = acp.prompts.recv().await.unwrap();
        assert_eq!(second.text, "second");
        second
            .reply
            .send(Ok(PromptResponse::new(StopReason::EndTurn)))
            .unwrap();

        assert!(run.await.unwrap().is_ok());
        assert!(acp.prompts.recv().await.is_none());
    }

    #[tokio::test]
    async fn ending_input_without_a_prompt_completes_without_a_request() {
        let Channels { workflow, mut acp } = Channels::open();
        let run = tokio::spawn(runner(workflow).run(&b""[..]));

        assert!(run.await.unwrap().is_ok());
        assert!(acp.prompts.recv().await.is_none());
    }

    #[tokio::test]
    async fn failed_prompt_is_reported_as_failed_completion() {
        let Channels { workflow, mut acp } = Channels::open();
        let run = tokio::spawn(runner(workflow).run(&b"first\nsecond\n"[..]));

        let first = acp.prompts.recv().await.unwrap();
        first.reply.send(Err(Error::internal_error())).unwrap();

        assert!(run.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn dropped_reply_is_reported_as_failed_completion() {
        let Channels { workflow, mut acp } = Channels::open();
        let run = tokio::spawn(runner(workflow).run(&b"first\n"[..]));

        drop(acp.prompts.recv().await.unwrap());

        assert!(run.await.unwrap().is_err());
    }

    fn commit_result(success: bool) -> ToolEvent {
        let commit = Tool::new("git_commit");
        ToolEvent::Result(if success {
            ToolResult::success(commit)
        } else {
            ToolResult::failure(commit)
        })
    }

    fn completing_runner(workflow_ends: WorkflowEnds) -> Runner<Programming> {
        let definition =
            WorkflowDefinition::default().completes_on_successful_tool(Tool::new("git_commit"));
        Runner::new(
            Workflow::<Programming, Defined>::from_definition(definition),
            workflow_ends,
        )
    }

    async fn assert_second_prompt_follows(event: ToolEvent) {
        let Channels { workflow, mut acp } = Channels::open();
        let run = tokio::spawn(completing_runner(workflow).run(&b"first\nsecond\n"[..]));

        let first = acp.prompts.recv().await.unwrap();
        acp.events.send(event).await.unwrap();
        first
            .reply
            .send(Ok(PromptResponse::new(StopReason::EndTurn)))
            .unwrap();

        let second = acp.prompts.recv().await.unwrap();
        assert_eq!(second.text, "second");
        second
            .reply
            .send(Ok(PromptResponse::new(StopReason::EndTurn)))
            .unwrap();
        assert!(run.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn successful_completion_tool_result_ends_the_run_without_further_prompts() {
        let Channels { workflow, mut acp } = Channels::open();
        let run = tokio::spawn(completing_runner(workflow).run(&b"first\nsecond\n"[..]));

        let first = acp.prompts.recv().await.unwrap();
        acp.events.send(commit_result(true)).await.unwrap();
        first
            .reply
            .send(Ok(PromptResponse::new(StopReason::EndTurn)))
            .unwrap();

        assert!(run.await.unwrap().is_ok());
        assert!(acp.prompts.recv().await.is_none());
    }

    #[tokio::test]
    async fn failed_completion_tool_result_keeps_the_workflow_running() {
        assert_second_prompt_follows(commit_result(false)).await;
    }

    #[tokio::test]
    async fn successful_result_of_another_tool_keeps_the_workflow_running() {
        let status = Tool::new("git_status");
        assert_second_prompt_follows(ToolEvent::Result(ToolResult::success(status))).await;
    }

    #[derive(Default)]
    struct Scripted;

    impl WorkflowPromptProvider for Workflow<Scripted, Started> {
        fn prompt(&self, prompt: String) -> WorkflowPrompt {
            WorkflowPrompt::new(format!("scripted: {prompt}"))
        }
    }

    impl WorkflowPromptProvider for Workflow<Scripted, Running> {
        fn prompt(&self, prompt: String) -> WorkflowPrompt {
            WorkflowPrompt::new(prompt)
        }
    }

    #[tokio::test]
    async fn runs_any_workflow_kind_that_provides_prompts() {
        let Channels { workflow, mut acp } = Channels::open();
        let definition =
            WorkflowDefinition::default().completes_on_successful_tool(Tool::new("git_commit"));
        let runner = Runner::new(
            Workflow::<Scripted, Defined>::from_definition(definition),
            workflow,
        );
        let run = tokio::spawn(runner.run(&b"first\nsecond\n"[..]));

        let first = acp.prompts.recv().await.unwrap();
        assert_eq!(first.text, "scripted: first");
        acp.events.send(commit_result(true)).await.unwrap();
        first
            .reply
            .send(Ok(PromptResponse::new(StopReason::EndTurn)))
            .unwrap();

        assert!(run.await.unwrap().is_ok());
        assert!(acp.prompts.recv().await.is_none());
    }
}
