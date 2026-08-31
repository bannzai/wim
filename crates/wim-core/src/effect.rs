//! What the core asks of its host after handling a key.

/// Everything the core cannot do itself, handed back to the host.
///
/// The core owns the buffer and the cursor, so the host reads those directly; effects carry
/// only what needs the outside world. File and quit requests arrive with the command line
/// issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// A key the current mode has no meaning for, or a command that could not be carried
    /// out. The host decides whether to show it.
    Error(String),
}
