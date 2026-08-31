//! What a finished sequence of Normal mode keys means.

use crate::buffer::Buffer;
use crate::key::KeyEvent;
use crate::motion::Motion;
use crate::position::Position;
use crate::textobject::{TextObject, TextRange};

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

/// The shape a Visual mode selection had when an operator took it.
///
/// Visual mode in wim is charwise, so a shape is a number of lines and a column on the last
/// of them rather than a whole-line span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionShape {
    /// How many lines below its first the selection ended on, `0` for a selection that
    /// stayed on one line.
    pub lines: usize,
    /// Where the selection ended: how many columns wide it was when it stayed on one line,
    /// and the column it ended on when it covered more than one.
    pub end_col: usize,
}

impl SelectionShape {
    /// The shape of `range`, the span a selection resolved to.
    pub fn of(range: TextRange) -> Self {
        let lines = range.end.line - range.start.line;
        Self {
            lines,
            end_col: if lines == 0 {
                range.end.col - range.start.col
            } else {
                range.end.col
            },
        }
    }

    /// The span this shape covers when it is applied again from `cursor` in `buffer`.
    ///
    /// A shape whose last line falls past the end of the buffer stops at the end of the text
    /// instead: the line it names does not exist, and a column on it would be read on the
    /// final line, which can leave the end of the span in front of the cursor and turn the
    /// repeat into a delete of the text before it.
    pub fn range_at(&self, buffer: &Buffer, cursor: Position) -> TextRange {
        let end = if self.lines == 0 {
            Position::new(cursor.line, cursor.col.saturating_add(self.end_col))
        } else {
            let line = cursor.line.saturating_add(self.lines);
            if line < buffer.line_count() {
                Position::new(line, self.end_col)
            } else {
                let last = buffer.line_count() - 1;
                Position::new(last, buffer.line_len(last))
            }
        };
        TextRange::charwise(cursor, cursor.max(end))
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
    /// The shape a selection had, which is what `.` repeats: by the time the repeat runs
    /// there is no selection left, so Vim applies the same shape from wherever the cursor
    /// then is.
    SelectionShape(SelectionShape),
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
    /// `q{a-z}`: keep the keys typed from here on in a register.
    RecordMacro {
        /// The register the keys will land in.
        register: char,
    },
    /// `q` while a macro is being recorded: stop, and fill the register.
    StopRecording,
    /// `@{a-z}` and `@@`: type the keys a register holds.
    PlayMacro {
        /// The register to read, `None` for `@@`, which is the last one played again.
        register: Option<char>,
        /// How many times to type them.
        count: Option<usize>,
    },
    /// `m{a-z}`: name the cursor's position.
    SetMark(char),
    /// `` `{a-z} `` and `'{a-z}`: move to a position named earlier.
    JumpMark {
        /// The mark to move to.
        name: char,
        /// `'` rather than `` ` ``: the first non-blank of the mark's line rather than the
        /// column the mark holds.
        to_line_start: bool,
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
    fn a_selection_shape_is_the_span_it_covers_from_a_new_cursor() {
        let buffer = Buffer::new("abcdef\nabcdef\nabcdef");
        let one_line = SelectionShape::of(TextRange::charwise(
            Position::new(0, 3),
            Position::new(0, 5),
        ));
        assert_eq!(
            one_line,
            SelectionShape {
                lines: 0,
                end_col: 2
            },
            "a selection inside one line keeps its width"
        );
        assert_eq!(
            one_line.range_at(&buffer, Position::new(2, 1)),
            TextRange::charwise(Position::new(2, 1), Position::new(2, 3))
        );

        let two_lines = SelectionShape::of(TextRange::charwise(
            Position::new(1, 2),
            Position::new(2, 4),
        ));
        assert_eq!(
            two_lines,
            SelectionShape {
                lines: 1,
                end_col: 4
            },
            "a selection over more than one line keeps the column it ended on"
        );
        assert_eq!(
            two_lines.range_at(&buffer, Position::new(0, 0)),
            TextRange::charwise(Position::new(0, 0), Position::new(1, 4))
        );
        assert_eq!(
            two_lines.range_at(&buffer, Position::new(2, 5)),
            TextRange::charwise(Position::new(2, 5), Position::new(2, 6)),
            "a shape whose last line is past the end of the buffer stops at the end of the \
             text rather than at a column in front of the cursor"
        );
        assert_eq!(
            two_lines.range_at(&Buffer::new("abcdef"), Position::new(0, 5)),
            TextRange::charwise(Position::new(0, 5), Position::new(0, 6))
        );
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
