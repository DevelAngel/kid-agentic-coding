use std::marker::PhantomData;

use super::{Client, Initialized};
use crate::acp::mcp::McpServer;

pub struct Active;
pub struct Closed;
pub struct Deleted;

pub struct Session<S> {
    mcp_servers: Vec<McpServer>,
    state: PhantomData<S>,
}

impl Client<Initialized> {
    pub fn new_session(self, mcp_servers: Vec<McpServer>) -> Session<Active> {
        Session {
            mcp_servers,
            state: PhantomData,
        }
    }
}

impl Session<Active> {
    pub fn mcp_servers(&self) -> &[McpServer] {
        &self.mcp_servers
    }

    pub fn close(self) -> Session<Closed> {
        Session {
            mcp_servers: self.mcp_servers,
            state: PhantomData,
        }
    }

    pub fn delete(self) -> Session<Deleted> {
        Session {
            mcp_servers: self.mcp_servers,
            state: PhantomData,
        }
    }
}

impl Session<Closed> {
    pub fn delete(self) -> Session<Deleted> {
        Session {
            mcp_servers: self.mcp_servers,
            state: PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_owns_selected_mcp_servers() {
        let session = Client::<super::super::Disconnected>::new()
            .connect()
            .initialize()
            .new_session(vec![McpServer::new("filesystem"), McpServer::new("git")]);

        assert_eq!(
            session.mcp_servers(),
            &[McpServer::new("filesystem"), McpServer::new("git")]
        );
    }

    #[test]
    fn active_session_can_be_closed_or_deleted() {
        let closed = Client::<super::super::Disconnected>::new()
            .connect()
            .initialize()
            .new_session(Vec::new())
            .close();
        let deleted = Client::<super::super::Disconnected>::new()
            .connect()
            .initialize()
            .new_session(Vec::new())
            .delete();

        let _: Session<Closed> = closed;
        let _: Session<Deleted> = deleted;
    }

    #[test]
    fn closed_session_can_be_deleted() {
        let session = Client::<super::super::Disconnected>::new()
            .connect()
            .initialize()
            .new_session(Vec::new())
            .close()
            .delete();

        let _: Session<Deleted> = session;
    }
}
