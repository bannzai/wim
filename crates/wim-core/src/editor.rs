//! The editor state machine: keys in, buffer and cursor out.

use std::collections::BTreeMap;

use crate::buffer::Buffer;
use crate::command::{Command, InsertAnchor, Operator, OperatorTarget};
use crate::edit;
use crate::effect::Effect;
use crate::ex;
use crate::grammar::Grammar;
use crate::key::{KeyCode, KeyEvent, KeyParseError, format_keys, parse_keys};
use crate::mode::Mode;
use crate::motion::{self, Motion, MotionContext, MotionKind};
use crate::position::Position;
use crate::register::{RegisterContent, Registers};
use crate::search::{self, Search};
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

/// The macro `q{a-z}` is filling: the register it will land in, and the keys typed into it
/// so far.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Recording {
    register: char,
    keys: Vec<KeyEvent>,
}

/// How deep `@` playbacks may nest.
///
/// A macro that plays another one is ordinary; a macro that plays itself is how a loop is
/// written in Vim, and the usual end of one is a key inside it failing. This limit is what
/// ends the ones that never fail, and it is far past the nesting a macro written by hand
/// uses.
const MACRO_DEPTH_LIMIT: usize = 32;

/// How many keys one `@` may feed, the keys of the macros it plays included.
///
/// A macro applied to a whole file legitimately runs for thousands of keys — `1000@q` is the
/// Vim idiom for "until it fails" — so the limit is well past what a real run needs and only
/// catches a playback that would not stop on its own.
const MACRO_STEP_LIMIT: usize = 100_000;

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
    /// The `:`, `/` or `?` line being typed, its prefix included.
    command_line: Option<String>,
    /// The search `n` and `N` repeat.
    last_search: Option<Search>,
    /// Whether an Ex command is running. The whole of one is a single undo unit, so the
    /// commands it drives open none of their own.
    running_ex: bool,
    /// The positions `m{a-z}` named, which `` ` `` and `'` move back to.
    marks: BTreeMap<char, Position>,
    /// The macro `q` is filling, `None` when nothing is being recorded.
    recording: Option<Recording>,
    /// The macro `@@` plays again.
    last_macro: Option<char>,
    /// How deeply the `@` playbacks running are nested, and how many keys they have fed
    /// between them, which [`MACRO_DEPTH_LIMIT`] and [`MACRO_STEP_LIMIT`] cut off.
    macro_depth: usize,
    macro_steps: usize,
    /// How deeply the editor is typing keys at itself — playing a macro, or running the keys
    /// of a `:norm` — rather than reading keys a user typed. A recording keeps only typed
    /// keys, as Vim's does.
    fed_keys: usize,
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

    /// The command line being typed, its `:`, `/` or `?` prefix included, for a host to show.
    /// `None` outside Command-line mode.
    pub fn command_line(&self) -> Option<&str> {
        self.command_line.as_deref()
    }

    /// The search `n` repeats, which a host may show as the pattern in force.
    pub fn last_search(&self) -> Option<&Search> {
        self.last_search.as_ref()
    }

    /// The register `q` is recording into, which a host may show, `None` when nothing is
    /// being recorded.
    pub fn recording_register(&self) -> Option<char> {
        self.recording.as_ref().map(|recording| recording.register)
    }

    /// Where `m` put the mark `name`, `None` when it was never set or a change took its line
    /// away.
    pub fn mark(&self, name: char) -> Option<Position> {
        self.marks.get(&name).copied()
    }

    /// Reads one key in the current mode.
    ///
    /// A recording keeps the key, unless the editor is typing it at itself; the marks are
    /// moved over whatever the key changed.
    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if self.fed_keys == 0
            && let Some(recording) = &mut self.recording
        {
            recording.keys.push(key);
        }
        // Comparing the buffers costs a walk of the text, which is worth doing only when
        // there is a mark to move.
        let before = (!self.marks.is_empty()).then(|| self.buffer.clone());
        let effects = self.run_key(key);
        if let Some(before) = before
            && before.line_count() != self.buffer.line_count()
        {
            self.move_marks(&before);
        }
        effects
    }

    fn run_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        match self.mode {
            Mode::Insert => return self.handle_insert_key(key),
            // A command line is text being typed, so the Normal mode grammar never sees it.
            Mode::CommandLine => return self.handle_command_line_key(key),
            _ => {}
        }
        let command = self.grammar.feed(key, self.mode, self.recording.is_some());
        let effects = self.apply(command);
        if !matches!(self.mode, Mode::Insert | Mode::CommandLine) {
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
            Command::EnterCommandLine(prefix) => {
                self.visual_anchor = None;
                self.command_line = Some(prefix.to_string());
                self.mode = Mode::CommandLine;
            }
            Command::RepeatSearch { reverse, count } => return self.repeat_search(count, reverse),
            Command::SearchWord { backward, count } => return self.search_word(count, backward),
            Command::RecordMacro { register } => {
                self.recording = Some(Recording {
                    register,
                    keys: Vec::new(),
                });
            }
            Command::StopRecording => self.stop_recording(),
            Command::PlayMacro { register, count } => return self.play_macro(register, count),
            Command::SetMark(name) => {
                self.marks.insert(name, self.cursor);
            }
            Command::JumpMark {
                name,
                to_line_start,
            } => return self.jump_mark(name, to_line_start),
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

    /// Reads one key of a `:`, `/` or `?` line: `<CR>` runs it, `<Esc>` drops it, `<BS>` takes
    /// the last key back and drops the line when it takes the prefix itself back, and every
    /// other key is text.
    fn handle_command_line_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        match key.code {
            KeyCode::Esc => self.close_command_line(),
            KeyCode::Enter => return self.run_command_line(),
            KeyCode::Backspace => {
                let line = self.command_line.get_or_insert_default();
                line.pop();
                if line.is_empty() {
                    self.close_command_line();
                }
            }
            KeyCode::Tab => self.command_line.get_or_insert_default().push('\t'),
            KeyCode::Char(character) if !key.ctrl => {
                self.command_line.get_or_insert_default().push(character);
            }
            KeyCode::Char(_) => {
                return vec![Effect::Error(format!(
                    "{key} does nothing in {} mode",
                    Mode::CommandLine.label()
                ))];
            }
        }
        Vec::new()
    }

    fn close_command_line(&mut self) {
        self.command_line = None;
        self.mode = Mode::Normal;
    }

    /// Runs the line that was typed, which its prefix reads as an Ex command or as a search.
    fn run_command_line(&mut self) -> Vec<Effect> {
        let line = self.command_line.take().unwrap_or_default();
        self.mode = Mode::Normal;
        let mut characters = line.chars();
        let body: String = characters.by_ref().skip(1).collect();
        match line.chars().next() {
            Some(':') => self.run_ex(&body),
            Some(prefix) => self.run_search(&body, prefix == '?'),
            None => Vec::new(),
        }
    }

    /// Runs one Ex command as a single undo unit, however many edits it makes. A `:norm` that
    /// types another `:` command runs inside that unit rather than opening one of its own.
    fn run_ex(&mut self, body: &str) -> Vec<Effect> {
        if body.trim().is_empty() {
            return Vec::new();
        }
        let command = match ex::parse(body) {
            Ok(command) => command,
            Err(error) => return vec![Effect::Error(error.to_string())],
        };
        let outermost = !self.running_ex;
        if outermost {
            self.history.begin(&self.buffer);
            self.running_ex = true;
        }
        let effects = ex::execute(self, &command);
        if outermost {
            self.running_ex = false;
            self.history.commit(&self.buffer);
        }
        effects
    }

    /// Searches for what `/` or `?` was given, an empty pattern meaning the last one again.
    fn run_search(&mut self, pattern: &str, backward: bool) -> Vec<Effect> {
        let pattern = match (pattern.is_empty(), &self.last_search) {
            (false, _) => pattern.to_owned(),
            (true, Some(search)) => search.pattern.clone(),
            (true, None) => return vec![Effect::Error("there is no search to repeat".to_owned())],
        };
        self.last_search = Some(Search { pattern, backward });
        self.repeat_search(None, false)
    }

    /// `n` and `N`: walks `count` matches of the last search, `N` the other way round.
    fn repeat_search(&mut self, count: Option<usize>, reverse: bool) -> Vec<Effect> {
        let Some(search) = self.last_search.clone() else {
            return vec![Effect::Error("there is no search to repeat".to_owned())];
        };
        let backward = search.backward != reverse;
        for _ in 0..at_least_one(count) {
            match search::find(&self.buffer, self.cursor, &search.pattern, backward) {
                Ok(found) => self.cursor = found,
                Err(error) => return vec![Effect::Error(error.to_string())],
            }
        }
        self.motion_context.desired_col = self.cursor.col;
        Vec::new()
    }

    /// `*` and `#`: searches for the word under the cursor, which becomes the search `n`
    /// repeats.
    fn search_word(&mut self, count: Option<usize>, backward: bool) -> Vec<Effect> {
        let Some(pattern) = search::word_pattern(&self.buffer, self.cursor) else {
            return vec![Effect::Error(
                "there is no word under the cursor".to_owned(),
            )];
        };
        self.last_search = Some(Search { pattern, backward });
        self.repeat_search(count, false)
    }

    /// Ends the recording `q` started, keeping its keys in the register it named, written out
    /// in the key notation so that a paste can put them into the buffer as text.
    ///
    /// The `q` that ended the recording is not part of the macro. [`Editor::handle_key`] has
    /// already kept it, as it keeps every typed key, so it comes back off here.
    fn stop_recording(&mut self) {
        let Some(mut recording) = self.recording.take() else {
            return;
        };
        recording.keys.pop();
        self.registers.store_named(
            recording.register,
            RegisterContent::charwise(format_keys(&recording.keys)),
        );
    }

    /// `@{a-z}` and `@@`: types the keys a register holds, `count` times over.
    fn play_macro(&mut self, register: Option<char>, count: Option<usize>) -> Vec<Effect> {
        let Some(name) = register.or(self.last_macro) else {
            return vec![Effect::Error("there is no macro to play again".to_owned())];
        };
        let Some(content) = self.registers.get(Some(name)).cloned() else {
            return vec![Effect::Error(format!("register \"{name} is empty"))];
        };
        let keys = match parse_keys(&content.text) {
            Ok(keys) => keys,
            Err(error) => {
                return vec![Effect::Error(format!(
                    "register \"{name} does not hold keys: {error}"
                ))];
            }
        };
        self.last_macro = Some(name);
        if self.macro_depth >= MACRO_DEPTH_LIMIT {
            return vec![Effect::Error(format!(
                "macros are nested more than {MACRO_DEPTH_LIMIT} deep"
            ))];
        }
        if self.macro_depth == 0 {
            self.macro_steps = 0;
        }
        self.macro_depth += 1;
        let mut effects = Vec::new();
        for _ in 0..at_least_one(count) {
            if !self.feed_keys(&keys, &mut effects) {
                break;
            }
        }
        self.macro_depth -= 1;
        effects
    }

    /// Types `keys` at the editor the way a macro and `:norm` do: keys it feeds itself rather
    /// than keys a user typed, so a recording in progress keeps none of them.
    ///
    /// Returns whether every key ran without complaining. The first that reports an error ends
    /// the run, which is what stops a macro at the first thing it cannot do, and a macro that
    /// feeds more than [`MACRO_STEP_LIMIT`] keys is stopped the same way.
    pub(crate) fn feed_keys(&mut self, keys: &[KeyEvent], effects: &mut Vec<Effect>) -> bool {
        self.fed_keys += 1;
        let mut ran = true;
        for key in keys {
            if self.macro_depth > 0 {
                self.macro_steps += 1;
                if self.macro_steps > MACRO_STEP_LIMIT {
                    effects.push(Effect::Error(format!(
                        "a macro ran for more than {MACRO_STEP_LIMIT} keys"
                    )));
                    ran = false;
                    break;
                }
            }
            let mut produced = self.handle_key(*key);
            ran = !produced
                .iter()
                .any(|effect| matches!(effect, Effect::Error(_)));
            effects.append(&mut produced);
            if !ran {
                break;
            }
        }
        self.fed_keys -= 1;
        ran
    }

    /// `` ` `` and `'`: moves to a mark, `'` to the first non-blank of the line it is on.
    fn jump_mark(&mut self, name: char, to_line_start: bool) -> Vec<Effect> {
        let Some(mark) = self.mark(name) else {
            return vec![Effect::Error(format!("mark {name} is not set"))];
        };
        let mark = self.buffer.clamp(mark);
        self.set_cursor(if to_line_start {
            motion::first_non_blank(&self.buffer, mark.line)
        } else {
            mark
        });
        Vec::new()
    }

    /// Moves the marks over a change that added or took away lines.
    ///
    /// Tracking is by line and best-effort rather than the exact tracking Vim does through
    /// every edit: a mark below the first line that differs moves by however many lines came
    /// or went, and a mark on a line that went away is dropped. A change inside a line leaves
    /// the marks on it where they are, so a mark's column can end up past the end of its line;
    /// a jump clamps it back onto the line.
    fn move_marks(&mut self, before: &Buffer) {
        // A line past the end of a buffer reads as empty, so a change that only added or took
        // away empty lines at the end finds no line that differs; those lines came or went at
        // the end of the shorter buffer.
        let changed_at = (0..before.line_count().max(self.buffer.line_count()))
            .find(|line| before.line_text(*line) != self.buffer.line_text(*line))
            .unwrap_or_else(|| before.line_count().min(self.buffer.line_count()));
        let added = self.buffer.line_count() as isize - before.line_count() as isize;
        self.marks.retain(|_, mark| {
            if mark.line < changed_at {
                return true;
            }
            match mark.line.checked_add_signed(added) {
                Some(line) if line >= changed_at => {
                    mark.line = line;
                    true
                }
                _ => false,
            }
        });
    }

    /// Moves the cursor onto a position it can occupy, which is how an Ex command running over
    /// a range walks from line to line.
    pub(crate) fn set_cursor(&mut self, pos: Position) {
        self.cursor = self.buffer.clamp(pos);
        self.motion_context.desired_col = self.cursor.col;
    }

    /// The buffer an Ex command edits.
    pub(crate) fn buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffer
    }

    /// Fills the unnamed register with what `:d` took, as `dd` does.
    pub(crate) fn store_deleted(&mut self, content: RegisterContent) {
        self.registers.store(None, content);
    }

    /// Closes whatever the keys of a `:norm` left open, the way Vim closes an unfinished
    /// command at the end of a `:normal`. Two `<Esc>`s are enough: one leaves Insert or
    /// Visual mode, and a second drops the keys a half-typed command left pending.
    pub(crate) fn leave_pending_modes(&mut self) {
        let mut closing = Vec::new();
        for _ in 0..2 {
            if self.mode == Mode::Normal && !self.grammar.is_pending() {
                break;
            }
            self.feed_keys(&[KeyEvent::key(KeyCode::Esc)], &mut closing);
        }
    }

    /// Opens the undo unit the command about to run belongs to. The Insert session an `i`,
    /// a `c` or an `o` starts is one unit, so its later keys open nothing of their own, and
    /// so does every command an Ex command drives.
    fn open_change(&mut self) {
        if self.running_ex {
            return;
        }
        self.history.begin(&self.buffer);
    }

    /// Closes the undo unit `command` opened and makes it the change `.` repeats, unless it
    /// entered Insert mode: that unit runs until `<Esc>`, and the keys typed until then
    /// belong to it. A command that altered nothing is neither kept nor repeatable.
    fn close_change(&mut self, command: Command) {
        // Playing a macro is a way of typing keys rather than a change of its own: the
        // commands it played opened and closed their own units, and the last of them is the
        // change `.` repeats.
        if self.running_ex || matches!(command, Command::PlayMacro { .. }) {
            return;
        }
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
        if self.running_ex {
            return;
        }
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

    /// The effects `keys` asked the host for.
    fn effects(text: &str, keys: &str) -> Vec<Effect> {
        let mut editor = Editor::new(text);
        editor.handle_keys(keys).expect("key string should parse")
    }

    /// The one error `keys` reported, which fails the test when they reported anything else.
    fn error(text: &str, keys: &str) -> String {
        match effects(text, keys).as_slice() {
            [Effect::Error(complaint)] => complaint.clone(),
            other => panic!("expected one error, got {other:?}"),
        }
    }

    #[test]
    fn a_command_line_holds_the_keys_typed_into_it() {
        let editor = run("ab", ":wq");
        assert_eq!(editor.mode(), Mode::CommandLine);
        assert_eq!(editor.command_line(), Some(":wq"));

        assert_eq!(run("ab", ":wq<BS>").command_line(), Some(":w"));
        assert_eq!(run("ab", "/foo").command_line(), Some("/foo"));

        let editor = run("ab", ":wq<Esc>");
        assert_eq!(editor.mode(), Mode::Normal);
        assert_eq!(editor.command_line(), None);

        let editor = run("ab", ":w<BS><BS>");
        assert_eq!(
            editor.mode(),
            Mode::Normal,
            "backspacing over the : drops it"
        );
        assert_eq!(editor.command_line(), None);
    }

    #[test]
    fn search_moves_to_the_next_match_and_n_walks_on() {
        assert_eq!(
            run("foo bar foo", "/foo<CR>").cursor(),
            Position::new(0, 8),
            "the match the cursor is already on is stepped over"
        );
        assert_eq!(
            run("foo\nbar\nfoo", "/foo<CR>n").cursor(),
            Position::new(0, 0)
        );
        assert_eq!(
            run("a\nfoo\nb\nfoo", "/foo<CR>N").cursor(),
            Position::new(3, 0)
        );
        assert_eq!(
            run("a\nfoo\nb\nfoo", "?foo<CR>").cursor(),
            Position::new(3, 0)
        );
        assert_eq!(
            run("a\nfoo\nb\nfoo", "?foo<CR>n").cursor(),
            Position::new(1, 0),
            "n keeps the direction the search ran in"
        );
        assert_eq!(
            run("a\nfoo\nb\nfoo\nc\nfoo", "/foo<CR>2n").cursor(),
            Position::new(5, 0)
        );
        assert_eq!(
            run("a\nfoo\nb\nfoo", "/foo<CR>/<CR>").cursor(),
            Position::new(3, 0),
            "an empty pattern is the last one again"
        );
    }

    #[test]
    fn star_searches_for_the_word_under_the_cursor() {
        assert_eq!(
            run("foo bar\nfoobar\nfoo", "*").cursor(),
            Position::new(2, 0),
            "the word boundaries keep foobar out"
        );
        assert_eq!(run("foo bar\nfoo", "j0#").cursor(), Position::new(0, 0));
        assert_eq!(
            run("foo bar\nfoo", "*n").cursor(),
            Position::new(0, 0),
            "the word becomes the search n repeats"
        );
    }

    #[test]
    fn a_search_reports_what_it_could_not_do() {
        assert_eq!(error("abc", "/zz<CR>"), "pattern not found: zz");
        assert!(error("abc", "/a(<CR>").starts_with("a( is not a valid pattern"));
        assert_eq!(error("abc", "n"), "there is no search to repeat");
        assert_eq!(error(" abc", "*"), "there is no word under the cursor");
    }

    #[test]
    fn write_and_quit_are_handed_to_the_host() {
        assert_eq!(
            effects("ab", ":w<CR>"),
            vec![Effect::SaveRequested { path: None }]
        );
        assert_eq!(
            effects("ab", ":w out.txt<CR>"),
            vec![Effect::SaveRequested {
                path: Some("out.txt".to_owned())
            }]
        );
        assert_eq!(
            effects("ab", ":q<CR>"),
            vec![Effect::QuitRequested { force: false }]
        );
        assert_eq!(
            effects("ab", ":q!<CR>"),
            vec![Effect::QuitRequested { force: true }]
        );
        for keys in [":wq<CR>", ":x<CR>"] {
            assert_eq!(
                effects("ab", keys),
                vec![
                    Effect::SaveRequested { path: None },
                    Effect::QuitRequested { force: false }
                ],
                "{keys}"
            );
        }
        assert_eq!(
            effects("ab", ":<CR>"),
            Vec::new(),
            "an empty line does nothing"
        );
        assert_eq!(error("ab", ":nope<CR>"), "not an editor command: nope");
    }

    #[test]
    fn substitute_replaces_over_the_lines_the_range_names() {
        assert_eq!(
            run("foo foo\nfoo", ":s/foo/bar/<CR>").text(),
            "bar foo\nfoo"
        );
        assert_eq!(
            run("foo foo\nfoo", ":s/foo/bar/g<CR>").text(),
            "bar bar\nfoo"
        );
        assert_eq!(
            run("foo\nfoo\nfoo", ":%s/foo/bar/<CR>").text(),
            "bar\nbar\nbar"
        );
        assert_eq!(
            run("a\nfoo\nfoo\nfoo", ":2,3s/foo/bar/<CR>").text(),
            "a\nbar\nbar\nfoo"
        );
        assert_eq!(run("FOO\nfoo", ":%s/foo/x/i<CR>").text(), "x\nx");
        assert_eq!(
            run("a=1\nb=2", r":%s/(\w+)=(\w+)/$2=$1/<CR>").text(),
            "1=a\n2=b",
            "a capture is put back with $1"
        );
        assert_eq!(
            run("one\ntwo\nthree", ":/two/,/three/s/^/- /<CR>").text(),
            "one\n- two\n- three",
            "a pattern names a line of the range"
        );
        assert_eq!(
            run("a/b", r":s/a\/b/c/<CR>").text(),
            "c",
            "a delimiter inside a pattern is escaped"
        );
        assert_eq!(
            run("foo\nbar", ":s/foo/bar/<CR>").cursor(),
            Position::new(0, 0)
        );
        assert_eq!(error("abc", ":%s/zz/y/<CR>"), "pattern not found: zz");
    }

    #[test]
    fn delete_takes_the_lines_the_range_names() {
        assert_eq!(run("a\nb\nc\nd", ":2,3d<CR>").text(), "a\nd");
        assert_eq!(run("a\nb\nc", ":d<CR>").text(), "b\nc");
        assert_eq!(run("a\nb\nc", ":%d<CR>").text(), "");
        assert_eq!(run("a\nb\nc", ":2,$d<CR>").text(), "a");
        assert_eq!(
            run("a\nb\nc", ":2,3d<CR>").registers().get(None),
            Some(&RegisterContent::linewise("b\nc".to_owned())),
            ":d fills the unnamed register the way dd does"
        );
        assert_eq!(
            error("a\nb\nc", ":3,2d<CR>"),
            "the range ends before it starts"
        );
    }

    #[test]
    fn global_runs_its_command_on_every_line_that_matches() {
        assert_eq!(run("a1\nb\na2\nc", ":g/^a/d<CR>").text(), "b\nc");
        assert_eq!(run("a1\nb\na2\nc", ":v/^a/d<CR>").text(), "a1\na2");
        assert_eq!(
            run(
                "import a\nlet b\nimport c",
                ":g/^import/norm A;<lt>Esc><CR>"
            )
            .text(),
            "import a;\nlet b\nimport c;",
            "the keys reach every marked line even though earlier ones grew"
        );
        assert_eq!(run("ax\nb\ncx", ":g/x/s/x/y/<CR>").text(), "ay\nb\ncy");
        assert_eq!(
            run("a\nb\na\nb\na", ":g/a/norm dd<CR>").text(),
            "b\nb",
            "the marks hold while the lines under them move up"
        );
        assert_eq!(error("a\nb", ":g/zz/d<CR>"), "pattern not found: zz");
        assert_eq!(error("a\nb", ":g/a/w<CR>"), ":g runs d, s or norm, not :w");
    }

    #[test]
    fn normal_types_its_keys_at_the_start_of_each_line_in_the_range() {
        assert_eq!(
            run("ab\ncd\nef", ":%norm A!<lt>Esc><CR>").text(),
            "ab!\ncd!\nef!"
        );
        assert_eq!(
            run("ab\ncd\nef", ":2,3norm x<CR>").text(),
            "ab\nd\nf",
            "keys that leave no mode open need no <Esc> of their own"
        );
        assert_eq!(
            run("foo bar", ":norm ciwX<lt>Esc><CR>").text(),
            "X bar",
            "an unfinished Insert session is closed at the end of the keys"
        );
        assert_eq!(
            run("ab\ncd", ":%norm xzx<CR>").text(),
            "b\nd",
            "the key that failed ends that line's run, and the next line still runs"
        );
        assert!(
            error("ab", ":norm i<lt>Nope><CR>")
                .starts_with(":norm was given keys that do not parse"),
        );
    }

    #[test]
    fn an_ex_command_is_one_undo_unit() {
        assert_eq!(
            run("foo\nfoo\nfoo", ":%s/foo/bar/<CR>u").text(),
            "foo\nfoo\nfoo"
        );
        assert_eq!(
            run("import a\nimport b", ":g/^import/norm A;<lt>Esc><CR>u").text(),
            "import a\nimport b"
        );
        assert_eq!(run("a\nb\nc", ":%d<CR>u").text(), "a\nb\nc");
        assert_eq!(run("foo\nfoo", ":%s/foo/bar/<CR>u<C-r>").text(), "bar\nbar");
    }

    #[test]
    fn a_line_that_is_only_a_number_moves_the_cursor_to_it() {
        assert_eq!(run("a\n  b\nc", ":2<CR>").cursor(), Position::new(1, 2));
        assert_eq!(run("a\nb\nc", ":$<CR>").cursor(), Position::new(2, 0));
    }

    #[test]
    fn a_recording_holds_the_keys_typed_between_the_two_qs() {
        assert_eq!(run("abc", "qa").recording_register(), Some('a'));

        let editor = run("abc", "qaxq");
        assert_eq!(editor.recording_register(), None);
        assert_eq!(
            editor.registers().get(Some('a')),
            Some(&RegisterContent::charwise("x".to_owned())),
            "the q that ended the recording is not part of the macro"
        );
        assert_eq!(
            editor.registers().get(None).map(|held| held.text.as_str()),
            Some("a"),
            "recording into a register leaves the unnamed one to the delete the macro made"
        );
    }

    #[test]
    fn a_recording_keeps_the_keys_typed_at_it_and_not_the_ones_it_typed_itself() {
        let editor = run("ab\ncd", "qa:%norm x<CR>q");
        assert_eq!(editor.text(), "b\nd");
        assert_eq!(
            editor
                .registers()
                .get(Some('a'))
                .map(|held| held.text.as_str()),
            Some(":%norm x<CR>"),
            "the keys :norm typed on each line are not keys the user typed"
        );

        let editor = run("abcdef", "qbxqqa@b@bq");
        assert_eq!(
            editor
                .registers()
                .get(Some('a'))
                .map(|held| held.text.as_str()),
            Some("@b@b"),
            "a nested playback is kept as the @ that asked for it"
        );
    }

    #[test]
    fn playing_a_macro_types_its_keys_again() {
        assert_eq!(run("a\nb\nc", "qaA!<Esc>jq@a@@").text(), "a!\nb!\nc!");
        assert_eq!(run("1\n2\n3\n4", "qaA;<Esc>jq3@a").text(), "1;\n2;\n3;\n4;");
        assert_eq!(
            run("ab\ncd", "qaxq@au").text(),
            "b\ncd",
            "the commands a macro played are undone one at a time"
        );
        assert_eq!(
            run("a b c d", "qadwq@a.").text(),
            "d",
            ". repeats the last change the macro made rather than the whole macro"
        );
    }

    #[test]
    fn playing_a_macro_reports_what_it_could_not_do() {
        assert_eq!(error("ab", "@a"), "register \"a is empty");
        assert_eq!(error("ab", "@@"), "there is no macro to play again");
    }

    #[test]
    fn a_macro_that_plays_itself_stops_at_the_nesting_limit() {
        let mut editor = Editor::new("ab");
        let effects = editor
            .handle_keys("qaA!<Esc>@aq@a")
            .expect("key string should parse");
        assert_eq!(
            editor.text(),
            format!("ab{}", "!".repeat(MACRO_DEPTH_LIMIT + 1)),
            "the recording itself typed the first one, and the playbacks the rest"
        );
        assert!(
            effects.iter().any(|effect| matches!(
                effect,
                Effect::Error(complaint) if complaint.contains("nested")
            )),
            "{effects:?}"
        );
    }

    #[test]
    fn a_mark_is_a_position_the_jump_keys_go_back_to() {
        assert_eq!(
            run("alpha\nbravo", "jllmagg`a").cursor(),
            Position::new(1, 2)
        );
        assert_eq!(
            run("alpha\n  bravo", "j4lmagg'a").cursor(),
            Position::new(1, 2),
            "' goes to the first non-blank of the marked line"
        );
        assert_eq!(error("ab", "`a"), "mark a is not set");
    }

    #[test]
    fn a_mark_moves_with_the_lines_a_change_adds_or_takes_away() {
        assert_eq!(
            run("a\nb\nc", "jjmaggOx<Esc>").mark('a'),
            Some(Position::new(3, 0)),
            "a line opened above the mark pushes it down"
        );
        assert_eq!(
            run("a\nb\nc", "jjmaggdd").mark('a'),
            Some(Position::new(1, 0))
        );
        assert_eq!(
            run("a\nb\nc", "jmajdd").mark('a'),
            Some(Position::new(1, 0)),
            "a change below the mark leaves it alone"
        );
        assert_eq!(
            run("a\nb\nc", "jmadd").mark('a'),
            None,
            "a mark on a line that went away is dropped"
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
