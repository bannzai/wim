//! `wim edit`: typing keys at a file with the autocmds of a config wired up.
//!
//! This is the native half of what `documents/CONFIG.md` describes, and the smallest host that
//! shows an autocmd doing something: the core reports an event, the config says what that event
//! runs, and the handler — an Ex line, a key sequence or a plugin — runs here. The browser demo
//! is the other half, over the same config format and the same event names (`web/main.js`).
//!
//! What it is not is an editor to sit in. There is no screen and no second key: the keys are
//! given on the command line and the run ends when they do, the way `vimacro` runs. The one
//! thing it does that `vimacro` does not is carry a config, because a config brings plugins with
//! it and a plugin needs the wasm host this binary already has.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args;
use wim_core::{Editor, Effect, Event, KeyCode, KeyEvent, parse_keys};
use wim_plugin_host::{Plugin as Host, Position, Snapshot};

use crate::PROGRAM;
use crate::config::{self, Autocmd, Config, Handler};
use crate::plugin;

/// Types keys at a file, running the autocmds a config declares.
#[derive(Debug, Args)]
#[command(long_about = "\
Types keys at a file, running the autocmds a config declares.

The keys are written in wim's key notation: <Esc>, <CR>, <BS>, <Tab>, <lt> for a literal '<' and
<C-x> for control combinations; every other character stands for itself.

The file is read into the buffer and written back where ':w' says, not at the end of the run: an
autocmd on 'buffer-write' is what runs in between, and what it edits is what gets written. Every
autocmd that runs is reported on standard output, one line each.

Handlers are declared in a wim.jsonc given with --config, in the format documents/CONFIG.md
describes. A handler of kind 'plugin' names a plugin, and --plugin says which .wasm that name
was loaded from; a plugin is only given the events it subscribes to, so one that subscribes to
none of what it is bound to is refused before a key is typed.

The plugins run with nothing of this machine reachable from inside them: no files, no network, no
clock.")]
pub struct Edit {
    /// The file to edit.
    #[arg(value_name = "FILE")]
    file: PathBuf,

    /// The keys to type.
    #[arg(long, value_name = "KEYS", allow_hyphen_values = true)]
    keys: String,

    /// The wim.jsonc to read autocmds from. Without one nothing is bound and no handler runs.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// A plugin an autocmd may name, as the name it is declared under and the .wasm it is.
    #[arg(long, value_name = "NAME=WASM")]
    plugin: Vec<String>,
}

/// Why a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// Every key ran.
    Ran,
    /// A key or a handler was rejected, which ends the run where it stands.
    Failed,
    /// `:q`.
    Quit,
}

