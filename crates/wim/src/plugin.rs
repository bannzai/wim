//! `wim plugin`: running a plugin from the command line.
//!
//! This is the smallest thing that exercises a plugin end to end — load a component, hand it a
//! buffer, apply what it hands back — and it exists to check that a plugin does what it says
//! outside an editor: the buffer is whatever comes in on standard input rather than a file wim is
//! editing, and the command is named on the command line rather than typed as a `:` line. Typing
//! one at an editor reaches the same call through `wim edit` (`crates/wim/src/edit.rs`).

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand};
use wim_plugin_host::{Edit, LineEdit, Plugin as Host, Position, Snapshot};

use crate::PROGRAM;

/// Runs a plugin without an editor around it.
#[derive(Debug, Args)]
pub struct Plugin {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run one of a plugin's commands over a buffer.
    Run(Run),
    /// Render a plugin's panel over a buffer.
    Render(Render),
}

#[derive(Debug, Args)]
#[command(long_about = "\
Runs one of a plugin's commands over a buffer and writes the result.

The buffer is --input, or standard input when --input is not given, and it stands in for the one
an editor would be holding: the plugin is given a copy of it and answers with an edit, which is
applied here and written to standard output. A plugin that answers with a message instead writes
that message to standard error and leaves the buffer alone.

The plugin runs with nothing of this machine reachable from inside it: no files, no network, no
clock. A component asking for any of that is refused rather than loaded.")]
struct Run {
    /// The .wasm component to load.
    #[arg(value_name = "WASM")]
    wasm: PathBuf,

    /// The command to run, written the way it would be without its leading colon.
    #[arg(value_name = "COMMAND")]
    command: String,

    /// Arguments for the command.
    #[arg(value_name = "ARG")]
    args: Vec<String>,

    /// Buffer to run over. Read from standard input when not given.
    #[arg(long, value_name = "TEXT")]
    input: Option<String>,
}

#[derive(Debug, Args)]
#[command(long_about = "\
Renders a plugin's panel over a buffer and writes the HTML.

The buffer is --input, or standard input when --input is not given, and it stands in for the one
an editor would be holding. The panel's HTML goes to standard output and its heading to standard
error, so that redirecting standard output leaves the HTML on its own.

A plugin decides which buffers it has a panel for, and the name of the buffer is what it decides
by, so --name is what a run gives it: markdown-preview answers with a panel for a name ending in
.md and with nothing for any other. Nothing is not a failure — it is the answer the ABI has a
host close the panel on — so a run that gets it writes nothing to standard output and says so on
standard error, and ends successfully.

The HTML is not trusted: the browser host draws it in an isolated frame where nothing in it can
run (wit/README.md), and this command writes it to standard output so that it can be redirected
and verified.

The plugin runs with nothing of this machine reachable from inside it: no files, no network, no
clock. A component asking for any of that is refused rather than loaded.")]
struct Render {
    /// The .wasm component to load.
    #[arg(value_name = "WASM")]
    wasm: PathBuf,

    /// Buffer to render. Read from standard input when not given.
    #[arg(long, value_name = "TEXT")]
    input: Option<String>,

    /// The name the buffer is under, which is what a plugin reads the language off. Empty, which
    /// is what a buffer backed by no file has, when it is not given.
    #[arg(long, value_name = "NAME", default_value = "")]
    name: String,
}

/// Runs the subcommand and reports what went wrong under the program's name, the way the rest of
/// the binary does.
pub fn main(plugin: Plugin) -> ExitCode {
    let done = match plugin.command {
        Command::Run(run) => execute(run),
        Command::Render(render) => render_panel(render),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{PROGRAM}: {error}");
            ExitCode::FAILURE
        }
    }
}

fn execute(run: Run) -> Result<(), String> {
    let text = match run.input {
        Some(text) => text,
        None => read_input()?,
    };
    let mut host = Host::from_file(&run.wasm)
        .map_err(|error| format!("cannot load {}: {error}", run.wasm.display()))?;
    // No file backs this buffer and no one has moved a cursor through it, which is what an empty
    // name and a cursor at the start of the buffer say to the plugin.
    let buffer = Snapshot {
        name: String::new(),
        text,
        cursor: Position { line: 0, column: 0 },
    };
    let edit = host
        .run(&run.command, &run.args, &buffer)
        .map_err(|error| format!(":{} failed: {error}", run.command))?;
    let (text, message) = apply(&buffer.text, edit)?;
    if let Some(message) = message {
        eprintln!("{message}");
    }
    io::stdout()
        .write_all(text.as_bytes())
        .map_err(|error| format!("cannot write the buffer: {error}"))
}

/// Renders the plugin's panel over the buffer and writes what it answered with.
fn render_panel(render: Render) -> Result<(), String> {
    let text = match render.input {
        Some(text) => text,
        None => read_input()?,
    };
    let mut host = Host::from_file(&render.wasm)
        .map_err(|error| format!("cannot load {}: {error}", render.wasm.display()))?;
    // No one has moved a cursor through this buffer, which is what a cursor at the start says to
    // the plugin. The name is the run's to give, because it is what a plugin decides by.
    let buffer = Snapshot {
        name: render.name,
        text,
        cursor: Position { line: 0, column: 0 },
    };
    let Some(panel) = host
        .render(&buffer)
        .map_err(|error| format!("the panel failed to render: {error}"))?
    else {
        // The answer a host closes the panel on. Nothing goes to standard output, so a run that
        // is redirected into a file leaves an empty one rather than a stale panel.
        eprintln!("no panel for {}", display_name(&buffer.name));
        return Ok(());
    };
    eprintln!("{}", panel.title);
    io::stdout()
        .write_all(panel.html.as_bytes())
        .map_err(|error| format!("cannot write the panel: {error}"))
}

