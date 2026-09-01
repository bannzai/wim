//! The host against the sample plugin, end to end.
//!
//! `plugins/hello-wim` is the worked example of every interface of the ABI, so running it is what
//! shows the two halves fit: the bindings here are generated from `wit/plugin.wit` for the host,
//! the ones in the plugin are generated from the same file for the guest, and nothing but the
//! component travels between them.
//!
//! Building that component needs the `wasm32-wasip2` target, which not every machine has
//! (`wit/README.md`), so the component is not built here: its path is taken from
//! `WIM_PLUGIN_WASM` and these tests step aside when it is not set. `make test-plugin-host`
//! builds the component and sets it, and that is what CI runs.

use std::env;
use std::path::PathBuf;

use wim_plugin_host::{Edit, Error, Event, Plugin, Position, Snapshot};

/// Where the built component is looked for.
const WASM: &str = "WIM_PLUGIN_WASM";

/// The buffer every call below is made over.
fn buffer() -> Snapshot {
    Snapshot {
        name: "notes.txt".to_string(),
        text: "hello\nwim\n".to_string(),
        cursor: Position { line: 0, column: 0 },
    }
}

/// The sample plugin, or `None` on a machine that cannot build one.
fn hello_wim() -> Option<Plugin> {
    let Some(path) = env::var_os(WASM).map(PathBuf::from) else {
        eprintln!("skipping: {WASM} is not set, so there is no component to run");
        return None;
    };
    Some(
        Plugin::from_file(&path)
            .unwrap_or_else(|error| panic!("{} should load as a plugin: {error}", path.display())),
    )
}

#[test]
fn the_sample_plugin_publishes_its_command() {
    let Some(mut plugin) = hello_wim() else {
        return;
    };
    let commands = plugin.list_commands().expect("the commands should be read");
    let names: Vec<&str> = commands
        .iter()
        .map(|command| command.name.as_str())
        .collect();
    assert_eq!(names, ["upcase"]);
    assert!(
        !commands[0].description.is_empty(),
        "a published command describes itself"
    );
}

#[test]
fn running_the_command_rewrites_the_buffer() {
    let Some(mut plugin) = hello_wim() else {
        return;
    };
    let edit = plugin
        .run("upcase", &[], &buffer())
        .expect("the command should run");
    assert_eq!(edit, Edit::ReplaceAll("HELLO\nWIM\n".to_string()));
}

#[test]
fn what_the_plugin_refuses_comes_back_in_its_own_words() {
    let Some(mut plugin) = hello_wim() else {
        return;
    };
    let Err(Error::Plugin(message)) = plugin.run("nope", &[], &buffer()) else {
        panic!("the plugin has no command named `nope`");
    };
    assert_eq!(message, "hello-wim has no command named `nope`");

    let Err(Error::Plugin(message)) = plugin.run("upcase", &["x".to_string()], &buffer()) else {
        panic!(":upcase takes no arguments");
    };
    assert_eq!(message, ":upcase takes no arguments");
}

#[test]
fn the_plugin_is_delivered_the_event_it_subscribed_to() {
    let Some(mut plugin) = hello_wim() else {
        return;
    };
    let subscriptions = plugin
        .subscriptions()
        .expect("the subscriptions should be read");
    assert_eq!(subscriptions, ["buffer-write"]);

    let event = Event {
        name: subscriptions[0].clone(),
        payload: String::new(),
    };
    let edit = plugin
        .on_event(&event, &buffer())
        .expect("the event should be delivered");
    assert_eq!(
        edit,
        Edit::Message("hello-wim saw `buffer-write` on notes.txt".to_string())
    );
}

#[test]
fn the_panel_is_rendered_over_the_buffer_it_is_given() {
    let Some(mut plugin) = hello_wim() else {
        return;
    };
    let panel = plugin
        .render(&buffer())
        .expect("the panel should render")
        .expect("the sample plugin has a panel");
    assert_eq!(panel.title, "hello-wim");
    assert_eq!(
        panel.html,
        "<h1>hello-wim</h1><p>notes.txt &middot; 2 line(s)</p>"
    );
}

#[test]
fn a_plugin_keeps_nothing_from_one_call_to_the_next() {
    let Some(mut plugin) = hello_wim() else {
        return;
    };
    // Every call is given the whole buffer and answers with a value, so the same call over
    // another buffer sees that buffer and nothing of the one before it.
    let first = Snapshot {
        text: "one\n".to_string(),
        ..buffer()
    };
    let second = Snapshot {
        text: "two\n".to_string(),
        ..buffer()
    };
    assert_eq!(
        plugin.run("upcase", &[], &first).expect("the first run"),
        Edit::ReplaceAll("ONE\n".to_string())
    );
    assert_eq!(
        plugin.run("upcase", &[], &second).expect("the second run"),
        Edit::ReplaceAll("TWO\n".to_string())
    );
}
