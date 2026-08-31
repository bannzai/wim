//! The Normal and Operator-pending key grammar: `[count] operator [count] motion|object`.

use crate::command::{Command, InsertAnchor, Operator, OperatorTarget};
use crate::key::{KeyCode, KeyEvent};
use crate::mode::Mode;
use crate::motion::{Find, Motion};
use crate::textobject::TextObject;

/// Keys that are typed but not resolved yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Stage {
    /// Nothing beyond counts and an operator.
    #[default]
    Start,
    /// `f`, `F`, `t` or `T` is waiting for the character to search for.
    AwaitFindTarget { backward: bool, till: bool },
    /// `g` is waiting for the key that completes it.
    AwaitGoto,
    /// `i` or `a` is waiting for the key that names the object.
    AwaitTextObject { around: bool },
}

/// Collects keys until they spell out a [`Command`].
///
/// A command is `[count] operator [count] (motion | text object)`, with the two counts
/// multiplied together the way Vim multiplies them, so `2d3w` reaches as far as `d6w`. Keys
/// that cannot continue what has been typed are rejected and drop the pending keys with
/// them, and `<Esc>` drops them on request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Grammar {
    count_before_operator: Option<usize>,
    operator: Option<Operator>,
    count_after_operator: Option<usize>,
    stage: Stage,
}

impl Grammar {
    /// Reads one key and returns what it completes, or [`Command::Pending`] while the
    /// command is still being typed.
    ///
    /// `mode` only tells Visual apart from Normal: a selection takes counts and motions, and
    /// leaves operators and text objects to the operator issue.
    pub fn feed(&mut self, key: KeyEvent, mode: Mode) -> Command {
        if key.code == KeyCode::Esc {
            self.reset();
            return Command::Cancel;
        }
        let Some(character) = key.as_char() else {
            self.reset();
            return Command::Rejected(key);
        };
        match self.stage {
            Stage::AwaitFindTarget { backward, till } => {
                self.stage = Stage::Start;
                return self.finish_motion(Motion::Find(Find {
                    target: character,
                    backward,
                    till,
                }));
            }
            Stage::AwaitGoto => {
                self.stage = Stage::Start;
                return match character {
                    'g' => self.finish_motion(Motion::FirstLine),
                    _ => self.reject(key),
                };
            }
            Stage::AwaitTextObject { around } => {
                self.stage = Stage::Start;
                return match TextObject::from_key(character, around) {
                    Some(object) => self.finish(OperatorTarget::TextObject(object)),
                    None => self.reject(key),
                };
            }
            Stage::Start => {}
        }

        if let Some(digit) = character.to_digit(10)
            && (digit != 0 || self.count_slot().is_some())
        {
            let slot = self.count_slot_mut();
            // Counts saturate rather than wrap: a count nobody can type out is already past
            // every line and column in the buffer, and motions clamp to what exists.
            *slot = Some(
                slot.unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(digit as usize),
            );
            return Command::Pending;
        }

        if let Some(motion) = simple_motion(character) {
            return self.finish_motion(motion);
        }
        match character {
            'f' | 'F' | 't' | 'T' => {
                self.stage = Stage::AwaitFindTarget {
                    backward: character.is_uppercase(),
                    till: character.eq_ignore_ascii_case(&'t'),
                };
                return Command::Pending;
            }
            'g' => {
                self.stage = Stage::AwaitGoto;
                return Command::Pending;
            }
            _ => {}
        }

        // `v` both starts a selection and drops it, so it is the one command a selection
        // shares with Normal mode.
        if character == 'v' && self.operator.is_none() {
            return self.emit(Command::ToggleVisual);
        }
        if mode == Mode::Visual {
            return self.reject(key);
        }

        if let Some(operator) = Operator::from_key(character) {
            return match self.operator {
                None => {
                    self.operator = Some(operator);
                    Command::Pending
                }
                // `dd`, `cc`, `yy`.
                Some(pending) if pending == operator => self.finish(OperatorTarget::Lines),
                Some(_) => self.reject(key),
            };
        }

        if self.operator.is_some() {
            return match character {
                'i' => {
                    self.stage = Stage::AwaitTextObject { around: false };
                    Command::Pending
                }
                'a' => {
                    self.stage = Stage::AwaitTextObject { around: true };
                    Command::Pending
                }
                _ => self.reject(key),
            };
        }

        let count = self.count();
        match character {
            'i' => self.emit(Command::EnterInsert(InsertAnchor::BeforeCursor)),
            'I' => self.emit(Command::EnterInsert(InsertAnchor::FirstNonBlank)),
            'a' => self.emit(Command::EnterInsert(InsertAnchor::AfterCursor)),
            'A' => self.emit(Command::EnterInsert(InsertAnchor::LineEnd)),
            'o' => self.emit(Command::EnterInsert(InsertAnchor::LineBelow)),
            'O' => self.emit(Command::EnterInsert(InsertAnchor::LineAbove)),
            'x' => self.emit(Command::DeleteChar {
                before: false,
                count,
            }),
            'X' => self.emit(Command::DeleteChar {
                before: true,
                count,
            }),
            _ => self.reject(key),
        }
    }