pub fn main(edit: Edit) -> ExitCode {
    match run(edit) {
        Ok(run) => {
            for report in run.reports {
                println!("{report}");
            }
            if let Some(complaint) = run.complaint {
                eprintln!("{PROGRAM}: {complaint}");
                return ExitCode::FAILURE;
            }
            if run.failed {
                // What failed is in the report lines; the status is what a script reads.
                eprintln!("{PROGRAM}: an autocmd failed");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{PROGRAM}: {error}");
            ExitCode::FAILURE
        }
    }
}

/// What a run left behind: what the autocmds did, and how it ended.
struct Ran {
    reports: Vec<String>,
    /// Whether a handler failed, which the run ends with a non-zero status for.
    failed: bool,
    /// What the core refused a key with, `None` when every key ran. A refused key ends the run
    /// where it stands, and the reports of the keys in front of it are still the run's to print.
    complaint: Option<String>,
}

/// Runs the keys over the file and hands back what the autocmds did, one line each.
fn run(edit: Edit) -> Result<Ran, String> {
    let keys = parse_keys(&edit.keys)
        .map_err(|error| format!("the key sequence does not parse: {error}"))?;
    let config = match &edit.config {
        Some(path) => config::read(path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?,
        None => Config::default(),
    };
    let plugins = load_plugins(&edit.plugin)?;
    let text = fs::read_to_string(&edit.file)
        .map_err(|error| format!("{}: {error}", edit.file.display()))?;
    let mut session = Session::new(&edit.file, &text, config, plugins);
    session.check_subscriptions()?;
    // A key the core refuses ends the run, but it does not undo the keys in front of it: a `:w`
    // that already ran a handler and put the file where it is has to be reported, or a script
    // reading standard output is told nothing about an autocmd that already had its way with the
    // file. So this comes back as a run that ended badly rather than as an error with nothing in
    // it — an error is for a run that never got as far as a key.
    let complaint = match session.feed(&keys)? {
        Stop::Failed => Some(session.complaint.take().unwrap_or_default()),
        Stop::Ran | Stop::Quit => None,
    };
    Ok(Ran {
        reports: session.reports,
        failed: session.failed,
        complaint,
    })
}

/// The plugins named on the command line, loaded and keyed by the name a handler names them by.
fn load_plugins(declared: &[String]) -> Result<BTreeMap<String, Host>, String> {
    let mut plugins = BTreeMap::new();
    for declaration in declared {
        let (name, path) = declaration.split_once('=').ok_or_else(|| {
            format!("--plugin is written as NAME=WASM, and {declaration} names no wasm")
        })?;
        let host = Host::from_file(path).map_err(|error| format!("cannot load {path}: {error}"))?;
        plugins.insert(name.to_owned(), host);
    }
    Ok(plugins)
}

/// One run: the buffer, the file it came from, and everything the autocmds may reach.
struct Session {
    editor: Editor,
    path: PathBuf,
    /// Whether the file held CRLF line endings, which the core does not edit and the file gets
    /// back when it is written.
    crlf: bool,
    config: Config,
    plugins: BTreeMap<String, Host>,
    /// What the autocmds that ran did, in the order they ran.
    reports: Vec<String>,
    /// What ended the run, for the caller to report.
    complaint: Option<String>,
    /// Whether a handler failed, which the run ends with a non-zero status for even though the
    /// keys themselves went through.
    failed: bool,
    /// Whether a handler is running, which is what keeps a handler that edits the buffer from
    /// being run again by the event its own edit reports. Vim's autocmds nest only when they
    /// are asked to; here they never do.
    in_handler: bool,
}

impl Session {
    fn new(path: &Path, text: &str, config: Config, plugins: BTreeMap<String, Host>) -> Self {
        Self {
            editor: Editor::new(&text.replace("\r\n", "\n")),
            path: path.to_owned(),
            crlf: text.contains("\r\n"),
            config,
            plugins,
            reports: Vec::new(),
            complaint: None,
            failed: false,
            in_handler: false,
        }
    }

    /// Refuses a plugin handler that would never run: one naming a plugin that was not loaded,
    /// or one bound to an event that plugin does not subscribe to, which the ABI forbids the
    /// host to deliver (`wit/plugin.wit`).
    fn check_subscriptions(&mut self) -> Result<(), String> {
        for autocmd in self.config.autocmds.clone() {
            let Handler::Plugin { plugin } = &autocmd.handler else {
                continue;
            };
            let host = self
                .plugins
                .get_mut(plugin)
                .ok_or_else(|| format!("no plugin was loaded as {plugin}: pass --plugin"))?;
            let subscriptions = host.subscriptions().map_err(|error| {
                format!("{plugin} cannot be asked what it subscribes to: {error}")
            })?;
            if !subscriptions.contains(&autocmd.event) {
                return Err(format!(
                    "{plugin} does not subscribe to {}, so it would never be given it",
                    autocmd.event
                ));
            }
        }
        Ok(())
    }

    /// Types `keys` at the editor, one at a time so that a `:q` stops the run where it is
    /// written.
    fn feed(&mut self, keys: &[KeyEvent]) -> Result<Stop, String> {
        for key in keys {
            let effects = self.editor.handle_key(*key);
            match self.carry_out(effects)? {
                Stop::Ran => {}
                stop => return Ok(stop),
            }
        }
        Ok(Stop::Ran)
    }

    /// Carries out what the core handed back, in the order it handed it back.
    fn carry_out(&mut self, effects: Vec<Effect>) -> Result<Stop, String> {
        for effect in effects {
            match effect {
                Effect::Error(message) => {
                    self.complaint = Some(message);
                    return Ok(Stop::Failed);
                }
                Effect::Event(event) => self.dispatch(&event),
                Effect::SaveRequested { path } => {
                    self.write(path.as_deref())?;
                    // The core cannot raise this one: whether a write happened is the host's to
                    // know, and it has only just happened here.
                    self.dispatch(&Event::BufferWritePost);
                }
                Effect::QuitRequested { .. } => return Ok(Stop::Quit),
            }
        }
        Ok(Stop::Ran)
    }

    /// Runs every handler bound to `event`.
    fn dispatch(&mut self, event: &Event) {
        if self.in_handler {
            return;
        }
        let bound: Vec<Autocmd> = self
            .config
            .autocmds
            .iter()
            .filter(|autocmd| autocmd.event == event.name())
            .cloned()
            .collect();
        if bound.is_empty() {
            return;
        }
        self.in_handler = true;
        for autocmd in &bound {
            let done = self.run_handler(event, &autocmd.handler);
            self.reports.push(format!("{} {done}", event.name()));
        }
        self.in_handler = false;
    }

    /// Runs one handler and says what it did, which is the line it is reported on.
    ///
    /// A handler that fails is reported rather than ending the run: the keys the editor was
    /// given are what the run is, and every other handler bound to the event still has to run.
    /// The failure is in the report line and in the exit status the run ends with.
    fn run_handler(&mut self, event: &Event, handler: &Handler) -> String {
        let done = match handler {
            // The leading `:` is how an Ex command is written down, and the command line the
            // keys type opens with one of its own.
            Handler::Ex { command } => self
                .type_keys(&ex_keys(command))
                .map(|()| format!("ex: {command}")),
            Handler::Keys { keys } => parse_keys(keys)
                .map_err(|error| format!("keys do not parse: {error}"))
                .and_then(|typed| self.type_keys(&typed))
                .map(|()| format!("keys: {keys}")),
            Handler::Plugin { plugin } => self
                .call(event, plugin)
                .map(|message| format!("plugin {plugin}: {message}")),
        };
        match done {
            Ok(done) => done,
            Err(complaint) => {
                self.failed = true;
                format!("{} failed: {complaint}", handler_kind(handler))
            }
        }
    }

    /// Types the keys of a handler at the editor and carries out what they asked for.
    ///
    /// Each key is carried out before the next one is typed, the way the run's own keys are: a
    /// `:w` halfway through a handler puts the buffer as it stands at that point on the file
    /// rather than what the keys after it went on to write, and a key the core rejects ends the
    /// handler where it stands rather than letting the rest of the sequence edit the buffer of a
    /// handler that is going to be reported as failed.
    fn type_keys(&mut self, keys: &[KeyEvent]) -> Result<(), String> {
        // A handler that left a command half-typed would leave the keys that follow it being
        // read as part of that command, the way `:norm` closes what its keys left open.
        let closing = [KeyEvent::key(KeyCode::Esc), KeyEvent::key(KeyCode::Esc)];
        for key in keys.iter().chain(&closing) {
            let effects = self.editor.handle_key(*key);
            // A `:q` inside a handler is not the run's to end — the run is the keys the editor
            // was given — so what comes back is only ever read for what failed.
            if self.carry_out(effects)? == Stop::Failed {
                return Err(self.complaint.take().unwrap_or_default());
            }
        }
        Ok(())
    }

    /// Gives `event` to a plugin and applies the edit it answers with.
    ///
    /// The buffer crosses by value and the answer comes back by value, which is the whole of
    /// what the ABI lets a plugin touch: the edit is applied here, by the host.
    fn call(&mut self, event: &Event, plugin: &str) -> Result<String, String> {
        let buffer = Snapshot {
            name: self.path.display().to_string(),
            text: self.editor.text(),
            cursor: Position {
                line: self.editor.cursor().line as u32,
                column: self.editor.cursor().col as u32,
            },
        };
        let host = self
            .plugins
            .get_mut(plugin)
            .ok_or_else(|| format!("no plugin was loaded as {plugin}"))?;
        let edit = host
            .on_event(
                &wim_plugin_host::Event {
                    name: event.name().to_owned(),
                    payload: event.payload(),
                },
                &buffer,
            )
            .map_err(|error| format!("{plugin} failed on {}: {error}", event.name()))?;
        let (text, message) = plugin::apply(&buffer.text, edit)?;
        if text != buffer.text {
            // The core has no way of being handed a buffer, so the edited text becomes an editor
            // of its own — which is what the browser host does with a plugin's edit as well. The
            // undo history and the cursor of the run so far go with the old one.
            self.editor = Editor::new(&text);
            return Ok("rewrote the buffer".to_owned());
        }
        Ok(message.unwrap_or_else(|| "left the buffer alone".to_owned()))
    }

    /// Writes the buffer to `path`, or to the file it was read from when `:w` named none.
    fn write(&mut self, path: Option<&str>) -> Result<(), String> {
        let text = self.editor.text();
        let text = if self.crlf {
            text.replace('\n', "\r\n")
        } else {
            text
        };
        let destination = path.map_or(self.path.clone(), PathBuf::from);
        fs::write(&destination, text)
            .map_err(|error| format!("cannot write {}: {error}", destination.display()))
    }
}

/// The kind a handler was declared as, which is how a failure names the handler that failed.
fn handler_kind(handler: &Handler) -> &'static str {
    match handler {
        Handler::Ex { .. } => "ex",
        Handler::Keys { .. } => "keys",
        Handler::Plugin { .. } => "plugin",
    }
}

/// The keys that type `line` into the command line, its `:` and the `<CR>` that runs it included.
///
/// Each character of `line` is a key of its own, so the `<Esc>` of a `:norm` arrives as the five
/// characters the command line keeps and `:norm` reads back, rather than as the key that would
/// drop the command line.
fn ex_keys(line: &str) -> Vec<KeyEvent> {
    let mut keys = vec![KeyEvent::char(':')];
    keys.extend(line.chars().map(KeyEvent::char));
    keys.push(KeyEvent::key(KeyCode::Enter));
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(text: &str, config: Config) -> Session {
        Session::new(Path::new("notes.txt"), text, config, BTreeMap::new())
    }

    fn config(autocmds: &str) -> Config {
        config::parse(&format!(r#"{{"autocmds": [{autocmds}]}}"#)).expect("a config")
    }

    #[test]
    fn a_handler_runs_when_the_event_it_is_bound_to_is_reported() {
        let mut session = session(
            "foo\n",
            config(
                r#"{ "event": "text-changed", "handler": { "kind": "ex", "command": "s/o/0/g" } }"#,
            ),
        );
        let keys = parse_keys("A!<Esc>").expect("keys should parse");
        assert_eq!(
            session.feed(&keys).expect("the run should go through"),
            Stop::Ran
        );
        assert_eq!(session.editor.text(), "f00!\n");
        assert_eq!(session.reports, ["text-changed ex: s/o/0/g"]);
    }

    #[test]
    fn what_a_handler_changes_does_not_run_it_again() {
        // The handler edits the text, which is another `text-changed` — and running the handler
        // on that one would never stop.
        let mut session = session(
            "a\n",
            // Plain key notation: a config is read as keys rather than typed into a command
            // line, so `<Esc>` is the key rather than the five characters `:norm` would need.
            config(
                r#"{ "event": "text-changed", "handler": { "kind": "keys", "keys": "A!<Esc>" } }"#,
            ),
        );
        let keys = parse_keys("x").expect("keys should parse");
        assert_eq!(
            session.feed(&keys).expect("the run should go through"),
            Stop::Ran
        );
        assert_eq!(session.editor.text(), "!\n");
        assert_eq!(session.reports.len(), 1, "{:?}", session.reports);
    }

    #[test]
    fn an_event_nothing_is_bound_to_runs_nothing() {
        let mut session = session("a\n", Config::default());
        let keys = parse_keys("x").expect("keys should parse");
        assert_eq!(
            session.feed(&keys).expect("the run should go through"),
            Stop::Ran
        );
        assert!(session.reports.is_empty(), "{:?}", session.reports);
    }

    #[test]
    fn a_quit_ends_the_run_where_it_is_written() {
        let mut session = session("ab\n", Config::default());
        let keys = parse_keys(":q<CR>x").expect("keys should parse");
        assert_eq!(
            session.feed(&keys).expect("the run should go through"),
            Stop::Quit
        );
        assert_eq!(session.editor.text(), "ab\n", "the x after it never ran");
    }

    #[test]
    fn a_key_the_core_rejects_ends_the_run_and_is_reported() {
        let mut session = session("ab\n", Config::default());
        let keys = parse_keys("z").expect("keys should parse");
        assert_eq!(
            session.feed(&keys).expect("the run should go through"),
            Stop::Failed
        );
        assert!(session.complaint.is_some(), "the complaint is the core's");
    }

    #[test]
    fn a_handler_stops_at_the_first_key_the_core_rejects() {
        // `z` is no command of its own, and the keys behind it never reach the buffer: a handler
        // that is going to be reported as failed leaves nothing half-done for the handlers after
        // it to run over.
        let mut session = session(
            "ab\n",
            config(
                r#"{ "event": "text-changed", "handler": { "kind": "keys", "keys": "zA!<Esc>" } }"#,
            ),
        );
        let keys = parse_keys("x").expect("keys should parse");
        assert_eq!(
            session.feed(&keys).expect("the run should go through"),
            Stop::Ran
        );
        assert_eq!(session.editor.text(), "b\n");
        assert!(session.failed);
        assert_eq!(session.reports.len(), 1, "{:?}", session.reports);
        assert!(session.reports[0].starts_with("text-changed keys failed:"));
    }

    #[test]
    fn a_plugin_handler_that_names_nothing_loaded_is_refused_before_a_key_is_typed() {
        let mut session = session(
            "a\n",
            config(
                r#"{ "event": "text-changed", "handler": { "kind": "plugin", "plugin": "nope" } }"#,
            ),
        );
        let error = session
            .check_subscriptions()
            .expect_err("no plugin was loaded as nope");
        assert!(error.contains("nope"), "{error}");
    }

    #[test]
    fn a_plugin_declaration_that_names_no_wasm_is_refused() {
        let Err(error) = load_plugins(&["hello-wim".to_owned()]) else {
            panic!("a declaration that names no wasm loads nothing");
        };
        assert!(error.contains("NAME=WASM"), "{error}");
    }
}
