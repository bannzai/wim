//! The editor state machine: keys in, buffer and cursor out.

use crate::buffer::Buffer;
use crate::command::{Command, InsertAnchor, Operator, OperatorTarget};
use crate::effect::Effect;
use crate::grammar::Grammar;
use crate::key::{KeyCode, KeyEvent, KeyParseError, parse_keys};
use crate::mode::Mode;
use crate::motion::{self, Motion, MotionContext, MotionKind};
use crate::position::Position;
use crate::textobject::{self, TextObject, TextObjectKind, TextRange};

/// An operator together with the span it was resolved against.
///
/// Operators do not touch the buffer yet — deleting, changing and yanking arrive with the
/// operator issue. Until then the editor keeps the last span it worked out so that hosts and
/// tests can see what the grammar resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedOperation {
    /// The operator that was typed.
    pub operator: Operator,
    /// The text it would act on.
    pub range: TextRange,
}

/// A buffer, a cursor and the mode that decides what keys mean.
///
/// This is the whole boundary to a host: feed it keys with [`Editor::handle_key`], read the
/// buffer and the cursor back, and carry out the [`Effect`]s it returns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Editor {
    buffer: Buffer,
    cursor: Position,
    mode: Mode,
    grammar: Grammar,
    motion_context: MotionContext,
    visual_anchor: Option<Position>,
    last_operation: Option<ResolvedOperation>,
}

impl Editor {
    /// An editor over `text`, in Normal mode with the cursor at the start.
    pub fn new(text: &str) -> Self {
        Self {
            buffer: Buffer::new(text),
            ..Self::default()
        }
    }

    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    /// The text being edited, as it would be written back out.
    pub fn text(&self) -> String {
        self.buffer.to_string()
    }

