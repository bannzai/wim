//! Modal editing core of wim.
//!
//! This crate must stay free of file IO, rendering and platform dependencies so that it
//! keeps compiling for `wasm32-unknown-unknown` as well as native targets.
//!
//! A host feeds [`KeyEvent`]s to an [`Editor`] and reads the buffer, the cursor and the
//! [`Effect`]s back; nothing else crosses the boundary.

pub mod buffer;
pub mod command;
pub mod editor;
pub mod effect;
pub mod grammar;
pub mod key;
pub mod mode;
pub mod motion;
pub mod position;
pub mod textobject;

pub use buffer::Buffer;
pub use command::{Command, InsertAnchor, Operator, OperatorTarget};
pub use editor::{Editor, ResolvedOperation};
pub use effect::Effect;
pub use grammar::Grammar;
pub use key::{KeyCode, KeyEvent, KeyParseError, format_keys, parse_keys};
pub use mode::Mode;
pub use motion::{Find, Motion, MotionContext, MotionKind, MotionOutcome, MotionTarget};
pub use position::Position;
pub use textobject::{TextObject, TextObjectKind, TextRange};

/// Version of this crate, exposed so that frontends can report the core they were built against.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_not_empty() {
        assert!(!version().is_empty());
    }

    #[test]
    fn a_key_string_drives_an_editor_through_the_re_exported_api() {
        let mut editor = Editor::new("bar");
        editor
            .handle_keys("ifoo <Esc>")
            .expect("key string should parse");
        assert_eq!(editor.text(), "foo bar");
        assert_eq!(editor.mode(), Mode::Normal);
        assert_eq!(editor.cursor(), Position::new(0, 3));
    }
}
