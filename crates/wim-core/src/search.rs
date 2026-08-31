//! Regular expression search over the buffer, behind `/`, `?`, `n`, `N`, `*` and `#`.
//!
//! Patterns are read with the `regex` crate's syntax rather than Vim's: `\(…\)` is written
//! `(…)`, `\|` is written `|`, and a capture is referred to as `$1` in a `:s` replacement.
//! The one setting wim turns on is multi-line matching, so that `^` and `$` mean the start
//! and the end of a line the way they do in Vim rather than of the whole buffer.

use std::fmt;

use regex::{Regex, RegexBuilder};

use crate::buffer::Buffer;
use crate::motion::{Class, class_at};
use crate::position::Position;
use crate::textobject::{self, TextObject, TextObjectKind};

/// The search `n` and `N` repeat: what was looked for, and which way the search ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    /// The pattern, as it was typed.
    pub pattern: String,
    /// Towards the start of the buffer, which `?` searches and `/` does not.
    pub backward: bool,
}

/// Why a search came back with nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchError {
    /// The pattern is not a regular expression.
    InvalidPattern {
        /// The pattern as it was typed.
        pattern: String,
        /// What the regex crate said about it.
        reason: String,
    },
    /// The pattern is a fine regular expression that the buffer does not hold.
    NotFound {
        /// The pattern as it was typed.
        pattern: String,
    },
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPattern { pattern, reason } => {
                write!(f, "{pattern} is not a valid pattern: {reason}")
            }
            Self::NotFound { pattern } => write!(f, "pattern not found: {pattern}"),
        }
    }
}

impl std::error::Error for SearchError {}

/// Compiles `pattern` the way every search in wim reads it.
pub fn compile(pattern: &str, ignore_case: bool) -> Result<Regex, SearchError> {
    RegexBuilder::new(pattern)
        .multi_line(true)
        .case_insensitive(ignore_case)
        .build()
        .map_err(|error| SearchError::InvalidPattern {
            pattern: pattern.to_owned(),
            reason: error.to_string(),
        })
}

/// Where the next match of `pattern` starts, searching from `from` and wrapping around the
/// end of the buffer the way Vim's `wrapscan` does.
///
/// The search starts at the position after `from`, so that repeating it walks through the
/// matches instead of standing still on the one the cursor is already on.
pub fn find(
    buffer: &Buffer,
    from: Position,
    pattern: &str,
    backward: bool,
) -> Result<Position, SearchError> {
    let regex = compile(pattern, false)?;
    let text = buffer.to_string();
    let cursor = byte_of_char(&text, buffer.char_index(from));
    let found = if backward {
        // Matching runs forwards, so the match before the cursor is the last one that starts
        // in front of it, and the wrap is the last match in the buffer.
        let before = regex
            .find_iter(&text)
            .take_while(|found| found.start() < cursor)
            .last();
        before.or_else(|| regex.find_iter(&text).last())
    } else {
        let after = char_after(&text, cursor).and_then(|next| regex.find_at(&text, next));
        after.or_else(|| regex.find(&text))
    };
    let found = found.ok_or_else(|| SearchError::NotFound {
        pattern: pattern.to_owned(),
    })?;
    Ok(buffer.position_at_char(text[..found.start()].chars().count()))
}

/// The pattern `*` and `#` search for: the word under the cursor, bounded so that it matches
/// that word alone rather than every word it is part of. `None` when the cursor is on a blank,
/// which is no word.
pub fn word_pattern(buffer: &Buffer, cursor: Position) -> Option<String> {
    let class = class_at(buffer, cursor, false);
    if class == Class::Blank {
        return None;
    }
    let word = textobject::resolve(
        buffer,
        cursor,
        TextObject {
            kind: TextObjectKind::Word { big: false },
            around: false,
        },
        Some(1),
    )?;
    let text = regex::escape(&buffer.text_between(word.start, word.end));
    // `\b` sits between a word character and a non-word one, so it can only bound a word made
    // of them; a run of symbols is searched for as it stands.
    match class {
        Class::Keyword => Some(format!(r"\b{text}\b")),
        _ => Some(text),
    }
}

/// Byte offset of character `char_index`, or the end of `text` when it holds fewer.
fn byte_of_char(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(byte, _)| byte)
}

/// Byte offset of the character after the one at `byte`, `None` at the end of `text`.
fn char_after(text: &str, byte: usize) -> Option<usize> {
    let character = text[byte..].chars().next()?;
    Some(byte + character.len_utf8())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found_at(text: &str, from: Position, pattern: &str, backward: bool) -> Position {
        find(&Buffer::new(text), from, pattern, backward).expect("the pattern should be found")
    }

    #[test]
    fn a_forward_search_starts_after_the_cursor() {
        let text = "foo bar foo";
        assert_eq!(
            found_at(text, Position::new(0, 0), "foo", false),
            Position::new(0, 8),
            "the match the cursor sits on is stepped over"
        );
        assert_eq!(
            found_at(text, Position::new(0, 4), "foo", false),
            Position::new(0, 8)
        );
    }

    #[test]
    fn a_search_wraps_around_the_end_of_the_buffer() {
        let text = "foo\nbar\nfoo";
        assert_eq!(
            found_at(text, Position::new(2, 0), "foo", false),
            Position::new(0, 0)
        );
        assert_eq!(
            found_at(text, Position::new(0, 0), "foo", true),
            Position::new(2, 0)
        );
    }

    #[test]
    fn a_backward_search_lands_on_the_match_in_front_of_the_cursor() {
        let text = "foo bar foo baz";
        assert_eq!(
            found_at(text, Position::new(0, 12), "foo", true),
            Position::new(0, 8)
        );
        assert_eq!(
            found_at(text, Position::new(0, 8), "foo", true),
            Position::new(0, 0)
        );
    }

    #[test]
    fn line_anchors_match_at_every_line() {
        let text = "one\ntwo\nthree";
        assert_eq!(
            found_at(text, Position::new(0, 0), "^t", false),
            Position::new(1, 0)
        );
        assert_eq!(
            found_at(text, Position::new(0, 0), "o$", false),
            Position::new(1, 2)
        );
    }

    #[test]
    fn a_search_counts_columns_in_graphemes() {
        assert_eq!(
            found_at("あいう\nかきく", Position::new(0, 0), "きく", false),
            Position::new(1, 1)
        );
    }

    #[test]
    fn a_pattern_that_is_not_a_regex_is_reported() {
        let error = find(&Buffer::new("ab"), Position::new(0, 0), "a(", false);
        assert!(
            matches!(error, Err(SearchError::InvalidPattern { .. })),
            "{error:?}"
        );
        let error = find(&Buffer::new("ab"), Position::new(0, 0), "zz", false);
        assert_eq!(
            error,
            Err(SearchError::NotFound {
                pattern: "zz".to_owned()
            })
        );
    }

    #[test]
    fn the_word_under_the_cursor_is_searched_for_on_its_own() {
        let buffer = Buffer::new("foo foobar +=+");
        assert_eq!(
            word_pattern(&buffer, Position::new(0, 1)).as_deref(),
            Some(r"\bfoo\b")
        );
        assert_eq!(
            word_pattern(&buffer, Position::new(0, 3)),
            None,
            "a blank is no word"
        );
        assert_eq!(
            word_pattern(&buffer, Position::new(0, 11)).as_deref(),
            Some(r"\+=\+"),
            "symbols have no word boundary to bind to"
        );
    }
}