/// A buffer that is backed by no file has an empty name, which reads as a gap in a report.
fn display_name(name: &str) -> String {
    if name.is_empty() {
        "a buffer with no name".to_string()
    } else {
        name.to_string()
    }
}

/// The buffer read from standard input, which is where it comes from when --input is not given.
fn read_input() -> Result<String, String> {
    let mut text = String::new();
    io::stdin()
        .read_to_string(&mut text)
        .map_err(|error| format!("cannot read the buffer: {error}"))?;
    Ok(text)
}

/// The buffer after `edit`, and the message the plugin asked to have shown, if any.
///
/// An editor would do this to its own buffer; here it is done to the text that was read, so that
/// what comes out is the buffer as the plugin left it whichever kind of edit it chose. `wim edit`
/// applies what a plugin's event handler answers with through here as well, so that one edit means
/// the same thing whichever call it came back from.
pub(crate) fn apply(text: &str, edit: Edit) -> Result<(String, Option<String>), String> {
    match edit {
        Edit::ReplaceAll(replacement) => Ok((replacement, None)),
        Edit::ReplaceLines(lines) => Ok((replace_lines(text, &lines)?, None)),
        Edit::Message(message) => Ok((text.to_string(), Some(message))),
        Edit::Noop => Ok((text.to_string(), None)),
    }
}

/// `text` with the lines `[start, end)` replaced.
///
/// Lines are split so that each keeps its own newline: a buffer whose last line has none stays
/// that way unless the replacement is the piece that lands at the end.
fn replace_lines(text: &str, edit: &LineEdit) -> Result<String, String> {
    let lines: Vec<&str> = if text.is_empty() {
        // An empty buffer is one empty line in the editor (`Buffer::line_count`), and a plugin
        // that answers with `replace-lines { start: 0, end: 1 }` means that line. Splitting
        // yields no lines at all, which would make the same edit out of range here only.
        vec![""]
    } else {
        text.split_inclusive('\n').collect()
    };
    let start = edit.start as usize;
    let end = edit.end as usize;
    if start > end || end > lines.len() {
        return Err(format!(
            "lines {start}..{end} are not in a buffer of {} line(s)",
            lines.len()
        ));
    }
    let mut replaced = String::new();
    replaced.extend(lines[..start].iter().copied());
    replaced.push_str(&edit.text);
    replaced.extend(lines[end..].iter().copied());
    Ok(replaced)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_edit(start: u32, end: u32, text: &str) -> LineEdit {
        LineEdit {
            start,
            end,
            text: text.to_string(),
        }
    }

    #[test]
    fn replacing_the_whole_buffer_is_what_comes_out() {
        assert_eq!(
            apply("hello\n", Edit::ReplaceAll("HELLO\n".to_string())),
            Ok(("HELLO\n".to_string(), None))
        );
    }

    #[test]
    fn a_message_leaves_the_buffer_alone() {
        assert_eq!(
            apply("hello\n", Edit::Message("saw it".to_string())),
            Ok(("hello\n".to_string(), Some("saw it".to_string())))
        );
        assert_eq!(
            apply("hello\n", Edit::Noop),
            Ok(("hello\n".to_string(), None))
        );
    }

    #[test]
    fn lines_are_replaced_where_the_edit_says() {
        let text = "one\ntwo\nthree\n";
        assert_eq!(
            replace_lines(text, &line_edit(1, 2, "TWO\n")),
            Ok("one\nTWO\nthree\n".to_string())
        );
        assert_eq!(replace_lines(text, &line_edit(0, 3, "")), Ok(String::new()));
    }

    #[test]
    fn an_edit_that_starts_and_ends_at_the_same_line_inserts() {
        assert_eq!(
            replace_lines("one\ntwo\n", &line_edit(1, 1, "half\n")),
            Ok("one\nhalf\ntwo\n".to_string())
        );
        assert_eq!(
            replace_lines("one\n", &line_edit(1, 1, "two\n")),
            Ok("one\ntwo\n".to_string())
        );
    }

    #[test]
    fn a_last_line_without_a_newline_keeps_not_having_one() {
        assert_eq!(
            replace_lines("one\ntwo", &line_edit(0, 1, "ONE\n")),
            Ok("ONE\ntwo".to_string())
        );
    }

    #[test]
    fn an_empty_buffer_still_has_the_line_the_editor_shows() {
        assert_eq!(
            replace_lines("", &line_edit(0, 1, "one\n")),
            Ok("one\n".to_string())
        );
        assert_eq!(
            replace_lines("", &line_edit(0, 0, "one\n")),
            Ok("one\n".to_string())
        );
        assert!(replace_lines("", &line_edit(0, 2, "one\n")).is_err());
    }

    #[test]
    fn lines_the_buffer_does_not_have_are_refused() {
        assert!(replace_lines("one\n", &line_edit(0, 2, "x")).is_err());
        assert!(replace_lines("one\n", &line_edit(1, 0, "x")).is_err());
    }
}
