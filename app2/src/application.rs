use anyhow::Context as _;
use std::marker::PhantomData;

use agent_client_protocol::AcpAgent;
use tokio::io::AsyncBufRead;

use crate::channel::Channels;
use crate::workflow::Runner;
use crate::{Client, ClientDisconnected, Defined, InitialWorkflow, Programming, Workflow};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ApplicationEntry<W> {
    marker: PhantomData<W>,
}

impl<W> ApplicationEntry<W>
where
    Workflow<W, Defined>: InitialWorkflow,
{
    pub fn workflow(self) -> Workflow<W, Defined> {
        Workflow::<W, Defined>::initial()
    }
}

pub struct Application {
    agent_args: Vec<String>,
}

impl Application {
    pub fn new(agent_args: Vec<String>) -> Self {
        Self { agent_args }
    }

    pub async fn run(
        self,
        input: impl AsyncBufRead + Unpin + Send + 'static,
    ) -> anyhow::Result<()> {
        let agent_command = self.agent_args.join(" ");
        let agent = AcpAgent::from_args(self.agent_args)?;
        let initial = ApplicationEntry::<Programming>::default().workflow();
        let Channels { workflow, acp } = Channels::open();

        let acp_task = tokio::spawn(Client::<ClientDisconnected>::new(agent).serve(acp));
        let workflow_task = tokio::spawn(Runner::new(initial, workflow).run(input));
        let (workflow_result, acp_result) = tokio::join!(workflow_task, acp_task);

        workflow_result.context("workflow task panicked")??;
        acp_result
            .context("ACP task panicked")?
            .context(format!("Agent {agent_command} stopped with an error"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failing_agent_is_reported_with_its_command() {
        let error = Application::new(vec!["/bin/false".to_owned()])
            .run(&b""[..])
            .await
            .unwrap_err();

        assert!(format!("{error:#}").contains("/bin/false"));
    }
}
