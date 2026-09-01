//! End-to-end tests of `wim plugin run`.
//!
//! What the subcommand adds over the host library is where the buffer comes from and where the
//! result goes, so these run the binary over the sample plugin and read what it wrote.
//!
//! Building that plugin needs the `wasm32-wasip2` target, which not every machine has
//! (`wit/README.md`), so its path is taken from `WIM_PLUGIN_WASM` and the tests that need it step
//! aside when it is not set. `make test-plugin-host` builds the component and sets it, and that
//! is what CI runs.

use std::env;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

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

/// Runs `wim plugin run` with `arguments`, writing `stdin` to it.
fn plugin_run(arguments: &[&str], stdin: &str) -> Output {
    let mut child = Command::cargo_bin("wim")
        .expect("the binary should be built")
        .args(["plugin", "run"])
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
