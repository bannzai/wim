//! Turning a resolved span or a single key into an edit of the buffer.
//!
//! Every function here takes the buffer it edits and gives back where the cursor lands, so
//! that the editor is left with the bookkeeping — registers, undo and the mode — rather than
//! with Vim's rules about what an edit does to the text.

use unicode_segmentation::UnicodeSegmentation;

use crate::buffer::Buffer;
use crate::motion::{Class, class_of, first_non_blank};
use crate::position::Position;
use crate::register::RegisterContent;
use crate::textobject::TextRange;

/// The most text one paste may put into the buffer.
///
/// A paste is `count` copies of a register, and the grammar saturates a count typed with
/// more digits than a number can hold, so `999999999999999999999p` asks for a string no
/// allocator can hand out: `String::repeat` aborts the process rather than failing. The cap
/// is 64 MiB because that is far past any paste made on purpose — a whole source file
/// pasted a hundred times over is still well inside it — while staying inside the 4 GiB
/// address space of the wasm32 build the core has to keep working in.
pub const MAX_PASTE_BYTES: usize = 64 * 1024 * 1024;

/// The text `range` holds, without touching the buffer.
pub fn yank(buffer: &Buffer, range: TextRange) -> RegisterContent {
    let text = buffer.text_between(range.start, range.end);
    if range.linewise {
        RegisterContent::linewise(text)
    } else {
        RegisterContent::charwise(text)
    }
}

/// Takes `range` out of the buffer and returns what it held and where the cursor lands.
///
/// A charwise delete leaves the cursor where the span started, moved back onto the line when
/// the span reached its end. A linewise delete leaves it on the first non-blank of the line
/// that moved up into the deleted one's place, or of the line before it when the deleted
/// lines were the last.
pub fn delete(buffer: &mut Buffer, range: TextRange) -> (RegisterContent, Position) {
    if !range.linewise {
        let text = buffer.delete(range.start, range.end);
        return (RegisterContent::charwise(text), buffer.clamp(range.start));
    }
    let text = buffer.delete_lines(range.start.line, range.end.line);
    let line = range.start.line.min(buffer.line_count() - 1);
    (
        RegisterContent::linewise(text),
        first_non_blank(buffer, line),
    )
}

/// Empties `range` for a change and returns what it held and where Insert mode starts.
///
/// A linewise change leaves one empty line where the lines were and starts typing at its
/// column 0: unlike Vim's `cc`, the indent of the first line is not carried over.
pub fn change(buffer: &mut Buffer, range: TextRange) -> (RegisterContent, Position) {
    if !range.linewise {
        let text = buffer.delete(range.start, range.end);
        return (RegisterContent::charwise(text), range.start);
    }
    let text = buffer.delete_lines(range.start.line, range.end.line);
    open_empty_line(buffer, range.start.line);
    (
        RegisterContent::linewise(text),
        Position::new(range.start.line.min(buffer.line_count() - 1), 0),
    )
}

/// Puts `content` into the buffer `count` times over and returns where the cursor lands.
///
/// Charwise text goes after the cursor's grapheme, or in front of it with `before`, and the
/// cursor ends on the last grapheme put in. Linewise text goes onto lines of its own below
/// the cursor's line, or above it with `before`, and the cursor ends on the first non-blank
/// of the first line put in.
///
/// `None` when the copies would come to more than [`MAX_PASTE_BYTES`], which is a paste that
/// cannot be carried out rather than one that goes wrong.
pub fn paste(
    buffer: &mut Buffer,
    cursor: Position,
    content: &RegisterContent,
    before: bool,
    count: usize,
) -> Option<Position> {
    if content.text.len().checked_mul(count)? > MAX_PASTE_BYTES {
        return None;
    }
    let text = content.text.repeat(count);
    if content.linewise {
        let line = if before { cursor.line } else { cursor.line + 1 };
        buffer.insert_lines(line, &text);
        return Some(first_non_blank(buffer, line));
    }
    let at = if before || buffer.line_len(cursor.line) == 0 {
        cursor
    } else {
        Position::new(cursor.line, cursor.col + 1)
    };
    let last = buffer.char_index(at) + text.chars().count();
    buffer.insert(at, &text);
    Some(buffer.clamp(buffer.position_at_char(last.saturating_sub(1))))
}

/// Replaces the `count` graphemes at the cursor with `replacement`, Vim's `r`.
///
/// `None` when the line holds fewer than `count` graphemes from the cursor on, which Vim
/// treats as a command that does nothing at all. A count so large that the column past the
/// last grapheme cannot be counted to is one such count.
pub fn replace(
    buffer: &mut Buffer,
    cursor: Position,
    replacement: char,
    count: usize,
) -> Option<Position> {
    let end = cursor.col.checked_add(count)?;
    if end > buffer.line_len(cursor.line) {
        return None;
    }
    buffer.delete(cursor, Position::new(cursor.line, end));
    buffer.insert(cursor, &replacement.to_string().repeat(count));
    Some(Position::new(cursor.line, end - 1))
}

