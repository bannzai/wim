//! Key events and the textual key notation macros, golden tests and the CLI are written in.

use std::fmt;

/// A key press with its modifiers stripped off.
///
/// Shift is not represented here: it is already part of the character, `A` rather than
/// shift + `a`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    /// A key that stands for one character.
    Char(char),
    /// `<Esc>`
    Esc,
    /// `<CR>`
    Enter,
    /// `<BS>`
    Backspace,
    /// `<Tab>`
    Tab,
}

/// A single key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    /// The key that was pressed.
    pub code: KeyCode,
    /// Whether control was held, written `<C-x>` in the key notation. Only meaningful
    /// together with [`KeyCode::Char`].
    pub ctrl: bool,
}

impl KeyEvent {
    /// A plain character key.
    pub fn char(character: char) -> Self {
        Self {
            code: KeyCode::Char(character),
            ctrl: false,
        }
    }

    /// A character key with control held. ASCII letters are lowered so that `<C-A>` and
    /// `<C-a>` are the same event, which is how terminals report them.
    pub fn ctrl(character: char) -> Self {
        Self {
            code: KeyCode::Char(character.to_ascii_lowercase()),
            ctrl: true,
        }
    }

    /// A key that has no character of its own, such as `<Esc>`.
    pub fn key(code: KeyCode) -> Self {
        Self { code, ctrl: false }
    }

    /// The character this key stands for, `None` for keys that carry no character and for
    /// control combinations, which are commands rather than text.
    pub fn as_char(&self) -> Option<char> {
        match self.code {
            KeyCode::Char(character) if !self.ctrl => Some(character),
            _ => None,
        }
    }
}

impl fmt::Display for KeyEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.code {
            KeyCode::Esc => f.write_str("<Esc>"),
            KeyCode::Enter => f.write_str("<CR>"),
            KeyCode::Backspace => f.write_str("<BS>"),
            KeyCode::Tab => f.write_str("<Tab>"),
            KeyCode::Char(character) if self.ctrl => write!(f, "<C-{character}>"),
            // `<lt>` keeps the output parseable: a bare `<` would open a notation.
            KeyCode::Char('<') => f.write_str("<lt>"),
            KeyCode::Char(character) => write!(f, "{character}"),
        }
    }
}

/// Why a key string could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyParseError {
    /// A `<…>` group that is none of the supported notations. A literal `<` is written
    /// `<lt>`.
    UnknownNotation {
        /// The group as it was written, angle brackets included.
        notation: String,
        /// Index of the opening `<`, counted in characters.
        at: usize,
    },
}

impl fmt::Display for KeyParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownNotation { notation, at } => {
                write!(f, "unknown key notation {notation} at {at}")
            }
        }
    }
}

impl std::error::Error for KeyParseError {}

/// Reads a key string such as `ciwfoo<Esc>` into the keys it stands for.
///
/// The notations are `<Esc>`, `<CR>`, `<BS>`, `<Tab>`, `<lt>` for a literal `<`, and `<C-x>`
/// for control combinations; names are matched case insensitively. Every other character
/// stands for itself, and so does a `<` that opens no complete group, which is what lets
/// `di<` be written without escaping. A complete `<…>` group that names no known key is an
/// error rather than literal text, so that a typo in a macro is reported instead of typed.
pub fn parse_keys(keys: &str) -> Result<Vec<KeyEvent>, KeyParseError> {
    let characters: Vec<char> = keys.chars().collect();
    let mut events = Vec::new();
    let mut index = 0;
    while index < characters.len() {
        if let Some(body) = notation_body(&characters[index..]) {
            let event = parse_notation(&body).ok_or_else(|| KeyParseError::UnknownNotation {
                notation: format!("<{body}>"),
                at: index,
            })?;
            events.push(event);
            index += body.chars().count() + 2;
            continue;
        }
        events.push(KeyEvent::char(characters[index]));
        index += 1;
    }
    Ok(events)
}

/// Renders keys back into the notation [`parse_keys`] reads.
pub fn format_keys(keys: &[KeyEvent]) -> String {
    keys.iter().map(KeyEvent::to_string).collect()
}

