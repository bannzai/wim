//! The Ex command line: the `:` commands and the line ranges they run over.
//!
//! Parsing and running are kept apart: [`parse`] turns a typed line into an [`ExCommand`]
//! without looking at a buffer, and [`execute`] runs one against an [`Editor`]. Everything a
//! command cannot do itself — writing a file, quitting — leaves as an [`Effect`] for the host.
//!
//! What is supported is Vim's shape rather than all of Vim: the range is `%`, `N`, `.`, `$`
//! or `/pattern/`, optionally as a `first,last` pair, with no `+1` arithmetic and no marks;
//! `:s` uses `/` as its delimiter, with `\/` for a literal one; and `:g` runs `d`, `s` or
//! `norm` over the lines it marks. A name that is none of those is not an error here: it leaves
//! as an [`Effect::UnknownExCommand`] for the host, which is where a plugin's commands live.
//!
//! The keys of `:norm` are read with [`parse_keys`], so a key that has no character of its own
//! is written in that notation — `:norm A;<Esc>` ends the Insert session `A` opened. Those are
//! five characters typed into the command line rather than an `<Esc>` key press, which would
//! drop the command line instead; a key string that drives the editor from outside therefore
//! writes the opening bracket as `<lt>`, as in `:g/^import/norm A;<lt>Esc>`.

use std::fmt;
use std::ops::ControlFlow;

use crate::edit;
use crate::editor::Editor;
use crate::effect::{Effect, Event};
use crate::key::{KeyEvent, KeyParseError, parse_keys};
use crate::motion::first_non_blank;
use crate::position::Position;
use crate::search::{self, SearchError};
use crate::textobject::TextRange;

/// One end of a line range.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Address {
    /// `.`: the line the cursor is on.
    Current,
    /// `$`: the last line of the buffer.
    Last,
    /// A line number as typed, counted from 1.
    Line(usize),
    /// `/pattern/` or `?pattern?`: the line the search lands on.
    Pattern {
        pattern: String,
        /// `?` rather than `/`.
        backward: bool,
    },
}

/// The lines a command runs over. A range written as a single address covers that line alone.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LineRange {
    first: Address,
    last: Address,
}

/// What a `:` line asks for, once the range in front of it has been taken off.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ExKind {
    /// `:w [path]`
    Write { path: Option<String> },
    /// `:q` and `:q!`
    Quit { force: bool },
    /// `:wq [path]` and `:x`
    WriteQuit { path: Option<String>, force: bool },
    /// `:s/pattern/replacement/[g][i]`
    Substitute {
        pattern: String,
        /// The replacement, in which `$1` stands for a capture.
        replacement: String,
        /// `g`: every match on a line rather than the first.
        all: bool,
        /// `i`
        ignore_case: bool,
    },
    /// `:g/pattern/{command}` and `:v/pattern/{command}`
    Global {
        pattern: String,
        /// `:v`: the lines that do not match.
        invert: bool,
        /// What to run on each marked line, which carries no range of its own.
        command: Box<ExKind>,
    },
    /// `:norm {keys}`
    Normal { keys: String },
    /// `:d`
    Delete,
    /// A line with a range and no command, which moves the cursor there.
    Goto,
    /// A name that is no command of the core's, which the host may have one for.
    Unknown {
        /// The name as it was typed: the whole token up to the first blank, whatever it is
        /// made of.
        name: String,
        /// Everything after the name, kept as text for whatever command the host resolves.
        args: String,
    },
}

/// A `:` line, ready to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExCommand {
    range: Option<LineRange>,
    kind: ExKind,
}

/// Why a `:` line could not be run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExError {
    /// A command was typed without something it cannot run without.
    Incomplete(String),
    /// The range names lines the buffer cannot give.
    BadRange(String),
    /// The pattern does not compile, or matched nothing where a match was needed.
    BadPattern(SearchError),
    /// A `:s` flag other than `g` and `i`.
    UnknownFlag(char),
    /// The keys of `:norm` are not a key string.
    BadKeys(KeyParseError),
    /// `:g` was given something other than `d`, `s` or `norm`, or something with a range of
    /// its own.
    BadGlobalCommand(String),
}