/// Flips the case of `count` graphemes from the cursor on and returns the position after the
/// last of them, Vim's `~`. Characters that have no other case are stepped over unchanged.
pub fn flip_case(buffer: &mut Buffer, cursor: Position, count: usize) -> Position {
    let mut cursor = cursor;
    for _ in 0..count {
        let Some(grapheme) = buffer.grapheme_at(cursor) else {
            break;
        };
        let flipped = flipped_case(&grapheme);
        if flipped != grapheme {
            buffer.delete(cursor, Position::new(cursor.line, cursor.col + 1));
            buffer.insert(cursor, &flipped);
        }
        cursor.col += flipped.graphemes(true).count().max(1);
    }
    buffer.clamp(cursor)
}

/// Joins the `joins` line breaks below `line` away, Vim's `J`, and returns the position of
/// the last join. `None` when there was no line left to join.
///
/// The blanks that indented a joined line are dropped and one space is put in their place,
/// unless the line joined to is empty or already ends in a blank, or the joined line held
/// nothing but its indent.
pub fn join(buffer: &mut Buffer, line: usize, joins: usize) -> Option<Position> {
    let mut joined = None;
    for _ in 0..joins {
        if line + 1 >= buffer.line_count() {
            break;
        }
        let end = buffer.line_len(line);
        let next = buffer.line_text(line + 1);
        let indent = next
            .graphemes(true)
            .take_while(|grapheme| class_of(grapheme, false) == Class::Blank)
            .count();
        buffer.delete(Position::new(line, end), Position::new(line + 1, indent));

        let after_a_blank = end > 0
            && buffer
                .grapheme_at(Position::new(line, end - 1))
                .is_some_and(|grapheme| class_of(&grapheme, false) == Class::Blank);
        if end > 0 && !after_a_blank && next.graphemes(true).count() > indent {
            buffer.insert(Position::new(line, end), " ");
        }
        joined = Some(buffer.clamp(Position::new(line, end)));
    }
    joined
}

/// Makes line `line` an empty line, pushing the line that was there down. `line` may be one
/// past the last line, which appends the empty line instead.
fn open_empty_line(buffer: &mut Buffer, line: usize) {
    if buffer.line_count() == 1 && buffer.line_len(0) == 0 && !buffer.has_trailing_newline() {
        // The buffer is already the one empty line an emptied buffer holds.
        return;
    }
    if line < buffer.line_count() {
        buffer.insert(Position::new(line, 0), "\n");
        return;
    }
    let last = buffer.line_count() - 1;
    buffer.insert_line_break(Position::new(last, buffer.line_len(last)));
}

