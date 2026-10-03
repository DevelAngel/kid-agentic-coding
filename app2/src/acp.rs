use std::marker::PhantomData;
pub struct Disconnected;
pub struct Connected;
pub struct Initialized;
pub struct Client<S> {
    state: PhantomData<S>,
}
impl Client<Disconnected> {
    pub fn new() -> Self {
        Self { state: PhantomData }
    }
    pub fn connect(self) -> Client<Connected> {
        Client { state: PhantomData }
    }
}
impl Default for Client<Disconnected> {
    fn default() -> Self {
        Self::new()
    }
}
impl Client<Connected> {
    pub fn initialize(self) -> Client<Initialized> {
        Client { state: PhantomData }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpServer {
    name: String,
}
impl McpServer {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
    pub fn name(&self) -> &str {
        &self.name
    }
}
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
    fn client_initializes_after_connecting() {
        let client = Client::<Disconnected>::new().connect().initialize();
        let _: Client<Initialized> = client;
    }

    #[test]
    fn session_owns_selected_mcp_servers() {
        let session = Client::<Disconnected>::new()
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
        let closed = Client::<Disconnected>::new()
            .connect()
            .initialize()
            .new_session(Vec::new())
            .close();
        let deleted = Client::<Disconnected>::new()
            .connect()
            .initialize()
            .new_session(Vec::new())
            .delete();

        let _: Session<Closed> = closed;
        let _: Session<Deleted> = deleted;
    }

    #[test]
    fn closed_session_can_be_deleted() {
        let session = Client::<Disconnected>::new()
            .connect()
            .initialize()
            .new_session(Vec::new())
            .close()
            .delete();

        let _: Session<Deleted> = session;
    }
}
