use agent_client_protocol::Error;
use agent_client_protocol::schema::v1::{PromptResponse, SessionNotification};
use tokio::sync::{mpsc, oneshot};

pub(crate) type AgentEvent = SessionNotification;
pub(crate) type Completion = anyhow::Result<()>;

pub(crate) struct PromptRequest {
    pub(crate) text: String,
    pub(crate) reply: oneshot::Sender<Result<PromptResponse, Error>>,
}

pub(crate) struct SessionRequest;

pub(crate) struct WorkflowEnds {
    pub(crate) prompts: mpsc::Sender<PromptRequest>,
    pub(crate) sessions: mpsc::Sender<SessionRequest>,
    pub(crate) events: mpsc::Receiver<AgentEvent>,
    pub(crate) completion: mpsc::Sender<Completion>,
}

pub(crate) struct AcpEnds {
    pub(crate) prompts: mpsc::Receiver<PromptRequest>,
    pub(crate) sessions: mpsc::Receiver<SessionRequest>,
    pub(crate) events: mpsc::Sender<AgentEvent>,
    pub(crate) completion: mpsc::Sender<Completion>,
}

pub(crate) struct Channels {
    pub(crate) workflow: WorkflowEnds,
    pub(crate) acp: AcpEnds,
    pub(crate) completions: mpsc::Receiver<Completion>,
}

impl Channels {
    pub(crate) fn open() -> Self {
        let (prompts_tx, prompts_rx) = mpsc::channel(1);
        let (sessions_tx, sessions_rx) = mpsc::channel(1);
        let (events_tx, events_rx) = mpsc::channel(32);
        let (completion_tx, completions) = mpsc::channel(2);

        Self {
            workflow: WorkflowEnds {
                prompts: prompts_tx,
                sessions: sessions_tx,
                events: events_rx,
                completion: completion_tx.clone(),
            },
            acp: AcpEnds {
                prompts: prompts_rx,
                sessions: sessions_rx,
                events: events_tx,
                completion: completion_tx,
            },
            completions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn prompt_flows_from_workflow_to_acp_and_reply_returns() {
        let Channels {
            workflow, mut acp, ..
        } = Channels::open();
        let (reply, reply_rx) = oneshot::channel();

        workflow
            .prompts
            .send(PromptRequest {
                text: "hello".to_owned(),
                reply,
            })
            .await
            .unwrap();
        let request = acp.prompts.recv().await.unwrap();
        assert_eq!(request.text, "hello");
        request.reply.send(Err(Error::internal_error())).unwrap();

        assert!(reply_rx.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn both_modules_report_completion_to_the_receiver() {
        let Channels {
            workflow,
            acp,
            mut completions,
        } = Channels::open();

        workflow.completion.send(Ok(())).await.unwrap();
        acp.completion
            .send(Err(anyhow::anyhow!("boom")))
            .await
            .unwrap();

        assert!(completions.recv().await.unwrap().is_ok());
        assert!(completions.recv().await.unwrap().is_err());
    }
}