/// `text` with its uppercase characters lowered and its lowercase ones raised.
fn flipped_case(text: &str) -> String {
    let mut flipped = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_uppercase() {
            flipped.extend(character.to_lowercase());
        } else if character.is_lowercase() {
            flipped.extend(character.to_uppercase());
        } else {
            flipped.push(character);
        }
    }
    flipped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_charwise_delete_leaves_the_cursor_where_the_span_started() {
        let mut buffer = Buffer::new("foo bar");
        let (content, cursor) = delete(
            &mut buffer,
            TextRange::charwise(Position::new(0, 4), Position::new(0, 7)),
        );
        assert_eq!(buffer.to_string(), "foo ");
        assert_eq!(content, RegisterContent::charwise("bar".to_owned()));
        assert_eq!(cursor, Position::new(0, 3), "the span reached the line end");
    }

    #[test]
    fn a_linewise_delete_leaves_the_cursor_on_the_line_that_moved_up() {
        let mut buffer = Buffer::new("ab\n  cd\nef");
        let lines = TextRange::lines(&buffer, 0, 0);
        let (content, cursor) = delete(&mut buffer, lines);
        assert_eq!(buffer.to_string(), "  cd\nef");
        assert_eq!(content, RegisterContent::linewise("ab".to_owned()));
        assert_eq!(cursor, Position::new(0, 2), "onto the first non-blank");

        let mut buffer = Buffer::new("ab\ncd");
        let lines = TextRange::lines(&buffer, 1, 1);
        let (_, cursor) = delete(&mut buffer, lines);
        assert_eq!(buffer.to_string(), "ab");
        assert_eq!(cursor, Position::new(0, 0), "the line before, when last");
    }

    #[test]
    fn a_linewise_change_leaves_an_empty_line_to_type_on() {
        let mut buffer = Buffer::new("ab\ncd\nef");
        let lines = TextRange::lines(&buffer, 0, 1);
        let (content, cursor) = change(&mut buffer, lines);
        assert_eq!(buffer.to_string(), "\nef");
        assert_eq!(content, RegisterContent::linewise("ab\ncd".to_owned()));
        assert_eq!(cursor, Position::new(0, 0));

        let mut buffer = Buffer::new("ab\ncd");
        let lines = TextRange::lines(&buffer, 1, 1);
        change(&mut buffer, lines);
        assert_eq!(buffer.to_string(), "ab\n\n");

        let mut buffer = Buffer::new("ab");
        let lines = TextRange::lines(&buffer, 0, 0);
        let (_, cursor) = change(&mut buffer, lines);
        assert_eq!(buffer.to_string(), "");
        assert_eq!(cursor, Position::new(0, 0));
    }

    #[test]
    fn charwise_paste_goes_around_the_cursor_grapheme() {
        let mut buffer = Buffer::new("ab");
        let content = RegisterContent::charwise("XY".to_owned());
        assert_eq!(
            paste(&mut buffer, Position::new(0, 0), &content, false, 1),
            Some(Position::new(0, 2))
        );
        assert_eq!(buffer.to_string(), "aXYb");

        let mut buffer = Buffer::new("ab");
        assert_eq!(
            paste(&mut buffer, Position::new(0, 0), &content, true, 2),
            Some(Position::new(0, 3))
        );
        assert_eq!(buffer.to_string(), "XYXYab");

        let mut buffer = Buffer::new("");
        paste(&mut buffer, Position::new(0, 0), &content, false, 1);
        assert_eq!(buffer.to_string(), "XY", "an empty line has no after");
    }

    #[test]
    fn linewise_paste_goes_onto_lines_of_its_own() {
        let content = RegisterContent::linewise("  xy".to_owned());

        let mut buffer = Buffer::new("ab\ncd");
        assert_eq!(
            paste(&mut buffer, Position::new(0, 1), &content, false, 1),
            Some(Position::new(1, 2))
        );
        assert_eq!(buffer.to_string(), "ab\n  xy\ncd");

        let mut buffer = Buffer::new("ab\ncd");
        assert_eq!(
            paste(&mut buffer, Position::new(0, 1), &content, true, 2),
            Some(Position::new(0, 2))
        );
        assert_eq!(buffer.to_string(), "  xy\n  xy\nab\ncd");

        let mut buffer = Buffer::new("ab");
        paste(&mut buffer, Position::new(0, 0), &content, false, 1);
        assert_eq!(buffer.to_string(), "ab\n  xy", "no trailing newline grows");
    }

    #[test]
    fn a_paste_no_allocation_could_hold_is_refused() {
        let mut buffer = Buffer::new("ab");
        let content = RegisterContent::charwise("XY".to_owned());
        assert_eq!(
            paste(
                &mut buffer,
                Position::new(0, 0),
                &content,
                false,
                usize::MAX
            ),
            None,
            "the copies would come to more than a string can hold"
        );
        assert_eq!(
            paste(
                &mut buffer,
                Position::new(0, 0),
                &content,
                false,
                MAX_PASTE_BYTES
            ),
            None,
            "and to more than the cap even when they can be counted"
        );
        assert_eq!(buffer.to_string(), "ab", "a refused paste alters nothing");
    }

    #[test]
    fn replacing_needs_the_graphemes_to_be_there() {
        let mut buffer = Buffer::new("あいう");
        assert_eq!(
            replace(&mut buffer, Position::new(0, 0), 'x', 2),
            Some(Position::new(0, 1))
        );
        assert_eq!(buffer.to_string(), "xxう");
        assert_eq!(replace(&mut buffer, Position::new(0, 1), 'x', 3), None);
        assert_eq!(
            replace(&mut buffer, Position::new(0, 1), 'x', usize::MAX),
            None,
            "a count that cannot even be counted to the line end with"
        );
        assert_eq!(buffer.to_string(), "xxう");
    }

    #[test]
    fn flipping_case_walks_over_what_has_no_case() {
        let mut buffer = Buffer::new("aB.c");
        assert_eq!(
            flip_case(&mut buffer, Position::new(0, 0), 9),
            Position::new(0, 3)
        );
        assert_eq!(buffer.to_string(), "Ab.C");
    }

    #[test]
    fn joining_puts_one_space_where_the_break_was() {
        let mut buffer = Buffer::new("ab\n   cd\nef");
        assert_eq!(join(&mut buffer, 0, 2), Some(Position::new(0, 5)));
        assert_eq!(buffer.to_string(), "ab cd ef");

        let mut buffer = Buffer::new("ab \ncd");
        join(&mut buffer, 0, 1);
        assert_eq!(
            buffer.to_string(),
            "ab cd",
            "a line ending in a blank keeps it"
        );

        let mut buffer = Buffer::new("\ncd");
        join(&mut buffer, 0, 1);
        assert_eq!(buffer.to_string(), "cd", "an empty line takes no space");

        let mut buffer = Buffer::new("ab\n  ");
        join(&mut buffer, 0, 1);
        assert_eq!(buffer.to_string(), "ab", "a line of blanks brings nothing");

        assert_eq!(join(&mut Buffer::new("ab"), 0, 1), None);
    }
}
