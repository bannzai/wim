//! First-party Markdown Preview plugin: the demo `documents/PROJECT.md` names as the thing a
//! terminal Vim cannot have, done over the plugin ABI rather than built into the editor.
//!
//! It implements one interface of the world and stands the other two down. The panel of `ui` is
//! the whole plugin: the host hands it the buffer and it hands back the buffer rendered as HTML.
//! `commands` publishes nothing — a preview is not something to run, it is something that is
//! either open or closed — and `events` subscribes to nothing, because the ABI already has the
//! host call `render` when the panel opens and when the buffer changes (`wit/plugin.wit`). An
//! autocmd bound to this plugin would be refused for that reason, which is the design saying so.
//!
//! The HTML is not sanitized here. CommonMark passes raw HTML through by definition, so a buffer
//! holding a `<script>` renders as one; what makes that safe is where the host puts the panel,
//! and both hosts put it somewhere nothing in it can run (`wit/README.md`).

wit_bindgen::generate!({
    path: "../../wit",
    world: "plugin",
});

use exports::wim::plugin::commands::{Command, Guest as Commands};
use exports::wim::plugin::events::{Event, Guest as Events};
use exports::wim::plugin::ui::{Guest as Ui, Panel};
use pulldown_cmark::{Options, Parser, html};
use wim::plugin::buffer::{Edit, Snapshot};

/// The heading the host shows over the panel.
const TITLE: &str = "Markdown Preview";

/// The file extensions a buffer is previewed for, matched without regard to case. These are the
/// two the CommonMark reference registers for Markdown and the two GitHub renders a file for, so
/// they are the ones a buffer that is meant to be read as Markdown is written under.
const EXTENSIONS: [&str; 2] = ["md", "markdown"];

struct MarkdownPreview;

impl Commands for MarkdownPreview {
    fn list_commands() -> Vec<Command> {
        Vec::new()
    }

    fn run(name: String, _args: Vec<String>, _buf: Snapshot) -> Result<Edit, String> {
        Err(format!(
            "markdown-preview publishes no commands, and has none named `{name}`"
        ))
    }
}

impl Events for MarkdownPreview {
    fn subscriptions() -> Vec<String> {
        Vec::new()
    }

    fn on_event(ev: Event, _buf: Snapshot) -> Result<Edit, String> {
        // Unreachable through a host that keeps to the ABI: nothing is subscribed to, so nothing
        // is delivered. Refusing rather than answering `noop` is what says so out loud.
        Err(format!(
            "markdown-preview subscribes to no events, and was given `{}`",
            ev.name
        ))
    }
}

impl Ui for MarkdownPreview {
    fn render(buf: Snapshot) -> Option<Panel> {
        if !is_markdown(&buf.name) {
            // None closes the panel, which is what a buffer that is not Markdown should leave
            // behind: a preview of a Rust file is not a preview of anything.
            return None;
        }
        Some(Panel {
            title: TITLE.to_string(),
            html: to_html(&buf.text),
        })
    }
}

export!(MarkdownPreview);

/// Whether a buffer called `name` is one to preview.
///
/// The name is the only thing a plugin is told about where the buffer came from, and a buffer
/// that came from no file has none — which is a buffer with no language, not a Markdown one.
fn is_markdown(name: &str) -> bool {
    let Some((_, extension)) = name.rsplit_once('.') else {
        return false;
    };
    // `notes.d/README` splits at a dot that is part of a directory rather than an extension, and
    // what follows it is a path and not the name of a language.
    if extension.contains('/') || extension.contains('\\') {
        return false;
    }
    EXTENSIONS
        .iter()
        .any(|known| extension.eq_ignore_ascii_case(known))
}

/// `text` rendered as HTML.
fn to_html(text: &str) -> String {
    let mut rendered = String::with_capacity(text.len());
    html::push_html(&mut rendered, Parser::new_ext(text, options()));
    rendered
}

/// The Markdown dialect the preview reads.
///
/// CommonMark plus the four extensions GitHub adds to it, because the files this previews are
/// written to be read there: a table, a `~~strikethrough~~`, a `- [ ]` task list or a footnote
/// would otherwise render as the punctuation it is written with. Nothing here changes what plain
/// CommonMark means, so a file using none of them renders the same either way.
fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(name: &str, text: &str) -> Snapshot {
        Snapshot {
            name: name.to_string(),
            text: text.to_string(),
            cursor: wim::plugin::buffer::Position { line: 0, column: 0 },
        }
    }

    #[test]
    fn headings_paragraphs_and_emphasis_render_as_themselves() {
        assert_eq!(
            to_html("# Title\n\nA *word* and a `call`.\n"),
            "<h1>Title</h1>\n<p>A <em>word</em> and a <code>call</code>.</p>\n"
        );
    }

    #[test]
    fn lists_and_fences_render_as_themselves() {
        assert_eq!(
            to_html("- one\n- two\n"),
            "<ul>\n<li>one</li>\n<li>two</li>\n</ul>\n"
        );
        assert_eq!(
            to_html("```rust\nfn main() {}\n```\n"),
            "<pre><code class=\"language-rust\">fn main() {}\n</code></pre>\n"
        );
    }

    #[test]
    fn the_github_extensions_are_the_ones_that_are_on() {
        assert_eq!(
            to_html("| a | b |\n| - | - |\n| 1 | 2 |\n"),
            "<table><thead><tr><th>a</th><th>b</th></tr></thead><tbody>\n\
             <tr><td>1</td><td>2</td></tr>\n</tbody></table>\n"
        );
        assert_eq!(to_html("~~gone~~\n"), "<p><del>gone</del></p>\n");
        assert!(to_html("- [x] done\n").contains("type=\"checkbox\""));
    }

    #[test]
    fn what_the_text_says_is_escaped_and_what_it_marks_up_is_not() {
        // The `<` of a paragraph is text and comes out escaped, while a tag written as a tag is
        // raw HTML that CommonMark passes through. The second is why the host does not trust
        // what comes back (`wit/README.md`).
        assert_eq!(to_html("a < b\n"), "<p>a &lt; b</p>\n");
        assert_eq!(to_html("<script>x()</script>\n"), "<script>x()</script>\n");
    }

    #[test]
    fn a_markdown_buffer_is_the_one_with_a_panel() {
        assert!(MarkdownPreview::render(buffer("notes.md", "# Title\n")).is_some());
        assert!(MarkdownPreview::render(buffer("NOTES.MARKDOWN", "# Title\n")).is_some());
        let panel = MarkdownPreview::render(buffer("notes.md", "# Title\n")).unwrap();
        assert_eq!(panel.title, TITLE);
        assert_eq!(panel.html, "<h1>Title</h1>\n");
    }

    #[test]
    fn every_other_buffer_closes_the_panel() {
        assert!(MarkdownPreview::render(buffer("main.rs", "# Title\n")).is_none());
        // A buffer that came from no file has no name, and no name is not a Markdown one.
        assert!(MarkdownPreview::render(buffer("", "# Title\n")).is_none());
        assert!(MarkdownPreview::render(buffer("notes.d/README", "# Title\n")).is_none());
    }

    #[test]
    fn the_interfaces_the_panel_does_not_need_publish_nothing() {
        assert!(MarkdownPreview::list_commands().is_empty());
        assert!(MarkdownPreview::subscriptions().is_empty());
        assert!(
            MarkdownPreview::run("preview".to_string(), Vec::new(), buffer("notes.md", ""))
                .is_err()
        );
    }
}
