//! Cursor motions and the ranges operators consume.

use unicode_segmentation::UnicodeSegmentation;

use crate::buffer::Buffer;
use crate::position::Position;

/// A motion the user asked for, before it is applied to a buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// `h`
    Left,
    /// `l`
    Right,
    /// `j`
    Down,
    /// `k`
    Up,
    /// `0`
    LineStart,
    /// `^`
    FirstNonBlank,
    /// `$`
    LineEnd,
    /// `w` / `W`
    WordForward { big: bool },
    /// `b` / `B`
    WordBackward { big: bool },
    /// `e` / `E`
    WordEnd { big: bool },
    /// `gg`
    FirstLine,
    /// `G`
    LastLine,
    /// `f` / `F` / `t` / `T`
    Find(Find),
    /// `;` / `,`
    RepeatFind { reverse: bool },
}

/// A character search inside the current line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Find {
    pub target: char,
    /// `F` / `T` search towards the start of the line.
    pub backward: bool,
    /// `t` / `T` stop next to the match instead of on it.
    pub till: bool,
}

/// How an operator has to read the span between the cursor and a motion target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionKind {
    Charwise { inclusive: bool },
    Linewise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotionTarget {
    pub pos: Position,
    pub kind: MotionKind,
}

/// State that outlives a single motion.
///
/// `desired_col` is the column `j` and `k` aim for, kept across lines that are too short to
/// hold it; `usize::MAX` is the sticky end-of-line that `$` leaves behind. `last_find` is
/// what `;` and `,` repeat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MotionContext {
    pub desired_col: usize,
    pub last_find: Option<Find>,
}

/// Result of resolving a motion.
///
/// `target` is `None` when the motion cannot move, while `context` always carries the state
/// the next motion has to see — a failed `f` still decides what `;` searches for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotionOutcome {
    pub target: Option<MotionTarget>,
    pub context: MotionContext,
}

/// Resolves `motion` against the buffer without touching any editor state.
///
/// `count` is the count the user typed, `None` when they typed none, which `G` reads as the
/// last line and every other motion as 1.
///
/// Targets are clamped to what the motion can reach, except that `l` and `w` may land one
/// column past the last grapheme of a line so that an operator can consume that last
/// grapheme; a caller moving the cursor puts the target through [`Buffer::clamp`].
pub fn resolve(
    buffer: &Buffer,
    cursor: Position,
    motion: Motion,
    count: Option<usize>,
    context: &MotionContext,
) -> MotionOutcome {
    let cursor = buffer.clamp(cursor);
    let n = count.unwrap_or(1).max(1);
    let mut context = *context;
    let last_line = buffer.line_count() - 1;

    if matches!(motion, Motion::Down | Motion::Up) {
        context.desired_col = context.desired_col.max(cursor.col);
    }
    if let Motion::Find(find) = motion {
        context.last_find = Some(find);
    }

    let target = match motion {
        Motion::Left => (cursor.col > 0).then(|| {
            charwise(
                Position::new(cursor.line, cursor.col.saturating_sub(n)),
                false,
            )
        }),
        Motion::Right => {
            let len = buffer.line_len(cursor.line);
            (len > 0)
                .then(|| charwise(Position::new(cursor.line, (cursor.col + n).min(len)), false))
        }
        Motion::Down => (cursor.line + n <= last_line)
            .then(|| linewise(desired(buffer, cursor.line + n, context.desired_col))),
        Motion::Up => (n <= cursor.line)
            .then(|| linewise(desired(buffer, cursor.line - n, context.desired_col))),
        Motion::LineStart => Some(charwise(Position::new(cursor.line, 0), false)),
        Motion::FirstNonBlank => Some(charwise(first_non_blank(buffer, cursor.line), false)),
        Motion::LineEnd => {
            let line = (cursor.line + n - 1).min(last_line);
            Some(charwise(Position::new(line, buffer.last_col(line)), true))
        }
        Motion::WordForward { big } => word_forward(buffer, cursor, n, big),
        Motion::WordBackward { big } => {
            repeat(cursor, n, |pos| word_backward_step(buffer, pos, big))
                .map(|pos| charwise(pos, false))
        }
        Motion::WordEnd { big } => {
            repeat(cursor, n, |pos| word_end_step(buffer, pos, big)).map(|pos| charwise(pos, true))
        }
        Motion::FirstLine => Some(linewise(first_non_blank(
            buffer,
            count.unwrap_or(1).saturating_sub(1).min(last_line),
        ))),
        Motion::LastLine => Some(linewise(first_non_blank(
            buffer,
            count.map_or(last_line, |line| line.saturating_sub(1).min(last_line)),
        ))),
        Motion::Find(find) => find_in_line(buffer, cursor, find, n, false),
        Motion::RepeatFind { reverse } => context.last_find.and_then(|find| {
            let find = Find {
                backward: find.backward != reverse,
                ..find
            };
            find_in_line(buffer, cursor, find, n, true)
        }),
    };

    match motion {
        Motion::Down | Motion::Up => {}
        Motion::LineEnd => context.desired_col = usize::MAX,
        _ => {
            if let Some(target) = target {
                context.desired_col = target.pos.col.min(buffer.last_col(target.pos.line));
            }
        }
    }

    MotionOutcome { target, context }
}

