//! Append-only chat history: user/agent text and tool call clusters
//! (which may themselves interleave thoughts and tool calls), in the
//! order they occurred.

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

/// Outcome of an interactive session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionNoticeKind {
    Error,
    Stopped,
}

/// A session outcome shown in the chat history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionNotice {
    pub kind: SessionNoticeKind,
    pub text: String,
}

/// A single tool call within a [`ToolCluster`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallEntry {
    pub name: String,
    pub status: Status,
    pub parameters: Option<String>,
    pub result: Option<String>,
}

/// A single item within a [`ToolCluster`]: either a thought, or a tool
/// call going through [`Status::Pending`] → [`Status::Running`] →
/// [`Status::Done`]/[`Status::Failed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Thought(String),
    ToolCall(ToolCallEntry),
}

/// A run of consecutive thoughts and tool calls, rendered as one
/// collapsible row. Broken only by an [`Message::Agent`] or
/// [`Message::User`] message in between, so it never spans unrelated
/// turns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCluster {
    steps: Vec<Step>,
    expanded: bool,
}

impl ToolCluster {
    /// Thoughts and tool calls in this cluster, in insertion order.
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// Whether the cluster is showing its individual steps rather than
    /// just a summary line, regardless of [`Self::status`].
    pub fn expanded(&self) -> bool {
        self.expanded
    }

    /// Number of tool call steps in the cluster (thoughts excluded).
    pub fn tool_call_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|step| matches!(step, Step::ToolCall(_)))
            .count()
    }

    /// Aggregate status across the cluster's tool call steps:
    /// [`Status::Running`] if any is running, else [`Status::Failed`] if
    /// any failed, else [`Status::Done`] only if every tool call step is
    /// done (or there are none, i.e. a thoughts-only cluster), else
    /// [`Status::Pending`].
    pub fn status(&self) -> Status {
        let mut aggregate = Status::Done;
        for step in &self.steps {
            let Step::ToolCall(entry) = step else {
                continue;
            };
            match entry.status {
                Status::Running => return Status::Running,
                Status::Failed => aggregate = Status::Failed,
                Status::Pending if aggregate != Status::Failed => aggregate = Status::Pending,
                Status::Done | Status::Pending => {}
            }
        }
        aggregate
    }

    /// The steps rendered under the summary line: all of them when
    /// [`Self::expanded`], the last three while
    /// [`Status::Pending`]/[`Status::Running`] or `keep_live` is set,
    /// otherwise none. `keep_live` defers the collapse of a just-settled
    /// cluster while it is still the newest message (see
    /// [`crate::bubble_layout::BubbleLayout`]).
    pub fn visible_steps(&self, keep_live: bool) -> &[Step] {
        if self.expanded {
            &self.steps
        } else if keep_live || matches!(self.status(), Status::Pending | Status::Running) {
            let start = self.steps.len().saturating_sub(3);
            &self.steps[start..]
        } else {
            &[]
        }
    }

    /// Number of rows this cluster renders as: 1 for a plain summary, or
    /// summary + a truncation marker (if any steps are hidden) + one row
    /// per step returned by [`Self::visible_steps`].
    pub fn visible_row_count(&self, keep_live: bool) -> usize {
        let shown = self.visible_steps(keep_live);
        if shown.is_empty() {
            1
        } else {
            1 + usize::from(self.steps.len() > shown.len()) + shown.len()
        }
    }
}

/// A single entry in the chat history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    User(UserMessage),
    Agent(AgentMessage),
    ToolCluster(ToolCluster),
    SessionNotice(SessionNotice),
}