    pub fn cursor(&self) -> Position {
        self.cursor
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The selection, both ends inclusive, `None` outside Visual mode.
    ///
    /// Operators over a selection come with the operator issue; for now the selection only
    /// follows the cursor.
    pub fn selection(&self) -> Option<(Position, Position)> {
        let anchor = self.visual_anchor?;
        Some((anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    /// The last operator the grammar resolved, and the span it would act on.
    pub fn last_operation(&self) -> Option<ResolvedOperation> {
        self.last_operation
    }

    /// Reads one key in the current mode.
    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if self.mode == Mode::Insert {
            return self.handle_insert_key(key);
        }
        let command = self.grammar.feed(key, self.mode);
        let effects = self.apply(command);
        if self.mode != Mode::Insert {
            self.mode = if self.grammar.is_operator_pending() {
                Mode::OperatorPending
            } else if self.visual_anchor.is_some() {
                Mode::Visual
            } else {
                Mode::Normal
            };
        }
        effects
    }

    /// Reads a whole key string, such as `ciwfoo<Esc>`.
    pub fn handle_keys(&mut self, keys: &str) -> Result<Vec<Effect>, KeyParseError> {
        let mut effects = Vec::new();
        for key in parse_keys(keys)? {
            effects.append(&mut self.handle_key(key));
        }
        Ok(effects)
    }

    /// Works out the span `operator` would act on, `None` when the target resolves to
    /// nothing — a motion that cannot move, or a text object the cursor is not in.
    ///
    /// The span is not applied to the buffer: that is the operator issue's job. `c` on `w`
    /// is the one place where the operator changes the span, because Vim's `cw` stops at the
    /// end of the word rather than at the start of the next one.
    pub fn resolve_operator_target(
        &self,
        operator: Operator,
        count: Option<usize>,
        target: OperatorTarget,
    ) -> Option<TextRange> {
        match target {
            OperatorTarget::Lines => {
                let last = (self.cursor.line + count.unwrap_or(1).max(1) - 1)
                    .min(self.buffer.line_count() - 1);
                Some(TextRange::lines(&self.buffer, self.cursor.line, last))
            }
            OperatorTarget::TextObject(object) => {
                textobject::resolve(&self.buffer, self.cursor, object, count)
            }
            OperatorTarget::Motion(motion) => self.motion_range(operator, motion, count),
        }
    }

    fn motion_range(
        &self,
        operator: Operator,
        motion: Motion,
        count: Option<usize>,
    ) -> Option<TextRange> {
        if let (Operator::Change, Motion::WordForward { big }) = (operator, motion)
            && motion::class_at(&self.buffer, self.cursor, big) != motion::Class::Blank
        {
            return self.change_word_range(big, count);
        }
        let target = motion::resolve(
            &self.buffer,
            self.cursor,
            motion,
            count,
            &self.motion_context,
        )
        .target?;
        match target.kind {
            MotionKind::Linewise => Some(TextRange::lines(
                &self.buffer,
                self.cursor.line.min(target.pos.line),
                self.cursor.line.max(target.pos.line),
            )),
            MotionKind::Charwise { inclusive } => {
                let start = self.cursor.min(target.pos);
                let mut end = self.cursor.max(target.pos);
                if inclusive {
                    end.col += 1;
                }
                Some(TextRange::charwise(start, end))
            }
        }
    }

    /// `cw` on a word changes to the end of that word, so that it takes only the last
    /// character when the cursor sits on it. Further counts walk on like `e`.
    fn change_word_range(&self, big: bool, count: Option<usize>) -> Option<TextRange> {
        let word = textobject::resolve(
            &self.buffer,
            self.cursor,
            TextObject {
                kind: TextObjectKind::Word { big },
                around: false,
            },
            Some(1),
        )?;
        let count = count.unwrap_or(1).max(1);
        if count == 1 {
            return Some(TextRange::charwise(self.cursor, word.end));
        }
        let last_of_word = Position::new(word.end.line, word.end.col - 1);
        let mut end = motion::resolve(
            &self.buffer,
            last_of_word,
            Motion::WordEnd { big },
            Some(count - 1),
            &self.motion_context,
        )
        .target?
        .pos;
        end.col += 1;
        Some(TextRange::charwise(self.cursor, end))
    }

    fn apply(&mut self, command: Command) -> Vec<Effect> {
        match command {
            Command::Move { motion, count } => self.move_cursor(motion, count),
            Command::Operate {
                operator,
                count,
                target,
            } => self.record_operation(operator, count, target),
            Command::DeleteChar { before, count } => {
                let motion = if before { Motion::Left } else { Motion::Right };
                self.record_operation(Operator::Delete, count, OperatorTarget::Motion(motion));
            }
            Command::EnterInsert(anchor) => self.enter_insert(anchor),
            Command::ToggleVisual => {
                self.visual_anchor = match self.visual_anchor {
                    Some(_) => None,
                    None => Some(self.cursor),
                };
            }
            Command::Cancel => self.visual_anchor = None,
            Command::Pending => {}
            Command::Rejected(key) => {
                return vec![Effect::Error(format!(
                    "{key} does nothing in {} mode",
                    self.mode.label()
                ))];
            }
        }
        Vec::new()
    }

    fn move_cursor(&mut self, motion: Motion, count: Option<usize>) {
        let outcome = motion::resolve(
            &self.buffer,
            self.cursor,
            motion,
            count,
            &self.motion_context,
        );
        self.motion_context = outcome.context;
        if let Some(target) = outcome.target {
            self.cursor = self.buffer.clamp(target.pos);
        }
    }

    fn record_operation(
        &mut self,
        operator: Operator,
        count: Option<usize>,
        target: OperatorTarget,
    ) {
        if let OperatorTarget::Motion(motion) = target {
            // A search the operator consumed is still what `;` repeats afterwards.
            self.motion_context = motion::resolve(
                &self.buffer,
                self.cursor,
                motion,
                count,
                &self.motion_context,
            )
            .context;
        }
        self.last_operation = self
            .resolve_operator_target(operator, count, target)
            .map(|range| ResolvedOperation { operator, range });
    }

    fn enter_insert(&mut self, anchor: InsertAnchor) {
        match anchor {
            InsertAnchor::BeforeCursor => {}
            InsertAnchor::FirstNonBlank => self.move_cursor(Motion::FirstNonBlank, None),
            InsertAnchor::AfterCursor => {
                if self.buffer.line_len(self.cursor.line) > 0 {
                    self.cursor.col += 1;
                }
            }
            InsertAnchor::LineEnd => self.cursor.col = self.buffer.line_len(self.cursor.line),
            InsertAnchor::LineBelow => {
                let line = self.cursor.line;
                self.insert_line_break(Position::new(line, self.buffer.line_len(line)));
                self.cursor = Position::new(line + 1, 0);
            }
            InsertAnchor::LineAbove => {
                let at = Position::new(self.cursor.line, 0);
                self.insert_line_break(at);
                self.cursor = at;
            }
        }
        self.visual_anchor = None;
        self.mode = Mode::Insert;
    }

    fn handle_insert_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                // Leaving Insert mode steps back onto the last character typed.
                self.cursor = self.buffer.clamp(Position::new(
                    self.cursor.line,
                    self.cursor.col.saturating_sub(1),
                ));
                self.motion_context.desired_col = self.cursor.col;
            }
            KeyCode::Enter => {
                self.insert_line_break(self.cursor);
                self.cursor = Position::new(self.cursor.line + 1, 0);
            }
            KeyCode::Backspace => self.backspace(),
            KeyCode::Tab => self.insert_text("\t"),
            KeyCode::Char(character) if !key.ctrl => {
                self.insert_text(character.encode_utf8(&mut [0u8; 4]))
            }
            KeyCode::Char(_) => {
                return vec![Effect::Error(format!(
                    "{key} does nothing in {} mode",
                    Mode::Insert.label()
                ))];
            }
        }
        Vec::new()
    }

