//! Bindings that let a browser drive a [`wim_core::Editor`].
//!
//! The boundary is deliberately coarse (`documents/PROJECT.md`): the host hands over a key
//! string and gets back the lines that changed plus what the core asks of it, then reads the
//! state it draws from through the getters here. Nothing about drawing or file IO lives in
//! this crate either; it only translates the core's types into ones JS can hold.

use serde_json::json;
use wasm_bindgen::prelude::*;
use wim_core::{Editor, Effect};

/// An editor a browser owns.
#[wasm_bindgen]
pub struct WimEditor {
    editor: Editor,
}

/// What one batch of keys did: the lines to redraw, and what the core asks of the host.
#[wasm_bindgen]
pub struct KeyOutcome {
    damage_start: usize,
    damage_end: usize,
    effects: String,
}

#[wasm_bindgen]
impl KeyOutcome {
    /// First line the keys changed.
    ///
    /// The range is half-open and empty when the text is untouched, which a key that only
    /// moves the cursor or switches mode leaves it. Where the cursor is drawn is the host's
    /// own business, so the damage says nothing about it.
    #[wasm_bindgen(getter)]
    pub fn damage_start(&self) -> usize {
        self.damage_start
    }

    /// One past the last line the keys changed.
    #[wasm_bindgen(getter)]
    pub fn damage_end(&self) -> usize {
        self.damage_end
    }

    /// The [`Effect`]s as a JSON array, for the host to parse and carry out.
    ///
    /// One object per effect: `{"kind":"error","message":…}` for a key the mode had no
    /// meaning for, `{"kind":"save","path":…}` for `:w`, `{"kind":"quit","force":…}` for
    /// `:q`. JSON rather than an exported type per effect keeps the boundary to the one
    /// value a batch of keys returns.
    #[wasm_bindgen(getter)]
    pub fn effects(&self) -> String {
        self.effects.clone()
    }
}

#[wasm_bindgen]
impl WimEditor {
    /// An editor over `text`, in Normal mode with the cursor at the start.
    #[wasm_bindgen(constructor)]
    pub fn new(text: &str) -> Self {
        Self {
            editor: Editor::new(text),
        }
    }

    /// The text being edited, as it would be written back out.
    pub fn text(&self) -> String {
        self.editor.text()
    }

    pub fn line_count(&self) -> usize {
        self.editor.buffer().line_count()
    }

    /// Text of `line` without its line ending, empty when `line` does not exist.
    pub fn line(&self, line: usize) -> String {
        self.editor.buffer().line_text(line)
    }

    pub fn cursor_line(&self) -> usize {
        self.editor.cursor().line
    }

    pub fn cursor_col(&self) -> usize {
        self.editor.cursor().col
    }

    /// Name of the current mode, as a mode line shows it.
    pub fn mode(&self) -> String {
        self.editor.mode().label().to_owned()
    }

    /// The `:`, `/` or `?` line being typed, its prefix included, `undefined` outside
    /// Command-line mode.
    pub fn command_line(&self) -> Option<String> {
        self.editor.command_line().map(str::to_owned)
    }

    /// Feeds a key string such as `ihello<Esc>`, throwing when it does not parse.
    ///
    /// A key string that fails to parse changes nothing: the core reads the whole of it
    /// before it runs any of it.
    pub fn handle_keys(&mut self, keys: &str) -> Result<KeyOutcome, JsError> {
        let before = self.lines();
        let effects = self
            .editor
            .handle_keys(keys)
            .map_err(|error| JsError::new(&error.to_string()))?;
        let (damage_start, damage_end) = damaged_lines(&before, &self.lines());
        Ok(KeyOutcome {
            damage_start,
            damage_end,
            effects: effects_json(&effects),
        })
    }
}

impl WimEditor {
    fn lines(&self) -> Vec<String> {
        (0..self.editor.buffer().line_count())
            .map(|line| self.editor.buffer().line_text(line))
            .collect()
    }
}

/// The half-open range of lines that differ between `before` and `after`.
///
/// A change that adds or removes a line shifts every line under it, so the range then runs to
/// the end of the buffer rather than stopping at the last line whose text differs.
fn damaged_lines(before: &[String], after: &[String]) -> (usize, usize) {
    let start = before
        .iter()
        .zip(after)
        .take_while(|(before, after)| before == after)
        .count();
    if before.len() != after.len() {
        return (start, after.len());
    }
    let end = after.len()
        - after
            .iter()
            .rev()
            .zip(before.iter().rev())
            .take_while(|(after, before)| after == before)
            .count();
    (start.min(end), end)
}

fn effects_json(effects: &[Effect]) -> String {
    let effects: Vec<serde_json::Value> = effects
        .iter()
        .map(|effect| match effect {
            Effect::Error(message) => json!({ "kind": "error", "message": message }),
            Effect::SaveRequested { path } => json!({ "kind": "save", "path": path }),
            Effect::QuitRequested { force } => json!({ "kind": "quit", "force": force }),
        })
        .collect();
    serde_json::Value::Array(effects).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| (*line).to_owned()).collect()
    }

    #[test]
    fn text_that_did_not_change_is_an_empty_damage_range() {
        assert_eq!(
            damaged_lines(&lines(&["alpha", "bravo"]), &lines(&["alpha", "bravo"])),
            (0, 0)
        );
    }

    #[test]
    fn an_edited_line_damages_that_line_alone() {
        assert_eq!(
            damaged_lines(
                &lines(&["alpha", "bravo", "charlie"]),
                &lines(&["alpha", "BRAVO", "charlie"]),
            ),
            (1, 2)
        );
    }

    #[test]
    fn a_deleted_line_damages_everything_under_it() {
        assert_eq!(
            damaged_lines(
                &lines(&["alpha", "bravo", "charlie"]),
                &lines(&["alpha", "charlie"]),
            ),
            (1, 2)
        );
    }

    #[test]
    fn typing_moves_the_cursor_and_damages_the_line_it_typed_into() {
        let mut editor = WimEditor::new("bar");
        let outcome = editor.handle_keys("ifoo <Esc>").expect("keys should parse");
        assert_eq!(editor.text(), "foo bar");
        assert_eq!(editor.mode(), "NORMAL");
        assert_eq!((editor.cursor_line(), editor.cursor_col()), (0, 3));
        assert_eq!((outcome.damage_start(), outcome.damage_end()), (0, 1));
        assert_eq!(outcome.effects(), "[]");
    }

    #[test]
    fn opening_a_line_damages_the_lines_it_pushed_down() {
        let mut editor = WimEditor::new("alpha\ncharlie");
        let outcome = editor
            .handle_keys("obravo<Esc>")
            .expect("keys should parse");
        assert_eq!(editor.line_count(), 3);
        assert_eq!(editor.line(1), "bravo");
        assert_eq!((outcome.damage_start(), outcome.damage_end()), (1, 3));
    }

    #[test]
    fn a_command_line_is_readable_while_it_is_typed_and_returns_its_effect() {
        let mut editor = WimEditor::new("alpha");
        editor.handle_keys(":w note").expect("keys should parse");
        assert_eq!(editor.mode(), "COMMAND");
        assert_eq!(editor.command_line().as_deref(), Some(":w note"));

        let outcome = editor.handle_keys("<CR>").expect("keys should parse");
        assert_eq!(outcome.effects(), r#"[{"kind":"save","path":"note"}]"#);
        assert_eq!(editor.command_line(), None);
    }
}
