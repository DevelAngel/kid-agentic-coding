/// The confetti notification. Not JSON: the message is the bare word on
/// its own line.
pub struct Confetti;

impl Confetti {
    pub fn to_line(&self) -> &'static str {
        "confetti"
    }

    /// Accepts only the exact message a sender writes, newline included.
    pub fn parse(message: &[u8]) -> Option<Self> {
        (message == b"confetti\n").then_some(Self)
    }
}