fn charwise(pos: Position, inclusive: bool) -> MotionTarget {
    MotionTarget {
        pos,
        kind: MotionKind::Charwise { inclusive },
    }
}

fn linewise(pos: Position) -> MotionTarget {
    MotionTarget {
        pos,
        kind: MotionKind::Linewise,
    }
}

fn desired(buffer: &Buffer, line: usize, desired_col: usize) -> Position {
    Position::new(line, desired_col.min(buffer.last_col(line)))
}

/// The column `^` reaches on `line`: its first grapheme that is not a blank, or its last
/// column when the whole line is blank.
pub(crate) fn first_non_blank(buffer: &Buffer, line: usize) -> Position {
    let text = buffer.line_text(line);
    let col = text
        .graphemes(true)
        .position(|grapheme| class_of(grapheme, false) != Class::Blank)
        .unwrap_or_else(|| buffer.last_col(line));
    Position::new(line, col)
}

/// Applies `step` up to `n` times, keeping however far it got. `None` when it never moved.
fn repeat(
    cursor: Position,
    n: usize,
    mut step: impl FnMut(Position) -> Option<Position>,
) -> Option<Position> {
    let mut pos = cursor;
    for _ in 0..n {
        match step(pos) {
            Some(next) => pos = next,
            None => break,
        }
    }
    (pos != cursor).then_some(pos)
}

/// The class boundaries `w`, `b` and `e` break words at. `big` collapses keywords and
/// punctuation into one class, which is what `W`, `B` and `E` do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Class {
    Blank,
    Keyword,
    Punct,
}

pub(crate) fn class_of(grapheme: &str, big: bool) -> Class {
    match grapheme.chars().next() {
        None => Class::Blank,
        Some(c) if c.is_whitespace() => Class::Blank,
        Some(_) if big => Class::Keyword,
        Some(c) if c.is_alphanumeric() || c == '_' => Class::Keyword,
        Some(_) => Class::Punct,
    }
}

pub(crate) fn class_at(buffer: &Buffer, pos: Position, big: bool) -> Class {
    buffer
        .grapheme_at(pos)
        .map_or(Class::Blank, |grapheme| class_of(&grapheme, big))
}

fn is_empty_line(buffer: &Buffer, line: usize) -> bool {
    buffer.line_len(line) == 0
}