    /// Whether an operator is waiting for the range it applies to.
    pub fn is_operator_pending(&self) -> bool {
        self.operator.is_some()
    }

    /// Whether any key has been typed that has not resolved into a command yet.
    pub fn is_pending(&self) -> bool {
        self.count_before_operator.is_some()
            || self.operator.is_some()
            || self.stage != Stage::Start
    }

    /// Drops every key typed so far.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The count the command was given, the two counts multiplied together.
    fn count(&self) -> Option<usize> {
        match (self.count_before_operator, self.count_after_operator) {
            (None, None) => None,
            (before, after) => Some(before.unwrap_or(1).saturating_mul(after.unwrap_or(1))),
        }
    }

    fn count_slot(&self) -> Option<usize> {
        match self.operator {
            Some(_) => self.count_after_operator,
            None => self.count_before_operator,
        }
    }

    fn count_slot_mut(&mut self) -> &mut Option<usize> {
        match self.operator {
            Some(_) => &mut self.count_after_operator,
            None => &mut self.count_before_operator,
        }
    }

    /// A motion either moves the cursor or gives an operator its range.
    fn finish_motion(&mut self, motion: Motion) -> Command {
        match self.operator {
            Some(_) => self.finish(OperatorTarget::Motion(motion)),
            None => {
                let command = Command::Move {
                    motion,
                    count: self.count(),
                };
                self.emit(command)
            }
        }
    }

    fn finish(&mut self, target: OperatorTarget) -> Command {
        let command = Command::Operate {
            operator: self.operator.expect("an operator is waiting for a target"),
            count: self.count(),
            target,
        };
        self.emit(command)
    }

    fn emit(&mut self, command: Command) -> Command {
        self.reset();
        command
    }

    fn reject(&mut self, key: KeyEvent) -> Command {
        self.reset();
        Command::Rejected(key)
    }
}

