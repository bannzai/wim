//! Text objects: the spans `iw`, `a"`, `i(` and friends hand to an operator.

use unicode_segmentation::UnicodeSegmentation;

use crate::buffer::Buffer;
use crate::motion::{Class, class_of, next_pos, prev_pos};
use crate::position::Position;

/// A span of text an operator consumes, `end` exclusive.
///
/// Either end may sit one column past the last grapheme of its line, which is how a span
/// that reaches a line ending is written; [`Buffer::char_index`] reads such a column as the
/// end of that line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextRange {
    /// First position the operator takes.
    pub start: Position,
    /// First position past the span.
    pub end: Position,
    /// Whole lines. `start` is the first column of the first line and `end` is the end of
    /// the last line; an operator on a linewise span also takes the line breaks that
    /// terminate those lines.
    pub linewise: bool,
}

impl TextRange {
    /// A span of characters inside or across lines.
    pub fn charwise(start: Position, end: Position) -> Self {
        Self {
            start,
            end,
            linewise: false,
        }
    }

    /// A span covering `first_line` through `last_line` in full.
    pub fn lines(buffer: &Buffer, first_line: usize, last_line: usize) -> Self {
        Self {
            start: Position::new(first_line, 0),
            end: Position::new(last_line, buffer.line_len(last_line)),
            linewise: true,
        }
    }

    /// Whether the span holds no text, which `i"` on `""` resolves to.
    pub fn is_empty(&self) -> bool {
        !self.linewise && self.start == self.end
    }
}

/// What kind of text the object is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextObjectKind {
    /// `w` / `W`: a run of characters of one class, `big` collapsing every non-blank class
    /// into one.
    Word { big: bool },
    /// `"` / `'` / `` ` ``: text between two of the same quote on the cursor's line.
    Quote(char),
    /// `(` `{` `[` `<` and their closing halves: text between a matching pair, which may
    /// span lines. `b` and `B` are the aliases for `(` and `{`.
    Block { open: char, close: char },
}

/// A text object, the `iw` or `a(` an operator is given instead of a motion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextObject {
    /// The text the object is made of.
    pub kind: TextObjectKind,
    /// `a` rather than `i`: the delimiters or the trailing whitespace come along.
    pub around: bool,
}

impl TextObject {
    /// Reads the key that follows `i` or `a`, `None` for keys that name no object.
    pub fn from_key(key: char, around: bool) -> Option<Self> {
        let kind = match key {
            'w' => TextObjectKind::Word { big: false },
            'W' => TextObjectKind::Word { big: true },
            '"' | '\'' | '`' => TextObjectKind::Quote(key),
            '(' | ')' | 'b' => TextObjectKind::Block {
                open: '(',
                close: ')',
            },
            '{' | '}' | 'B' => TextObjectKind::Block {
                open: '{',
                close: '}',
            },
            '[' | ']' => TextObjectKind::Block {
                open: '[',
                close: ']',
            },
            '<' | '>' => TextObjectKind::Block {
                open: '<',
                close: '>',
            },
            _ => return None,
        };
        Some(Self { kind, around })
    }
}

/// Resolves `object` around `cursor`, `None` when the cursor is not in one.
///
/// `count` takes more words for word objects and steps out to an enclosing pair for block
/// objects; quote objects ignore it, as Vim does. Quotes are not escaped: a `\"` inside a
/// string closes it. No object resolves linewise yet — the paragraph objects and Vim's
/// promotion of a multi-line `i{` to whole lines come with later issues.
pub fn resolve(
    buffer: &Buffer,
    cursor: Position,
    object: TextObject,
    count: Option<usize>,
) -> Option<TextRange> {
    let cursor = buffer.clamp(cursor);
    let count = count.unwrap_or(1).max(1);
    match object.kind {
        TextObjectKind::Word { big } => word(buffer, cursor, big, object.around, count),
        TextObjectKind::Quote(quote) => quoted(buffer, cursor, quote, object.around),
        TextObjectKind::Block { open, close } => {
            block(buffer, cursor, open, close, object.around, count)
        }
    }
}

/// A maximal run of graphemes of one class within a line, in columns.
struct Run {
    start: usize,
    end: usize,
    class: Class,
}

