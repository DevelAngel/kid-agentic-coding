mod mcp;
mod session;

pub use mcp::McpServer;
pub use session::{Active, Closed, Deleted, Session};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_initializes_after_connecting() {
        let client = Client::<Disconnected>::new().connect().initialize();
        let _: Client<Initialized> = client;
    }
}
