//! Undo and redo, kept as whole-buffer snapshots.

use crate::buffer::Buffer;
use crate::position::Position;

/// One change, as the buffer and the cursor on either side of it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Change {
    /// The buffer as it was before the change.
    before: Buffer,
    /// The buffer the change left behind.
    after: Buffer,
    /// Where the cursor was when the change was opened.
    before_cursor: Position,
    /// Where the change left the cursor when it closed.
    after_cursor: Position,
}

/// The changes `u` walks back through and `<C-r>` walks forward again.
///
/// A change is stored as a copy of the whole buffer rather than as the edit that made it.
/// A rope shares its text between clones, so a snapshot costs a pointer walk instead of a
/// copy of the file, and undoing is then a swap rather than an inverse edit that every new
/// editing command would have to provide.
///
/// The cursor is part of a change: `u` puts it back where the change started and `<C-r>`
/// where the change left it. Working the position out from the two buffers instead lands
/// somewhere else whenever the text a change took also stands next to it — on `"a\na\n"`,
/// `dd` on the first line leaves buffers that first differ on the second one.
///
/// One editing command is one change. The Insert session that `i`, `c` or `o` starts is one
/// command, so it is one change too — it opens where the session does, the line `o` creates
/// included, and closes at `<Esc>`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    undoable: Vec<Change>,
    redoable: Vec<Change>,
    pending: Option<(Buffer, Position)>,
}

impl History {
    /// Opens a change, remembering the buffer it is about to alter and the cursor it starts
    /// from. Opening a change that is already open does nothing, which is what keeps an
    /// Insert session to one snapshot.
    pub fn begin(&mut self, before: &Buffer, cursor: Position) {
        if self.pending.is_none() {
            self.pending = Some((before.clone(), cursor));
        }
    }

    /// Closes the open change, keeping it only when it altered the text. Returns whether it
    /// did, which is also whether the command is worth repeating with `.`.
    pub fn commit(&mut self, after: &Buffer, cursor: Position) -> bool {
        let Some((before, before_cursor)) = self.pending.take() else {
            return false;
        };
        if &before == after {
            return false;
        }
        self.undoable.push(Change {
            before,
            after: after.clone(),
            before_cursor,
            after_cursor: cursor,
        });
        // A new change is the end of the line the redo stack walked back from.
        self.redoable.clear();
        true
    }

    /// Hands back the buffer from before the last change, and the cursor that change started
    /// from. `None` when every change has been undone already.
    pub fn undo(&mut self) -> Option<(Buffer, Position)> {
        let change = self.undoable.pop()?;
        let restored = (change.before.clone(), change.before_cursor);
        self.redoable.push(change);
        Some(restored)
    }

    /// Hands back the buffer an undo walked away from, and the cursor that change left
    /// behind. `None` when there is nothing left to redo.
    pub fn redo(&mut self) -> Option<(Buffer, Position)> {
        let change = self.redoable.pop()?;
        let restored = (change.after.clone(), change.after_cursor);
        self.undoable.push(change);
        Some(restored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The position a test that is not about the cursor opens or closes a change with.
    const ANYWHERE: Position = Position { line: 0, col: 0 };

    #[test]
    fn a_change_that_altered_nothing_is_not_kept() {
        let mut history = History::default();
        history.begin(&Buffer::new("ab"), ANYWHERE);
        assert!(!history.commit(&Buffer::new("ab"), ANYWHERE));
        assert_eq!(history.undo(), None);
    }

    #[test]
    fn undo_and_redo_walk_the_snapshots() {
        let mut history = History::default();
        history.begin(&Buffer::new("one"), ANYWHERE);
        assert!(history.commit(&Buffer::new("two"), ANYWHERE));

        assert_eq!(
            history.undo(),
            Some((Buffer::new("one"), ANYWHERE)),
            "the buffer from before the change"
        );
        assert_eq!(history.undo(), None);
        assert_eq!(history.redo(), Some((Buffer::new("two"), ANYWHERE)));
        assert_eq!(history.redo(), None);
    }

    #[test]
    fn a_change_holds_the_cursor_on_either_side_of_it() {
        let mut history = History::default();
        history.begin(&Buffer::new("one"), Position::new(3, 4));
        history.commit(&Buffer::new("two"), Position::new(1, 2));

        assert_eq!(
            history.undo(),
            Some((Buffer::new("one"), Position::new(3, 4))),
            "undo goes back to where the change started"
        );
        assert_eq!(
            history.redo(),
            Some((Buffer::new("two"), Position::new(1, 2))),
            "redo goes forward to where it ended"
        );
    }

    #[test]
    fn a_new_change_drops_what_could_be_redone() {
        let mut history = History::default();
        history.begin(&Buffer::new("one"), ANYWHERE);
        history.commit(&Buffer::new("two"), ANYWHERE);
        history.undo();

        history.begin(&Buffer::new("one"), ANYWHERE);
        history.commit(&Buffer::new("three"), ANYWHERE);
        assert_eq!(history.redo(), None);
    }

    #[test]
    fn the_second_begin_of_one_change_keeps_the_first_snapshot() {
        let mut history = History::default();
        history.begin(&Buffer::new("one"), Position::new(0, 1));
        history.begin(&Buffer::new("halfway"), Position::new(0, 5));
        history.commit(&Buffer::new("two"), ANYWHERE);
        assert_eq!(
            history.undo(),
            Some((Buffer::new("one"), Position::new(0, 1)))
        );
    }
}
