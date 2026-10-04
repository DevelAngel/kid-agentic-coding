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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_keeps_its_name() {
        let server = McpServer::new("filesystem");

        assert_eq!(server.name(), "filesystem");
    }
}
