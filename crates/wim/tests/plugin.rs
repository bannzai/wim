//! End-to-end tests of `wim plugin run` and `wim plugin render`.
//!
//! What the subcommands add over the host library is where the buffer comes from and where the
//! result goes, so these run the binary over the first-party plugins and read what it wrote.
//!
//! Building those plugins needs a wasm32 target, which not every machine has (`wit/README.md`),
//! so their paths are taken from `WIM_PLUGIN_WASM` and `WIM_MARKDOWN_PREVIEW_WASM` and the tests
//! that need one step aside when it is not set. `make test-plugin-host` builds the components
//! and sets both, and that is what CI runs.

use std::env;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

/// Where the built sample component is looked for.
const WASM: &str = "WIM_PLUGIN_WASM";

/// Where the built Markdown Preview component is looked for.
const MARKDOWN_PREVIEW_WASM: &str = "WIM_MARKDOWN_PREVIEW_WASM";

/// The component `variable` names, or `None` on a machine that cannot build one.
fn component(variable: &str) -> Option<PathBuf> {
    let Some(path) = env::var_os(variable).map(PathBuf::from) else {
        eprintln!("skipping: {variable} is not set, so there is no component to run");
        return None;
    };
    assert!(
        path.is_file(),
        "{variable} names {}, which is not a file",
        path.display()
    );
    Some(path)
}

/// The sample plugin, or `None` on a machine that cannot build one.
fn hello_wim() -> Option<PathBuf> {
    component(WASM)
}

/// The Markdown Preview plugin, or `None` on a machine that cannot build one.
fn markdown_preview() -> Option<PathBuf> {
    component(MARKDOWN_PREVIEW_WASM)
}

/// Runs `wim plugin run` with `arguments`, writing `stdin` to it.
fn plugin_run(arguments: &[&str], stdin: &str) -> Output {
    plugin(&["run"], arguments, stdin)
}

/// Runs `wim plugin render` with `arguments`, writing `stdin` to it.
fn plugin_render(arguments: &[&str], stdin: &str) -> Output {
    plugin(&["render"], arguments, stdin)
}

/// Runs `wim plugin <subcommand>` with `arguments`, writing `stdin` to it.
fn plugin(subcommand: &[&str], arguments: &[&str], stdin: &str) -> Output {
    let mut child = Command::cargo_bin("wim")
        .expect("the binary should be built")
        .arg("plugin")
        .args(subcommand)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary should start");
    child
        .stdin
        .take()
        .expect("standard input should be piped")
        .write_all(stdin.as_bytes())
        .expect("the buffer should be written");
    child.wait_with_output().expect("the run should finish")
}

fn stdout(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).expect("the buffer comes back as text")
}

fn stderr(output: &Output) -> &str {
    std::str::from_utf8(&output.stderr).expect("what is reported comes back as text")
}

#[test]
fn the_buffer_given_as_an_argument_comes_back_edited() {
    let Some(wasm) = hello_wim() else {
        return;
    };
    let output = plugin_run(
        &[&wasm.to_string_lossy(), "upcase", "--input", "hello\nwim\n"],
        "",
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "HELLO\nWIM\n");
}

#[test]
fn the_buffer_is_read_from_standard_input_when_it_is_not_given() {
    let Some(wasm) = hello_wim() else {
        return;
    };
    let output = plugin_run(&[&wasm.to_string_lossy(), "upcase"], "hello\nwim\n");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "HELLO\nWIM\n");
}

#[test]
fn what_the_plugin_refuses_is_reported_and_nothing_is_written() {
    let Some(wasm) = hello_wim() else {
        return;
    };
    let output = plugin_run(&[&wasm.to_string_lossy(), "nope", "--input", "hello\n"], "");
    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert!(
        stderr(&output).contains("hello-wim has no command named `nope`"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn arguments_after_the_command_reach_the_plugin() {
    let Some(wasm) = hello_wim() else {
        return;
    };
    // `:upcase` takes none, so passing one is how the arguments are shown to have arrived.
    let output = plugin_run(
        &[&wasm.to_string_lossy(), "upcase", "x", "--input", "hi"],
        "",
    );
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains(":upcase takes no arguments"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn the_panel_of_a_markdown_buffer_comes_back_as_html() {
    let Some(wasm) = markdown_preview() else {
        return;
    };
    let output = plugin_render(
        &[
            &wasm.to_string_lossy(),
            "--name",
            "notes.md",
            "--input",
            "# Title\n\n- one\n",
        ],
        "",
    );
    assert!(output.status.success(), "{}", stderr(&output));
    // The very HTML the plugin's own tests pin, arriving through wasmtime rather than through a
    // call in the same process: the two halves of the ABI carried the string across unchanged.
    assert_eq!(
        stdout(&output),
        "<h1>Title</h1>\n<ul>\n<li>one</li>\n</ul>\n"
    );
    // The heading goes to standard error, so that a redirect of standard output is the HTML.
    assert!(
        stderr(&output).contains("Markdown Preview"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn the_buffer_to_render_is_read_from_standard_input_when_it_is_not_given() {
    let Some(wasm) = markdown_preview() else {
        return;
    };
    let output = plugin_render(&[&wasm.to_string_lossy(), "--name", "notes.md"], "*hi*\n");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "<p><em>hi</em></p>\n");
}

#[test]
fn a_buffer_the_plugin_has_no_panel_for_writes_nothing_and_still_succeeds() {
    let Some(wasm) = markdown_preview() else {
        return;
    };
    // `none` is the answer the ABI has a host close the panel on, so it ends the run successfully
    // (`wit/plugin.wit`). A buffer backed by no file has no name and is one of those.
    for arguments in [
        vec![&*wasm.to_string_lossy(), "--name", "main.rs"],
        vec![&*wasm.to_string_lossy()],
    ] {
        let output = plugin_render(&arguments, "# Title\n");
        assert!(output.status.success(), "{}", stderr(&output));
        assert_eq!(stdout(&output), "");
        assert!(stderr(&output).contains("no panel"), "{}", stderr(&output));
    }
}

#[test]
fn a_plugin_that_always_has_a_panel_renders_over_any_buffer() {
    let Some(wasm) = hello_wim() else {
        return;
    };
    // hello-wim's panel is not Markdown and does not depend on the name, which is what makes it
    // the check that `render` reaches any plugin rather than the one it was added for.
    let output = plugin_render(&[&wasm.to_string_lossy(), "--input", "one\ntwo\n"], "");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "<h1>hello-wim</h1><p>[No Name] &middot; 2 line(s)</p>"
    );
}

#[test]
fn a_wasm_that_is_not_a_component_is_refused() {
    // A core module, which is what a plugin built for a wasm32 target other than wasip2 comes
    // out as. No plugin is needed to check this, so it runs everywhere.
    let directory = TempDir::new().expect("a temporary directory should be available");
    let module = directory.path().join("core.wasm");
    std::fs::write(&module, [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00])
        .expect("the file should be written");
    let output = plugin_run(&[&module.to_string_lossy(), "upcase", "--input", "hi"], "");
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("not a wasm component"),
        "{}",
        stderr(&output)
    );
}