    fn insert_text(&mut self, text: &str) {
        // The cursor is tracked in characters over the edit because inserting a combining
        // mark grows the grapheme the cursor is on instead of adding one.
        let index = self.buffer.char_index(self.cursor) + text.chars().count();
        self.buffer.insert(self.cursor, text);
        self.cursor = self.buffer.position_at_char(index);
    }

    /// Inserts a line break, plus the newline that terminates the buffer when the break lands
    /// at its very end.
    ///
    /// A trailing newline terminates the last line rather than starting an empty one, so an
    /// empty last line exists only in text that ends with a newline. Opening a line at the
    /// end of a buffer that had none therefore gives it one.
    fn insert_line_break(&mut self, at: Position) {
        let at_buffer_end = at.line + 1 == self.buffer.line_count()
            && at.col >= self.buffer.line_len(at.line)
            && !self.buffer.has_trailing_newline();
        self.buffer
            .insert(at, if at_buffer_end { "\n\n" } else { "\n" });
    }

    fn backspace(&mut self) {
        let start = match self.cursor.col.checked_sub(1) {
            Some(col) => Position::new(self.cursor.line, col),
            None => match self.cursor.line.checked_sub(1) {
                // Joining lines: the only thing between them is the line break.
                Some(line) => Position::new(line, self.buffer.line_len(line)),
                None => return,
            },
        };
        self.buffer.delete(start, self.cursor);
        self.cursor = start;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `keys` over `text` and returns the editor they left behind.
    fn run(text: &str, keys: &str) -> Editor {
        let mut editor = Editor::new(text);
        editor.handle_keys(keys).expect("key string should parse");
        editor
    }

    /// The text the operator `keys` resolved to, and whether the span is linewise. A
    /// linewise span holds the lines without the line break that terminates the last one.
    fn operated(text: &str, keys: &str) -> (Operator, String, bool) {
        let editor = run(text, keys);
        let operation = editor
            .last_operation()
            .expect("the keys should resolve an operator");
        let mut buffer = Buffer::new(text);
        let taken = buffer.delete(operation.range.start, operation.range.end);
        (operation.operator, taken, operation.range.linewise)
    }

    #[test]
    fn motions_move_the_cursor_with_their_counts() {
        assert_eq!(run("foo bar baz", "2w").cursor(), Position::new(0, 8));
        assert_eq!(run("foo bar baz", "$").cursor(), Position::new(0, 10));
        assert_eq!(
            run("あいう\nかきく\nさしす", "2j").cursor(),
            Position::new(2, 0)
        );
        assert_eq!(run("あいう\nかきく", "jl0").cursor(), Position::new(1, 0));
        assert_eq!(run("foo bar", "$b").cursor(), Position::new(0, 4));
        assert_eq!(run("a\nb\nc", "G").cursor(), Position::new(2, 0));
        assert_eq!(run("a\nb\nc", "2gg").cursor(), Position::new(1, 0));
        assert_eq!(run("abcabc", "2fb").cursor(), Position::new(0, 4));
        assert_eq!(run("abcabc", "fb;").cursor(), Position::new(0, 4));
    }

    #[test]
    fn a_motion_that_cannot_move_leaves_the_cursor_alone() {
        let editor = run("abc", "k");
        assert_eq!(editor.cursor(), Position::new(0, 0));
        assert_eq!(editor.mode(), Mode::Normal);
    }

    #[test]
    fn an_unknown_key_reports_an_error_without_moving() {
        let mut editor = Editor::new("abc");
        let effects = editor.handle_keys("z").expect("key string should parse");
        assert_eq!(effects.len(), 1, "{effects:?}");
        assert!(matches!(effects[0], Effect::Error(_)));
        assert_eq!(editor.cursor(), Position::new(0, 0));
    }

    #[test]
    fn insert_mode_is_entered_at_the_place_the_key_names() {
        for (keys, cursor) in [
            ("i", Position::new(0, 2)),
            ("I", Position::new(0, 1)),
            ("a", Position::new(0, 3)),
            ("A", Position::new(0, 6)),
        ] {
            let editor = run(" foo  ", &format!("2l{keys}"));
            assert_eq!(editor.cursor(), cursor, "{keys}");
            assert_eq!(editor.mode(), Mode::Insert, "{keys}");
        }
    }

    #[test]
    fn typing_in_insert_mode_edits_the_buffer() {
        let editor = run("bar", "ifoo ");
        assert_eq!(editor.text(), "foo bar");
        assert_eq!(editor.cursor(), Position::new(0, 4));
        assert_eq!(run("bar", "ciwfoo<Esc>").mode(), Mode::Normal);
    }

    #[test]
    fn escape_leaves_insert_mode_one_column_back() {
        let editor = run("bar", "afoo<Esc>");
        assert_eq!(editor.text(), "bfooar");
        assert_eq!(editor.cursor(), Position::new(0, 3));
        assert_eq!(editor.mode(), Mode::Normal);

        let editor = run("bar", "i<Esc>");
        assert_eq!(editor.cursor(), Position::new(0, 0), "column 0 stays put");
    }

    #[test]
    fn insert_mode_handles_japanese_text() {
        let editor = run("です", "iこんにちは<Esc>");
        assert_eq!(editor.text(), "こんにちはです");
        assert_eq!(editor.cursor(), Position::new(0, 4));
    }

    #[test]
    fn backspace_deletes_and_joins_lines() {
        assert_eq!(run("あい", "aうえ<BS><Esc>").text(), "あうい");
        assert_eq!(run("ab", "i<BS>x<Esc>").text(), "xab");

        let editor = run("ab\ncd", "ji<BS>X<Esc>");
        assert_eq!(editor.text(), "abXcd");
        assert_eq!(editor.cursor(), Position::new(0, 2));
    }

    #[test]
    fn enter_splits_the_line_and_tab_inserts_a_tab() {
        let editor = run("abcd", "lli<CR><Tab><Esc>");
        assert_eq!(editor.text(), "ab\n\tcd");
        assert_eq!(editor.cursor(), Position::new(1, 0));
    }

    #[test]
    fn enter_at_the_end_of_the_buffer_keeps_the_new_line() {
        let editor = run("ab", "A<CR>c<Esc>");
        assert_eq!(editor.text(), "ab\nc\n");
        assert_eq!(editor.cursor(), Position::new(1, 0));
    }

    #[test]
    fn o_opens_a_line_below_and_uppercase_o_above() {
        let editor = run("ab\ncd", "oxy<Esc>");
        assert_eq!(editor.text(), "ab\nxy\ncd");
        assert_eq!(editor.cursor(), Position::new(1, 1));

        let editor = run("ab\ncd", "Oxy<Esc>");
        assert_eq!(editor.text(), "xy\nab\ncd");

        let editor = run("ab\ncd", "jozz<Esc>");
        assert_eq!(editor.text(), "ab\ncd\nzz\n");
        assert_eq!(editor.cursor(), Position::new(2, 1));
    }

    #[test]
    fn opening_a_line_at_the_end_of_a_buffer_leaves_an_empty_line() {
        let editor = run("ab", "o<Esc>");
        assert_eq!(editor.text(), "ab\n\n");
        assert_eq!(editor.buffer().line_count(), 2);
        assert_eq!(editor.cursor(), Position::new(1, 0));
    }

    #[test]
    fn visual_mode_stretches_a_selection_and_escape_drops_it() {
        let editor = run("foo bar", "vll");
        assert_eq!(editor.mode(), Mode::Visual);
        assert_eq!(
            editor.selection(),
            Some((Position::new(0, 0), Position::new(0, 2)))
        );

        let editor = run("foo bar", "$vbb");
        assert_eq!(
            editor.selection(),
            Some((Position::new(0, 0), Position::new(0, 6))),
            "the selection reaches backwards from its anchor"
        );

        let editor = run("foo bar", "vll<Esc>");
        assert_eq!(editor.mode(), Mode::Normal);
        assert_eq!(editor.selection(), None);
        assert_eq!(editor.cursor(), Position::new(0, 2));

        assert_eq!(run("foo", "vv").mode(), Mode::Normal);
    }

    #[test]
    fn an_operator_puts_the_editor_in_operator_pending_mode() {
        let editor = run("foo bar", "d");
        assert_eq!(editor.mode(), Mode::OperatorPending);
        assert!(editor.last_operation().is_none());

        let editor = run("foo bar", "d<Esc>");
        assert_eq!(editor.mode(), Mode::Normal);
        assert!(editor.last_operation().is_none());
    }

    #[test]
    fn operators_resolve_the_span_a_motion_reaches() {
        assert_eq!(operated("foo bar baz", "dw").1, "foo ");
        assert_eq!(operated("foo bar baz", "d2w").1, "foo bar ");
        assert_eq!(operated("foo bar baz", "2d3w").1, "foo bar baz");
        assert_eq!(operated("foo bar", "de").1, "foo", "e is inclusive");
        assert_eq!(operated("foo bar", "d$").1, "foo bar");
        assert_eq!(operated("foo bar", "wdb").1, "foo ", "b reaches backwards");
        assert_eq!(operated("あいうえお", "d2l").1, "あい");
        assert_eq!(operated("foo bar", "dtr").1, "foo ba");
        let (operator, taken, _) = operated("foo bar", "y2w");
        assert_eq!((operator, taken.as_str()), (Operator::Yank, "foo bar"));
    }

    #[test]
    fn operators_over_linewise_motions_take_whole_lines() {
        assert_eq!(
            operated("ab\ncd\nef", "dj"),
            (Operator::Delete, "ab\ncd".to_owned(), true)
        );
        assert_eq!(operated("ab\ncd\nef", "dG").1, "ab\ncd\nef");
        assert_eq!(operated("ab\ncd\nef", "jdgg").1, "ab\ncd");
        assert_eq!(
            operated("ab\ncd\nef", "2jd2gg").1,
            "cd\nef",
            "the count on gg is the line to reach, not a repetition"
        );
        assert_eq!(operated("ab\ncd\nef", "jdk").1, "ab\ncd");
    }

    #[test]
    fn a_doubled_operator_takes_the_cursor_line_and_the_lines_below() {
        assert_eq!(
            operated("ab\ncd\nef", "dd"),
            (Operator::Delete, "ab".to_owned(), true)
        );
        assert_eq!(operated("ab\ncd\nef", "2dd").1, "ab\ncd");
        assert_eq!(
            operated("ab\ncd\nef", "9yy").1,
            "ab\ncd\nef",
            "counts clamp"
        );
        assert_eq!(operated("ab\ncd", "jcc").1, "cd");
    }

    #[test]
    fn operators_resolve_text_objects() {
        assert_eq!(
            operated("foo bar baz", "wd2aw").1,
            " bar baz",
            "the last word has no trailing blank, so the leading one comes along"
        );
        assert_eq!(operated("foo bar baz", "d2aw").1, "foo bar ");
        assert_eq!(operated("say \"hi\" now", "ci\"").1, "hi");
        assert_eq!(operated("f(a, b)", "wdi(").1, "a, b");
        assert_eq!(operated("日本語 です", "daw").1, "日本語 ");
    }

    #[test]
    fn change_over_a_word_stops_at_the_end_of_that_word() {
        assert_eq!(operated("foo bar", "cw").1, "foo", "cw acts like ce");
        assert_eq!(
            operated("foo bar", "llcw").1,
            "o",
            "on the last character of a word cw takes only it"
        );
        assert_eq!(operated("foo bar baz", "c2w").1, "foo bar");
        assert_eq!(operated("foo bar", "dw").1, "foo ", "dw keeps the blanks");
        assert_eq!(
            operated("foo   bar", "llllcw").1,
            "  ",
            "on whitespace cw is an ordinary w"
        );
    }

    #[test]
    fn x_and_uppercase_x_resolve_to_the_characters_around_the_cursor() {
        assert_eq!(
            operated("あいうえお", "3x"),
            (Operator::Delete, "あいう".to_owned(), false)
        );
        assert_eq!(operated("あいうえお", "3lX").1, "う");
        assert_eq!(operated("abc", "9x").1, "abc", "counts clamp to the line");
        assert!(run("abc", "X").last_operation().is_none());
    }

    #[test]
    fn a_text_object_the_cursor_is_not_in_resolves_to_nothing() {
        let editor = run("foo bar", "di(");
        assert!(editor.last_operation().is_none());
        assert_eq!(editor.mode(), Mode::Normal);
    }

    #[test]
    fn a_count_is_dropped_when_the_command_is_cancelled() {
        let editor = run("ab\ncd\nef", "2<Esc>j");
        assert_eq!(editor.cursor(), Position::new(1, 0));
    }

    #[test]
    fn key_strings_that_do_not_parse_are_reported() {
        let mut editor = Editor::new("abc");
        assert!(editor.handle_keys("i<Escape>").is_err());
    }
}
