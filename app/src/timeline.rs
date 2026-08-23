//! Append-only log of tool calls and agent thoughts, rendered as a
//! vertical timeline alongside the chat bubbles.

/// Progress of a tool call. Thoughts are always [`Status::Done`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pending,
    Running,
    Done,
    Failed,
}

/// What kind of event a [`TimelineEntry`] represents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    ToolCall { name: String },
    Thought,
}

/// A single entry in the timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineEntry {
    pub kind: EntryKind,
    pub status: Status,
    pub lines: Vec<String>,
}

/// Opaque handle to a [`TimelineEntry`], returned by [`TimelineLog::push_tool_call`]
/// and consumed by [`TimelineLog::update_tool_call_status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryId(usize);

/// Ordered log of tool calls and thoughts. The only way to add entries is
/// through [`TimelineLog::push_tool_call`] and [`TimelineLog::push_thought`],
/// so the stored [`EntryKind`] always matches how it was inserted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimelineLog {
    entries: Vec<TimelineEntry>,
}

impl TimelineLog {
    /// Creates an empty timeline log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a pending tool call entry and returns a handle for later
    /// status updates via [`update_tool_call_status`](Self::update_tool_call_status).
    pub fn push_tool_call(&mut self, name: impl Into<String>, lines: Vec<String>) -> EntryId {
        let id = EntryId(self.entries.len());
        self.entries.push(TimelineEntry {
            kind: EntryKind::ToolCall { name: name.into() },
            status: Status::Pending,
            lines,
        });
        id
    }

    /// Appends a thought entry with [`Status::Done`].
    pub fn push_thought(&mut self, lines: Vec<String>) {
        self.entries.push(TimelineEntry {
            kind: EntryKind::Thought,
            status: Status::Done,
            lines,
        });
    }

    /// Updates the status of the tool call entry identified by `id`.
    /// A no-op if `id` no longer refers to an entry.
    pub fn update_tool_call_status(&mut self, id: EntryId, status: Status) {
        if let Some(entry) = self.entries.get_mut(id.0) {
            entry.status = status;
        }
    }

    /// Number of entries in the log.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the log has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries in insertion order.
    pub fn entries(&self) -> &[TimelineEntry] {
        &self.entries
    }
}
