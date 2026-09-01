//! End-to-end tests of `wim edit`: a `wim.jsonc` declares autocmds, and the binary runs them
//! over a real file.
//!
//! This is the native half of the acceptance check the browser run makes as well
//! (`web/e2e/autocmd.spec.js`): the same config format, the same event names, and — for the
//! plugin handler — the same `.wasm`. The plugin one takes a component to run, so its path is
//! taken from `WIM_PLUGIN_WASM` and it steps aside when that is not set, the way the other
//! plugin tests do (`crates/wim/tests/plugin.rs`). The handlers that need no plugin run
//! everywhere, `cargo test --workspace` included.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

/// Where the built component is looked for.
const WASM: &str = "WIM_PLUGIN_WASM";

/// The sample plugin, or `None` on a machine that cannot build one.
fn hello_wim() -> Option<PathBuf> {
    let Some(path) = env::var_os(WASM).map(PathBuf::from) else {
        eprintln!("skipping: {WASM} is not set, so there is no component to run");
        return None;
    };
    assert!(
        path.is_file(),
        "{WASM} names {}, which is not a file",
        path.display()
    );
    Some(path)
}

/// A directory holding a file to edit and the config to edit it under.
struct Workspace {
    directory: TempDir,
}

impl Workspace {
    fn new(text: &str, config: &str) -> Self {
        let directory = TempDir::new().expect("a temporary directory should be available");
        fs::write(directory.path().join("notes.txt"), text).expect("the file should be written");
        fs::write(directory.path().join("wim.jsonc"), config)
            .expect("the config should be written");
        Self { directory }
    }

    fn file(&self) -> PathBuf {
        self.directory.path().join("notes.txt")
    }

    fn config(&self) -> PathBuf {
        self.directory.path().join("wim.jsonc")
    }

    /// The file as it stands, which is what a `:w` in the keys left behind.
    fn text(&self) -> String {
        fs::read_to_string(self.file()).expect("the file should be readable")
    }
}

/// Runs `wim edit` over `workspace` with `keys`, and whatever else `arguments` adds.
fn edit(workspace: &Workspace, keys: &str, arguments: &[&str]) -> Output {
    Command::cargo_bin("wim")
        .expect("the binary should be built")
        .arg("edit")
        .arg(workspace.file())
        .args(["--keys", keys])
        .arg("--config")
        .arg(workspace.config())
        .args(arguments)
        .output()
        .expect("the run should finish")
}

fn stdout(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).expect("what it reports comes back as text")
}

fn stderr(output: &Output) -> &str {
    std::str::from_utf8(&output.stderr).expect("what it reports comes back as text")
}

/// `--plugin hello-wim=<path>`, which is how a plugin handler's name is given a `.wasm`.
fn declare(wasm: &Path) -> String {
    format!("hello-wim={}", wasm.display())
}

#[test]
fn an_ex_handler_on_a_write_edits_the_buffer_before_it_is_written() {
    // The buffer is written by the `:w` in the keys, and the trailing blanks are gone from what
    // landed on the file: the handler ran in front of the write rather than after it.
    let workspace = Workspace::new(
        "alpha   \nbravo\t\n",
        r#"{
          // Trailing blanks go on the way out, the way an editor is usually told to. The
          // pattern is a plain Rust regex rather than Vim's dialect (`documents/PROJECT.md`),
          // so it is `\\s+$` and not `\\s\\+$`.
          "autocmds": [
            { "event": "buffer-write", "handler": { "kind": "ex", "command": "%s/\\s+$//" } }
          ]
        }"#,
    );
    let output = edit(&workspace, ":w<CR>", &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(workspace.text(), "alpha\nbravo\n");
    assert_eq!(stdout(&output), "buffer-write ex: %s/\\s+$//\n");
}

#[test]
fn a_keys_handler_runs_on_a_change_and_is_not_run_by_its_own() {
    let workspace = Workspace::new(
        "one\ntwo\n",
        r#"{
          "autocmds": [
            { "event": "text-changed", "handler": { "kind": "keys", "keys": "GA!<Esc>" } }
          ]
        }"#,
    );
    let output = edit(&workspace, "dd:w<CR>", &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(workspace.text(), "two!\n");
    assert_eq!(
        stdout(&output),
        "text-changed keys: GA!<Esc>\n",
        "the handler's own edit is another text-changed, and running it again would not stop"
    );
}

#[test]
fn a_write_inside_a_handler_puts_the_buffer_as_it_then_stood_on_the_file() {
    // The `:w` is halfway through the handler's keys, so what lands on the file is the buffer as
    // it stood when the write was asked for: the `Y` typed after it is in the buffer the run ends
    // with and not in the bytes on disk.
    let workspace = Workspace::new(
        "one\n",
        r#"{
          "autocmds": [
            { "event": "text-changed", "handler": { "kind": "keys", "keys": "iX<Esc>:w<CR>iY<Esc>" } }
          ]
        }"#,
    );
    let output = edit(&workspace, "x", &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(workspace.text(), "Xne\n");
    assert_eq!(stdout(&output), "text-changed keys: iX<Esc>:w<CR>iY<Esc>\n");
}

#[test]
fn a_write_asked_for_by_a_post_write_handler_does_not_run_that_handler_again() {
    // `buffer-write-post` is raised by the write the keys asked for, and the handler it runs asks
    // for a write of its own. That write happens — a `:w` inside a handler still writes — but the
    // event it would raise is the handler's own, which autocmds here never nest on: the run
    // settles with the handler having run once. The browser host settles the same way over its
    // queued writes (`web/e2e/autocmd.spec.js`).
    let workspace = Workspace::new(
        "one\n",
        r#"{
          "autocmds": [
            { "event": "buffer-write-post", "handler": { "kind": "keys", "keys": "A!<Esc>:w<CR>" } }
          ]
        }"#,
    );
    let output = edit(&workspace, ":w<CR>", &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "buffer-write-post keys: A!<Esc>:w<CR>\n",
        "the handler's own write raises no post event to run it again"
    );
    assert_eq!(workspace.text(), "one!\n", "the handler's write landed");
}

