//! In-memory log sink for the TUI. Writing log lines to stderr would
//! corrupt the terminal display, since ratatui's alternate screen only
//! covers stdout. Log lines land here instead, for display in a popup.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard};
use tracing_subscriber::fmt::MakeWriter;

/// Number of log lines kept; older lines are dropped once exceeded.
const CAPACITY: usize = 500;

/// A bounded, shared ring buffer of log lines. Cheap to clone; clones
/// share the same underlying buffer.
#[derive(Clone, Default)]
pub struct LogBuffer {
    lines: Arc<Mutex<VecDeque<String>>>,
}

impl LogBuffer {
    /// A snapshot of the currently buffered log lines, oldest first.
    pub fn lines(&self) -> Vec<String> {
        let lines = self.lock();
        lines.iter().cloned().collect()
    }

    fn lock(&self) -> MutexGuard<'_, VecDeque<String>> {
        self.lines
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Writer handed to `tracing_subscriber::fmt`; each `write` call is one
/// formatted log line, split and appended to the shared buffer.
pub struct LogBufferWriter(LogBuffer);

impl io::Write for LogBufferWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        let mut lines = self.0.lock();
        for line in text.lines() {
            if lines.len() >= CAPACITY {
                lines.pop_front();
            }
            lines.push_back(line.to_owned());
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogBuffer {
    type Writer = LogBufferWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogBufferWriter(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::LogBuffer;
    use std::io::Write;
    use tracing_subscriber::fmt::MakeWriter;

    #[test]
    fn write_appends_a_line() {
        let buffer = LogBuffer::default();
        let mut writer = MakeWriter::make_writer(&buffer);
        writer.write_all(b"hello\n").expect("write succeeds");

        assert_eq!(buffer.lines(), vec!["hello".to_owned()]);
    }

    #[test]
    fn write_splits_multiple_lines_in_one_call() {
        let buffer = LogBuffer::default();
        let mut writer = MakeWriter::make_writer(&buffer);
        writer
            .write_all(b"first\nsecond\n")
            .expect("write succeeds");

        assert_eq!(
            buffer.lines(),
            vec!["first".to_owned(), "second".to_owned()]
        );
    }

    #[test]
    fn oldest_line_is_dropped_once_capacity_is_exceeded() {
        let buffer = LogBuffer::default();
        let mut writer = MakeWriter::make_writer(&buffer);
        for i in 0..super::CAPACITY + 1 {
            writer
                .write_all(format!("line {i}\n").as_bytes())
                .expect("write succeeds");
        }

        let lines = buffer.lines();
        assert_eq!(lines.len(), super::CAPACITY);
        assert_eq!(lines.first(), Some(&"line 1".to_owned()));
        assert_eq!(lines.last(), Some(&format!("line {}", super::CAPACITY)));
    }

    #[test]
    fn clones_share_the_same_buffer() {
        let buffer = LogBuffer::default();
        let clone = buffer.clone();
        let mut writer = MakeWriter::make_writer(&clone);
        writer.write_all(b"shared\n").expect("write succeeds");

        assert_eq!(buffer.lines(), vec!["shared".to_owned()]);
    }
}