fn runs(text: &str, big: bool) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for (col, grapheme) in text.graphemes(true).enumerate() {
        let class = class_of(grapheme, big);
        match runs.last_mut() {
            Some(run) if run.class == class => run.end = col + 1,
            _ => runs.push(Run {
                start: col,
                end: col + 1,
                class,
            }),
        }
    }
    runs
}

/// `iw` is `count` runs from the one the cursor is in, blank runs included; `aw` is `count`
/// words with the whitespace that follows each, falling back to the whitespace in front of
/// the first word when the last one has none.
fn word(
    buffer: &Buffer,
    cursor: Position,
    big: bool,
    around: bool,
    count: usize,
) -> Option<TextRange> {
    let text = buffer.line_text(cursor.line);
    let runs = runs(&text, big);
    let first = runs
        .iter()
        .position(|run| cursor.col >= run.start && cursor.col < run.end)?;

    let (start_run, end_run) = if around {
        let mut end = first;
        let mut took_trailing_blank = false;
        for _ in 0..count {
            if end >= runs.len() {
                break;
            }
            took_trailing_blank = false;
            let started_on_blank = runs[end].class == Class::Blank;
            end += 1;
            if started_on_blank {
                // Leading whitespace belongs to the word behind it.
                if end < runs.len() {
                    end += 1;
                }
            } else if end < runs.len() && runs[end].class == Class::Blank {
                end += 1;
                took_trailing_blank = true;
            }
        }
        let start = match first.checked_sub(1) {
            Some(previous) if !took_trailing_blank && runs[previous].class == Class::Blank => {
                previous
            }
            _ => first,
        };
        (start, end - 1)
    } else {
        (first, (first + count - 1).min(runs.len() - 1))
    };

    Some(TextRange::charwise(
        Position::new(cursor.line, runs[start_run].start),
        Position::new(cursor.line, runs[end_run].end),
    ))
}

/// Quotes pair up from the start of the line, so the object is the first pair that ends at or
/// after the cursor. `a"` takes the quotes and the whitespace behind them.
fn quoted(buffer: &Buffer, cursor: Position, quote: char, around: bool) -> Option<TextRange> {
    let text = buffer.line_text(cursor.line);
    let columns: Vec<usize> = text
        .graphemes(true)
        .enumerate()
        .filter(|(_, grapheme)| grapheme.chars().eq([quote]))
        .map(|(col, _)| col)
        .collect();
    let (open, close) = columns
        .chunks_exact(2)
        .map(|pair| (pair[0], pair[1]))
        .find(|(_, close)| *close >= cursor.col)?;

    let (start, mut end) = if around {
        (open, close + 1)
    } else {
        (open + 1, close)
    };
    if around {
        let blanks = text
            .graphemes(true)
            .skip(end)
            .take_while(|grapheme| class_of(grapheme, false) == Class::Blank)
            .count();
        end += blanks;
    }
    Some(TextRange::charwise(
        Position::new(cursor.line, start),
        Position::new(cursor.line, end),
    ))
}

/// The pair that encloses the cursor, or the pair the cursor is one half of. `count` steps
/// out through that many enclosing pairs.
fn block(
    buffer: &Buffer,
    cursor: Position,
    open: char,
    close: char,
    around: bool,
    count: usize,
) -> Option<TextRange> {
    let mut open_pos = cursor;
    let mut close_pos = cursor;
    for level in 0..count {
        let from = if level == 0 { cursor } else { open_pos };
        open_pos = if grapheme_is(buffer, from, open) && level == 0 {
            from
        } else {
            search_back(buffer, prev_pos(buffer, from)?, open, close)?
        };
        let from = if level == 0 { cursor } else { close_pos };
        close_pos = if grapheme_is(buffer, from, close) && level == 0 {
            from
        } else {
            search_forward(buffer, next_pos(buffer, from)?, open, close)?
        };
    }

    let (start, end) = if around {
        (open_pos, Position::new(close_pos.line, close_pos.col + 1))
    } else {
        (Position::new(open_pos.line, open_pos.col + 1), close_pos)
    };
    Some(TextRange::charwise(start, end))
}

