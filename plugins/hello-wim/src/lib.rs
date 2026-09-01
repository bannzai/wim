//! First-party sample plugin. It implements one command, one event hook and one panel so that
//! every interface of the ABI in `wit/plugin.wit` has a worked example.

wit_bindgen::generate!({
    path: "../../wit",
    world: "plugin",
});

use exports::wim::plugin::commands::{Command, Guest as Commands};
use exports::wim::plugin::events::{Event, Guest as Events};
use exports::wim::plugin::ui::{Guest as Ui, Panel};
use wim::plugin::buffer::{Edit, Snapshot};

/// The Ex command the host registers this plugin under.
const COMMAND: &str = "upcase";

/// The only event this plugin subscribes to.
const EVENT: &str = "buffer-write";

struct HelloWim;

impl Commands for HelloWim {
    fn list_commands() -> Vec<Command> {
        vec![Command {
            name: COMMAND.to_string(),
            description: "Uppercases the whole buffer.".to_string(),
        }]
    }

    fn run(name: String, args: Vec<String>, buf: Snapshot) -> Result<Edit, String> {
        if name != COMMAND {
            return Err(format!("hello-wim has no command named `{name}`"));
        }
        upcase(&args, &buf.text).map(Edit::ReplaceAll)
    }
}

impl Events for HelloWim {
    fn subscriptions() -> Vec<String> {
        vec![EVENT.to_string()]
    }

    fn on_event(ev: Event, buf: Snapshot) -> Result<Edit, String> {
        Ok(Edit::Message(event_message(&ev.name, &buf.name)))
    }
}

impl Ui for HelloWim {
    fn render(buf: Snapshot) -> Option<Panel> {
        Some(Panel {
            title: "hello-wim".to_string(),
            html: panel_html(&buf.name, &buf.text),
        })
    }
}

export!(HelloWim);

/// The body of `:upcase`. The command takes no arguments, so anything passed is a mistake worth
/// reporting rather than ignoring.
fn upcase(args: &[String], text: &str) -> Result<String, String> {
    if !args.is_empty() {
        return Err(format!(":{COMMAND} takes no arguments"));
    }
    Ok(text.to_uppercase())
}

fn event_message(event: &str, name: &str) -> String {
    format!("hello-wim saw `{event}` on {}", display_name(name))
}

fn panel_html(name: &str, text: &str) -> String {
    format!(
        "<h1>hello-wim</h1><p>{} &middot; {} line(s)</p>",
        escape(&display_name(name)),
        text.lines().count(),
    )
}

/// A buffer that is not backed by a file has an empty name, which reads as a gap in the panel.
fn display_name(name: &str) -> String {
    if name.is_empty() {
        "[No Name]".to_string()
    } else {
        name.to_string()
    }
}

/// Panels are HTML, so whatever comes out of a buffer goes through here first. The host
/// sanitizes what it renders as well, but a plugin that emits broken markup is its own bug.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upcase_rewrites_every_line() {
        assert_eq!(upcase(&[], "hello\nwim\n").unwrap(), "HELLO\nWIM\n");
    }

    #[test]
    fn upcase_rejects_arguments() {
        assert_eq!(
            upcase(&["x".to_string()], "hello"),
            Err(":upcase takes no arguments".to_string())
        );
    }

    #[test]
    fn event_message_names_the_buffer() {
        assert_eq!(
            event_message("buffer-write", "src/main.rs"),
            "hello-wim saw `buffer-write` on src/main.rs"
        );
        assert_eq!(
            event_message("buffer-write", ""),
            "hello-wim saw `buffer-write` on [No Name]"
        );
    }

    #[test]
    fn panel_escapes_the_buffer_name() {
        assert_eq!(
            panel_html("<script>.rs", "one\ntwo"),
            "<h1>hello-wim</h1><p>&lt;script&gt;.rs &middot; 2 line(s)</p>"
        );
    }
}
