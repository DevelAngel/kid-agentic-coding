//! Append-only chat history distinguishing user and agent messages.

/// Text sent by the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMessage {
    pub text: String,
}

/// Text produced by the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentMessage {
    pub text: String,
}

/// A single entry in the chat history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    User(UserMessage),
    Agent(AgentMessage),
}

/// Ordered chat history. The only way to add entries is through
/// [`ChatLog::push_user`] and [`ChatLog::push_agent`], so the stored
/// [`Message`] variant always matches how it was inserted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatLog {
    messages: Vec<Message>,
}

impl ChatLog {
    /// Creates an empty chat log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a user message.
    pub fn push_user(&mut self, text: impl Into<String>) {
        self.messages
            .push(Message::User(UserMessage { text: text.into() }));
    }

    /// Appends an agent message.
    pub fn push_agent(&mut self, text: impl Into<String>) {
        self.messages
            .push(Message::Agent(AgentMessage { text: text.into() }));
    }

    /// Number of messages in the log.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Whether the log has no messages.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Messages in insertion order.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }
}