/// Walks back to the `open` that has no `close` of its own in between.
fn search_back(buffer: &Buffer, from: Position, open: char, close: char) -> Option<Position> {
    let mut pos = from;
    let mut depth = 0usize;
    loop {
        if grapheme_is(buffer, pos, close) {
            depth += 1;
        } else if grapheme_is(buffer, pos, open) {
            match depth.checked_sub(1) {
                Some(remaining) => depth = remaining,
                None => return Some(pos),
            }
        }
        pos = prev_pos(buffer, pos)?;
    }
}

/// Walks forward to the `close` that has no `open` of its own in between.
fn search_forward(buffer: &Buffer, from: Position, open: char, close: char) -> Option<Position> {
    let mut pos = from;
    let mut depth = 0usize;
    loop {
        if grapheme_is(buffer, pos, open) {
            depth += 1;
        } else if grapheme_is(buffer, pos, close) {
            match depth.checked_sub(1) {
                Some(remaining) => depth = remaining,
                None => return Some(pos),
            }
        }
        pos = next_pos(buffer, pos)?;
    }
}

fn grapheme_is(buffer: &Buffer, pos: Position, character: char) -> bool {
    buffer
        .grapheme_at(pos)
        .is_some_and(|grapheme| grapheme.chars().eq([character]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolves `keys` — the two keys that follow an operator — and returns the text it spans.
    fn taken(text: &str, cursor: Position, keys: &str, count: Option<usize>) -> Option<String> {
        let buffer = Buffer::new(text);
        let mut keys = keys.chars();
        let around = keys.next() == Some('a');
        let object = TextObject::from_key(keys.next().expect("object key"), around)
            .expect("key should name an object");
        let range = resolve(&buffer, cursor, object, count)?;
        let mut buffer = buffer;
        Some(buffer.delete(range.start, range.end))
    }

    #[test]
    fn iw_takes_the_run_the_cursor_is_in() {
        let text = "foo(bar) baz";
        assert_eq!(
            taken(text, Position::new(0, 1), "iw", None).as_deref(),
            Some("foo")
        );
        assert_eq!(
            taken(text, Position::new(0, 3), "iw", None).as_deref(),
            Some("(")
        );
        assert_eq!(
            taken(text, Position::new(0, 8), "iw", None).as_deref(),
            Some(" ")
        );
    }

    #[test]
    fn big_iw_collapses_punctuation_into_the_word() {
        assert_eq!(
            taken("foo(bar) baz", Position::new(0, 1), "iW", None).as_deref(),
            Some("foo(bar)")
        );
    }

    #[test]
    fn iw_with_a_count_takes_runs_including_the_blanks() {
        assert_eq!(
            taken("foo bar baz", Position::new(0, 0), "iw", Some(2)).as_deref(),
            Some("foo ")
        );
        assert_eq!(
            taken("foo bar baz", Position::new(0, 0), "iw", Some(3)).as_deref(),
            Some("foo bar")
        );
    }

    #[test]
    fn aw_takes_the_word_and_the_whitespace_after_it() {
        assert_eq!(
            taken("foo bar baz", Position::new(0, 1), "aw", None).as_deref(),
            Some("foo ")
        );
    }

    #[test]
    fn aw_falls_back_to_the_whitespace_in_front() {
        assert_eq!(
            taken("foo bar", Position::new(0, 5), "aw", None).as_deref(),
            Some(" bar")
        );
    }

    #[test]
    fn aw_from_whitespace_takes_the_word_behind_it() {
        assert_eq!(
            taken("foo   bar baz", Position::new(0, 4), "aw", None).as_deref(),
            Some("   bar")
        );
    }

    #[test]
    fn aw_with_a_count_takes_whole_words() {
        assert_eq!(
            taken("foo bar baz qux", Position::new(0, 0), "aw", Some(2)).as_deref(),
            Some("foo bar ")
        );
        assert_eq!(
            taken("foo bar baz", Position::new(0, 0), "aw", Some(9)).as_deref(),
            Some("foo bar baz")
        );
    }

    #[test]
    fn word_objects_count_japanese_graphemes() {
        assert_eq!(
            taken("日本語 テキスト です", Position::new(0, 1), "iw", None).as_deref(),
            Some("日本語")
        );
        assert_eq!(
            taken("日本語 テキスト です", Position::new(0, 5), "aw", None).as_deref(),
            Some("テキスト ")
        );
        assert_eq!(
            taken("あ、いう", Position::new(0, 0), "iw", None).as_deref(),
            Some("あ")
        );
    }

    #[test]
    fn word_objects_do_not_resolve_on_an_empty_line() {
        assert_eq!(taken("ab\n\ncd", Position::new(1, 0), "iw", None), None);
    }

    #[test]
    fn quote_objects_take_the_pair_the_cursor_is_in() {
        let text = "say \"hello there\" now";
        assert_eq!(
            taken(text, Position::new(0, 7), "i\"", None).as_deref(),
            Some("hello there")
        );
        assert_eq!(
            taken(text, Position::new(0, 7), "a\"", None).as_deref(),
            Some("\"hello there\" ")
        );
    }

    #[test]
    fn quote_objects_look_forward_from_the_cursor() {
        let text = "say '日本語' now";
        assert_eq!(
            taken(text, Position::new(0, 0), "i'", None).as_deref(),
            Some("日本語")
        );
        assert_eq!(
            taken(text, Position::new(0, 4), "a'", None).as_deref(),
            Some("'日本語' ")
        );
    }

    #[test]
    fn quote_objects_pick_the_pair_after_an_earlier_one() {
        let text = "\"a\" \"b\"";
        assert_eq!(
            taken(text, Position::new(0, 5), "i\"", None).as_deref(),
            Some("b")
        );
    }

    #[test]
    fn an_empty_quote_pair_resolves_to_an_empty_span() {
        let buffer = Buffer::new("x = \"\"");
        let range = resolve(
            &buffer,
            Position::new(0, 5),
            TextObject::from_key('"', false).unwrap(),
            None,
        )
        .expect("the pair should resolve");
        assert!(range.is_empty());
        assert_eq!(range.start, Position::new(0, 5));
    }

    #[test]
    fn an_unclosed_quote_does_not_resolve() {
        assert_eq!(taken("say \"hello", Position::new(0, 6), "i\"", None), None);
    }

    #[test]
    fn block_objects_take_the_enclosing_pair() {
        let text = "foo(bar, baz)";
        assert_eq!(
            taken(text, Position::new(0, 5), "i(", None).as_deref(),
            Some("bar, baz")
        );
        assert_eq!(
            taken(text, Position::new(0, 5), "a)", None).as_deref(),
            Some("(bar, baz)")
        );
        assert_eq!(
            taken(text, Position::new(0, 5), "ib", None).as_deref(),
            Some("bar, baz")
        );
    }

    #[test]
    fn block_objects_resolve_from_either_delimiter() {
        let text = "foo(bar)";
        assert_eq!(
            taken(text, Position::new(0, 3), "i(", None).as_deref(),
            Some("bar")
        );
        assert_eq!(
            taken(text, Position::new(0, 7), "i(", None).as_deref(),
            Some("bar")
        );
    }

    #[test]
    fn block_objects_skip_nested_pairs() {
        let text = "a(b(c)d)e";
        assert_eq!(
            taken(text, Position::new(0, 4), "i(", None).as_deref(),
            Some("c")
        );
        assert_eq!(
            taken(text, Position::new(0, 6), "i(", None).as_deref(),
            Some("b(c)d")
        );
        assert_eq!(
            taken(text, Position::new(0, 4), "i(", Some(2)).as_deref(),
            Some("b(c)d")
        );
    }

    #[test]
    fn block_objects_span_lines() {
        let text = "if {\n  body\n}\n";
        assert_eq!(
            taken(text, Position::new(1, 3), "iB", None).as_deref(),
            Some("\n  body\n")
        );
        assert_eq!(
            taken(text, Position::new(1, 3), "a{", None).as_deref(),
            Some("{\n  body\n}")
        );
    }

    #[test]
    fn angle_and_square_blocks_resolve() {
        assert_eq!(
            taken("<div>", Position::new(0, 2), "i<", None).as_deref(),
            Some("div")
        );
        assert_eq!(
            taken("xs[42]", Position::new(0, 3), "a[", None).as_deref(),
            Some("[42]")
        );
    }

    #[test]
    fn an_unmatched_block_does_not_resolve() {
        assert_eq!(taken("foo(bar", Position::new(0, 5), "i(", None), None);
        assert_eq!(taken("foo bar", Position::new(0, 5), "i(", None), None);
    }
}