impl fmt::Display for ExError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Incomplete(complaint) => f.write_str(complaint),
            Self::BadRange(complaint) => f.write_str(complaint),
            Self::BadPattern(error) => error.fmt(f),
            Self::UnknownFlag(flag) => write!(f, "unknown flag on :s: {flag}"),
            Self::BadKeys(error) => write!(f, ":norm was given keys that do not parse: {error}"),
            Self::BadGlobalCommand(command) => {
                write!(f, ":g runs d, s or norm, not {command}")
            }
        }
    }
}

impl std::error::Error for ExError {}

/// Reads a typed command line, without its leading `:`.
pub fn parse(line: &str) -> Result<ExCommand, ExError> {
    let (range, rest) = parse_range(line.trim_start())?;
    Ok(ExCommand {
        range,
        kind: parse_kind(rest.trim_start())?,
    })
}

/// Runs `command` and returns what the host has to carry out. Anything that went wrong comes
/// back as an [`Effect::Error`] rather than stopping the editor.
pub(crate) fn execute(editor: &mut Editor, command: &ExCommand) -> Vec<Effect> {
    match run(editor, command) {
        Ok(effects) => effects,
        Err(error) => vec![Effect::Error(error.to_string())],
    }
}

fn run(editor: &mut Editor, command: &ExCommand) -> Result<Vec<Effect>, ExError> {
    match &command.kind {
        // The event goes in front of the request it belongs to: a host carries the effects out
        // in order, so a `buffer-write` handler has run — and whatever it edited is in the
        // buffer — by the time the host reads the text the save writes.
        ExKind::Write { path } => Ok(vec![
            Effect::Event(Event::BufferWrite),
            Effect::SaveRequested { path: path.clone() },
        ]),
        ExKind::Quit { force } => Ok(vec![Effect::QuitRequested { force: *force }]),
        ExKind::WriteQuit { path, force } => Ok(vec![
            Effect::Event(Event::BufferWrite),
            Effect::SaveRequested { path: path.clone() },
            Effect::QuitRequested { force: *force },
        ]),
        ExKind::Goto => {
            let (_, last) = resolve_range(editor, command.range.as_ref())?;
            editor.set_cursor(first_non_blank(editor.buffer(), last));
            Ok(Vec::new())
        }
        ExKind::Delete => {
            let (first, last) = resolve_range(editor, command.range.as_ref())?;
            delete_lines(editor, first, last);
            Ok(Vec::new())
        }
        ExKind::Substitute {
            pattern,
            replacement,
            all,
            ignore_case,
        } => {
            let (first, last) = resolve_range(editor, command.range.as_ref())?;
            substitute(
                editor,
                (first, last),
                pattern,
                replacement,
                *all,
                *ignore_case,
            )?;
            Ok(Vec::new())
        }
        ExKind::Normal { keys } => {
            let (first, last) = resolve_range(editor, command.range.as_ref())?;
            let keys = parse_keys(keys).map_err(ExError::BadKeys)?;
            let mut effects = Vec::new();
            walk_marked_lines(
                editor,
                &(first..=last).collect::<Vec<usize>>(),
                |editor, line| {
                    run_keys_on_line(editor, line, &keys, &mut effects);
                    if walk_ended(&effects) {
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                },
            );
            Ok(effects)
        }
        ExKind::Global {
            pattern,
            invert,
            command: over,
        } => global(editor, command.range.as_ref(), pattern, *invert, over),
        // The lines a range names would be the core's to resolve and nothing carries them over,
        // so a host's command is one that runs over the buffer it is handed and takes no range.
        ExKind::Unknown { name, .. } if command.range.is_some() => {
            Err(ExError::BadRange(format!(":{name} takes no range")))
        }
        ExKind::Unknown { name, args } => Ok(vec![Effect::UnknownExCommand {
            name: name.clone(),
            args: args.clone(),
        }]),
    }
}

/// Runs `command` on every line of the buffer that `pattern` matches, or that it does not
/// with `invert`. `:g` runs over the whole buffer unless the line named a range.
fn global(
    editor: &mut Editor,
    range: Option<&LineRange>,
    pattern: &str,
    invert: bool,
    command: &ExKind,
) -> Result<Vec<Effect>, ExError> {
    let (first, last) = match range {
        Some(_) => resolve_range(editor, range)?,
        None => (0, editor.buffer().line_count() - 1),
    };
    let regex = search::compile(pattern, false).map_err(ExError::BadPattern)?;
    let marks: Vec<usize> = (first..=last)
        .filter(|line| regex.is_match(&editor.buffer().line_text(*line)) != invert)
        .collect();
    if marks.is_empty() {
        return Err(ExError::BadPattern(SearchError::NotFound {
            pattern: pattern.to_owned(),
        }));
    }

    let mut effects = Vec::new();
    match command {
        // `:d` and `:s` reach the buffer without a key being read, so the walk is told what
        // they did rather than hearing it from `Editor::handle_key`.
        ExKind::Delete => walk_marked_lines(editor, &marks, |editor, line| {
            editor.edit_directly(|editor| delete_lines(editor, line, line));
            ControlFlow::Continue(())
        }),
        ExKind::Normal { keys } => {
            let keys = parse_keys(keys).map_err(ExError::BadKeys)?;
            walk_marked_lines(editor, &marks, |editor, line| {
                run_keys_on_line(editor, line, &keys, &mut effects);
                if walk_ended(&effects) {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            });
        }
        ExKind::Substitute {
            pattern,
            replacement,
            all,
            ignore_case,
        } => {
            // Compiling here reports a broken pattern once, rather than on every marked line.
            search::compile(pattern, *ignore_case).map_err(ExError::BadPattern)?;
            walk_marked_lines(editor, &marks, |editor, line| {
                editor.edit_directly(|editor| {
                    // A marked line the substitution does not match is not a failure of the
                    // `:g`.
                    let _ = substitute(
                        editor,
                        (line, line),
                        pattern,
                        replacement,
                        *all,
                        *ignore_case,
                    );
                });
                ControlFlow::Continue(())
            });
        }
        other => return Err(ExError::BadGlobalCommand(format!("{other:?}"))),
    }
    Ok(effects)
}

/// Runs `act` on each of the marked lines, the way `:g` does: the lines are picked before
/// anything runs, and each one is found again through however many lines the edits before it
/// added or took away. A line the edits removed is skipped, and `act` ends the walk early by
/// breaking.
fn walk_marked_lines(
    editor: &mut Editor,
    marks: &[usize],
    mut act: impl FnMut(&mut Editor, usize) -> ControlFlow<()>,
) {
    editor.begin_line_walk(marks.iter().copied());
    while let Some(line) = editor.next_walked_line() {
        if act(editor, line).is_break() {
            break;
        }
    }
    editor.end_line_walk();
}

/// Whether the effects hold something that ends a walk before its lines run out.
///
/// A request to put the editor down is one: `:q` inside a `:norm` ends the run over the text
/// rather than the run over the one line that typed it. A `:` line the host's to run is the
/// other, for the reason it ends the keys of a single line ([`Editor::feed_keys`]): the host
/// does not get to run it until the keys have returned, so the lines behind it would be walked
/// over the buffer the command was not run over yet, each of them leaving the cursor somewhere
/// else for the one snapshot the host finally takes.
fn walk_ended(effects: &[Effect]) -> bool {
    effects.iter().any(|effect| {
        matches!(
            effect,
            Effect::QuitRequested { .. } | Effect::UnknownExCommand { .. }
        )
    })
}

/// Types `keys` at the start of `line`, as `:norm` does. A key that fails ends the line's run,
/// and an unfinished command is closed the way `<Esc>` would close it.
fn run_keys_on_line(
    editor: &mut Editor,
    line: usize,
    keys: &[KeyEvent],
    effects: &mut Vec<Effect>,
) {
    editor.set_cursor(Position::new(line, 0));
    editor.feed_keys(keys, effects);
    editor.leave_pending_modes();
}

/// Replaces what `pattern` matches on each line of the range, `:s`.
fn substitute(
    editor: &mut Editor,
    (first, last): (usize, usize),
    pattern: &str,
    replacement: &str,
    all: bool,
    ignore_case: bool,
) -> Result<(), ExError> {
    let regex = search::compile(pattern, ignore_case).map_err(ExError::BadPattern)?;
    let mut changed = None;
    for line in first..=last.min(editor.buffer().line_count() - 1) {
        let text = editor.buffer().line_text(line);
        let replaced = if all {
            regex.replace_all(&text, replacement)
        } else {
            regex.replace(&text, replacement)
        };
        if replaced.as_ref() == text.as_str() {
            continue;
        }
        let replaced = replaced.into_owned();
        let buffer = editor.buffer_mut();
        // The replacement goes in before the old text goes out: a line emptied in between
        // would be a line the buffer no longer holds when it is the last one, and the text
        // put back would land on the line above it.
        let columns_before = buffer.line_len(line);
        buffer.insert(Position::new(line, 0), &replaced);
        let columns_together = buffer.line_len(line);
        buffer.delete(
            Position::new(line, columns_together - columns_before),
            Position::new(line, columns_together),
        );
        changed = Some(line);
    }
    let Some(line) = changed else {
        return Err(ExError::BadPattern(SearchError::NotFound {
            pattern: pattern.to_owned(),
        }));
    };
    editor.set_cursor(first_non_blank(editor.buffer(), line));
    Ok(())
}

/// Takes lines `first` through `last` out of the buffer, `:d`, filling the unnamed register
/// with them the way `dd` does.
fn delete_lines(editor: &mut Editor, first: usize, last: usize) {
    let range = TextRange::lines(editor.buffer(), first, last);
    let (content, cursor) = edit::delete(editor.buffer_mut(), range);
    editor.store_deleted(content);
    editor.set_cursor(cursor);
}

/// The lines `range` covers, as buffer line indices. No range is the cursor's line.
fn resolve_range(editor: &Editor, range: Option<&LineRange>) -> Result<(usize, usize), ExError> {
    let Some(range) = range else {
        return Ok((editor.cursor().line, editor.cursor().line));
    };
    let first = resolve_address(editor, &range.first, editor.cursor().line)?;
    // A second address searches on from the first, so that `:/start/,/end/` reads as a span.
    let last = resolve_address(editor, &range.last, first)?;
    if last < first {
        return Err(ExError::BadRange(
            "the range ends before it starts".to_owned(),
        ));
    }
    Ok((first, last))
}

fn resolve_address(editor: &Editor, address: &Address, from: usize) -> Result<usize, ExError> {
    let last_line = editor.buffer().line_count() - 1;
    match address {
        Address::Current => Ok(editor.cursor().line.min(last_line)),
        Address::Last => Ok(last_line),
        // Line numbers are counted from 1, and a number past the end names the last line.
        Address::Line(number) => Ok(number.saturating_sub(1).min(last_line)),
        Address::Pattern { pattern, backward } => {
            let start = if *backward {
                Position::new(from, 0)
            } else {
                Position::new(from, editor.buffer().line_len(from))
            };
            search::find(editor.buffer(), start, pattern, *backward)
                .map(|found| found.line)
                .map_err(ExError::BadPattern)
        }
    }
}

/// Splits the range off the front of a command line.
fn parse_range(line: &str) -> Result<(Option<LineRange>, &str), ExError> {
    if let Some(rest) = line.strip_prefix('%') {
        return Ok((
            Some(LineRange {
                first: Address::Line(1),
                last: Address::Last,
            }),
            rest,
        ));
    }
    let Some((first, rest)) = parse_address(line) else {
        return Ok((None, line));
    };
    let Some(rest) = rest.strip_prefix(',') else {
        return Ok((
            Some(LineRange {
                first: first.clone(),
                last: first,
            }),
            rest,
        ));
    };
    let Some((last, rest)) = parse_address(rest) else {
        return Err(ExError::BadRange(format!(
            "the range has no line after the comma: {line}"
        )));
    };
    Ok((Some(LineRange { first, last }), rest))
}

/// Reads one address off the front of `line`, `None` when `line` opens with none.
fn parse_address(line: &str) -> Option<(Address, &str)> {
    let mut characters = line.char_indices();
    let (_, opener) = characters.next()?;
    match opener {
        '.' => Some((Address::Current, &line[1..])),
        '$' => Some((Address::Last, &line[1..])),
        '/' | '?' => {
            let (pattern, rest) = split_field(&line[1..], opener);
            Some((
                Address::Pattern {
                    pattern,
                    backward: opener == '?',
                },
                rest.unwrap_or(""),
            ))
        }
        digit if digit.is_ascii_digit() => {
            let end = line
                .find(|character: char| !character.is_ascii_digit())
                .unwrap_or(line.len());
            let number = line[..end].parse().unwrap_or(usize::MAX);
            Some((Address::Line(number), &line[end..]))
        }
        _ => None,
    }
}

fn parse_kind(rest: &str) -> Result<ExKind, ExError> {
    // A line that is nothing but a range moves the cursor to where the range ends.
    if rest.is_empty() {
        return Ok(ExKind::Goto);
    }
    let name: String = rest.chars().take_while(char::is_ascii_alphabetic).collect();
    let typed = &rest[name.len()..];
    if !names_a_builtin(&name, typed) {
        return Ok(unknown(rest));
    }
    let (force, args) = match typed.strip_prefix('!') {
        Some(args) => (true, args),
        None => (false, typed),
    };
    match name.as_str() {
        "w" | "write" => Ok(ExKind::Write { path: path(args) }),
        "q" | "quit" => Ok(ExKind::Quit { force }),
        "wq" => Ok(ExKind::WriteQuit {
            path: path(args),
            force,
        }),
        "x" | "xit" => Ok(ExKind::WriteQuit { path: None, force }),
        "d" | "delete" => Ok(ExKind::Delete),
        "s" | "substitute" => parse_substitute(args),
        "g" | "global" => parse_global(args, false),
        "v" | "vglobal" => parse_global(args, true),
        // The keys are text rather than a command, so they keep the blanks inside them and
        // lose only the one that separates them from the command name.
        "norm" | "normal" => Ok(ExKind::Normal {
            keys: args.strip_prefix(' ').unwrap_or(args).to_owned(),
        }),
        _ => Ok(unknown(rest)),
    }
}

/// Whether the letters a line opens with name a command of the core's.
///
/// They do only when the rest of the line — `typed` — goes on the way that command's own syntax
/// does: the name is the whole of it, a blank separates its arguments, or it opens with the `!`
/// of a force or the delimiter a `:s` or `:g` pattern is written between. `write-json` and `x2`
/// go on in none of those ways, so the letters they open with are not `:w` and `:x` with an
/// argument stuck to them; they are names of their own, and it is [`unknown`] that reads them.
fn names_a_builtin(name: &str, typed: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    match typed.chars().next() {
        None => true,
        Some(character) => character.is_whitespace() || matches!(character, '!' | '/' | '?'),
    }
}

/// The line as a command the host may have: the whole blank-delimited token it opens with, and
/// the rest of it as the arguments.
///
/// The ABI puts no shape on what a plugin publishes (`wit/plugin.wit`), so `sort-lines`,
/// `format_json` and `tool2` are names as much as `upcase` is, and so is a name the core reads
/// one of its own out of the front of. What an argument is belongs to the host's command: the
/// `!` is theirs as much as the rest, so what crosses is the line as it was typed less the one
/// blank after the name.
fn unknown(rest: &str) -> ExKind {
    let (name, typed) = split_name(rest);
    ExKind::Unknown {
        name: name.to_owned(),
        args: typed
            .strip_prefix(char::is_whitespace)
            .unwrap_or(typed)
            .to_owned(),
    }
}

/// The blank-delimited token `rest` opens with, and everything after it.
fn split_name(rest: &str) -> (&str, &str) {
    match rest.find(char::is_whitespace) {
        Some(end) => (&rest[..end], &rest[end..]),
        None => (rest, ""),
    }
}

fn parse_substitute(args: &str) -> Result<ExKind, ExError> {
    let Some(args) = args.strip_prefix('/') else {
        return Err(ExError::Incomplete(
            ":s needs a pattern, as in :s/old/new/".to_owned(),
        ));
    };
    let (pattern, rest) = split_field(args, '/');
    if pattern.is_empty() {
        return Err(ExError::Incomplete(
            ":s needs a pattern, as in :s/old/new/".to_owned(),
        ));
    }
    let (replacement, flags) = match rest {
        Some(rest) => split_field(rest, '/'),
        None => (String::new(), None),
    };
    let mut all = false;
    let mut ignore_case = false;
    for flag in flags.unwrap_or("").chars() {
        match flag {
            'g' => all = true,
            'i' => ignore_case = true,
            other => return Err(ExError::UnknownFlag(other)),
        }
    }
    Ok(ExKind::Substitute {
        pattern,
        replacement,
        all,
        ignore_case,
    })
}

fn parse_global(args: &str, invert: bool) -> Result<ExKind, ExError> {
    let Some(args) = args.strip_prefix('/') else {
        return Err(ExError::Incomplete(
            ":g needs a pattern, as in :g/pattern/d".to_owned(),
        ));
    };
    let (pattern, rest) = split_field(args, '/');
    if pattern.is_empty() {
        return Err(ExError::Incomplete(
            ":g needs a pattern, as in :g/pattern/d".to_owned(),
        ));
    }
    let command = rest.unwrap_or("").trim();
    if command.is_empty() {
        return Err(ExError::Incomplete(
            ":g needs a command to run, as in :g/pattern/d".to_owned(),
        ));
    }
    let command = parse(command)?;
    if command.range.is_some() {
        return Err(ExError::BadGlobalCommand(
            "a command with a range of its own".to_owned(),
        ));
    }
    if !matches!(
        command.kind,
        ExKind::Delete | ExKind::Substitute { .. } | ExKind::Normal { .. }
    ) {
        return Err(ExError::BadGlobalCommand(command.kind_name().to_owned()));
    }
    Ok(ExKind::Global {
        pattern,
        invert,
        command: Box::new(command.kind),
    })
}

impl ExCommand {
    /// The command's name, for the message a `:g` that cannot run it reports.
    fn kind_name(&self) -> &'static str {
        match self.kind {
            ExKind::Write { .. } => ":w",
            ExKind::Quit { .. } => ":q",
            ExKind::WriteQuit { .. } => ":wq",
            ExKind::Substitute { .. } => ":s",
            ExKind::Global { .. } => ":g",
            ExKind::Normal { .. } => ":norm",
            ExKind::Delete => ":d",
            ExKind::Goto => "a line number",
            ExKind::Unknown { .. } => "a command of the host's",
        }
    }
}

/// The text up to the next `delimiter` that is not escaped, and whatever follows it. A
/// backslash in front of the delimiter stands for the delimiter itself; every other backslash
/// is kept, so that a pattern's own escapes reach the regex.
fn split_field(text: &str, delimiter: char) -> (String, Option<&str>) {
    let mut field = String::new();
    let mut characters = text.char_indices();
    while let Some((index, character)) = characters.next() {
        match character {
            _ if character == delimiter => {
                return (field, Some(&text[index + character.len_utf8()..]));
            }
            '\\' => match characters.next() {
                Some((_, escaped)) if escaped == delimiter => field.push(delimiter),
                Some((_, escaped)) => {
                    field.push('\\');
                    field.push(escaped);
                }
                None => field.push('\\'),
            },
            _ => field.push(character),
        }
    }
    (field, None)
}

/// The file name an argument names, `None` when it names none.
fn path(args: &str) -> Option<String> {
    let path = args.trim();
    (!path.is_empty()).then(|| path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(line: &str) -> ExKind {
        parse(line).expect("the line should parse").kind
    }

    #[test]
    fn the_file_commands_carry_their_path_and_their_force() {
        assert_eq!(kind("w"), ExKind::Write { path: None });
        assert_eq!(
            kind("w out.txt"),
            ExKind::Write {
                path: Some("out.txt".to_owned())
            }
        );
        assert_eq!(kind("q"), ExKind::Quit { force: false });
        assert_eq!(kind("q!"), ExKind::Quit { force: true });
        assert_eq!(
            kind("wq"),
            ExKind::WriteQuit {
                path: None,
                force: false
            }
        );
        assert_eq!(
            kind("x"),
            ExKind::WriteQuit {
                path: None,
                force: false
            }
        );
    }

    #[test]
    fn a_range_is_read_off_the_front_of_the_line() {
        assert_eq!(
            parse("%s/a/b/").expect("the line should parse").range,
            Some(LineRange {
                first: Address::Line(1),
                last: Address::Last
            })
        );
        assert_eq!(
            parse("2,3d").expect("the line should parse").range,
            Some(LineRange {
                first: Address::Line(2),
                last: Address::Line(3)
            })
        );
        assert_eq!(
            parse("5d").expect("the line should parse").range,
            Some(LineRange {
                first: Address::Line(5),
                last: Address::Line(5)
            }),
            "one address covers that line alone"
        );
        assert_eq!(
            parse(".,$d").expect("the line should parse").range,
            Some(LineRange {
                first: Address::Current,
                last: Address::Last
            })
        );
        assert_eq!(
            parse("/foo/,?bar?d").expect("the line should parse").range,
            Some(LineRange {
                first: Address::Pattern {
                    pattern: "foo".to_owned(),
                    backward: false
                },
                last: Address::Pattern {
                    pattern: "bar".to_owned(),
                    backward: true
                }
            })
        );
        assert_eq!(parse("d").expect("the line should parse").range, None);
    }

    #[test]
    fn substitute_reads_its_pattern_replacement_and_flags() {
        assert_eq!(
            kind("s/foo/bar/gi"),
            ExKind::Substitute {
                pattern: "foo".to_owned(),
                replacement: "bar".to_owned(),
                all: true,
                ignore_case: true
            }
        );
        assert_eq!(
            kind("s/foo/bar"),
            ExKind::Substitute {
                pattern: "foo".to_owned(),
                replacement: "bar".to_owned(),
                all: false,
                ignore_case: false
            },
            "the closing delimiter may be left off"
        );
        assert_eq!(
            kind(r"s/a\/b/c/"),
            ExKind::Substitute {
                pattern: "a/b".to_owned(),
                replacement: "c".to_owned(),
                all: false,
                ignore_case: false
            }
        );
        assert_eq!(
            kind(r"s/(\w+)=(\w+)/$2=$1/"),
            ExKind::Substitute {
                pattern: r"(\w+)=(\w+)".to_owned(),
                replacement: "$2=$1".to_owned(),
                all: false,
                ignore_case: false
            },
            "a pattern's own escapes are left for the regex"
        );
        assert_eq!(kind("s/a//"), kind("s/a"));
        assert_eq!(parse("s/a/b/z"), Err(ExError::UnknownFlag('z')));
        assert!(matches!(parse("s"), Err(ExError::Incomplete(_))));
    }

    #[test]
    fn global_carries_the_command_it_runs() {
        assert_eq!(
            kind("g/^import/norm A;"),
            ExKind::Global {
                pattern: "^import".to_owned(),
                invert: false,
                command: Box::new(ExKind::Normal {
                    keys: "A;".to_owned()
                })
            }
        );
        assert_eq!(
            kind("v/foo/d"),
            ExKind::Global {
                pattern: "foo".to_owned(),
                invert: true,
                command: Box::new(ExKind::Delete)
            }
        );
        assert!(matches!(
            parse("g/foo/w"),
            Err(ExError::BadGlobalCommand(_))
        ));
        assert!(matches!(
            parse("g/foo/2,3d"),
            Err(ExError::BadGlobalCommand(_))
        ));
        assert!(matches!(parse("g/foo/"), Err(ExError::Incomplete(_))));
    }

    #[test]
    fn normal_keeps_its_keys_as_they_were_typed() {
        assert_eq!(
            kind("norm A;<Esc>"),
            ExKind::Normal {
                keys: "A;<Esc>".to_owned()
            }
        );
        assert_eq!(
            kind("normal! ciwx"),
            ExKind::Normal {
                keys: "ciwx".to_owned()
            }
        );
        assert_eq!(
            kind("norm  A "),
            ExKind::Normal {
                keys: " A ".to_owned()
            },
            "only the blank that separates the keys from the name is dropped"
        );
    }

    #[test]
    fn a_line_that_is_only_a_range_moves_the_cursor() {
        assert_eq!(kind("3"), ExKind::Goto);
    }

    #[test]
    fn a_name_the_core_has_no_command_for_keeps_what_was_typed_after_it() {
        assert_eq!(
            kind("nope"),
            ExKind::Unknown {
                name: "nope".to_owned(),
                args: String::new()
            }
        );
        assert_eq!(
            kind("upcase  a b "),
            ExKind::Unknown {
                name: "upcase".to_owned(),
                args: " a b ".to_owned()
            },
            "only the blank that separates the arguments from the name is dropped"
        );
        assert_eq!(
            kind("upcase!"),
            ExKind::Unknown {
                name: "upcase!".to_owned(),
                args: String::new()
            },
            "the ! is part of the name the host looks up, not a force the core takes off"
        );
    }

    #[test]
    fn a_hosts_command_may_be_named_with_more_than_letters() {
        for (typed, name) in [
            ("sort-lines", "sort-lines"),
            ("format_json", "format_json"),
            ("tool2", "tool2"),
        ] {
            assert_eq!(
                kind(typed),
                ExKind::Unknown {
                    name: name.to_owned(),
                    args: String::new()
                },
                "the whole token is the name, not the letters it opens with"
            );
        }
        assert_eq!(
            kind("sort-lines --numeric"),
            ExKind::Unknown {
                name: "sort-lines".to_owned(),
                args: "--numeric".to_owned()
            }
        );
    }

    #[test]
    fn a_name_that_opens_with_the_letters_of_a_core_command_is_still_the_hosts() {
        // The core's own name has to be the whole token: `write-json` is not `:w` writing to a
        // file called `-json`, and `x2` is not `:x` with `2` after it.
        for typed in ["write-json", "x2", "normal-mode", "d3", "gg-open", "s_case"] {
            assert_eq!(
                kind(typed),
                ExKind::Unknown {
                    name: typed.to_owned(),
                    args: String::new()
                },
                "{typed} names a command of the host's"
            );
        }
        assert_eq!(
            kind("write-json --pretty out.json"),
            ExKind::Unknown {
                name: "write-json".to_owned(),
                args: "--pretty out.json".to_owned()
            }
        );
    }

    #[test]
    fn a_name_that_opens_with_punctuation_is_the_hosts_rather_than_a_line_number() {
        assert_eq!(
            kind("!ls"),
            ExKind::Unknown {
                name: "!ls".to_owned(),
                args: String::new()
            },
            "the empty name is the one a line that is only a range has"
        );
        assert_eq!(
            kind("@macro one"),
            ExKind::Unknown {
                name: "@macro".to_owned(),
                args: "one".to_owned()
            }
        );
    }

    #[test]
    fn the_core_still_reads_its_own_commands_off_the_way_they_are_written() {
        // The ways a core command goes on: nothing after the name, a blank in front of an
        // argument, the `!` of a force, and the `/` a pattern is written between.
        assert_eq!(kind("q"), ExKind::Quit { force: false });
        assert_eq!(kind("q!"), ExKind::Quit { force: true });
        assert_eq!(
            kind("w out.txt"),
            ExKind::Write {
                path: Some("out.txt".to_owned())
            }
        );
        assert_eq!(
            kind("s/a/b/"),
            ExKind::Substitute {
                pattern: "a".to_owned(),
                replacement: "b".to_owned(),
                all: false,
                ignore_case: false
            }
        );
        assert_eq!(
            kind("g/x/d"),
            ExKind::Global {
                pattern: "x".to_owned(),
                invert: false,
                command: Box::new(ExKind::Delete)
            }
        );
        assert_eq!(
            kind("norm A;"),
            ExKind::Normal {
                keys: "A;".to_owned()
            }
        );
    }
}
