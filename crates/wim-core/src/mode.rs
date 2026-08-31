//! Editing modes.

/// The mode the editor is in, which decides how the next key is read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Keys are commands.
    #[default]
    Normal,
    /// Keys are text.
    Insert,
    /// An operator has been typed and is waiting for the range it applies to.
    OperatorPending,
    /// A selection is being stretched by motions. Characterwise; linewise Visual comes with
    /// the operator issue.
    Visual,
    /// A `:`, `/` or `?` line is being typed. Keys are text until `<CR>` runs the line or
    /// `<Esc>` drops it.
    CommandLine,
}

impl Mode {
    /// Short name for a mode line, matching what Vim shows.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Insert => "INSERT",
            Self::OperatorPending => "OP-PENDING",
            Self::Visual => "VISUAL",
            Self::CommandLine => "COMMAND",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_is_the_mode_an_editor_starts_in() {
        assert_eq!(Mode::default(), Mode::Normal);
        assert_eq!(Mode::default().label(), "NORMAL");
    }
}