fn simple_motion(key: char) -> Option<Motion> {
    let motion = match key {
        'h' => Motion::Left,
        'l' => Motion::Right,
        'j' => Motion::Down,
        'k' => Motion::Up,
        // `0` only reaches here when no count is being typed.
        '0' => Motion::LineStart,
        '^' => Motion::FirstNonBlank,
        '$' => Motion::LineEnd,
        'w' => Motion::WordForward { big: false },
        'W' => Motion::WordForward { big: true },
        'b' => Motion::WordBackward { big: false },
        'B' => Motion::WordBackward { big: true },
        'e' => Motion::WordEnd { big: false },
        'E' => Motion::WordEnd { big: true },
        'G' => Motion::LastLine,
        ';' => Motion::RepeatFind { reverse: false },
        ',' => Motion::RepeatFind { reverse: true },
        _ => return None,
    };
    Some(motion)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::parse_keys;
    use crate::textobject::TextObjectKind;

    /// Feeds every key of `keys` and returns what the last one resolved to.
    fn resolve(keys: &str) -> Command {
        resolve_in(keys, Mode::Normal)
    }

    fn resolve_in(keys: &str, mode: Mode) -> Command {
        let mut grammar = Grammar::default();
        let mut last = Command::Pending;
        for key in parse_keys(keys).expect("key string should parse") {
            last = grammar.feed(key, mode);
        }
        last
    }

    /// Every key of `keys` but the last has to leave the command unresolved.
    fn assert_pending_until_last(keys: &str) {
        let mut grammar = Grammar::default();
        let parsed = parse_keys(keys).expect("key string should parse");
        for key in &parsed[..parsed.len() - 1] {
            assert_eq!(
                grammar.feed(*key, Mode::Normal),
                Command::Pending,
                "{key} of {keys} should be pending"
            );
            assert!(grammar.is_pending());
        }
    }

    const WORD: Motion = Motion::WordForward { big: false };

    #[test]
    fn a_bare_motion_moves() {
        assert_eq!(
            resolve("w"),
            Command::Move {
                motion: WORD,
                count: None
            }
        );
    }

    #[test]
    fn a_count_multiplies_a_motion() {
        assert_pending_until_last("12j");
        assert_eq!(
            resolve("12j"),
            Command::Move {
                motion: Motion::Down,
                count: Some(12)
            }
        );
    }

    #[test]
    fn zero_is_a_motion_unless_a_count_is_being_typed() {
        assert_eq!(
            resolve("0"),
            Command::Move {
                motion: Motion::LineStart,
                count: None
            }
        );
        assert_eq!(
            resolve("10j"),
            Command::Move {
                motion: Motion::Down,
                count: Some(10)
            }
        );
        assert_eq!(
            resolve("d0"),
            Command::Operate {
                operator: Operator::Delete,
                count: None,
                target: OperatorTarget::Motion(Motion::LineStart),
            }
        );
        assert_eq!(
            resolve("d10l"),
            Command::Operate {
                operator: Operator::Delete,
                count: Some(10),
                target: OperatorTarget::Motion(Motion::Right),
            }
        );
    }

    #[test]
    fn an_operator_waits_for_its_target() {
        let mut grammar = Grammar::default();
        assert_eq!(
            grammar.feed(KeyEvent::char('d'), Mode::Normal),
            Command::Pending
        );
        assert!(grammar.is_operator_pending());
        assert_eq!(
            grammar.feed(KeyEvent::char('w'), Mode::Normal),
            Command::Operate {
                operator: Operator::Delete,
                count: None,
                target: OperatorTarget::Motion(WORD),
            }
        );
        assert!(!grammar.is_operator_pending());
    }

    #[test]
    fn counts_around_an_operator_multiply() {
        assert_pending_until_last("2d3w");
        assert_eq!(
            resolve("2d3w"),
            Command::Operate {
                operator: Operator::Delete,
                count: Some(6),
                target: OperatorTarget::Motion(WORD),
            }
        );
    }

    #[test]
    fn a_doubled_operator_takes_lines() {
        for (keys, operator, count) in [
            ("dd", Operator::Delete, None),
            ("2dd", Operator::Delete, Some(2)),
            ("d2d", Operator::Delete, Some(2)),
            ("yy", Operator::Yank, None),
            ("cc", Operator::Change, None),
        ] {
            assert_eq!(
                resolve(keys),
                Command::Operate {
                    operator,
                    count,
                    target: OperatorTarget::Lines,
                },
                "{keys}"
            );
        }
    }

    #[test]
    fn an_operator_takes_a_text_object() {
        assert_pending_until_last("d2aw");
        assert_eq!(
            resolve("d2aw"),
            Command::Operate {
                operator: Operator::Delete,
                count: Some(2),
                target: OperatorTarget::TextObject(TextObject {
                    kind: TextObjectKind::Word { big: false },
                    around: true,
                }),
            }
        );
        assert_eq!(
            resolve("ci\""),
            Command::Operate {
                operator: Operator::Change,
                count: None,
                target: OperatorTarget::TextObject(TextObject {
                    kind: TextObjectKind::Quote('"'),
                    around: false,
                }),
            }
        );
        assert_eq!(
            resolve("yiB"),
            Command::Operate {
                operator: Operator::Yank,
                count: None,
                target: OperatorTarget::TextObject(TextObject {
                    kind: TextObjectKind::Block {
                        open: '{',
                        close: '}'
                    },
                    around: false,
                }),
            }
        );
    }

    #[test]
    fn find_waits_for_the_character_to_search_for() {
        assert_pending_until_last("fx");
        assert_eq!(
            resolve("fx"),
            Command::Move {
                motion: Motion::Find(Find {
                    target: 'x',
                    backward: false,
                    till: false
                }),
                count: None,
            }
        );
        assert_eq!(
            resolve("d2Tあ"),
            Command::Operate {
                operator: Operator::Delete,
                count: Some(2),
                target: OperatorTarget::Motion(Motion::Find(Find {
                    target: 'あ',
                    backward: true,
                    till: true
                })),
            }
        );
    }

    #[test]
    fn gg_needs_both_keys() {
        assert_pending_until_last("2gg");
        assert_eq!(
            resolve("2gg"),
            Command::Move {
                motion: Motion::FirstLine,
                count: Some(2)
            }
        );
        assert_eq!(
            resolve("d2gg"),
            Command::Operate {
                operator: Operator::Delete,
                count: Some(2),
                target: OperatorTarget::Motion(Motion::FirstLine),
            }
        );
        assert_eq!(resolve("gx"), Command::Rejected(KeyEvent::char('x')));
    }

    #[test]
    fn mode_change_and_single_key_commands_resolve_at_once() {
        assert_eq!(
            resolve("i"),
            Command::EnterInsert(InsertAnchor::BeforeCursor)
        );
        assert_eq!(resolve("A"), Command::EnterInsert(InsertAnchor::LineEnd));
        assert_eq!(resolve("o"), Command::EnterInsert(InsertAnchor::LineBelow));
        assert_eq!(resolve("v"), Command::ToggleVisual);
        assert_eq!(
            resolve("3x"),
            Command::DeleteChar {
                before: false,
                count: Some(3)
            }
        );
        assert_eq!(
            resolve("X"),
            Command::DeleteChar {
                before: true,
                count: None
            }
        );
    }

    #[test]
    fn escape_drops_the_keys_typed_so_far() {
        let mut grammar = Grammar::default();
        for key in parse_keys("2d3").expect("key string should parse") {
            grammar.feed(key, Mode::Normal);
        }
        assert!(grammar.is_pending());
        assert_eq!(
            grammar.feed(KeyEvent::key(KeyCode::Esc), Mode::Normal),
            Command::Cancel
        );
        assert!(!grammar.is_pending());
        assert_eq!(
            grammar.feed(KeyEvent::char('w'), Mode::Normal),
            Command::Move {
                motion: WORD,
                count: None
            },
            "the dropped count must not reach the next command"
        );
    }

    #[test]
    fn a_key_that_continues_nothing_is_rejected_and_drops_the_pending_keys() {
        let mut grammar = Grammar::default();
        grammar.feed(KeyEvent::char('2'), Mode::Normal);
        grammar.feed(KeyEvent::char('d'), Mode::Normal);
        assert_eq!(
            grammar.feed(KeyEvent::char('z'), Mode::Normal),
            Command::Rejected(KeyEvent::char('z'))
        );
        assert!(!grammar.is_pending());
        assert_eq!(resolve("dy"), Command::Rejected(KeyEvent::char('y')));
        assert_eq!(resolve("dix"), Command::Rejected(KeyEvent::char('x')));
        assert_eq!(
            resolve("<C-r>"),
            Command::Rejected(KeyEvent::ctrl('r')),
            "control keys are not part of the grammar yet"
        );
    }

    #[test]
    fn a_selection_takes_motions_but_leaves_operators_to_the_operator_issue() {
        assert_eq!(
            resolve_in("2w", Mode::Visual),
            Command::Move {
                motion: WORD,
                count: Some(2)
            }
        );
        assert_eq!(resolve_in("v", Mode::Visual), Command::ToggleVisual);
        assert_eq!(
            resolve_in("d", Mode::Visual),
            Command::Rejected(KeyEvent::char('d'))
        );
    }
}
