//! Cursor position in a buffer.

/// A place in a buffer, addressed by line and by grapheme cluster within that line.
///
/// Ordering is the reading order of the text, so a range can be normalised with `min`/`max`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Position {
    pub line: usize,
    pub col: usize,
}

impl Position {
    pub fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_by_line_then_col() {
        assert!(Position::new(0, 9) < Position::new(1, 0));
        assert!(Position::new(1, 0) < Position::new(1, 1));
        assert_eq!(Position::default(), Position::new(0, 0));
    }
}