/// Opaque handle to a [`Step`], returned by [`ChatLog::push_tool_call`]
/// and consumed by [`ChatLog::update_tool_call_status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryId {
    message_index: usize,
    step_index: usize,
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

    /// Appends a user message. Ends whatever tool cluster is currently
    /// open, so the next thought or tool call starts a fresh one.
    pub fn push_user(&mut self, text: impl Into<String>) {
        self.messages
            .push(Message::User(UserMessage { text: text.into() }));
    }

    /// Appends a session outcome and ends the current tool cluster.
    pub fn push_session_notice(&mut self, kind: SessionNoticeKind, text: impl Into<String>) {
        self.messages.push(Message::SessionNotice(SessionNotice {
            kind,
            text: text.into(),
        }));
    }

    /// Appends an agent message. Ends whatever tool cluster is currently
    /// open, so the next thought or tool call starts a fresh one.
    pub fn push_agent(&mut self, text: impl Into<String>) {
        self.messages
            .push(Message::Agent(AgentMessage { text: text.into() }));
    }

    /// Appends a thought, joining the open cluster at the end of the log
    /// if there is one, or starting a new one otherwise.
    pub fn push_thought(&mut self, text: impl Into<String>) {
        self.push_step(Step::Thought(text.into()));
    }

    /// Appends a pending tool call, joining the open cluster at the end of
    /// the log if there is one, or starting a new one otherwise. Returns a
    /// handle for later status updates via
    /// [`update_tool_call_status`](Self::update_tool_call_status).
    pub fn push_tool_call(&mut self, name: impl Into<String>) -> EntryId {
        self.push_tool_call_with_parameters(name, None)
    }

    /// Appends a pending tool call with optional audit parameters.
    pub fn push_tool_call_with_parameters(
        &mut self,
        name: impl Into<String>,
        parameters: Option<String>,
    ) -> EntryId {
        self.push_step(Step::ToolCall(ToolCallEntry {
            name: name.into(),
            status: Status::Pending,
            parameters,
            result: None,
        }))
    }

    fn push_step(&mut self, step: Step) -> EntryId {
        let message_index = self.messages.len().saturating_sub(1);
        if let Some(Message::ToolCluster(cluster)) = self.messages.last_mut() {
            cluster.steps.push(step);
            return EntryId {
                message_index,
                step_index: cluster.steps.len() - 1,
            };
        }

        self.messages.push(Message::ToolCluster(ToolCluster {
            steps: vec![step],
            expanded: false,
        }));
        EntryId {
            message_index: self.messages.len() - 1,
            step_index: 0,
        }
    }

    /// Updates the parameters of the tool call step identified by `id`.
    pub fn update_tool_call_parameters(&mut self, id: EntryId, parameters: String) {
        if let Some(Message::ToolCluster(cluster)) = self.messages.get_mut(id.message_index)
            && let Some(Step::ToolCall(entry)) = cluster.steps.get_mut(id.step_index)
        {
            entry.parameters = Some(parameters);
        }
    }

    /// Updates the result of the tool call step identified by `id`.
    /// A no-op if `id` no longer refers to a tool call step.
    pub fn update_tool_call_result(&mut self, id: EntryId, result: String) {
        if let Some(Message::ToolCluster(cluster)) = self.messages.get_mut(id.message_index)
            && let Some(Step::ToolCall(entry)) = cluster.steps.get_mut(id.step_index)
        {
            entry.result = Some(result);
        }
    }

    /// Returns the tool call entry identified by `id`.
    pub fn tool_call(&self, id: EntryId) -> Option<&ToolCallEntry> {
        match self.messages.get(id.message_index) {
            Some(Message::ToolCluster(cluster)) => match cluster.steps.get(id.step_index) {
                Some(Step::ToolCall(entry)) => Some(entry),
                _ => None,
            },
            _ => None,
        }
    }

    /// Updates the status of the tool call step identified by `id`.
    /// A no-op if `id` no longer refers to a tool call step.
    pub fn update_tool_call_status(&mut self, id: EntryId, status: Status) {
        if let Some(Message::ToolCluster(cluster)) = self.messages.get_mut(id.message_index)
            && let Some(Step::ToolCall(entry)) = cluster.steps.get_mut(id.step_index)
        {
            entry.status = status;
        }
    }

    /// Returns the tool call handles in a cluster in insertion order.
    pub fn tool_call_ids(&self, message_index: usize) -> Vec<EntryId> {
        let Some(Message::ToolCluster(cluster)) = self.messages.get(message_index) else {
            return Vec::new();
        };
        cluster
            .steps
            .iter()
            .enumerate()
            .filter_map(|(step_index, step)| {
                matches!(step, Step::ToolCall(_)).then_some(EntryId {
                    message_index,
                    step_index,
                })
            })
            .collect()
    }

    /// Returns the step index for a tool call handle in a cluster.
    pub fn tool_call_step_index(&self, message_index: usize, id: EntryId) -> Option<usize> {
        (id.message_index == message_index
            && matches!(
                self.messages.get(message_index),
                Some(Message::ToolCluster(_))
            ))
        .then_some(id.step_index)
    }

    /// Toggles whether the tool cluster at `message_index` shows its
    /// individual steps. A no-op if there is no cluster at that index.
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
