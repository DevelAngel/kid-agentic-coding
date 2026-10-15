use std::marker::PhantomData;

use agent_client_protocol::Error;
use agent_client_protocol::schema::v1::{PromptResponse, SessionId};
use tokio::sync::mpsc;

use super::{Client, Command, Connected, Router};
use crate::ToolEvent;
use crate::acp::mcp::McpServer;

pub struct Active;
pub struct Closed;
pub struct Deleted;

pub struct Session<S> {
    id: SessionId,
    mcp_servers: Vec<McpServer>,
    commands: mpsc::Sender<Command>,
    router: Router,
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
            router: self.state.router.clone(),
            state: PhantomData,
        })
    }

    pub fn route(&self, session: &Session<Active>, sink: mpsc::Sender<ToolEvent>) {
        self.state.router.register(session.id().clone(), sink);
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
            router: self.router,
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
        self.router.remove(&self.id);
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
