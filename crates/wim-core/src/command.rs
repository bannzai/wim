//! What a finished sequence of Normal mode keys means.

use crate::key::KeyEvent;
use crate::motion::Motion;
use crate::textobject::TextObject;

/// An operator: a command that acts on a range of text.
///
/// Applying one to the buffer comes with the operator issue; the grammar already resolves
/// which range it would act on.
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
    /// Apply an operator to a range.
    Operate {
        /// Which operator.
        operator: Operator,
        /// The counts around the operator, already multiplied together.
        count: Option<usize>,
        /// What it acts on.
        target: OperatorTarget,
    },
    /// `x` and `X`: delete graphemes at or in front of the cursor without an operator.
    DeleteChar {
        /// `X` rather than `x`.
        before: bool,
        /// How many graphemes.
        count: Option<usize>,
    },
    /// Enter Insert mode.
    EnterInsert(InsertAnchor),
    /// `v`: start a selection, or drop the one that is already up.
    ToggleVisual,
    /// The keys so far are a prefix of a command; more are needed.
    Pending,
    /// `<Esc>`: drop the keys typed so far and leave the mode they were typed in.
    Cancel,
    /// A key that means nothing where it was typed. The pending keys are dropped with it.
    Rejected(KeyEvent),
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
}