#[test]
fn a_config_with_no_autocmds_leaves_the_run_as_the_keys_wrote_it() {
    let workspace = Workspace::new("one\n", "{}");
    let output = edit(&workspace, "x:w<CR>", &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(workspace.text(), "ne\n");
    assert_eq!(stdout(&output), "");
}

#[test]
fn a_run_without_a_write_leaves_the_file_alone() {
    let workspace = Workspace::new(
        "one\n",
        r#"{"autocmds": [{ "event": "buffer-write",
                          "handler": { "kind": "keys", "keys": "x" } }]}"#,
    );
    let output = edit(&workspace, "x", &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(workspace.text(), "one\n", "nothing asked for a write");
    assert_eq!(stdout(&output), "");
}

#[test]
fn an_event_nothing_raises_is_refused_before_a_key_is_typed() {
    let workspace = Workspace::new(
        "one\n",
        r#"{"autocmds": [{ "event": "BufWritePre",
                          "handler": { "kind": "keys", "keys": "x" } }]}"#,
    );
    let output = edit(&workspace, "x:w<CR>", &[]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("no event is called BufWritePre"),
        "{}",
        stderr(&output)
    );
    assert_eq!(workspace.text(), "one\n");
}

#[test]
fn a_handler_that_fails_is_reported_and_the_ones_after_it_still_run() {
    // Nothing to trim, so the `:s` finds no match and says so. The demo reports the same thing on
    // its autocmd line and goes on to the next handler (`web/e2e/autocmd.spec.js`); here the run
    // ends with a status that says one of them failed.
    let workspace = Workspace::new(
        "alpha\n",
        r#"{
          "autocmds": [
            { "event": "buffer-write", "handler": { "kind": "ex", "command": "%s/\\s+$//" } },
            { "event": "buffer-write", "handler": { "kind": "keys", "keys": "A!<Esc>" } }
          ]
        }"#,
    );
    let output = edit(&workspace, ":w<CR>", &[]);
    assert!(!output.status.success());
    assert_eq!(
        stdout(&output),
        "buffer-write ex failed: pattern not found: \\s+$\nbuffer-write keys: A!<Esc>\n"
    );
    assert_eq!(
        workspace.text(),
        "alpha!\n",
        "the handler after it still ran"
    );
}

#[test]
fn a_key_the_core_refuses_still_reports_the_handlers_that_already_ran() {
    // The `z` is no command and ends the run where it stands — but the `:w` in front of it had
    // already run its handler and put the file where it is. A script reading standard output has
    // to be told that, or a side effect the run left behind is one it never reported.
    let workspace = Workspace::new(
        "one\n",
        r#"{"autocmds": [{ "event": "buffer-write",
                          "handler": { "kind": "keys", "keys": "A!<Esc>" } }]}"#,
    );
    let output = edit(&workspace, ":w<CR>z", &[]);
    assert!(!output.status.success());
    assert_eq!(stdout(&output), "buffer-write keys: A!<Esc>\n");
    assert!(
        !stderr(&output).trim().is_empty(),
        "what the core refused is still reported"
    );
    assert_eq!(workspace.text(), "one!\n", "the handler's write landed");
}

#[test]
fn a_plugin_handler_is_given_the_event_and_answers_over_the_abi() {
    let Some(wasm) = hello_wim() else {
        return;
    };
    let workspace = Workspace::new(
        "hello\n",
        r#"{
          "autocmds": [
            { "event": "buffer-write", "handler": { "kind": "plugin", "plugin": "hello-wim" } }
          ]
        }"#,
    );
    let output = edit(&workspace, ":w<CR>", &["--plugin", &declare(&wasm)]);
    assert!(output.status.success(), "{}", stderr(&output));
    // What hello-wim answers with is a message naming the event it was given and the buffer it
    // was given it over, which is the name the file is open under here.
    let reported = stdout(&output);
    assert!(
        reported.starts_with("buffer-write plugin hello-wim: hello-wim saw `buffer-write` on "),
        "{reported}"
    );
    assert!(reported.trim_end().ends_with("notes.txt"), "{reported}");
    assert_eq!(workspace.text(), "hello\n", "a message alters nothing");
}

#[test]
fn a_plugin_is_not_given_an_event_it_does_not_subscribe_to() {
    let Some(wasm) = hello_wim() else {
        return;
    };
    // hello-wim subscribes to `buffer-write` and to nothing else, and the ABI has the host
    // deliver nothing it did not subscribe to. Binding it to another event is therefore a
    // handler that could never run, which is refused rather than left to be found.
    let workspace = Workspace::new(
        "hello\n",
        r#"{"autocmds": [{ "event": "text-changed",
                          "handler": { "kind": "plugin", "plugin": "hello-wim" } }]}"#,
    );
    let output = edit(&workspace, "x:w<CR>", &["--plugin", &declare(&wasm)]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("does not subscribe to text-changed"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_plugin_handler_that_was_never_loaded_is_refused() {
    let workspace = Workspace::new(
        "hello\n",
        r#"{"autocmds": [{ "event": "buffer-write",
                          "handler": { "kind": "plugin", "plugin": "hello-wim" } }]}"#,
    );
    let output = edit(&workspace, ":w<CR>", &[]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("no plugin was loaded as hello-wim"),
        "{}",
        stderr(&output)
    );
}
