//! What a finished sequence of Normal mode keys means.

use crate::key::KeyEvent;
use crate::motion::Motion;
use crate::textobject::TextObject;

/// An operator: a command that acts on a range of text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    /// `d`
    Delete,
    /// `c`
    Change,
    /// `y`
    Yank,
}

impl Operator {
    /// Reads the key that names an operator, `None` for every other key.
    pub fn from_key(key: char) -> Option<Self> {
        match key {
            'd' => Some(Self::Delete),
            'c' => Some(Self::Change),
            'y' => Some(Self::Yank),
            _ => None,
        }
    }

    /// The key this operator is typed with, which is also the key that doubles it into its
    /// linewise form.
    pub fn key(&self) -> char {
        match self {
            Self::Delete => 'd',
            Self::Change => 'c',
            Self::Yank => 'y',
        }
    }
}

/// What an operator was told to act on, before a buffer turns it into a range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorTarget {
    /// The span between the cursor and where a motion lands.
    Motion(Motion),
    /// A text object around the cursor.
    TextObject(TextObject),
    /// `dd`, `cc`, `yy`: the cursor's line and, with a count, the lines below it.
    Lines,
    /// The Visual mode selection the operator was typed over.
    Selection,
}

/// Where `i`, `I`, `a`, `A`, `o` and `O` leave the cursor when Insert mode starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertAnchor {
    /// `i`
    BeforeCursor,
    /// `I`
    FirstNonBlank,
    /// `a`
    AfterCursor,
    /// `A`
    LineEnd,
    /// `o`
    LineBelow,
    /// `O`
    LineAbove,
}

/// A command the grammar resolved out of the keys typed so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Move the cursor.
    Move {
        /// How to move.
        motion: Motion,
        /// How many times, `None` when the user typed no count.
        count: Option<usize>,
    },
    /// Apply an operator to a range. `x` `X` `D` `C` `S` `s` are the operators they stand
    /// for over the range their key implies, rather than commands of their own.
    Operate {
        /// Which operator.
        operator: Operator,
        /// The counts around the operator, already multiplied together.
        count: Option<usize>,
        /// The register `"a` named for the text taken, `None` for the unnamed one.
        register: Option<char>,
        /// What it acts on.
        target: OperatorTarget,
    },
    /// `p` and `P`: put a register's text back into the buffer.
    Paste {
        /// `P` rather than `p`: in front of the cursor rather than after it.
        before: bool,
        /// How many copies.
        count: Option<usize>,
        /// The register `"a` named to read, `None` for the unnamed one.
        register: Option<char>,
    },
    /// `r`: overwrite the graphemes at the cursor with one character.
    ReplaceChar {
        /// The character to write.
        replacement: char,
        /// How many graphemes to overwrite.
        count: Option<usize>,
    },
    /// `J`: join lines together.
    JoinLines {
        /// How many lines take part, two of them when the user typed no count.
        count: Option<usize>,
    },
    /// `~`: flip the case of the graphemes at the cursor and step over them.
    ToggleCase {
        /// How many graphemes.
        count: Option<usize>,
    },
    /// Enter Insert mode.
    EnterInsert(InsertAnchor),
    /// `u`: walk back through the changes made.
    Undo {
        /// How many changes.
        count: Option<usize>,
    },
    /// `<C-r>`: walk forward again through the changes an undo walked back from.
    Redo {
        /// How many changes.
        count: Option<usize>,
    },
    /// `.`: do the last change again.
    RepeatEdit {
        /// A count to use in place of the one the change was typed with.
        count: Option<usize>,
    },
    /// `v`: start a selection, or drop the one that is already up.
    ToggleVisual,
    /// `:`, `/` and `?`: start typing a command line, the key that opened it being the
    /// prefix that says what running it will mean.
    EnterCommandLine(char),
    /// `n` and `N`: the last search again.
    RepeatSearch {
        /// `N` rather than `n`: the other way round.
        reverse: bool,
        /// How many matches to walk.
        count: Option<usize>,
    },
    /// `*` and `#`: search for the word under the cursor.
    SearchWord {
        /// `#` rather than `*`: towards the start of the buffer.
        backward: bool,
        /// How many matches to walk.
        count: Option<usize>,
    },
    /// The keys so far are a prefix of a command; more are needed.
    Pending,
    /// `<Esc>`: drop the keys typed so far and leave the mode they were typed in.
    Cancel,
    /// A key that means nothing where it was typed. The pending keys are dropped with it.
    Rejected(KeyEvent),
}

impl Command {
    /// This command with `count` in place of the count it was typed with, which is what a
    /// count on `.` does to the change it repeats. A `None` count leaves the command alone,
    /// and so do the commands that take no count.
    pub fn with_count(self, count: Option<usize>) -> Self {
        if count.is_none() {
            return self;
        }
        match self {
            Self::Move { motion, .. } => Self::Move { motion, count },
            Self::Operate {
                operator,
                register,
                target,
                ..
            } => Self::Operate {
                operator,
                count,
                register,
                target,
            },
            Self::Paste {
                before, register, ..
            } => Self::Paste {
                before,
                count,
                register,
            },
            Self::ReplaceChar { replacement, .. } => Self::ReplaceChar { replacement, count },
            Self::JoinLines { .. } => Self::JoinLines { count },
            Self::ToggleCase { .. } => Self::ToggleCase { count },
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_keys_round_trip() {
        for operator in [Operator::Delete, Operator::Change, Operator::Yank] {
            assert_eq!(Operator::from_key(operator.key()), Some(operator));
        }
        assert_eq!(Operator::from_key('x'), None);
    }

    #[test]
    fn a_new_count_replaces_the_one_a_command_was_typed_with() {
        let paste = Command::Paste {
            before: false,
            count: Some(2),
            register: Some('a'),
        };
        assert_eq!(
            paste.with_count(Some(3)),
            Command::Paste {
                before: false,
                count: Some(3),
                register: Some('a')
            }
        );
        assert_eq!(paste.with_count(None), paste);
        assert_eq!(
            Command::EnterInsert(InsertAnchor::LineBelow).with_count(Some(3)),
            Command::EnterInsert(InsertAnchor::LineBelow),
            "a command with no count is unchanged"
        );
    }
}
