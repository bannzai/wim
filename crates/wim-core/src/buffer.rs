//! Text buffer built on a rope.

use std::fmt;

use ropey::{Rope, RopeSlice};
use unicode_segmentation::UnicodeSegmentation;

use crate::position::Position;

/// The text being edited.
///
/// Columns are grapheme cluster indices inside a line, so a multi-byte character and a
/// multi-codepoint cluster each occupy exactly one column. Line endings are not part of a
/// line's columns; a trailing newline terminates the last line rather than starting an
/// empty one, and it is preserved when the buffer is rendered back to text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Buffer {
    rope: Rope,
}

impl Buffer {
    pub fn new(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
        }
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines() - usize::from(self.has_trailing_newline())
    }

    pub fn has_trailing_newline(&self) -> bool {
        let len = self.rope.len_chars();
        len > 0 && self.rope.char(len - 1) == '\n'
    }

    /// Text of `line` without its line ending, or the empty string when `line` does not exist.
    pub fn line_text(&self, line: usize) -> String {
        self.line_slice(line).to_string()
    }

    /// Number of grapheme clusters in `line`.
    pub fn line_len(&self, line: usize) -> usize {
        self.line_text(line).graphemes(true).count()
    }

    /// Column of the last grapheme of `line`, which is the rightmost column a Normal mode
    /// cursor can occupy. An empty line yields 0.
    pub fn last_col(&self, line: usize) -> usize {
        self.line_len(line).saturating_sub(1)
    }

    pub fn grapheme_at(&self, pos: Position) -> Option<String> {
        self.line_text(pos.line)
            .graphemes(true)
            .nth(pos.col)
            .map(str::to_owned)
    }

    /// Moves `pos` onto a position a Normal mode cursor can occupy.
    pub fn clamp(&self, pos: Position) -> Position {
        let line = pos.line.min(self.line_count() - 1);
        Position::new(line, pos.col.min(self.last_col(line)))
    }

    /// Absolute char index of `pos`. A column past the end of the line maps to the end of
    /// that line, which is where text appended to a line goes.
    pub fn char_index(&self, pos: Position) -> usize {
        let line = pos.line.min(self.line_count() - 1);
        let mut offset = 0;
        for (col, grapheme) in self.line_text(line).graphemes(true).enumerate() {
            if col == pos.col {
                break;
            }
            offset += grapheme.chars().count();
        }
        self.rope.line_to_char(line) + offset
    }

    /// Unicode scalars in `pos`'s line in front of the grapheme `pos.col` names.
    ///
    /// Columns are graphemes here, while a plugin is handed a column counted in scalars
    /// (`wit/plugin.wit`), so this is what a host converts a cursor with before it hands one
    /// over. A column past the end of the line counts the whole line, which is the end of it
    /// that [`Buffer::char_index`] maps such a column to.
    pub fn scalar_col(&self, pos: Position) -> usize {
        self.line_text(pos.line)
            .graphemes(true)
            .take(pos.col)
            .map(|grapheme| grapheme.chars().count())
            .sum()
    }

    pub fn position_at_char(&self, char_index: usize) -> Position {
        let char_index = char_index.min(self.rope.len_chars());
        let line = self
            .rope
            .char_to_line(char_index)
            .min(self.line_count() - 1);
        let within_line = char_index - self.rope.line_to_char(line);
        let mut chars = 0;
        let mut col = 0;
        for grapheme in self.line_text(line).graphemes(true) {
            if chars >= within_line {
                break;
            }
            chars += grapheme.chars().count();
            col += 1;
        }
        Position::new(line, col)
    }

    /// The text between two positions, `end` exclusive.
    pub fn text_between(&self, start: Position, end: Position) -> String {
        let (start, end) = (self.char_index(start), self.char_index(end));
        self.rope.slice(start.min(end)..start.max(end)).to_string()
    }

    pub fn insert(&mut self, pos: Position, text: &str) {
        let index = self.char_index(pos);
        self.rope.insert(index, text);
    }

    /// Inserts a line break at `at`, plus the newline that terminates the buffer when the
    /// break lands at its very end.
    ///
    /// A trailing newline terminates the last line rather than starting an empty one, so an
    /// empty last line exists only in text that ends with a newline. Opening a line at the
    /// end of a buffer that had none therefore gives it one.
    pub fn insert_line_break(&mut self, at: Position) {
        let at_buffer_end = at.line + 1 == self.line_count()
            && at.col >= self.line_len(at.line)
            && !self.has_trailing_newline();
        self.insert(at, if at_buffer_end { "\n\n" } else { "\n" });
    }

    /// Inserts whole lines so that the first of them becomes line `line`, which may be one
    /// past the last line to append them.
    ///
    /// `text` is the shape a linewise register holds: every line terminated by a break. A
    /// buffer that does not end in a break keeps not ending in one, so that appending lines
    /// to it does not grow a trailing newline it never had.
    pub fn insert_lines(&mut self, line: usize, text: &str) {
        if line < self.line_count() {
            self.insert(Position::new(line, 0), text);
            return;
        }
        let end = self.rope.len_chars();
        if self.has_trailing_newline() {
            self.rope.insert(end, text);
        } else {
            self.rope.insert(end, "\n");
            self.rope
                .insert(end + 1, text.strip_suffix('\n').unwrap_or(text));
        }
    }

    /// Removes the text between the two positions, `end` exclusive, and returns it.
    pub fn delete(&mut self, start: Position, end: Position) -> String {
        let (start, end) = (self.char_index(start), self.char_index(end));
        let (start, end) = (start.min(end), start.max(end));
        let removed = self.rope.slice(start..end).to_string();
        self.rope.remove(start..end);
        removed
    }

    /// Removes lines `first` through `last` together with the break that joined them to the
    /// rest of the buffer, and returns them with a terminating newline.
    ///
    /// Taking the last lines out of a buffer that does not end in a break removes the break
    /// in front of them instead of the one behind, so that the buffer still does not end in
    /// one.
    pub fn delete_lines(&mut self, first: usize, last: usize) -> String {
        let last = last.min(self.line_count() - 1);
        let content_start = self.rope.line_to_char(first);
        let content_end = self.char_index(Position::new(last, self.line_len(last)));
        let mut removed = self.rope.slice(content_start..content_end).to_string();
        removed.push('\n');

        // Separators are removed by rope line boundaries rather than by one char, so a CRLF
        // break goes as a whole instead of leaving its `\r` (or `\n`) behind.
        let (start, end) = if last + 1 < self.line_count() || self.has_trailing_newline() {
            let line_end = self.rope.line_to_char(last) + self.rope.line(last).len_chars();
            (content_start, line_end)
        } else {
            let start = first
                .checked_sub(1)
                .map(|previous| self.char_index(Position::new(previous, self.line_len(previous))))
                .unwrap_or(content_start);
            (start, content_end)
        };
        self.rope.remove(start..end);
        removed
    }

    fn line_slice(&self, line: usize) -> RopeSlice<'_> {
        if line >= self.line_count() {
            return self.rope.slice(0..0);
        }
        let slice = self.rope.line(line);
        let mut end = slice.len_chars();
        if end > 0 && slice.char(end - 1) == '\n' {
            end -= 1;
        }
        if end > 0 && slice.char(end - 1) == '\r' {
            end -= 1;
        }
        slice.slice(..end)
    }
}