/// Text between a leading `<` and the first `>`, `None` when `characters` does not open a
/// complete group. A `<` inside ends the search, so `a<b<CR>` reads the group as `CR`.
fn notation_body(characters: &[char]) -> Option<String> {
    if characters.first() != Some(&'<') {
        return None;
    }
    let mut body = String::new();
    for character in &characters[1..] {
        match character {
            '>' => return (!body.is_empty()).then_some(body),
            '<' => return None,
            _ => body.push(*character),
        }
    }
    None
}

fn parse_notation(body: &str) -> Option<KeyEvent> {
    let name = body.to_ascii_lowercase();
    match name.as_str() {
        "esc" => return Some(KeyEvent::key(KeyCode::Esc)),
        "cr" => return Some(KeyEvent::key(KeyCode::Enter)),
        "bs" => return Some(KeyEvent::key(KeyCode::Backspace)),
        "tab" => return Some(KeyEvent::key(KeyCode::Tab)),
        "lt" => return Some(KeyEvent::char('<')),
        _ => {}
    }
    let modified = name.strip_prefix("c-")?;
    let mut characters = body.chars().skip(2);
    let character = characters.next()?;
    (modified.chars().count() == 1).then(|| KeyEvent::ctrl(character))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_plain_characters_as_themselves() {
        assert_eq!(
            parse_keys("ab"),
            Ok(vec![KeyEvent::char('a'), KeyEvent::char('b')])
        );
        assert_eq!(parse_keys("あ"), Ok(vec![KeyEvent::char('あ')]));
        assert_eq!(parse_keys(""), Ok(Vec::new()));
    }

    #[test]
    fn reads_the_named_notations() {
        assert_eq!(
            parse_keys("<Esc><CR><BS><Tab><lt>"),
            Ok(vec![
                KeyEvent::key(KeyCode::Esc),
                KeyEvent::key(KeyCode::Enter),
                KeyEvent::key(KeyCode::Backspace),
                KeyEvent::key(KeyCode::Tab),
                KeyEvent::char('<'),
            ])
        );
    }

    #[test]
    fn notation_names_are_case_insensitive() {
        assert_eq!(parse_keys("<esc>"), parse_keys("<Esc>"));
        assert_eq!(parse_keys("<C-R>"), parse_keys("<C-r>"));
    }

    #[test]
    fn reads_control_combinations() {
        assert_eq!(parse_keys("<C-r>"), Ok(vec![KeyEvent::ctrl('r')]));
        assert_eq!(
            parse_keys("<C-r>"),
            Ok(vec![KeyEvent {
                code: KeyCode::Char('r'),
                ctrl: true
            }])
        );
    }

    #[test]
    fn a_bare_angle_bracket_that_opens_no_group_is_literal() {
        assert_eq!(
            parse_keys("di<"),
            Ok(vec![
                KeyEvent::char('d'),
                KeyEvent::char('i'),
                KeyEvent::char('<'),
            ])
        );
        assert_eq!(
            parse_keys("a<b<Esc>"),
            Ok(vec![
                KeyEvent::char('a'),
                KeyEvent::char('<'),
                KeyEvent::char('b'),
                KeyEvent::key(KeyCode::Esc),
            ])
        );
    }

    #[test]
    fn an_unknown_group_is_an_error() {
        assert_eq!(
            parse_keys("a<Escape>"),
            Err(KeyParseError::UnknownNotation {
                notation: "<Escape>".to_owned(),
                at: 1,
            })
        );
        assert!(parse_keys("<C-ab>").is_err());
        assert!(parse_keys("<>").is_ok(), "an empty group is literal text");
    }

    #[test]
    fn keys_round_trip_through_the_notation() {
        for keys in [
            "ciwfoo<Esc>",
            "2d3w",
            "i<CR><BS><Tab><Esc>",
            "<C-r>u",
            "i<lt>div>",
            "dta;",
        ] {
            let events = parse_keys(keys).expect("key string should parse");
            assert_eq!(format_keys(&events), keys, "round trip of {keys}");
            assert_eq!(parse_keys(&format_keys(&events)), Ok(events));
        }
    }

    #[test]
    fn only_plain_character_keys_carry_text() {
        assert_eq!(KeyEvent::char('あ').as_char(), Some('あ'));
        assert_eq!(KeyEvent::ctrl('r').as_char(), None);
        assert_eq!(KeyEvent::key(KeyCode::Enter).as_char(), None);
    }
}
