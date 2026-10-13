use anyhow::Context as _;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use tokio::sync::{mpsc, oneshot};

use super::{Defined, Programming, Workflow, WorkflowPromptProvider};
use crate::channel::{AgentEvent, PromptRequest, WorkflowEnds};

pub(crate) struct Runner {
    workflow: Workflow<Programming, Defined>,
    ends: WorkflowEnds,
}

impl Runner {
    pub(crate) fn new(workflow: Workflow<Programming, Defined>, ends: WorkflowEnds) -> Self {
        Self { workflow, ends }
    }

    pub(crate) async fn run(self, mut input: impl AsyncBufRead + Unpin) {
        let WorkflowEnds {
            prompts,
            sessions: _sessions,
            mut events,
            completion,
        } = self.ends;

        let outcome = async {
            let workflow = self.workflow.start();
            let Some(first) = Self::read_prompt(&mut input).await? else {
                return Ok(());
            };
            let first = workflow.prompt(first).into_inner();
            Self::submit(&prompts, &mut events, first).await?;

            let workflow = workflow.run();
            while let Some(next) = Self::read_prompt(&mut input).await? {
                let next = workflow.prompt(next).into_inner();
                Self::submit(&prompts, &mut events, next).await?;
            }
            Ok(())
        }
        .await;

        drop(prompts);
        let _ = completion.send(outcome).await;
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
        events: &mut mpsc::Receiver<AgentEvent>,
        text: String,
    ) -> anyhow::Result<()> {
        let (reply, mut reply_rx) = oneshot::channel();
        prompts
            .send(PromptRequest { text, reply })
            .await
            .map_err(|_| anyhow::anyhow!("ACP module stopped before accepting the prompt"))?;

        loop {
            tokio::select! {
                Some(event) = events.recv() => {
                    tracing::info!(?event.update, "agent message");
                }
                response = &mut reply_rx => {
                    let response = response.context("ACP module dropped the prompt")?;
                    response?;
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InitialWorkflow;
    use crate::channel::Channels;
    use agent_client_protocol::Error;
    use agent_client_protocol::schema::v1::{PromptResponse, StopReason};

    fn runner(workflow_ends: WorkflowEnds) -> Runner {
        Runner::new(Workflow::<Programming, Defined>::initial(), workflow_ends)
    }

    #[tokio::test]
    async fn sends_context_with_first_prompt_then_bare_prompts_until_input_ends() {
        let Channels {
            workflow,
            mut acp,
            mut completions,
        } = Channels::open();
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

        assert!(completions.recv().await.unwrap().is_ok());
        assert!(acp.prompts.recv().await.is_none());
        run.await.unwrap();
    }

    #[tokio::test]
    async fn ending_input_without_a_prompt_completes_without_a_request() {
        let Channels {
            workflow,
            mut acp,
            mut completions,
        } = Channels::open();
        let run = tokio::spawn(runner(workflow).run(&b""[..]));

        assert!(completions.recv().await.unwrap().is_ok());
        assert!(acp.prompts.recv().await.is_none());
        run.await.unwrap();
    }

    #[tokio::test]
    async fn failed_prompt_is_reported_as_failed_completion() {
        let Channels {
            workflow,
            mut acp,
            mut completions,
        } = Channels::open();
        let run = tokio::spawn(runner(workflow).run(&b"first\nsecond\n"[..]));

        let first = acp.prompts.recv().await.unwrap();
        first.reply.send(Err(Error::internal_error())).unwrap();

        assert!(completions.recv().await.unwrap().is_err());
        run.await.unwrap();
    }

    #[tokio::test]
    async fn dropped_reply_is_reported_as_failed_completion() {
        let Channels {
            workflow,
            mut acp,
            mut completions,
        } = Channels::open();
        let run = tokio::spawn(runner(workflow).run(&b"first\n"[..]));

        drop(acp.prompts.recv().await.unwrap());

        assert!(completions.recv().await.unwrap().is_err());
        run.await.unwrap();
    }
}
