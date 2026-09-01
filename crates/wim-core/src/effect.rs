//! What the core asks of its host after handling a key.

use crate::mode::Mode;

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
    /// A `:` line whose name is no command of the core's, with whatever was typed after it.
    ///
    /// The core does not know every command there is: a plugin publishes commands of its own and
    /// the host is the one holding them (`wit/plugin.wit`), so a name the core has none of is
    /// handed over rather than refused. The host runs the command it finds under `name`, and
    /// refuses a name none of its plugins published itself, in the words the core used to refuse
    /// it in: `not an editor command: {name}`.
    ///
    /// Going through an effect rather than through the host reading the command line is what
    /// keeps a plugin's command working where the host cannot see the keys: `@a` replaying a
    /// recorded `:upcase<CR>` runs it, and so does one inside `:norm`.
    UnknownExCommand {
        /// The name as it was typed, without its `:`.
        name: String,
        /// The rest of the line, less the one blank that separates it from the name. It crosses
        /// as it was typed: what an argument is belongs to the command the host resolves.
        args: String,
    },
    /// Something happened that a host's autocmds may be bound to. The core only reports it;
    /// which handler runs is the host's (`documents/CONFIG.md`).
    Event(Event),
}

/// Something that happened to the editor, in the vocabulary autocmds are written against.
///
/// The names follow Vim's autocmd events, spelled the way the plugin ABI spells an event name
/// (`wit/plugin.wit`): `buffer-write` is Vim's `BufWrite`, which is its own name for
/// `BufWritePre` — the point before a write, not after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The buffer is about to be written. It is reported in front of the
    /// [`Effect::SaveRequested`] it belongs to, so a handler that edits the buffer is done
    /// before the host reads the text it writes.
    BufferWrite,
    /// The buffer was written. The core never reports this one: whether a write happened is
    /// the host's to know, and it raises the event itself once it has written.
    BufferWritePost,
    /// A change altered the text. One completed change is one event, so an Insert session is
    /// reported when `<Esc>` closes it rather than on every character typed, which is where
    /// Vim's `TextChanged` fires as well.
    TextChanged,
    /// A key left the editor in another mode.
    ModeChanged {
        /// The mode the key was read in.
        from: Mode,
        /// The mode it left behind.
        to: Mode,
    },
}

impl Event {
    /// Every name there is, which is what a host checks the events a config binds handlers to.
    /// An event bound to a name that is not one of these would never fire, so a host reports it
    /// rather than leaving it to be found by a handler that never runs.
    pub fn names() -> [&'static str; 4] {
        [
            Self::BufferWrite.name(),
            Self::BufferWritePost.name(),
            Self::TextChanged.name(),
            Self::ModeChanged {
                from: Mode::Normal,
                to: Mode::Normal,
            }
            .name(),
        ]
    }

    /// The name the event is written under in a config and delivered to a plugin under.
    pub fn name(&self) -> &'static str {
        match self {
            Self::BufferWrite => "buffer-write",
            Self::BufferWritePost => "buffer-write-post",
            Self::TextChanged => "text-changed",
            Self::ModeChanged { .. } => "mode-changed",
        }
    }

    /// What the event carries, as the JSON object the ABI passes to a plugin, and the empty
    /// string for an event that carries nothing (`wit/plugin.wit`).
    ///
    /// The JSON is written out here rather than built with a serializer because the core takes
    /// no dependencies it does not need, and the only values that go into it are mode labels —
    /// a fixed set of ASCII words with nothing in them a JSON string has to escape.
    pub fn payload(&self) -> String {
        match self {
            Self::ModeChanged { from, to } => {
                format!(r#"{{"from":"{}","to":"{}"}}"#, from.label(), to.label())
            }
            _ => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_carries_the_name_a_config_binds_a_handler_to() {
        assert_eq!(Event::BufferWrite.name(), "buffer-write");
        assert_eq!(Event::BufferWritePost.name(), "buffer-write-post");
        assert_eq!(Event::TextChanged.name(), "text-changed");
        assert_eq!(
            Event::ModeChanged {
                from: Mode::Normal,
                to: Mode::Insert
            }
            .name(),
            "mode-changed"
        );
    }

    #[test]
    fn every_event_is_one_a_config_can_name() {
        let names = Event::names();
        for event in [
            Event::BufferWrite,
            Event::BufferWritePost,
            Event::TextChanged,
            Event::ModeChanged {
                from: Mode::Normal,
                to: Mode::Insert,
            },
        ] {
            assert!(names.contains(&event.name()), "{event:?}");
        }
    }

    #[test]
    fn only_a_mode_change_carries_a_payload() {
        assert_eq!(
            Event::ModeChanged {
                from: Mode::Normal,
                to: Mode::Insert
            }
            .payload(),
            r#"{"from":"NORMAL","to":"INSERT"}"#
        );
        assert_eq!(Event::TextChanged.payload(), "");
        assert_eq!(Event::BufferWrite.payload(), "");
    }
}