/// The next grapheme position, stepping over line ends. The column past the last grapheme of
/// a line is not a position of its own.
pub(crate) fn next_pos(buffer: &Buffer, pos: Position) -> Option<Position> {
    if pos.col + 1 < buffer.line_len(pos.line) {
        Some(Position::new(pos.line, pos.col + 1))
    } else if pos.line + 1 < buffer.line_count() {
        Some(Position::new(pos.line + 1, 0))
    } else {
        None
    }
}

/// The previous grapheme position, stepping over line ends.
pub(crate) fn prev_pos(buffer: &Buffer, pos: Position) -> Option<Position> {
    if pos.col > 0 {
        Some(Position::new(pos.line, pos.col - 1))
    } else if pos.line > 0 {
        Some(Position::new(pos.line - 1, buffer.last_col(pos.line - 1)))
    } else {
        None
    }
}

fn word_forward(buffer: &Buffer, cursor: Position, n: usize, big: bool) -> Option<MotionTarget> {
    let mut pos = cursor;
    let mut ran_off = false;
    for _ in 0..n {
        match word_forward_step(buffer, pos, big) {
            Some(next) => pos = next,
            None => {
                ran_off = true;
                break;
            }
        }
    }
    if ran_off {
        let line = buffer.line_count() - 1;
        pos = Position::new(line, buffer.line_len(line));
    }
    (pos != cursor).then(|| charwise(pos, false))
}

/// Start of the next word, which is also every empty line on the way.
fn word_forward_step(buffer: &Buffer, cursor: Position, big: bool) -> Option<Position> {
    let mut pos = cursor;
    let class = class_at(buffer, pos, big);
    if class == Class::Blank || is_empty_line(buffer, pos.line) {
        pos = next_pos(buffer, pos)?;
    } else {
        loop {
            let next = next_pos(buffer, pos)?;
            let crossed_line = next.line != pos.line;
            pos = next;
            if crossed_line || class_at(buffer, pos, big) != class {
                break;
            }
        }
    }
    loop {
        if is_empty_line(buffer, pos.line) || class_at(buffer, pos, big) != Class::Blank {
            return Some(pos);
        }
        pos = next_pos(buffer, pos)?;
    }
}

/// End of the current or next word. Empty lines are not words to `e`.
fn word_end_step(buffer: &Buffer, cursor: Position, big: bool) -> Option<Position> {
    let mut pos = next_pos(buffer, cursor)?;
    while class_at(buffer, pos, big) == Class::Blank {
        pos = next_pos(buffer, pos)?;
    }
    let class = class_at(buffer, pos, big);
    while let Some(next) = next_pos(buffer, pos) {
        if next.line != pos.line || class_at(buffer, next, big) != class {
            break;
        }
        pos = next;
    }
    Some(pos)
}

/// Start of the previous word, which is also every empty line on the way.
fn word_backward_step(buffer: &Buffer, cursor: Position, big: bool) -> Option<Position> {
    let mut pos = prev_pos(buffer, cursor)?;
    loop {
        if is_empty_line(buffer, pos.line) {
            return Some(pos);
        }
        if class_at(buffer, pos, big) != Class::Blank {
            break;
        }
        pos = prev_pos(buffer, pos)?;
    }
    let class = class_at(buffer, pos, big);
    while let Some(prev) = prev_pos(buffer, pos) {
        if prev.line != pos.line || class_at(buffer, prev, big) != class {
            break;
        }
        pos = prev;
    }
    Some(pos)
}

