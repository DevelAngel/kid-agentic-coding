use agent_client_protocol::Error;
use agent_client_protocol::schema::v1::PromptResponse;
use tokio::sync::{mpsc, oneshot};

use crate::ToolEvent;

pub(crate) struct PromptRequest {
    pub(crate) text: String,
    pub(crate) reply: oneshot::Sender<Result<PromptResponse, Error>>,
}

pub(crate) struct WorkflowEnds {
    pub(crate) prompts: mpsc::Sender<PromptRequest>,
    pub(crate) events: mpsc::Receiver<ToolEvent>,
}

pub(crate) struct AcpEnds {
    pub(crate) prompts: mpsc::Receiver<PromptRequest>,
    pub(crate) events: mpsc::Sender<ToolEvent>,
}

pub(crate) struct Channels {
    pub(crate) workflow: WorkflowEnds,
    pub(crate) acp: AcpEnds,
}

impl Channels {
    pub(crate) fn open() -> Self {
        let (prompts_tx, prompts_rx) = mpsc::channel(1);
        let (events_tx, events_rx) = mpsc::channel(32);

        Self {
            workflow: WorkflowEnds {
                prompts: prompts_tx,
                events: events_rx,
            },
            acp: AcpEnds {
                prompts: prompts_rx,
                events: events_tx,
            },
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
}