impl fmt::Display for Buffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.rope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buffer_has_one_line() {
        let buffer = Buffer::default();
        assert_eq!(buffer.line_count(), 1);
        assert_eq!(buffer.line_len(0), 0);
        assert_eq!(buffer.last_col(0), 0);
    }

    #[test]
    fn trailing_newline_terminates_the_last_line() {
        assert_eq!(Buffer::new("abc").line_count(), 1);
        assert_eq!(Buffer::new("abc\n").line_count(), 1);
        assert_eq!(Buffer::new("abc\n\n").line_count(), 2);
        assert_eq!(Buffer::new("a\nb").line_count(), 2);
        assert_eq!(Buffer::new("\n").line_count(), 1);
    }

    #[test]
    fn keeps_the_original_text_including_the_trailing_newline() {
        assert_eq!(Buffer::new("abc\n").to_string(), "abc\n");
        assert!(Buffer::new("abc\n").has_trailing_newline());
        assert_eq!(Buffer::new("abc").to_string(), "abc");
        assert!(!Buffer::new("abc").has_trailing_newline());
    }

    #[test]
    fn line_text_drops_line_endings() {
        let buffer = Buffer::new("abc\r\ndef\nghi");
        assert_eq!(buffer.line_text(0), "abc");
        assert_eq!(buffer.line_text(1), "def");
        assert_eq!(buffer.line_text(2), "ghi");
        assert_eq!(buffer.line_text(3), "");
    }

    #[test]
    fn columns_count_graphemes_not_bytes() {
        let buffer = Buffer::new("あいうえお");
        assert_eq!(buffer.line_len(0), 5);
        assert_eq!(buffer.last_col(0), 4);
        assert_eq!(
            buffer.grapheme_at(Position::new(0, 2)).as_deref(),
            Some("う")
        );
        assert_eq!(buffer.grapheme_at(Position::new(0, 5)), None);
    }

    #[test]
    fn a_combining_cluster_is_one_column() {
        let buffer = Buffer::new("がか\u{3099}");
        assert_eq!(buffer.line_len(0), 2);
        assert_eq!(
            buffer.grapheme_at(Position::new(0, 1)).as_deref(),
            Some("か\u{3099}")
        );
    }

    #[test]
    fn char_index_round_trips_through_positions() {
        let buffer = Buffer::new("あいう\nabc");
        for (pos, index) in [
            (Position::new(0, 0), 0),
            (Position::new(0, 2), 2),
            (Position::new(1, 0), 4),
            (Position::new(1, 3), 7),
        ] {
            assert_eq!(buffer.char_index(pos), index, "char_index of {pos:?}");
        }
        for index in 0..=7 {
            let pos = buffer.position_at_char(index);
            assert_eq!(buffer.char_index(pos), index, "round trip of {index}");
        }
    }

    #[test]
    fn a_column_counts_more_scalars_than_graphemes_where_a_cluster_is_made_of_several() {
        // `e` plus a combining acute is one column and two scalars, and a joined family is one
        // column and five: two columns into either line is not two scalars into it.
        let buffer = Buffer::new("e\u{301}x\n👨‍👩‍👦y");
        assert_eq!(buffer.scalar_col(Position::new(0, 0)), 0);
        assert_eq!(buffer.scalar_col(Position::new(0, 1)), 2);
        assert_eq!(buffer.scalar_col(Position::new(0, 2)), 3);
        assert_eq!(buffer.scalar_col(Position::new(1, 1)), 5);
        // A column past the end of the line counts the line, as appending to it lands there.
        assert_eq!(buffer.scalar_col(Position::new(0, 9)), 3);
    }

    #[test]
    fn clamps_onto_the_last_grapheme_of_a_line() {
        let buffer = Buffer::new("あい\n\nabc");
        assert_eq!(buffer.clamp(Position::new(0, 9)), Position::new(0, 1));
        assert_eq!(buffer.clamp(Position::new(1, 9)), Position::new(1, 0));
        assert_eq!(buffer.clamp(Position::new(9, 9)), Position::new(2, 2));
    }

    #[test]
    fn inserts_and_deletes_by_grapheme_position() {
        let mut buffer = Buffer::new("あいう\nabc");
        buffer.insert(Position::new(0, 1), "X");
        assert_eq!(buffer.to_string(), "あXいう\nabc");

        let removed = buffer.delete(Position::new(0, 1), Position::new(0, 2));
        assert_eq!(removed, "X");
        assert_eq!(buffer.to_string(), "あいう\nabc");

        let removed = buffer.delete(Position::new(0, 3), Position::new(1, 0));
        assert_eq!(removed, "\n");
        assert_eq!(buffer.to_string(), "あいうabc");
    }

    #[test]
    fn appends_at_a_column_past_the_end_of_the_line() {
        let mut buffer = Buffer::new("あい\ncd");
        buffer.insert(Position::new(0, 2), "!");
        assert_eq!(buffer.to_string(), "あい!\ncd");
    }

    #[test]
    fn deleting_lines_takes_one_of_the_breaks_around_them() {
        let mut buffer = Buffer::new("ab\ncd\nef");
        assert_eq!(buffer.delete_lines(0, 0), "ab\n");
        assert_eq!(buffer.to_string(), "cd\nef");

        let mut buffer = Buffer::new("ab\ncd\nef");
        assert_eq!(buffer.delete_lines(2, 2), "ef\n");
        assert_eq!(
            buffer.to_string(),
            "ab\ncd",
            "the break in front goes instead, so the buffer still has no trailing newline"
        );

        let mut buffer = Buffer::new("ab\ncd\n");
        assert_eq!(buffer.delete_lines(1, 1), "cd\n");
        assert_eq!(buffer.to_string(), "ab\n");

        let mut buffer = Buffer::new("ab\ncd\nef");
        assert_eq!(buffer.delete_lines(0, 9), "ab\ncd\nef\n", "the last clamps");
        assert_eq!(buffer.to_string(), "");
    }

    #[test]
    fn deleting_lines_takes_a_crlf_break_as_a_whole() {
        let mut buffer = Buffer::new("ab\r\ncd\r\nef");
        buffer.delete_lines(0, 0);
        assert_eq!(buffer.to_string(), "cd\r\nef");

        let mut buffer = Buffer::new("ab\r\ncd\r\nef");
        buffer.delete_lines(2, 2);
        assert_eq!(
            buffer.to_string(),
            "ab\r\ncd",
            "the whole CRLF in front goes with the final line"
        );

        let mut buffer = Buffer::new("ab\r\ncd\r\n");
        buffer.delete_lines(1, 1);
        assert_eq!(buffer.to_string(), "ab\r\n");
    }

    #[test]
    fn inserting_lines_past_the_last_one_appends_them() {
        let mut buffer = Buffer::new("ab");
        buffer.insert_lines(1, "cd\n");
        assert_eq!(buffer.to_string(), "ab\ncd");

        let mut buffer = Buffer::new("ab\n");
        buffer.insert_lines(1, "cd\n");
        assert_eq!(buffer.to_string(), "ab\ncd\n");

        let mut buffer = Buffer::new("ab\ncd");
        buffer.insert_lines(1, "xy\n");
        assert_eq!(buffer.to_string(), "ab\nxy\ncd");
    }
}