/// `repeated` is set for `;` and `,`, which start one column further out so that repeating a
/// `t` search walks along the line instead of standing still next to the same match.
fn find_in_line(
    buffer: &Buffer,
    cursor: Position,
    find: Find,
    n: usize,
    repeated: bool,
) -> Option<MotionTarget> {
    let text = buffer.line_text(cursor.line);
    let mut encoded = [0u8; 4];
    let needle: &str = find.target.encode_utf8(&mut encoded);
    let skip = usize::from(find.till && repeated);
    let matches: Vec<usize> = text
        .graphemes(true)
        .enumerate()
        .filter(|(_, grapheme)| *grapheme == needle)
        .map(|(col, _)| col)
        .collect();

    let col = if find.backward {
        let limit = cursor.col.checked_sub(1 + skip)?;
        let found = *matches
            .iter()
            .rev()
            .filter(|col| **col <= limit)
            .nth(n - 1)?;
        if find.till { found + 1 } else { found }
    } else {
        let from = cursor.col + 1 + skip;
        let found = *matches.iter().filter(|col| **col >= from).nth(n - 1)?;
        if find.till { found - 1 } else { found }
    };

    // `t` and `T` fail rather than stand still when the match sits next to the cursor.
    (col != cursor.col).then(|| charwise(Position::new(cursor.line, col), !find.backward))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolves `motion` and returns where the cursor ends up, `None` when it cannot move.
    fn moved(buffer: &Buffer, cursor: Position, motion: Motion) -> Option<Position> {
        moved_n(buffer, cursor, motion, None)
    }

    fn moved_n(
        buffer: &Buffer,
        cursor: Position,
        motion: Motion,
        count: Option<usize>,
    ) -> Option<Position> {
        let outcome = resolve(buffer, cursor, motion, count, &MotionContext::default());
        outcome.target.map(|target| buffer.clamp(target.pos))
    }

    fn kind(buffer: &Buffer, cursor: Position, motion: Motion) -> MotionKind {
        resolve(buffer, cursor, motion, None, &MotionContext::default())
            .target
            .expect("motion should resolve")
            .kind
    }

    const WORD: Motion = Motion::WordForward { big: false };
    const BACK: Motion = Motion::WordBackward { big: false };
    const END: Motion = Motion::WordEnd { big: false };
    const BIG_WORD: Motion = Motion::WordForward { big: true };
    const BIG_BACK: Motion = Motion::WordBackward { big: true };
    const BIG_END: Motion = Motion::WordEnd { big: true };

    #[test]
    fn h_and_l_walk_graphemes() {
        let buffer = Buffer::new("あいうえお");
        let start = Position::new(0, 2);
        assert_eq!(
            moved(&buffer, start, Motion::Left),
            Some(Position::new(0, 1))
        );
        assert_eq!(
            moved(&buffer, start, Motion::Right),
            Some(Position::new(0, 3))
        );
        assert_eq!(
            moved_n(&buffer, start, Motion::Left, Some(2)),
            Some(Position::new(0, 0))
        );
        assert_eq!(
            moved_n(&buffer, start, Motion::Right, Some(2)),
            Some(Position::new(0, 4))
        );
    }

    #[test]
    fn h_and_l_stop_at_the_line_edges_without_wrapping() {
        let buffer = Buffer::new("ab\ncd");
        assert_eq!(moved(&buffer, Position::new(1, 0), Motion::Left), None);
        assert_eq!(
            moved_n(&buffer, Position::new(0, 1), Motion::Left, Some(9)),
            Some(Position::new(0, 0))
        );
        assert_eq!(
            moved_n(&buffer, Position::new(0, 0), Motion::Right, Some(9)),
            Some(Position::new(0, 1))
        );
        assert_eq!(
            moved(&buffer, Position::new(0, 1), Motion::Right),
            Some(Position::new(0, 1))
        );
    }

    #[test]
    fn l_targets_one_past_the_last_grapheme_so_operators_can_take_it() {
        let buffer = Buffer::new("あい");
        let outcome = resolve(
            &buffer,
            Position::new(0, 1),
            Motion::Right,
            None,
            &MotionContext::default(),
        );
        let target = outcome
            .target
            .expect("l should resolve on a non-empty line");
        assert_eq!(target.pos, Position::new(0, 2));
        assert_eq!(target.kind, MotionKind::Charwise { inclusive: false });
        assert_eq!(buffer.clamp(target.pos), Position::new(0, 1));
    }

    #[test]
    fn l_fails_on_an_empty_line() {
        let buffer = Buffer::new("\nab");
        assert_eq!(moved(&buffer, Position::new(0, 0), Motion::Right), None);
    }

    #[test]
    fn j_and_k_keep_the_desired_column_over_shorter_lines() {
        let buffer = Buffer::new("あいうえお\nab\n日本語テキスト");
        let mut context = MotionContext::default();
        let mut cursor = Position::new(0, 4);

        let outcome = resolve(&buffer, cursor, Motion::Down, None, &context);
        context = outcome.context;
        cursor = buffer.clamp(outcome.target.unwrap().pos);
        assert_eq!(cursor, Position::new(1, 1));
        assert_eq!(context.desired_col, 4);

        let outcome = resolve(&buffer, cursor, Motion::Down, None, &context);
        context = outcome.context;
        cursor = buffer.clamp(outcome.target.unwrap().pos);
        assert_eq!(cursor, Position::new(2, 4));

        let outcome = resolve(&buffer, cursor, Motion::Up, Some(2), &context);
        cursor = buffer.clamp(outcome.target.unwrap().pos);
        assert_eq!(cursor, Position::new(0, 4));
    }

    #[test]
    fn j_and_k_are_linewise_and_fail_at_the_buffer_edges() {
        let buffer = Buffer::new("a\nb");
        assert_eq!(
            kind(&buffer, Position::new(0, 0), Motion::Down),
            MotionKind::Linewise
        );
        assert_eq!(moved(&buffer, Position::new(1, 0), Motion::Down), None);
        assert_eq!(moved(&buffer, Position::new(0, 0), Motion::Up), None);
        assert_eq!(
            moved_n(&buffer, Position::new(0, 0), Motion::Down, Some(2)),
            None
        );
    }

    #[test]
    fn dollar_makes_the_desired_column_stick_to_the_line_end() {
        let buffer = Buffer::new("あいうえお\nab");
        let outcome = resolve(
            &buffer,
            Position::new(0, 0),
            Motion::LineEnd,
            None,
            &MotionContext::default(),
        );
        let target = outcome.target.unwrap();
        assert_eq!(target.pos, Position::new(0, 4));
        assert_eq!(target.kind, MotionKind::Charwise { inclusive: true });
        assert_eq!(outcome.context.desired_col, usize::MAX);

        let outcome = resolve(
            &buffer,
            Position::new(0, 4),
            Motion::Down,
            None,
            &outcome.context,
        );
        assert_eq!(outcome.target.unwrap().pos, Position::new(1, 1));
    }

    #[test]
    fn dollar_with_a_count_reaches_the_end_of_a_later_line() {
        let buffer = Buffer::new("ab\ncdef\ngh");
        assert_eq!(
            moved_n(&buffer, Position::new(0, 0), Motion::LineEnd, Some(2)),
            Some(Position::new(1, 3))
        );
        assert_eq!(
            moved_n(&buffer, Position::new(0, 0), Motion::LineEnd, Some(9)),
            Some(Position::new(2, 1))
        );
    }

    #[test]
    fn zero_and_caret_find_the_line_start() {
        let buffer = Buffer::new("  \tあいう");
        assert_eq!(
            moved(&buffer, Position::new(0, 5), Motion::LineStart),
            Some(Position::new(0, 0))
        );
        assert_eq!(
            moved(&buffer, Position::new(0, 5), Motion::FirstNonBlank),
            Some(Position::new(0, 3))
        );
    }

    #[test]
    fn caret_on_a_blank_only_line_stops_on_the_last_grapheme() {
        let buffer = Buffer::new("   ");
        assert_eq!(
            moved(&buffer, Position::new(0, 0), Motion::FirstNonBlank),
            Some(Position::new(0, 2))
        );
    }

    #[test]
    fn w_stops_at_class_boundaries() {
        let buffer = Buffer::new("foo(bar) baz");
        let steps = [
            Position::new(0, 3),
            Position::new(0, 4),
            Position::new(0, 7),
            Position::new(0, 9),
        ];
        let mut cursor = Position::new(0, 0);
        for expected in steps {
            cursor = moved(&buffer, cursor, WORD).expect("w should move");
            assert_eq!(cursor, expected);
        }
    }

    #[test]
    fn big_w_only_stops_at_blanks() {
        let buffer = Buffer::new("foo(bar) baz");
        assert_eq!(
            moved(&buffer, Position::new(0, 0), BIG_WORD),
            Some(Position::new(0, 9))
        );
        assert_eq!(
            moved(&buffer, Position::new(0, 9), BIG_BACK),
            Some(Position::new(0, 0))
        );
        assert_eq!(
            moved(&buffer, Position::new(0, 0), BIG_END),
            Some(Position::new(0, 7))
        );
    }

    #[test]
    fn word_motions_treat_japanese_as_keyword_graphemes() {
        let buffer = Buffer::new("日本語 テキスト です");
        assert_eq!(
            moved(&buffer, Position::new(0, 0), WORD),
            Some(Position::new(0, 4))
        );
        assert_eq!(
            moved_n(&buffer, Position::new(0, 0), WORD, Some(2)),
            Some(Position::new(0, 9))
        );
        assert_eq!(
            moved(&buffer, Position::new(0, 0), END),
            Some(Position::new(0, 2))
        );
        assert_eq!(
            moved(&buffer, Position::new(0, 9), BACK),
            Some(Position::new(0, 4))
        );
    }

    #[test]
    fn word_motions_cross_lines() {
        let buffer = Buffer::new("あい うえ\nかき");
        assert_eq!(
            moved_n(&buffer, Position::new(0, 0), WORD, Some(2)),
            Some(Position::new(1, 0))
        );
        assert_eq!(
            moved(&buffer, Position::new(1, 0), BACK),
            Some(Position::new(0, 3))
        );
        assert_eq!(
            moved(&buffer, Position::new(0, 4), END),
            Some(Position::new(1, 1))
        );
    }

    #[test]
    fn w_and_b_stop_on_an_empty_line_but_e_skips_it() {
        let buffer = Buffer::new("ab\n\ncd");
        assert_eq!(
            moved(&buffer, Position::new(0, 0), WORD),
            Some(Position::new(1, 0))
        );
        assert_eq!(
            moved(&buffer, Position::new(2, 0), BACK),
            Some(Position::new(1, 0))
        );
        assert_eq!(
            moved(&buffer, Position::new(0, 0), END),
            Some(Position::new(0, 1))
        );
        assert_eq!(
            moved(&buffer, Position::new(0, 1), END),
            Some(Position::new(2, 1))
        );
    }

    #[test]
    fn w_reaches_past_the_last_grapheme_at_the_end_of_the_buffer() {
        let buffer = Buffer::new("あい うえ");
        let outcome = resolve(
            &buffer,
            Position::new(0, 3),
            WORD,
            None,
            &MotionContext::default(),
        );
        let target = outcome.target.expect("w should resolve at the last word");
        assert_eq!(target.pos, Position::new(0, 5));
        assert_eq!(buffer.clamp(target.pos), Position::new(0, 4));
        assert_eq!(
            moved(&buffer, Position::new(0, 4), WORD),
            Some(Position::new(0, 4)),
            "the cursor has nowhere left to go, only an operator sees the extra column"
        );
    }

    #[test]
    fn e_is_inclusive_and_w_and_b_are_exclusive() {
        let buffer = Buffer::new("foo bar");
        let cursor = Position::new(0, 0);
        assert_eq!(
            kind(&buffer, cursor, END),
            MotionKind::Charwise { inclusive: true }
        );
        assert_eq!(
            kind(&buffer, cursor, WORD),
            MotionKind::Charwise { inclusive: false }
        );
        assert_eq!(
            kind(&buffer, Position::new(0, 4), BACK),
            MotionKind::Charwise { inclusive: false }
        );
    }

    #[test]
    fn b_and_e_fail_at_the_buffer_edges() {
        let buffer = Buffer::new("ab");
        assert_eq!(moved(&buffer, Position::new(0, 0), BACK), None);
        assert_eq!(moved(&buffer, Position::new(0, 1), END), None);
    }

    #[test]
    fn gg_and_g_jump_to_the_first_non_blank_of_a_line() {
        let buffer = Buffer::new("  あい\nbcd\n\t えふ");
        assert_eq!(
            moved(&buffer, Position::new(2, 2), Motion::FirstLine),
            Some(Position::new(0, 2))
        );
        assert_eq!(
            moved(&buffer, Position::new(0, 0), Motion::LastLine),
            Some(Position::new(2, 2))
        );
        assert_eq!(
            moved_n(&buffer, Position::new(0, 0), Motion::LastLine, Some(2)),
            Some(Position::new(1, 0))
        );
        assert_eq!(
            moved_n(&buffer, Position::new(0, 0), Motion::FirstLine, Some(2)),
            Some(Position::new(1, 0))
        );
        assert_eq!(
            moved_n(&buffer, Position::new(0, 0), Motion::LastLine, Some(99)),
            Some(Position::new(2, 2))
        );
        assert_eq!(
            kind(&buffer, Position::new(0, 0), Motion::LastLine),
            MotionKind::Linewise
        );
    }

    #[test]
    fn f_and_t_search_forward_within_the_line() {
        let buffer = Buffer::new("あかいあおい");
        let find = |target, backward, till| {
            Motion::Find(Find {
                target,
                backward,
                till,
            })
        };
        assert_eq!(
            moved(&buffer, Position::new(0, 0), find('い', false, false)),
            Some(Position::new(0, 2))
        );
        assert_eq!(
            moved_n(
                &buffer,
                Position::new(0, 0),
                find('い', false, false),
                Some(2)
            ),
            Some(Position::new(0, 5))
        );
        assert_eq!(
            moved(&buffer, Position::new(0, 0), find('い', false, true)),
            Some(Position::new(0, 1))
        );
        assert_eq!(
            moved(&buffer, Position::new(0, 0), find('ん', false, false)),
            None
        );
    }

    #[test]
    fn f_and_t_search_backward_within_the_line() {
        let buffer = Buffer::new("あかいあおい");
        let find = |target, backward, till| {
            Motion::Find(Find {
                target,
                backward,
                till,
            })
        };
        assert_eq!(
            moved(&buffer, Position::new(0, 5), find('あ', true, false)),
            Some(Position::new(0, 3))
        );
        assert_eq!(
            moved_n(
                &buffer,
                Position::new(0, 5),
                find('あ', true, false),
                Some(2)
            ),
            Some(Position::new(0, 0))
        );
        assert_eq!(
            moved(&buffer, Position::new(0, 5), find('あ', true, true)),
            Some(Position::new(0, 4))
        );
    }

    #[test]
    fn find_is_inclusive_forward_and_exclusive_backward() {
        let buffer = Buffer::new("abcabc");
        assert_eq!(
            kind(
                &buffer,
                Position::new(0, 0),
                Motion::Find(Find {
                    target: 'c',
                    backward: false,
                    till: false
                })
            ),
            MotionKind::Charwise { inclusive: true }
        );
        assert_eq!(
            kind(
                &buffer,
                Position::new(0, 5),
                Motion::Find(Find {
                    target: 'a',
                    backward: true,
                    till: false
                })
            ),
            MotionKind::Charwise { inclusive: false }
        );
    }

    #[test]
    fn find_does_not_leave_the_line() {
        let buffer = Buffer::new("abc\nxbz");
        assert_eq!(
            moved(
                &buffer,
                Position::new(0, 0),
                Motion::Find(Find {
                    target: 'x',
                    backward: false,
                    till: false
                })
            ),
            None
        );
    }

    #[test]
    fn t_fails_when_the_match_is_next_to_the_cursor() {
        let buffer = Buffer::new("abc");
        assert_eq!(
            moved(
                &buffer,
                Position::new(0, 0),
                Motion::Find(Find {
                    target: 'b',
                    backward: false,
                    till: false
                })
            ),
            Some(Position::new(0, 1))
        );
        assert_eq!(
            moved(
                &buffer,
                Position::new(0, 0),
                Motion::Find(Find {
                    target: 'b',
                    backward: false,
                    till: true
                })
            ),
            None
        );
    }

    #[test]
    fn semicolon_repeats_and_comma_reverses_the_last_find() {
        let buffer = Buffer::new("あかいあおい");
        let outcome = resolve(
            &buffer,
            Position::new(0, 0),
            Motion::Find(Find {
                target: 'い',
                backward: false,
                till: false,
            }),
            None,
            &MotionContext::default(),
        );
        let context = outcome.context;
        assert_eq!(outcome.target.unwrap().pos, Position::new(0, 2));

        let repeated = resolve(
            &buffer,
            Position::new(0, 2),
            Motion::RepeatFind { reverse: false },
            None,
            &context,
        );
        assert_eq!(repeated.target.unwrap().pos, Position::new(0, 5));
        assert_eq!(repeated.context.last_find, context.last_find);

        let reversed = resolve(
            &buffer,
            Position::new(0, 5),
            Motion::RepeatFind { reverse: true },
            None,
            &context,
        );
        let target = reversed.target.unwrap();
        assert_eq!(target.pos, Position::new(0, 2));
        assert_eq!(target.kind, MotionKind::Charwise { inclusive: false });
    }

    #[test]
    fn repeating_a_till_search_walks_past_the_adjacent_match() {
        let buffer = Buffer::new("ab.c.d");
        let outcome = resolve(
            &buffer,
            Position::new(0, 0),
            Motion::Find(Find {
                target: '.',
                backward: false,
                till: true,
            }),
            None,
            &MotionContext::default(),
        );
        assert_eq!(outcome.target.unwrap().pos, Position::new(0, 1));

        let repeated = resolve(
            &buffer,
            Position::new(0, 1),
            Motion::RepeatFind { reverse: false },
            None,
            &outcome.context,
        );
        assert_eq!(repeated.target.unwrap().pos, Position::new(0, 3));
    }

    #[test]
    fn a_failed_find_still_decides_what_semicolon_repeats() {
        let buffer = Buffer::new("abc\nxbx");
        let outcome = resolve(
            &buffer,
            Position::new(0, 0),
            Motion::Find(Find {
                target: 'x',
                backward: false,
                till: false,
            }),
            None,
            &MotionContext::default(),
        );
        assert!(outcome.target.is_none());
        assert_eq!(
            outcome.context.last_find,
            Some(Find {
                target: 'x',
                backward: false,
                till: false
            })
        );

        let repeated = resolve(
            &buffer,
            Position::new(1, 0),
            Motion::RepeatFind { reverse: false },
            None,
            &outcome.context,
        );
        assert_eq!(repeated.target.unwrap().pos, Position::new(1, 2));
    }

    #[test]
    fn repeat_without_a_previous_find_does_nothing() {
        let buffer = Buffer::new("abc");
        let outcome = resolve(
            &buffer,
            Position::new(0, 0),
            Motion::RepeatFind { reverse: false },
            None,
            &MotionContext::default(),
        );
        assert!(outcome.target.is_none());
        assert_eq!(outcome.context, MotionContext::default());
    }

    #[test]
    fn motions_start_from_a_cursor_clamped_into_the_buffer() {
        let buffer = Buffer::new("あい\nう");
        assert_eq!(
            moved(&buffer, Position::new(9, 9), Motion::LineStart),
            Some(Position::new(1, 0))
        );
    }
}
