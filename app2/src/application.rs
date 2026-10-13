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
        let Channels {
            workflow,
            acp,
            mut completions,
        } = Channels::open();

        tokio::spawn(Client::<ClientDisconnected>::new(agent).serve(acp));
        tokio::spawn(Runner::new(initial, workflow).run(input));

        let mut failure = None;
        for _ in 0..2 {
            match completions.recv().await {
                Some(Ok(())) => {}
                Some(Err(error)) => {
                    failure.get_or_insert(error);
                }
                None => {
                    failure.get_or_insert_with(|| {
                        anyhow::anyhow!("a module stopped without reporting completion")
                    });
                    break;
                }
            }
        }

        let Some(error) = failure else {
            return Ok(());
        };
        Err(error.context(format!("Agent {agent_command} stopped with an error")))
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
