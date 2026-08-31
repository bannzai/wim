//! The editor state machine: keys in, buffer and cursor out.

use crate::buffer::Buffer;
use crate::command::{Command, InsertAnchor, Operator, OperatorTarget};
use crate::edit;
use crate::effect::Effect;
use crate::grammar::Grammar;
use crate::key::{KeyCode, KeyEvent, KeyParseError, parse_keys};
use crate::mode::Mode;
use crate::motion::{self, Motion, MotionContext, MotionKind};
use crate::position::Position;
use crate::register::Registers;
use crate::textobject::{self, TextObject, TextObjectKind, TextRange};
use crate::undo::History;

/// The change `.` repeats.
///
/// A change that entered Insert mode is not done when the command that started it is: the
/// keys typed until `<Esc>` are part of it, and repeating it types them again.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LastEdit {
    command: Command,
    insert_keys: Vec<KeyEvent>,
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
    registers: Registers,
    history: History,
    last_edit: Option<LastEdit>,
    /// The change that runs until `<Esc>` closes it, and the keys typed into it so far.
    open_edit: Option<Command>,
    open_edit_keys: Vec<KeyEvent>,
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

    /// The registers a yank or a delete filled, which a host may show.
    pub fn registers(&self) -> &Registers {
        &self.registers
    }

    /// The selection, both ends inclusive, `None` outside Visual mode.
    pub fn selection(&self) -> Option<(Position, Position)> {
        let anchor = self.visual_anchor?;
        Some((anchor.min(self.cursor), anchor.max(self.cursor)))
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
    /// nothing — a motion that cannot move, a text object the cursor is not in, or a
    /// selection outside Visual mode.
    ///
    /// `c` on `w` is the one place where the operator changes the span, because Vim's `cw`
    /// stops at the end of the word rather than at the start of the next one.
    pub fn resolve_operator_target(
        &self,
        operator: Operator,
        count: Option<usize>,
        target: OperatorTarget,
    ) -> Option<TextRange> {
        match target {
            OperatorTarget::Lines => {
                let last =
                    (self.cursor.line + at_least_one(count) - 1).min(self.buffer.line_count() - 1);
                Some(TextRange::lines(&self.buffer, self.cursor.line, last))
            }
            OperatorTarget::TextObject(object) => {
                textobject::resolve(&self.buffer, self.cursor, object, count)
            }
            OperatorTarget::Motion(motion) => self.motion_range(operator, motion, count),
            OperatorTarget::Selection => {
                let (start, end) = self.selection()?;
                Some(TextRange::charwise(
                    start,
                    Position::new(end.line, end.col + 1),
                ))
            }
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
        let count = at_least_one(count);
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
        let effects = self.run(command);
        self.close_change(command);
        effects
    }

    fn run(&mut self, command: Command) -> Vec<Effect> {
        match command {
            Command::Move { motion, count } => self.move_cursor(motion, count),
            Command::Operate {
                operator,
                count,
                register,
                target,
            } => self.operate(operator, count, register, target),
            Command::Paste {
                before,
                count,
                register,
            } => return self.paste(before, count, register),
            Command::ReplaceChar { replacement, count } => {
                self.open_change();
                if let Some(cursor) = edit::replace(
                    &mut self.buffer,
                    self.cursor,
                    replacement,
                    at_least_one(count),
                ) {
                    self.cursor = cursor;
                }
            }
            Command::JoinLines { count } => {
                self.open_change();
                // A count on `J` is how many lines take part rather than how many breaks go
                // away, and a bare `J` joins two lines.
                let joins = at_least_one(count).max(2) - 1;
                if let Some(cursor) = edit::join(&mut self.buffer, self.cursor.line, joins) {
                    self.cursor = cursor;
                }
            }
            Command::ToggleCase { count } => {
                self.open_change();
                self.cursor = edit::flip_case(&mut self.buffer, self.cursor, at_least_one(count));
            }
            Command::EnterInsert(anchor) => {
                self.open_change();
                self.enter_insert(anchor);
            }
            Command::Undo { count } => return self.walk_history(count, true),
            Command::Redo { count } => return self.walk_history(count, false),
            Command::RepeatEdit { count } => return self.repeat_edit(count),
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

    fn operate(
        &mut self,
        operator: Operator,
        count: Option<usize>,
        register: Option<char>,
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
        let Some(range) = self.resolve_operator_target(operator, count, target) else {
            return;
        };
        self.visual_anchor = None;
        match operator {
            Operator::Yank => {
                self.registers
                    .store(register, edit::yank(&self.buffer, range));
                // A yank leaves the cursor at the start of what it took, so `yy` leaves it
                // alone: the line the span starts on is the line the cursor is already on.
                self.cursor = if range.linewise {
                    self.buffer
                        .clamp(Position::new(range.start.line, self.cursor.col))
                } else {
                    self.buffer.clamp(range.start)
                };
            }
            Operator::Delete => {
                self.open_change();
                let (content, cursor) = edit::delete(&mut self.buffer, range);
                self.registers.store(register, content);
                self.cursor = cursor;
            }
            Operator::Change => {
                self.open_change();
                let (content, cursor) = edit::change(&mut self.buffer, range);
                self.registers.store(register, content);
                self.cursor = cursor;
                self.mode = Mode::Insert;
            }
        }
    }

    fn paste(&mut self, before: bool, count: Option<usize>, register: Option<char>) -> Vec<Effect> {
        let Some(content) = self.registers.get(register).cloned() else {
            let name = register.map_or_else(
                || "the unnamed register".to_owned(),
                |name| format!("register \"{name}"),
            );
            return vec![Effect::Error(format!("{name} is empty"))];
        };
        self.open_change();
        self.cursor = edit::paste(
            &mut self.buffer,
            self.cursor,
            &content,
            before,
            at_least_one(count),
        );
        Vec::new()
    }

    /// Walks `count` changes back with `u`, or forward again with `<C-r>`.
    fn walk_history(&mut self, count: Option<usize>, backwards: bool) -> Vec<Effect> {
        let mut moved = false;
        for _ in 0..at_least_one(count) {
            let restored = if backwards {
                self.history.undo(&self.buffer)
            } else {
                self.history.redo(&self.buffer)
            };
            let Some(restored) = restored else {
                break;
            };
            self.cursor = restored.first_difference(&self.buffer);
            self.buffer = restored;
            moved = true;
        }
        if moved {
            self.motion_context.desired_col = self.cursor.col;
            return Vec::new();
        }
        let complaint = if backwards {
            "already at the oldest change"
        } else {
            "already at the newest change"
        };
        vec![Effect::Error(complaint.to_owned())]
    }

    /// Does the last change again, with `count` in place of the count it was typed with.
    fn repeat_edit(&mut self, count: Option<usize>) -> Vec<Effect> {
        let Some(edit) = self.last_edit.clone() else {
            return vec![Effect::Error("there is no change to repeat".to_owned())];
        };
        let mut effects = self.apply(edit.command.with_count(count));
        if self.mode == Mode::Insert {
            for key in edit.insert_keys {
                effects.append(&mut self.handle_key(key));
            }
            effects.append(&mut self.handle_key(KeyEvent::key(KeyCode::Esc)));
        }
        effects
    }

    /// Opens the undo unit the command about to run belongs to. The Insert session an `i`,
    /// a `c` or an `o` starts is one unit, so its later keys open nothing of their own.
    fn open_change(&mut self) {
        self.history.begin(&self.buffer);
    }

    /// Closes the undo unit `command` opened and makes it the change `.` repeats, unless it
    /// entered Insert mode: that unit runs until `<Esc>`, and the keys typed until then
    /// belong to it. A command that altered nothing is neither kept nor repeatable.
    fn close_change(&mut self, command: Command) {
        if self.mode == Mode::Insert {
            self.open_edit = Some(command);
            self.open_edit_keys.clear();
            return;
        }
        if self.history.commit(&self.buffer) {
            self.last_edit = Some(LastEdit {
                command,
                insert_keys: Vec::new(),
            });
        }
    }

    /// Closes the undo unit the Insert session belonged to, and makes the session — the
    /// command that started it and every key typed into it — the change `.` repeats.
    fn close_insert_session(&mut self) {
        let command = self.open_edit.take();
        let insert_keys = std::mem::take(&mut self.open_edit_keys);
        if self.history.commit(&self.buffer)
            && let Some(command) = command
        {
            self.last_edit = Some(LastEdit {
                command,
                insert_keys,
            });
        }
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
                self.buffer
                    .insert_line_break(Position::new(line, self.buffer.line_len(line)));
                self.cursor = Position::new(line + 1, 0);
            }
            InsertAnchor::LineAbove => {
                let at = Position::new(self.cursor.line, 0);
                self.buffer.insert_line_break(at);
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
                self.close_insert_session();
            }
            KeyCode::Enter => {
                self.open_edit_keys.push(key);
                self.buffer.insert_line_break(self.cursor);
                self.cursor = Position::new(self.cursor.line + 1, 0);
            }
            KeyCode::Backspace => {
                self.open_edit_keys.push(key);
                self.backspace();
            }
            KeyCode::Tab => {
                self.open_edit_keys.push(key);
                self.insert_text("\t");
            }
            KeyCode::Char(character) if !key.ctrl => {
                self.open_edit_keys.push(key);
                self.insert_text(character.encode_utf8(&mut [0u8; 4]));
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

/// The count a command was typed with, read as a number of repetitions: no count is one, and
/// a count of zero cannot be typed because `0` is a motion.
fn at_least_one(count: Option<usize>) -> usize {
    count.unwrap_or(1).max(1)
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

    /// The text `keys` leave behind, and what they left in the unnamed register.
    fn edited(text: &str, keys: &str) -> (String, String) {
        let editor = run(text, keys);
        let register = editor
            .registers()
            .get(None)
            .map_or_else(String::new, |held| held.text.clone());
        (editor.text(), register)
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
        assert_eq!(run("foo bar", "d").mode(), Mode::OperatorPending);

        let editor = run("foo bar", "d<Esc>");
        assert_eq!(editor.mode(), Mode::Normal);
        assert_eq!(
            editor.text(),
            "foo bar",
            "the dropped operator took nothing"
        );
    }

    #[test]
    fn operators_take_the_span_a_motion_reaches() {
        assert_eq!(
            edited("foo bar baz", "dw"),
            ("bar baz".to_owned(), "foo ".to_owned())
        );
        assert_eq!(edited("foo bar baz", "d2w").0, "baz");
        assert_eq!(edited("foo bar baz", "2d3w").0, "");
        assert_eq!(edited("foo bar", "de").0, " bar", "e is inclusive");
        assert_eq!(edited("foo bar", "d$").0, "");
        assert_eq!(
            edited("foo bar", "wdb"),
            ("bar".to_owned(), "foo ".to_owned())
        );
        assert_eq!(edited("あいうえお", "d2l").0, "うえお");
        assert_eq!(
            edited("foo bar", "dtr"),
            ("r".to_owned(), "foo ba".to_owned())
        );
        assert_eq!(
            edited("foo bar", "y2w"),
            ("foo bar".to_owned(), "foo bar".to_owned()),
            "a yank leaves the text where it is"
        );
    }

    #[test]
    fn a_charwise_delete_leaves_the_cursor_where_the_span_started() {
        assert_eq!(run("foo bar", "wdw").cursor(), Position::new(0, 3));
        assert_eq!(
            run("foo bar", "$x").cursor(),
            Position::new(0, 5),
            "taking the last grapheme of a line moves onto the new last one"
        );
        assert_eq!(run("foo bar", "wyb").cursor(), Position::new(0, 0));
    }

    #[test]
    fn operators_over_linewise_motions_take_whole_lines() {
        assert_eq!(
            edited("ab\ncd\nef", "dj"),
            ("ef".to_owned(), "ab\ncd\n".to_owned())
        );
        assert_eq!(edited("ab\ncd\nef", "dG").0, "");
        assert_eq!(edited("ab\ncd\nef", "jdgg").0, "ef");
        assert_eq!(
            edited("ab\ncd\nef", "2jd2gg").0,
            "ab",
            "the count on gg is the line to reach, not a repetition"
        );
        assert_eq!(edited("ab\ncd\nef", "jdk").0, "ef");
    }

    #[test]
    fn a_doubled_operator_takes_the_cursor_line_and_the_lines_below() {
        assert_eq!(
            edited("ab\ncd\nef", "dd"),
            ("cd\nef".to_owned(), "ab\n".to_owned())
        );
        assert_eq!(edited("ab\ncd\nef", "2dd").0, "ef");
        assert_eq!(
            edited("ab\ncd\nef", "9yy"),
            ("ab\ncd\nef".to_owned(), "ab\ncd\nef\n".to_owned()),
            "counts clamp"
        );

        let editor = run("ab\ncd", "jcc");
        assert_eq!(editor.text(), "ab\n\n");
        assert_eq!(editor.cursor(), Position::new(1, 0));
        assert_eq!(editor.mode(), Mode::Insert);
    }

    #[test]
    fn a_linewise_delete_leaves_the_cursor_on_the_first_non_blank_of_the_next_line() {
        assert_eq!(run("ab\n  cd\nef", "dd").cursor(), Position::new(0, 2));
        assert_eq!(
            run("ab\ncd", "jdd").cursor(),
            Position::new(0, 0),
            "the line before, when the deleted one was last"
        );
        assert_eq!(
            run("ab\ncd", "jyy").cursor(),
            Position::new(1, 0),
            "yy leaves the cursor alone"
        );
    }

    #[test]
    fn operators_resolve_text_objects() {
        assert_eq!(
            edited("foo bar baz", "wd2aw"),
            ("foo".to_owned(), " bar baz".to_owned()),
            "the last word has no trailing blank, so the leading one comes along"
        );
        assert_eq!(edited("foo bar baz", "d2aw").0, "baz");
        assert_eq!(run("say \"hi\" now", "ci\"").text(), "say \"\" now");
        assert_eq!(edited("f(a, b)", "wdi(").0, "f()");
        assert_eq!(edited("日本語 です", "daw").0, "です");
    }

    #[test]
    fn change_over_a_word_stops_at_the_end_of_that_word() {
        assert_eq!(
            run("foo bar", "cwX<Esc>").text(),
            "X bar",
            "cw acts like ce"
        );
        assert_eq!(
            run("foo bar", "llcwX<Esc>").text(),
            "foX bar",
            "on the last character of a word cw takes only it"
        );
        assert_eq!(run("foo bar baz", "c2wX<Esc>").text(), "X baz");
        assert_eq!(run("foo bar", "dw").text(), "bar", "dw keeps the blanks");
        assert_eq!(
            run("foo   bar", "llllcwX<Esc>").text(),
            "foo Xbar",
            "on whitespace cw is an ordinary w"
        );
    }

    #[test]
    fn x_and_uppercase_x_take_the_graphemes_around_the_cursor() {
        assert_eq!(
            edited("あいうえお", "3x"),
            ("えお".to_owned(), "あいう".to_owned())
        );
        assert_eq!(edited("あいうえお", "3lX").0, "あいえお");
        assert_eq!(edited("abc", "9x").0, "", "counts clamp to the line");
        assert_eq!(
            run("abc", "X").text(),
            "abc",
            "there is nothing in front of column 0"
        );
        assert_eq!(
            run("\nab", "x").text(),
            "\nab",
            "an empty line has nothing to take"
        );
    }

    #[test]
    fn the_shorthand_keys_edit_what_their_longhand_would() {
        assert_eq!(
            edited("foo bar", "wD"),
            ("foo ".to_owned(), "bar".to_owned())
        );
        assert_eq!(run("foo bar", "wCX<Esc>").text(), "foo X");
        assert_eq!(run("abc", "sX<Esc>").text(), "Xbc");
        assert_eq!(run("abc", "3sX<Esc>").text(), "X");
        assert_eq!(run("ab\ncd", "SX<Esc>").text(), "X\ncd");
    }

    #[test]
    fn a_text_object_the_cursor_is_not_in_leaves_the_buffer_alone() {
        let editor = run("foo bar", "di(");
        assert_eq!(editor.text(), "foo bar");
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

    #[test]
    fn charwise_paste_goes_after_the_cursor_and_uppercase_p_in_front_of_it() {
        let editor = run("abc", "ylp");
        assert_eq!(editor.text(), "aabc");
        assert_eq!(editor.cursor(), Position::new(0, 1));

        let editor = run("abc", "yl2P");
        assert_eq!(editor.text(), "aaabc");
        assert_eq!(editor.cursor(), Position::new(0, 1));
    }

    #[test]
    fn linewise_paste_goes_onto_lines_below_the_cursor_or_above_it() {
        let editor = run("ab\ncd", "yyp");
        assert_eq!(editor.text(), "ab\nab\ncd");
        assert_eq!(editor.cursor(), Position::new(1, 0));

        let editor = run("ab\n  cd", "ddP");
        assert_eq!(editor.text(), "ab\n  cd", "P puts back what dd took");
        assert_eq!(editor.cursor(), Position::new(0, 0));

        let editor = run("ab\n  cd", "jyyP");
        assert_eq!(editor.text(), "ab\n  cd\n  cd");
        assert_eq!(
            editor.cursor(),
            Position::new(1, 2),
            "onto the first non-blank"
        );

        assert_eq!(
            run("ab\ncd", "yyjp").text(),
            "ab\ncd\nab",
            "lines put after the last one leave the buffer without a trailing newline"
        );
    }

    #[test]
    fn pasting_an_empty_register_reports_an_error() {
        let mut editor = Editor::new("abc");
        let effects = editor.handle_keys("p").expect("key string should parse");
        assert!(
            matches!(effects.as_slice(), [Effect::Error(_)]),
            "{effects:?}"
        );
        assert_eq!(editor.text(), "abc");
    }

    #[test]
    fn a_named_register_keeps_its_text_while_the_unnamed_one_moves_on() {
        let editor = run("ab\ncd", "\"ayyjdd\"ap");
        assert_eq!(editor.text(), "ab\nab");
        assert_eq!(
            editor.registers().get(None).map(|held| held.text.as_str()),
            Some("cd\n"),
            "the delete in between filled the unnamed register"
        );
    }

    #[test]
    fn join_puts_one_space_where_the_break_was() {
        let editor = run("ab\n   cd", "J");
        assert_eq!(editor.text(), "ab cd");
        assert_eq!(editor.cursor(), Position::new(0, 2));

        assert_eq!(run("a\nb\nc", "3J").text(), "a b c");
        assert_eq!(run("ab", "J").text(), "ab", "the last line joins nothing");
    }

    #[test]
    fn replace_writes_one_character_over_the_cursor() {
        let editor = run("abc", "2rx");
        assert_eq!(editor.text(), "xxc");
        assert_eq!(editor.cursor(), Position::new(0, 1));
        assert_eq!(
            run("abc", "9rx").text(),
            "abc",
            "a count past the line end writes nothing"
        );
    }

    #[test]
    fn toggling_case_walks_the_cursor_along() {
        let editor = run("aBc", "3~");
        assert_eq!(editor.text(), "AbC");
        assert_eq!(
            editor.cursor(),
            Position::new(0, 2),
            "the cursor clamps back onto the line"
        );
        assert_eq!(run("aBc", "~").cursor(), Position::new(0, 1));
    }

    #[test]
    fn an_operator_over_a_selection_takes_it_and_leaves_visual_mode() {
        let editor = run("foo bar", "vwd");
        assert_eq!(editor.text(), "ar");
        assert_eq!(editor.mode(), Mode::Normal);
        assert_eq!(editor.cursor(), Position::new(0, 0));

        assert_eq!(run("foo bar", "vllx").text(), " bar");
        assert_eq!(run("foo bar", "vlcX<Esc>").text(), "Xo bar");

        let editor = run("foo bar", "wvey");
        assert_eq!(editor.text(), "foo bar");
        assert_eq!(
            editor.registers().get(None).map(|held| held.text.as_str()),
            Some("bar")
        );
        assert_eq!(editor.mode(), Mode::Normal);
    }

    #[test]
    fn undo_walks_back_one_command_at_a_time_and_redo_walks_forward() {
        let editor = run("foo bar", "dwu");
        assert_eq!(editor.text(), "foo bar");
        assert_eq!(editor.cursor(), Position::new(0, 0), "onto the change");

        assert_eq!(run("foo bar", "dwu<C-r>").text(), "bar");
        assert_eq!(run("ab\ncd\nef", "dddd2u").text(), "ab\ncd\nef");
    }

    #[test]
    fn an_insert_session_is_one_undo_unit() {
        assert_eq!(run("ab", "ixyz<Esc>u").text(), "ab");
        assert_eq!(
            run("ab", "oxy<Esc>u").text(),
            "ab",
            "the line o opened belongs to the session"
        );
        assert_eq!(run("foo bar", "ciwX<Esc>u").text(), "foo bar");
        assert_eq!(run("ab", "ixyz<Esc>u<C-r>").text(), "xyzab");
    }

    #[test]
    fn undoing_what_never_changed_reports_an_error() {
        let mut editor = Editor::new("ab");
        let effects = editor.handle_keys("u").expect("key string should parse");
        assert!(
            matches!(effects.as_slice(), [Effect::Error(_)]),
            "{effects:?}"
        );

        let effects = editor
            .handle_keys("yy<C-r>")
            .expect("key string should parse");
        assert!(
            matches!(effects.as_slice(), [Effect::Error(_)]),
            "a yank is not a change: {effects:?}"
        );
    }

    #[test]
    fn a_new_change_drops_what_could_be_redone() {
        assert_eq!(run("ab\ncd", "ddujdd<C-r>").text(), "ab");
    }

    #[test]
    fn repeat_does_the_last_change_again() {
        assert_eq!(run("foo bar baz", "dw.").text(), "baz");
        assert_eq!(run("abc", "x..").text(), "");
        assert_eq!(run("ab\ncd\nef", "ddj.").text(), "cd");
        assert_eq!(run("abc abc", "rXw.").text(), "Xbc Xbc");
    }

    #[test]
    fn repeat_types_the_insert_session_again() {
        assert_eq!(run("foo bar", "ciwX<Esc>w.").text(), "X X");
        assert_eq!(run("ab\ncd", "oxy<Esc>.").text(), "ab\nxy\nxy\ncd");
        assert_eq!(run("ab", "iX<Esc>.").text(), "XXab");
    }

    #[test]
    fn a_count_on_repeat_replaces_the_one_the_change_was_typed_with() {
        assert_eq!(run("a b c d e", "dw2.").text(), "d e");
        assert_eq!(run("abcdef", "2x3.").text(), "f");
    }

    #[test]
    fn repeat_without_a_change_to_repeat_reports_an_error() {
        let mut editor = Editor::new("ab");
        let effects = editor.handle_keys(".").expect("key string should parse");
        assert!(
            matches!(effects.as_slice(), [Effect::Error(_)]),
            "{effects:?}"
        );
    }

    #[test]
    fn a_yank_is_not_what_repeat_repeats() {
        assert_eq!(
            run("foo bar baz", "dwyww.").text(),
            "bar ",
            "the repeat deleted a word, so the yank in between is not the change"
        );
    }
}
