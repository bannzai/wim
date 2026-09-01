//! `vimacro`: applies a wim key sequence to files, the way `sed` applies a script.
//!
//! `wim-core` does no IO, so everything outside the buffer lives here: reading the input,
//! typing the keys at an [`Editor`], carrying out the [`Effect`]s the core hands back, and
//! writing the result out.

use std::fmt::Display;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use wim_core::{Editor, Effect, KeyCode, KeyEvent, parse_keys};

/// The name errors are reported under, and the name of the binary.
const PROGRAM: &str = "vimacro";

/// The name standard input is reported under.
const STDIN_NAME: &str = "<stdin>";

/// Applies wim key sequences and Ex commands to files.
#[derive(Debug, Parser)]
#[command(
    name = PROGRAM,
    version,
    about,
    long_about = "\
Applies wim key sequences and Ex commands to files, the way sed applies a script.

The keys are written in wim's key notation: <Esc>, <CR>, <BS>, <Tab>, <lt> for a literal
'<' and <C-x> for control combinations; every other character stands for itself. The
notation is documented in crates/wim-core/tests/golden/README.md.

KEYS is read from the first operand only when nothing else says what to run. --keys,
--global and --ex each name their own program, and every operand is then a file; that is
what --keys is for, since combining a key sequence with --ex leaves no operand to spare.

The cursor starts at line 1, column 1 of each input, and each input is read on its own: no
state carries from one file to the next.

Input and output:
  With no file operand, or with '-', the input is standard input and the result goes to
  standard output. Files are written back only with --in-place, so a run without it names
  at most one file, rather than running the results of several together.

Effects:
  :w is not carried out where it is written. With --in-place the result is written back to
  the file once, at the end of the run, and ':w path' names a further file to write it to;
  without --in-place no file is written at all. :q ends the run over that input, and the
  buffer as it stands is still written out.

Errors:
  A key the core rejects ends the run over that line and is reported on standard error,
  and the exit status is non-zero. --in-place then leaves that file as it was, while the
  buffer still goes to standard output, so that a pipe keeps carrying the text.

Line endings:
  A file that holds CRLF line endings is read as LF, which is what the core edits, and
  written back as CRLF."
)]
struct Cli {
    /// The key sequence to run, then the files to run it over.
    #[arg(value_name = "KEYS|FILE")]
    operands: Vec<String>,

    /// Key sequence to run, named explicitly so that it can be combined with --ex.
    #[arg(short = 'k', long, value_name = "KEYS", conflicts_with = "global")]
    keys: Option<String>,

    /// Run the key sequence at the start of every line, from the first to the last.
    #[arg(
        long,
        conflicts_with = "global",
        long_help = "\
Run the key sequence at the start of every line, from the first to the last.

The line to run on is found by its index rather than by following the cursor, so the keys
need no trailing 'j': the cursor is put at the start of line 1, then of line 2, and so on.
A run that took lines away leaves the following line at the index that ran, so the index
stays where it is; a run that added lines steps past what it added, the way :g does. That
is what ends the loop on a macro that would otherwise keep feeding itself lines."
    )]
    repeat_to_eof: bool,

    /// Run KEYS on every line PATTERN matches, as :g/PATTERN/norm KEYS does.
    #[arg(
        long,
        num_args = 2,
        value_names = ["PATTERN", "KEYS"],
        long_help = "\
Run KEYS on every line PATTERN matches, as :g/PATTERN/norm KEYS does.

PATTERN is a plain Rust regex, written without the surrounding slashes of the Ex command;
a '/' inside it is escaped when the :g line is built, so it needs no escaping of its own.
A pattern that matches no line is an error, as it is in Vim."
    )]
    global: Option<Vec<String>>,

    /// Run an Ex command line, before the key sequence when both are given.
    #[arg(long, value_name = "COMMAND", allow_hyphen_values = true)]
    ex: Option<String>,

    /// Write the result back to each file instead of to standard output.
    #[arg(short, long)]
    in_place: bool,
}

