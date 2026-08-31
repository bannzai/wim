//! The registers text travels through between a yank or a delete and a paste.

use std::collections::BTreeMap;

/// Text a register holds, and how a paste has to put it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterContent {
    /// The text itself. Linewise content ends with the break that terminates its last line.
    pub text: String,
    /// Whole lines rather than characters: a paste puts them on lines of their own.
    pub linewise: bool,
}

impl RegisterContent {
    pub fn charwise(text: String) -> Self {
        Self {
            text,
            linewise: false,
        }
    }

    /// Lines to be pasted as lines. `text` gains the terminating break if it has none.
    pub fn linewise(mut text: String) -> Self {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        Self {
            text,
            linewise: true,
        }
    }
}

/// The registers a yank or a delete fills and a paste reads.
///
/// wim keeps the unnamed register and the named `"a` to `"z`, and simplifies away the rest
/// of Vim's rules: there are no numbered registers, no small delete register, no read-only
/// registers, and no appending form `"A`. A yank or a delete always fills the unnamed
/// register, and fills a named one as well when the command named one; a paste reads the
/// register it names, and the unnamed one when it names none.
///
/// A macro recorded with `q{a-z}` shares this storage: it is kept as its keys written out in
/// the key notation, so `"qp` puts a macro into the buffer as text and `@q` reads it back out
/// of a register a paste could have filled.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Registers {
    unnamed: Option<RegisterContent>,
    named: BTreeMap<char, RegisterContent>,
}

impl Registers {
    /// Fills the unnamed register, and `name` too when the command named one.
    pub fn store(&mut self, name: Option<char>, content: RegisterContent) {
        if let Some(name) = name {
            self.named.insert(name, content.clone());
        }
        self.unnamed = Some(content);
    }

    /// Fills `name` and leaves the unnamed register as it was, which is how a recorded macro
    /// lands in a register without changing what a paste would put back.
    pub fn store_named(&mut self, name: char, content: RegisterContent) {
        self.named.insert(name, content);
    }

    /// What `name` holds, or the unnamed register when no name was given. `None` when the
    /// register has never been filled.
    pub fn get(&self, name: Option<char>) -> Option<&RegisterContent> {
        match name {
            Some(name) => self.named.get(&name),
            None => self.unnamed.as_ref(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_store_fills_the_unnamed_register_as_well() {
        let mut registers = Registers::default();
        registers.store(Some('a'), RegisterContent::charwise("foo".to_owned()));
        assert_eq!(
            registers.get(Some('a')).map(|held| held.text.as_str()),
            Some("foo")
        );
        assert_eq!(
            registers.get(None).map(|held| held.text.as_str()),
            Some("foo")
        );
        assert_eq!(registers.get(Some('b')), None);
    }

    #[test]
    fn an_unnamed_store_leaves_the_named_registers_alone() {
        let mut registers = Registers::default();
        registers.store(Some('a'), RegisterContent::linewise("keep".to_owned()));
        registers.store(None, RegisterContent::charwise("new".to_owned()));
        assert_eq!(
            registers.get(Some('a')),
            Some(&RegisterContent {
                text: "keep\n".to_owned(),
                linewise: true
            })
        );
        assert_eq!(
            registers.get(None).map(|held| held.text.as_str()),
            Some("new")
        );
    }
}
