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

impl Default for Client<Disconnected> {
    fn default() -> Self {
        Self { state: PhantomData }
    }
}

impl Client<Disconnected> {
    pub fn connect(self) -> Client<Connected> {
        Client { state: PhantomData }
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
        let client = Client::<Disconnected>::default().connect().initialize();
        let _: Client<Initialized> = client;
    }
}