impl Cli {
    /// The key sequence and the files, split out of the operands.
    ///
    /// The first operand is the key sequence only when nothing else says what to run, so
    /// that `vimacro --ex '%s/foo/bar/g' notes.txt` reads its one operand as a file.
    fn keys_and_files(&self) -> (Option<&str>, &[String]) {
        if self.keys.is_some() || self.global.is_some() || self.ex.is_some() {
            return (self.keys.as_deref(), self.operands.as_slice());
        }
        match self.operands.split_first() {
            Some((keys, files)) => (Some(keys.as_str()), files),
            None => (None, &[]),
        }
    }
}

/// Why a key run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// Every key ran.
    Ran,
    /// A key was rejected, which ends the run the way it ends the keys of a `:norm`.
    Failed,
    /// `:q`.
    Quit,
}

/// What running the program over one input left behind.
#[derive(Debug, Default)]
struct Applied {
    /// The buffer as it stands after the run.
    text: String,
    /// What the core rejected, in the order it was reported. A message that repeats — the
    /// same key failing on line after line under --repeat-to-eof — is kept once.
    errors: Vec<String>,
    /// The paths `:w path` named, which --in-place writes the result to as well.
    extra_paths: Vec<String>,
}

impl Applied {
    fn report(&mut self, message: String) {
        if !self.errors.contains(&message) {
            self.errors.push(message);
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let (keys, files) = cli.keys_and_files();
    let keys = check(&cli, keys, files);
    let mut ok = true;
    if files.is_empty() {
        ok = process(&cli, keys.as_deref(), None);
    } else {
        for file in files {
            let path = (file != "-").then(|| PathBuf::from(file));
            ok &= process(&cli, keys.as_deref(), path.as_deref());
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Reads the key sequences and the file list before any file is touched, so that a typo in a
/// macro is reported instead of half applied.
fn check(cli: &Cli, keys: Option<&str>, files: &[String]) -> Option<Vec<KeyEvent>> {
    if keys.is_none() && cli.global.is_none() && cli.ex.is_none() {
        usage_error("there is nothing to run: give a key sequence, --global or --ex");
    }
    if cli.repeat_to_eof && keys.is_none() {
        usage_error("--repeat-to-eof repeats a key sequence, and none was given");
    }
    if let Some([_, global_keys]) = cli.global.as_deref()
        && let Err(error) = parse_keys(global_keys)
    {
        usage_error(format!(
            "--global was given keys that do not parse: {error}"
        ));
    }
    let reads_stdin = files.is_empty() || files.iter().any(|file| file == "-");
    if cli.in_place && reads_stdin {
        usage_error("--in-place writes the result back to a file, and standard input is not one");
    }
    if !cli.in_place && files.len() > 1 {
        usage_error(
            "without --in-place the results of several files would run together on standard \
             output: pass --in-place, or name one file at a time",
        );
    }
    let keys = keys?;
    match parse_keys(keys) {
        Ok(keys) => Some(keys),
        Err(error) => usage_error(format!("the key sequence does not parse: {error}")),
    }
}

/// Reports a way of calling `vimacro` that makes no sense, in the shape clap reports the ones
/// it catches itself, and exits.
fn usage_error(message: impl Display) -> ! {
    Cli::command()
        .error(ErrorKind::InvalidValue, message)
        .exit()
}

/// Runs the program over one input — a file, or standard input for `None` — and reports
/// whether it went through without the core rejecting anything.
fn process(cli: &Cli, keys: Option<&[KeyEvent]>, path: Option<&Path>) -> bool {
    let name = path.map_or_else(|| STDIN_NAME.to_owned(), |path| path.display().to_string());
    let read = match path {
        Some(path) => std::fs::read_to_string(path),
        None => io::read_to_string(io::stdin()),
    };
    let text = match read {
        Ok(text) => text,
        Err(error) => {
            eprintln!("{PROGRAM}: {name}: {error}");
            return false;
        }
    };
    // The core edits LF text, so a CRLF file is read as LF and written back as it came.
    let crlf = text.contains("\r\n");
    let applied = apply(cli, keys, &text.replace("\r\n", "\n"));
    for message in &applied.errors {
        eprintln!("{PROGRAM}: {name}: {message}");
    }
    let result = if crlf {
        applied.text.replace('\n', "\r\n")
    } else {
        applied.text
    };
    if !cli.in_place {
        if let Err(error) = io::stdout().write_all(result.as_bytes()) {
            eprintln!("{PROGRAM}: {STDIN_NAME}: {error}");
            return false;
        }
        return applied.errors.is_empty();
    }
    if !applied.errors.is_empty() {
        // A half-applied macro is worse than none, so a file whose run was rejected keeps the
        // text it had.
        return false;
    }
    let path = path.expect("--in-place is refused for standard input");
    let mut ok = true;
    for target in std::iter::once(path).chain(applied.extra_paths.iter().map(Path::new)) {
        if let Err(error) = std::fs::write(target, &result) {
            eprintln!("{PROGRAM}: {}: {error}", target.display());
            ok = false;
        }
    }
    ok
}

/// Runs the whole program over `text`: the Ex command first, then the `:g` or the key
/// sequence.
fn apply(cli: &Cli, keys: Option<&[KeyEvent]>, text: &str) -> Applied {
    let mut editor = Editor::new(text);
    let mut applied = Applied::default();
    let mut stop = Stop::Ran;
    if let Some(command) = &cli.ex {
        // The leading `:` is how an Ex command is written down, and the command line the keys
        // type opens with one of its own.
        stop = run_ex(
            &mut editor,
            command.strip_prefix(':').unwrap_or(command),
            &mut applied,
        );
    }
    if stop == Stop::Ran {
        if let Some([pattern, global_keys]) = cli.global.as_deref() {
            let line = format!("g/{}/norm {global_keys}", escape_pattern(pattern));
            run_ex(&mut editor, &line, &mut applied);
        } else if let Some(keys) = keys {
            if cli.repeat_to_eof {
                repeat_to_eof(&mut editor, keys, &mut applied);
            } else {
                feed(&mut editor, keys, &mut applied);
            }
        }
    }
    applied.text = editor.text();
    applied
}

/// Runs `keys` at the start of every line, first to last.
fn repeat_to_eof(editor: &mut Editor, keys: &[KeyEvent], applied: &mut Applied) {
    let mut line = 0;
    while line < editor.buffer().line_count() {
        let before = editor.buffer().line_count();
        if go_to_line_start(editor, line, applied) != Stop::Ran {
            return;
        }
        let stop = feed(editor, keys, applied);
        leave_pending(editor);
        if stop == Stop::Quit {
            return;
        }
        let after = editor.buffer().line_count();
        // Lines that went away moved the next line onto the index that just ran; lines that
        // were added are stepped over, so that a macro which adds one cannot feed itself.
        line = if after < before {
            line
        } else {
            line + 1 + (after - before)
        };
    }
}

/// Puts the cursor at the start of `line`, counted from 0, with the `:{line}` of Ex and the
/// `0` of Normal mode, which is where `:norm` starts its keys.
fn go_to_line_start(editor: &mut Editor, line: usize, applied: &mut Applied) -> Stop {
    let mut keys = ex_keys(&(line + 1).to_string());
    keys.push(KeyEvent::char('0'));
    feed(editor, &keys, applied)
}

/// Closes whatever the keys left open, the way `:norm` does at the end of a line: one `<Esc>`
/// leaves Insert or Visual mode, and a second drops a half-typed command.
fn leave_pending(editor: &mut Editor) {
    for _ in 0..2 {
        editor.handle_key(KeyEvent::key(KeyCode::Esc));
    }
}

/// Types `line` into the command line and runs it.
fn run_ex(editor: &mut Editor, line: &str, applied: &mut Applied) -> Stop {
    feed(editor, &ex_keys(line), applied)
}

/// The keys that type `line` into the command line, its `:` and the `<CR>` that runs it
/// included.
///
/// Each character of `line` is a key of its own, so the `<Esc>` of a `:norm` arrives as the
/// five characters the command line keeps and `:norm` reads back, rather than as the key that
/// would drop the command line.
fn ex_keys(line: &str) -> Vec<KeyEvent> {
    let mut keys = vec![KeyEvent::char(':')];
    keys.extend(line.chars().map(KeyEvent::char));
    keys.push(KeyEvent::key(KeyCode::Enter));
    keys
}

/// Hides the `/` of a pattern from the `:g` line it goes into.
///
/// Only `/` is escaped: it is the delimiter of the Ex line and nothing in a Rust regex, so a
/// pattern is written as the regex it is.
fn escape_pattern(pattern: &str) -> String {
    pattern.replace('/', "\\/")
}

/// Types `keys` at the editor, one at a time so that a `:q` stops the run where it is written.
fn feed(editor: &mut Editor, keys: &[KeyEvent], applied: &mut Applied) -> Stop {
    for key in keys {
        for effect in editor.handle_key(*key) {
            match effect {
                Effect::Error(message) => {
                    applied.report(message);
                    return Stop::Failed;
                }
                Effect::SaveRequested { path } => {
                    if let Some(path) = path {
                        applied.extra_paths.push(path);
                    }
                }
                Effect::QuitRequested { .. } => return Stop::Quit,
                // An event is something to hang an autocmd on, and autocmds are declared in a
                // config a host reads. This one takes no config: what it runs is the keys it
                // was given and nothing else, so that a macro applied to a file does the same
                // thing wherever it is run. `wim edit` is the host that wires them up
                // (`crates/wim/src/edit.rs`).
                Effect::Event(_) => {}
            }
        }
    }
    Stop::Ran
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(arguments: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once(PROGRAM).chain(arguments.iter().copied()))
            .expect("arguments should parse")
    }

    #[test]
    fn the_first_operand_is_the_key_sequence_when_nothing_else_names_one() {
        let cli = cli(&["ciwfoo<Esc>", "a.txt"]);
        let (keys, files) = cli.keys_and_files();
        assert_eq!(keys, Some("ciwfoo<Esc>"));
        assert_eq!(files, ["a.txt"]);
    }

    #[test]
    fn an_option_that_names_a_program_leaves_every_operand_a_file() {
        for arguments in [
            vec!["--ex", "%s/foo/bar/g", "a.txt"],
            vec!["--global", "^import", "A;<Esc>", "a.txt"],
            vec!["--keys", "x", "a.txt"],
        ] {
            let cli = cli(&arguments);
            assert_eq!(cli.keys_and_files().1, ["a.txt"], "{arguments:?}");
        }
    }

    #[test]
    fn a_key_sequence_can_be_named_alongside_an_ex_command() {
        let cli = cli(&["--ex", "%s/foo/bar/g", "--keys", "A;<Esc>", "a.txt"]);
        let (keys, files) = cli.keys_and_files();
        assert_eq!(keys, Some("A;<Esc>"));
        assert_eq!(files, ["a.txt"]);
    }

    #[test]
    fn a_key_sequence_and_a_global_are_two_programs_and_are_refused_together() {
        assert!(Cli::try_parse_from([PROGRAM, "--keys", "x", "--global", "^a", "x"]).is_err());
        assert!(Cli::try_parse_from([PROGRAM, "--repeat-to-eof", "--global", "^a", "x"]).is_err());
    }

    #[test]
    fn only_the_delimiter_of_the_ex_line_is_escaped_in_a_pattern() {
        assert_eq!(escape_pattern("^import"), "^import");
        assert_eq!(escape_pattern("^/usr/"), "^\\/usr\\/");
        assert_eq!(escape_pattern("\\d+"), "\\d+");
    }

    #[test]
    fn an_ex_line_is_typed_as_the_characters_it_is_written_with() {
        let keys = ex_keys("g/^a/norm A;<Esc>");
        assert_eq!(keys.first(), Some(&KeyEvent::char(':')));
        assert_eq!(keys.last(), Some(&KeyEvent::key(KeyCode::Enter)));
        assert!(
            keys.contains(&KeyEvent::char('<')),
            "the angle bracket of <Esc> is a character the command line keeps"
        );
        assert!(!keys.contains(&KeyEvent::key(KeyCode::Esc)));
    }

    #[test]
    fn a_repeated_message_is_reported_once() {
        let mut applied = Applied::default();
        applied.report("j does nothing".to_owned());
        applied.report("j does nothing".to_owned());
        applied.report("k does nothing".to_owned());
        assert_eq!(applied.errors, ["j does nothing", "k does nothing"]);
    }
}
