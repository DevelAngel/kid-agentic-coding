use std::marker::PhantomData;

use agent_client_protocol::Error;
use agent_client_protocol::schema::v1::{PromptResponse, SessionId};
use tokio::sync::mpsc;

use super::{Client, Command, Connected};
use crate::acp::mcp::McpServer;

pub struct Active;
pub struct Closed;
pub struct Deleted;

pub struct Session<S> {
    id: SessionId,
    mcp_servers: Vec<McpServer>,
    commands: mpsc::Sender<Command>,
    state: PhantomData<S>,
}

impl Client<Connected> {
    pub async fn new_session(&self, mcp_servers: Vec<McpServer>) -> Result<Session<Active>, Error> {
        // The request consumes the list; the session keeps its own copy.
        let requested = mcp_servers.clone();
        let id = Command::request(&self.state.commands, |reply| Command::NewSession {
            mcp_servers: requested,
            reply,
        })
        .await?;

        Ok(Session {
            id,
            mcp_servers,
            commands: self.state.commands.clone(),
            state: PhantomData,
        })
    }
}

impl<S> Session<S> {
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    pub fn mcp_servers(&self) -> &[McpServer] {
        &self.mcp_servers
    }

    fn into_state<T>(self) -> Session<T> {
        Session {
            id: self.id,
            mcp_servers: self.mcp_servers,
            commands: self.commands,
            state: PhantomData,
        }
    }
}

impl Session<Active> {
    pub async fn prompt(&self, text: String) -> Result<PromptResponse, Error> {
        Command::request(&self.commands, |reply| Command::Prompt {
            session_id: self.id.clone(),
            text,
            reply,
        })
        .await
    }

    pub async fn close(self) -> Result<Session<Closed>, Error> {
        Command::request(&self.commands, |reply| Command::Close {
            session_id: self.id.clone(),
            reply,
        })
        .await?;
        Ok(self.into_state())
    }
}

impl Session<Closed> {
    pub async fn delete(self) -> Result<Session<Deleted>, Error> {
        Command::request(&self.commands, |reply| Command::Delete {
            session_id: self.id.clone(),
            reply,
        })
        .await?;
        Ok(self.into_state())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::test_agent::{Recorded, TestAgent};
    use agent_client_protocol::schema::v1::McpServerStdio;

    #[tokio::test]
    async fn new_session_sends_the_mcp_servers_and_owns_the_returned_id() {
        let agent = TestAgent::default();
        let client = agent.connect().await;
        let filesystem = McpServer::Stdio(McpServerStdio::new("filesystem", "/bin/true"));

        let session = client.new_session(vec![filesystem.clone()]).await.unwrap();

        assert_eq!(
            agent.recorded(),
            vec![Recorded::NewSession(vec![filesystem.clone()])]
        );
        assert_eq!(session.id(), &SessionId::new("test-session-0"));
        assert_eq!(session.mcp_servers(), &[filesystem]);
    }

    #[tokio::test]
    async fn prompts_are_addressed_to_the_session_they_are_sent_on() {
        let agent = TestAgent::default();
        let client = agent.connect().await;
        let first = client.new_session(Vec::new()).await.unwrap();
        let second = client.new_session(Vec::new()).await.unwrap();

        first.prompt("one".to_owned()).await.unwrap();
        second.prompt("two".to_owned()).await.unwrap();
        first.prompt("three".to_owned()).await.unwrap();

        let prompts: Vec<_> = agent
            .recorded()
            .into_iter()
            .filter_map(|entry| match entry {
                Recorded::Prompt(id, text) => Some((id, text)),
                _ => None,
            })
            .collect();
        assert_ne!(first.id(), second.id());
        assert_eq!(
            prompts,
            vec![
                (first.id().clone(), "one".to_owned()),
                (second.id().clone(), "two".to_owned()),
                (first.id().clone(), "three".to_owned()),
            ]
        );
    }

    #[tokio::test]
    async fn closing_a_session_sends_session_close() {
        let agent = TestAgent::default();
        let client = agent.connect().await;
        let session = client.new_session(Vec::new()).await.unwrap();
        let id = session.id().clone();

        let closed = session.close().await.unwrap();

        let _: Session<Closed> = closed;
        assert_eq!(
            agent.recorded(),
            vec![Recorded::NewSession(Vec::new()), Recorded::Close(id)]
        );
    }

    #[tokio::test]
    async fn deleting_a_closed_session_sends_session_delete() {
        let agent = TestAgent::deleting();
        let client = agent.connect().await;
        let session = client.new_session(Vec::new()).await.unwrap();
        let id = session.id().clone();

        let deleted = session.close().await.unwrap().delete().await.unwrap();

        let _: Session<Deleted> = deleted;
        assert_eq!(
            agent.recorded(),
            vec![
                Recorded::NewSession(Vec::new()),
                Recorded::Close(id.clone()),
                Recorded::Delete(id),
            ]
        );
    }

    #[tokio::test]
    async fn deleting_without_the_capability_is_an_error_and_sends_nothing() {
        let agent = TestAgent::default();
        let client = agent.connect().await;
        let session = client.new_session(Vec::new()).await.unwrap();
        let id = session.id().clone();
        let closed = session.close().await.unwrap();

        let result = closed.delete().await;

        assert!(result.is_err());
        assert_eq!(
            agent.recorded(),
            vec![Recorded::NewSession(Vec::new()), Recorded::Close(id)]
        );
    }
}
