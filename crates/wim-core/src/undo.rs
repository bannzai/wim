//! Undo and redo, kept as whole-buffer snapshots.

use crate::buffer::Buffer;

/// The buffers `u` walks back through and `<C-r>` walks forward again.
///
/// A change is stored as a copy of the whole buffer rather than as the edit that made it.
/// A rope shares its text between clones, so a snapshot costs a pointer walk instead of a
/// copy of the file, and undoing is then a swap rather than an inverse edit that every new
/// editing command would have to provide. The cursor is not part of a snapshot: both undo
/// and redo put it where the two buffers differ, which is the change either way.
///
/// One editing command is one snapshot. The Insert session that `i`, `c` or `o` starts is
/// one command, so it is one snapshot too — it opens where the session does, the line `o`
/// creates included, and closes at `<Esc>`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    undoable: Vec<Buffer>,
    redoable: Vec<Buffer>,
    pending: Option<Buffer>,
}

impl History {
    /// Opens a change, remembering the buffer it is about to alter. Opening a change that is
    /// already open does nothing, which is what keeps an Insert session to one snapshot.
    pub fn begin(&mut self, before: &Buffer) {
        if self.pending.is_none() {
            self.pending = Some(before.clone());
        }
    }

    /// Closes the open change, keeping it only when it altered the text. Returns whether it
    /// did, which is also whether the command is worth repeating with `.`.
    pub fn commit(&mut self, after: &Buffer) -> bool {
        let Some(before) = self.pending.take() else {
            return false;
        };
        if &before == after {
            return false;
        }
        self.undoable.push(before);
        // A new change is the end of the line the redo stack walked back from.
        self.redoable.clear();
        true
    }

    /// Hands back the buffer from before the last change, taking `current` in its place.
    /// `None` when every change has been undone already.
    pub fn undo(&mut self, current: &Buffer) -> Option<Buffer> {
        let previous = self.undoable.pop()?;
        self.redoable.push(current.clone());
        Some(previous)
    }

    /// Hands back the buffer an undo walked away from, taking `current` in its place.
    /// `None` when there is nothing left to redo.
    pub fn redo(&mut self, current: &Buffer) -> Option<Buffer> {
        let next = self.redoable.pop()?;
        self.undoable.push(current.clone());
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_that_altered_nothing_is_not_kept() {
        let mut history = History::default();
        history.begin(&Buffer::new("ab"));
        assert!(!history.commit(&Buffer::new("ab")));
        assert_eq!(history.undo(&Buffer::new("ab")), None);
    }

    #[test]
    fn undo_and_redo_walk_the_snapshots() {
        let mut history = History::default();
        history.begin(&Buffer::new("one"));
        assert!(history.commit(&Buffer::new("two")));

        assert_eq!(history.undo(&Buffer::new("two")), Some(Buffer::new("one")));
        assert_eq!(history.undo(&Buffer::new("one")), None);
        assert_eq!(history.redo(&Buffer::new("one")), Some(Buffer::new("two")));
        assert_eq!(history.redo(&Buffer::new("two")), None);
    }

    #[test]
    fn a_new_change_drops_what_could_be_redone() {
        let mut history = History::default();
        history.begin(&Buffer::new("one"));
        history.commit(&Buffer::new("two"));
        history.undo(&Buffer::new("two"));

        history.begin(&Buffer::new("one"));
        history.commit(&Buffer::new("three"));
        assert_eq!(history.redo(&Buffer::new("three")), None);
    }

    #[test]
    fn the_second_begin_of_one_change_keeps_the_first_snapshot() {
        let mut history = History::default();
        history.begin(&Buffer::new("one"));
        history.begin(&Buffer::new("halfway"));
        history.commit(&Buffer::new("two"));
        assert_eq!(history.undo(&Buffer::new("two")), Some(Buffer::new("one")));
    }
}
