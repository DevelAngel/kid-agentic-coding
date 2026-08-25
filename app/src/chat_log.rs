//! Append-only chat history: user/agent text, thoughts, and tool call
//! clusters, interleaved in the order they occurred.

/// Progress of a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pending,
    Running,
    Done,
    Failed,
}

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

/// A single tool call within a [`ToolCluster`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallEntry {
    pub name: String,
    pub status: Status,
}

/// A run of consecutive tool calls, rendered as one collapsible row.
/// Broken by any [`Message::Thought`], [`Message::Agent`], or
/// [`Message::User`] in between, so it never spans unrelated turns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCluster {
    entries: Vec<ToolCallEntry>,
    expanded: bool,
}

impl ToolCluster {
    /// Tool calls in this cluster, in insertion order.
    pub fn entries(&self) -> &[ToolCallEntry] {
        &self.entries
    }

    /// Whether the cluster is showing its individual entries rather than
    /// just a summary line.
    pub fn expanded(&self) -> bool {
        self.expanded
    }
}

/// A single entry in the chat history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    User(UserMessage),
    Agent(AgentMessage),
    Thought(String),
    ToolCluster(ToolCluster),
}

/// Opaque handle to a [`ToolCallEntry`], returned by
/// [`ChatLog::push_tool_call`] and consumed by
/// [`ChatLog::update_tool_call_status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryId {
    message_index: usize,
    entry_index: usize,
}

/// Ordered chat history. The only way to add entries is through
/// [`ChatLog::push_user`], [`ChatLog::push_agent`], [`ChatLog::push_thought`],
/// and [`ChatLog::push_tool_call`], so the stored [`Message`] variant always
/// matches how it was inserted.
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

    /// Appends a thought. Ends whatever tool cluster is currently open, so
    /// the next tool call starts a fresh one.
    pub fn push_thought(&mut self, text: impl Into<String>) {
        self.messages.push(Message::Thought(text.into()));
    }

    /// Appends a pending tool call, joining the open cluster at the end of
    /// the log if there is one, or starting a new one otherwise. Returns a
    /// handle for later status updates via
    /// [`update_tool_call_status`](Self::update_tool_call_status).
    pub fn push_tool_call(&mut self, name: impl Into<String>) -> EntryId {
        let entry = ToolCallEntry {
            name: name.into(),
            status: Status::Pending,
        };

        let message_index = self.messages.len().saturating_sub(1);
        if let Some(Message::ToolCluster(cluster)) = self.messages.last_mut() {
            cluster.entries.push(entry);
            return EntryId {
                message_index,
                entry_index: cluster.entries.len() - 1,
            };
        }

        self.messages.push(Message::ToolCluster(ToolCluster {
            entries: vec![entry],
            expanded: false,
        }));
        EntryId {
            message_index: self.messages.len() - 1,
            entry_index: 0,
        }
    }

    /// Updates the status of the tool call entry identified by `id`.
    /// A no-op if `id` no longer refers to an entry.
    pub fn update_tool_call_status(&mut self, id: EntryId, status: Status) {
        if let Some(Message::ToolCluster(cluster)) = self.messages.get_mut(id.message_index)
            && let Some(entry) = cluster.entries.get_mut(id.entry_index)
        {
            entry.status = status;
        }
    }

    /// Toggles whether the tool cluster at `message_index` shows its
    /// individual entries. A no-op if there is no cluster at that index.
    pub fn toggle_cluster(&mut self, message_index: usize) {
        if let Some(Message::ToolCluster(cluster)) = self.messages.get_mut(message_index) {
            cluster.expanded = !cluster.expanded;
        }
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
