//! `wim.jsonc`: the file a host reads its autocmds out of.
//!
//! The format is documented in `documents/CONFIG.md`, and the shape below is what that document
//! describes. Reading it is a host's business rather than the core's — the core reports events
//! and knows nothing of files (`crates/wim-core/src/effect.rs`) — so the schema lives here, next
//! to the host that carries the handlers out.
//!
//! The dialect is VS Code's JSONC: JSON with comments and trailing commas, and nothing else. It
//! is spelled out rather than left to the parser's defaults because the same file is read by the
//! browser demo with a reader of its own (`web/config.js`), and a config that only one of the two
//! hosts accepts is worse than one neither does.

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use jsonc_parser::ParseOptions;
use serde::Deserialize;
use wim_core::Event;

/// A parsed `wim.jsonc`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The handlers to run when the editor reports an event, in the order they are written.
    #[serde(default)]
    pub autocmds: Vec<Autocmd>,
}

/// One binding of a handler to an event.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Autocmd {
    /// The event that runs the handler, named as [`Event::name`] names it.
    pub event: String,
    /// What to run.
    pub handler: Handler,
}

/// What an autocmd runs. VimScript is not one of them: a handler is a built-in command, a key
/// sequence, or a plugin function (`documents/PROJECT.md`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Handler {
    /// An Ex command line, written without the `:` that opens it.
    Ex {
        /// The line to run, such as `%s/\s\+$//e`.
        command: String,
    },
    /// A key sequence, in the notation `wim_core::parse_keys` reads.
    Keys {
        /// The keys to type, such as `ggVGd`.
        keys: String,
    },
    /// A plugin, which is given the event through the `on-event` of the ABI (`wit/plugin.wit`).
    Plugin {
        /// The name the host loaded the plugin under.
        plugin: String,
    },
}

/// Why a config could not be read.
#[derive(Debug)]
pub enum Error {
    /// The file could not be read.
    Io(io::Error),
    /// The bytes are not JSONC.
    Syntax(jsonc_parser::errors::ParseError),
    /// It is JSONC, but not a config: a field that does not belong, a handler with no `kind`,
    /// or a value of the wrong type.
    Schema(serde_json::Error),
    /// An autocmd is bound to an event nothing raises.
    UnknownEvent(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(error) => write!(f, "{error}"),
            Error::Syntax(error) => write!(f, "{error}"),
            Error::Schema(error) => write!(f, "{error}"),
            Error::UnknownEvent(event) => write!(
                f,
                "no event is called {event}: an autocmd is bound to one of {}",
                Event::names().join(", ")
            ),
        }
    }
}

impl std::error::Error for Error {}

/// Reads the config at `path`.
pub fn read(path: &Path) -> Result<Config, Error> {
    parse(&fs::read_to_string(path).map_err(Error::Io)?)
}

/// Reads a config out of the text of one.
///
/// The parse is in two steps for the reason the crate takes both dependencies: `jsonc-parser`
/// reads the dialect and hands back a plain JSON value, and serde checks that value against the
/// schema — which is where an unknown field or a handler of an unknown kind is caught, in serde's
/// own wording rather than in one this would have to write.
pub fn parse(text: &str) -> Result<Config, Error> {
    let value: serde_json::Value =
        jsonc_parser::parse_to_serde_value(text, &options()).map_err(Error::Syntax)?;
    // An empty file is a config that binds nothing rather than a config that is missing: what
    // the reader makes of one is `null`, which is not an object serde can read.
    if value.is_null() {
        return Ok(Config::default());
    }
    let config: Config = serde_json::from_value(value).map_err(Error::Schema)?;
    for autocmd in &config.autocmds {
        if !Event::names().contains(&autocmd.event.as_str()) {
            return Err(Error::UnknownEvent(autocmd.event.clone()));
        }
    }
    Ok(config)
}

/// The dialect: JSON, comments and trailing commas. Everything else the reader can be told to
/// allow — single quotes, hexadecimal numbers, missing commas — is off, so that what this accepts
/// is what `web/config.js` accepts as well.
fn options() -> ParseOptions {
    ParseOptions {
        allow_comments: true,
        allow_trailing_commas: true,
        allow_loose_object_property_names: false,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_and_trailing_commas_are_read_the_way_vs_code_reads_them() {
        let config = parse(
            r#"{
              // The whole line.
              "autocmds": [
                /* and a block of one */
                { "event": "buffer-write", "handler": { "kind": "ex", "command": "%s/x/y/e" } },
              ],
            }"#,
        )
        .expect("the dialect should be read");
        assert_eq!(
            config.autocmds,
            vec![Autocmd {
                event: "buffer-write".to_owned(),
                handler: Handler::Ex {
                    command: "%s/x/y/e".to_owned()
                },
            }]
        );
    }

    #[test]
    fn every_kind_of_handler_can_be_declared() {
        let config = parse(
            r#"{
              "autocmds": [
                { "event": "text-changed", "handler": { "kind": "keys", "keys": "ggVGd" } },
                { "event": "buffer-write-post",
                  "handler": { "kind": "plugin", "plugin": "hello-wim" } }
              ]
            }"#,
        )
        .expect("the handlers should be read");
        assert_eq!(
            config.autocmds[0].handler,
            Handler::Keys {
                keys: "ggVGd".to_owned()
            }
        );
        assert_eq!(
            config.autocmds[1].handler,
            Handler::Plugin {
                plugin: "hello-wim".to_owned()
            }
        );
    }

    #[test]
    fn a_config_that_binds_nothing_is_one_a_host_can_run_with() {
        assert_eq!(parse("{}").expect("an empty object"), Config::default());
        assert_eq!(parse("").expect("an empty file"), Config::default());
        assert_eq!(
            parse("// nothing but a comment\n").expect("a file of comments"),
            Config::default()
        );
    }

    #[test]
    fn an_event_nothing_raises_is_reported_where_it_is_written() {
        let error = parse(
            r#"{"autocmds": [{ "event": "BufWritePre",
                              "handler": { "kind": "keys", "keys": "x" } }]}"#,
        )
        .expect_err("no event is called that");
        assert!(matches!(error, Error::UnknownEvent(_)), "{error}");
        assert!(error.to_string().contains("buffer-write"), "{error}");
    }

    #[test]
    fn a_field_that_is_not_in_the_schema_is_refused() {
        for text in [
            r#"{"autocmd": []}"#,
            r#"{"autocmds": [{ "event": "text-changed" }]}"#,
            r#"{"autocmds": [{ "event": "text-changed", "handler": { "kind": "vimscript" } }]}"#,
            r#"{"autocmds": [{ "event": "text-changed",
                               "handler": { "kind": "keys", "keys": "x", "count": 2 } }]}"#,
        ] {
            assert!(
                matches!(parse(text), Err(Error::Schema(_))),
                "{text} should not be a config"
            );
        }
    }

    #[test]
    fn the_dialect_stops_at_comments_and_trailing_commas() {
        assert!(
            matches!(parse("{'autocmds': []}"), Err(Error::Syntax(_))),
            "a single-quoted string is not JSONC"
        );
        assert!(
            matches!(parse(r#"{"autocmds": [] "x": 1}"#), Err(Error::Syntax(_))),
            "a missing comma is not JSONC"
        );
    }
}
