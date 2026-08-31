//! What the core asks of its host after handling a key.

/// Everything the core cannot do itself, handed back to the host.
///
/// The core owns the buffer and the cursor, so the host reads those directly; effects carry
/// only what needs the outside world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// A key the current mode has no meaning for, or a command that could not be carried
    /// out. The host decides whether to show it.
    Error(String),
    /// `:w`: write the buffer out. The core does no file IO, so the host reads the text back
    /// from the editor and writes it itself.
    SaveRequested {
        /// The path `:w path` named, `None` when the host is to use the one it opened.
        path: Option<String>,
    },
    /// `:q`: stop editing. The core keeps no notion of unsaved work, so the host decides what
    /// an unforced quit does with it.
    QuitRequested {
        /// `:q!` rather than `:q`: quit whatever the state of the buffer.
        force: bool,
    },
}
