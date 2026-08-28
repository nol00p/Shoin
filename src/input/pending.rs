//! The operator-pending state machine. SPEC.md §7.2.
//!
//! Normal-mode input accumulates as:
//!
//!     register? · count₁? · (operator · count₂? · motion|object) | action
//!
//! Every keypress either EXTENDS the pending state, RESOLVES it into a
//! `Command`, or INVALIDATES it. `partial` additionally handles multi-key
//! sequences — the user keymap's own (`<leader>ff`) and the built-in prefixes
//! (`gg`, `gb`, `<C-w>l`) through the same path.
//!
//! This machine holds NO editor state: it never sees the buffer, so what it
//! emits is always a complete command for `app` to run. That is the whole point
//! of the split — every panel and mode `app` grows from here adds no key
//! handling to it.


use super::action::{Action, Operator, Target};
use super::bindings::{self, Command, Verb};
use super::keymap::{Keymap, Lookup};

/// Which mode's grammar to resolve against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Table {
    Normal,
    Visual,
}

impl Table {
    fn name(self) -> &'static str {
        match self {
            Table::Normal => "normal",
            Table::Visual => "visual",
        }
    }

    fn is_visual(self) -> bool {
        matches!(self, Table::Visual)
    }
}

/// A key the machine owes an argument to before it can resolve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Awaiting {
    /// After `f`/`F`/`t`/`T` — the character to seek.
    FindChar { forward: bool, till: bool },
    /// After `r` — the replacement character.
    ReplaceChar,
    /// After `i`/`a` — the object's key.
    Object { around: bool },
    /// After `"` — the register name.
    Register,
    /// After a built-in prefix key (`g`, `z`, `<C-w>`).
    Prefix(char),
}

#[derive(Default)]
pub struct Pending {
    /// The count typed for the motion (or for a bare action).
    pub count: Option<usize>,
    /// The count typed BEFORE an operator. vim multiplies the two.
    pub op_count: Option<usize>,
    pub operator: Option<Operator>,
    /// The key that set `operator`, so pressing it again is the doubled
    /// linewise form (`dd`, `guu`) whatever that key happens to be bound to.
    op_key: Option<Key>,
    pub register: Option<char>,
    /// Keys typed so far that have not yet resolved to a binding.
    pub partial: Vec<Key>,
    awaiting: Option<Awaiting>,
    /// The key currently being fed, so a verb can know what pressed it.
    last_key: Option<Key>,
}

/// A normalized key. Config strings like `<C-s>` parse into this.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Key {
    /// Normalize a crossterm key event into a `Key`, or `None` for events we do
    /// not bind (modifier-only presses, function keys, …). A `Shift`+letter is
    /// folded into the uppercase char, so `shift` is only set for named keys.
    pub fn from_event(ev: ratatui::crossterm::event::KeyEvent) -> Option<Key> {
        use ratatui::crossterm::event::{KeyCode as Ct, KeyModifiers};
        let code = match ev.code {
            Ct::Char(c) => KeyCode::Char(c),
            Ct::Enter => KeyCode::Enter,
            Ct::Esc => KeyCode::Esc,
            Ct::Tab => KeyCode::Tab,
            Ct::Backspace => KeyCode::Backspace,
            Ct::Delete => KeyCode::Delete,
            Ct::Up => KeyCode::Up,
            Ct::Down => KeyCode::Down,
            Ct::Left => KeyCode::Left,
            Ct::Right => KeyCode::Right,
            Ct::Home => KeyCode::Home,
            Ct::End => KeyCode::End,
            Ct::PageUp => KeyCode::PageUp,
            Ct::PageDown => KeyCode::PageDown,
            _ => return None,
        };
        let m = ev.modifiers;
        Some(Key {
            code,
            ctrl: m.contains(KeyModifiers::CONTROL),
            alt: m.contains(KeyModifiers::ALT),
            // A letter already carries its case; only flag Shift for named keys.
            shift: m.contains(KeyModifiers::SHIFT) && !matches!(code, KeyCode::Char(_)),
        })
    }

    pub const fn char(c: char) -> Key {
        Key {
            code: KeyCode::Char(c),
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    /// The character this key types, if it is a plain one.
    fn as_char(&self) -> Option<char> {
        match self.code {
            KeyCode::Char(c) if !self.ctrl && !self.alt => Some(c),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Char(char),
    Enter,
    Esc,
    Tab,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
}

pub enum Resolution {
    /// Keep collecting — the sequence is a valid prefix, or a count is growing.
    Pending,
    /// Run this, then reset.
    Command(Command),
    /// Nothing matches. Reset; the key is spent.
    Invalid,
}

impl Pending {
    pub fn reset(&mut self) {
        *self = Pending::default();
    }

    /// A clean state with no half-typed command — a command boundary, which is
    /// what dot-recording and the user keymap both key off.
    pub fn is_clean(&self) -> bool {
        self.count.is_none()
            && self.op_count.is_none()
            && self.operator.is_none()
            && self.register.is_none()
            && self.partial.is_empty()
            && self.awaiting.is_none()
    }

    /// Feed one key. This is the whole of Normal- and Visual-mode input.
    pub fn feed(&mut self, key: Key, keymap: &Keymap, table: Table) -> Resolution {
        self.last_key = Some(key);
        // 1. An owed argument beats everything: `f` takes the next key whatever
        //    it is bound to, or `fd` would delete instead of seeking a `d`.
        if let Some(awaiting) = self.awaiting.take() {
            return self.take_argument(awaiting, key, keymap, table);
        }

        // 2. The user keymap, checked before the built-ins so a binding can
        //    shadow one. It applies with an operator pending too — that is what
        //    lets a rebound MOTION complete a rebound operator, which is the
        //    whole reason the grammar moved behind the keymap.
        if let Some(res) = self.user_binding(key, keymap, table) {
            return res;
        }

        // 3. The operator's own key again is its doubled linewise form: `dd`,
        //    and equally `guu`, where the second `u` is bound to undo.
        if let (Some(op), Some(k)) = (self.operator, self.op_key) {
            if k == key {
                return self.emit(Action::Operator { op, target: Target::Line });
            }
        }

        // 4. A count prefix. A leading `0` is the LineStart motion, not a digit.
        if let Some(c @ '0'..='9') = key.as_char() {
            if c != '0' || self.count.is_some() {
                let d = c as usize - '0' as usize;
                self.count = Some(self.count.unwrap_or(0).saturating_mul(10) + d);
                return Resolution::Pending;
            }
        }

        // 5. The built-in grammar.
        let verb = match table {
            Table::Normal => bindings::normal(key),
            Table::Visual => bindings::visual(key),
        };
        match verb {
            Some(v) => self.apply_verb(v, table),
            None => {
                self.reset();
                Resolution::Invalid
            }
        }
    }

    /// Resolve a key against the user's `[keys.*]` table, collecting a
    /// multi-key sequence if one is in flight. `None` means "not ours" — a
    /// lone unbound key falls through to the built-in grammar.
    fn user_binding(&mut self, key: Key, keymap: &Keymap, table: Table) -> Option<Resolution> {
        self.partial.push(key);
        match keymap.lookup(table.name(), &self.partial) {
            Lookup::Found(verb) => {
                let verb = verb.clone();
                self.partial.clear();
                Some(self.apply_verb(verb, table))
            }
            Lookup::Prefix => Some(Resolution::Pending),
            Lookup::None => {
                // A fresh single key with no binding falls through to the
                // built-ins; a longer failed sequence is simply dropped.
                let lone = self.partial.len() == 1;
                self.partial.clear();
                if lone {
                    None
                } else {
                    self.reset();
                    Some(Resolution::Invalid)
                }
            }
        }
    }

    /// Fold one resolved verb into the pending state.
    fn apply_verb(&mut self, verb: Verb, table: Table) -> Resolution {
        match verb {
            Verb::Motion(m) => self.with_target(Target::Motion(m), table),
            Verb::Goto(fallback) => {
                // `5G` is line 5; a bare `G` is the buffer end. The count is
                // the TARGET here, so it must not also repeat the motion.
                let m = match self.count.take() {
                    Some(n) => crate::text::motion::Motion::GotoLine(n.saturating_sub(1)),
                    None => fallback,
                };
                self.with_target(Target::Motion(m), table)
            }
            Verb::Operator(op) => {
                if table.is_visual() {
                    // The selection is already the range.
                    return self.emit(Action::Operator { op, target: Target::Selection });
                }
                if self.operator == Some(op) {
                    // `dd` — the doubled form is linewise on the current lines.
                    return self.emit(Action::Operator { op, target: Target::Line });
                }
                // A different pending operator (`dc`) is invalid: start fresh.
                self.operator = Some(op);
                self.op_key = self.partial.last().copied().or(self.last_key);
                // Set the count typed so far aside for the motion's own count.
                self.op_count = self.count.take();
                Resolution::Pending
            }
            Verb::Object { around } => {
                if !table.is_visual() && self.operator.is_none() {
                    // Bare `i`/`a` in Normal mode are Insert and Append; the
                    // grammar only reads them as objects behind an operator.
                    return self.apply_verb(
                        Verb::Act(if around { Action::Append } else { Action::Insert }),
                        table,
                    );
                }
                self.awaiting = Some(Awaiting::Object { around });
                Resolution::Pending
            }
            Verb::Find { forward, till } => {
                self.awaiting = Some(Awaiting::FindChar { forward, till });
                Resolution::Pending
            }
            Verb::ReplaceChar => {
                self.awaiting = Some(Awaiting::ReplaceChar);
                Resolution::Pending
            }
            Verb::Register => {
                self.awaiting = Some(Awaiting::Register);
                Resolution::Pending
            }
            Verb::Prefix(p) => {
                self.awaiting = Some(Awaiting::Prefix(p));
                Resolution::Pending
            }
            // Esc always resolves: it is how you abandon a half-typed command.
            Verb::Act(action @ Action::NormalMode) => {
                self.reset();
                Resolution::Command(Command::bare(action))
            }
            Verb::Act(action) => {
                // Any other direct action does not combine with a pending
                // operator; a dangling one makes the key a no-op.
                if self.operator.is_some() {
                    self.reset();
                    return Resolution::Invalid;
                }
                self.emit(action)
            }
        }
    }

    /// A motion or object: it gives a pending operator its range, or moves.
    fn with_target(&mut self, target: Target, table: Table) -> Resolution {
        match self.operator.take() {
            Some(op) => self.emit(Action::Operator { op, target }),
            None => match (target, table.is_visual()) {
                // In Visual an object becomes the selection rather than a range
                // to operate on: `vi(` selects, ready for any verb.
                (Target::Object { key, around }, true) => {
                    self.emit(Action::SelectObject { key, around })
                }
                (Target::Motion(m), _) => self.emit(Action::Move(m)),
                (Target::Object { key, around }, false) => {
                    self.emit(Action::SelectObject { key, around })
                }
                _ => {
                    self.reset();
                    Resolution::Invalid
                }
            },
        }
    }

    /// The key an argument-taking verb was waiting for.
    fn take_argument(
        &mut self,
        awaiting: Awaiting,
        key: Key,
        keymap: &Keymap,
        table: Table,
    ) -> Resolution {
        match awaiting {
            Awaiting::FindChar { forward, till } => match key.as_char() {
                Some(target) => self.with_target(
                    Target::Motion(crate::text::motion::Motion::FindChar { target, forward, till }),
                    table,
                ),
                None => {
                    self.reset();
                    Resolution::Invalid
                }
            },
            Awaiting::ReplaceChar => match key.as_char() {
                Some(c) => self.emit(Action::ReplaceChar(c)),
                None => {
                    self.reset();
                    Resolution::Invalid
                }
            },
            Awaiting::Object { around } => match key.as_char() {
                Some(obj) => self.with_target(Target::Object { key: obj, around }, table),
                None => {
                    self.reset();
                    Resolution::Invalid
                }
            },
            // A register prefix qualifies the command still to come, so unlike
            // every other prefix it leaves the pending count and operator
            // alone: `"a2dd` is one command.
            Awaiting::Register => {
                if let Some(name) = key.as_char() {
                    if name.is_ascii_alphanumeric() || name == '"' {
                        self.register = Some(name);
                    }
                }
                Resolution::Pending
            }
            Awaiting::Prefix(p) => match bindings::after_prefix(p, key, table.is_visual()) {
                Some(verb) => self.apply_verb(verb, table),
                // An unknown second key drops the sequence, as vim does — but
                // the user keymap may still bind it (`z` is a config prefix).
                None => match self.user_binding(key, keymap, table) {
                    Some(res) => res,
                    None => {
                        self.reset();
                        Resolution::Invalid
                    }
                },
            },
        }
    }

    /// Package the pending state into a command and clear it.
    fn emit(&mut self, action: Action) -> Resolution {
        let count = self.take_count();
        let register = self.register;
        self.reset();
        Resolution::Command(Command::new(action, count, register))
    }

    /// Consume the pending count(s), defaulting to 1. An operator count and a
    /// motion count MULTIPLY, as in vim.
    fn take_count(&mut self) -> usize {
        let motion = self.count.take().unwrap_or(1).max(1);
        let operator = self.op_count.take().unwrap_or(1).max(1);
        motion.saturating_mul(operator)
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::KeysConfig;
    use crate::text::motion::Motion;

    fn keymap() -> Keymap {
        Keymap::from_config(&KeysConfig::default(), " ").0
    }

    /// Feed a string of plain keys, returning every command that resolved.
    fn run(p: &mut Pending, keys: &str, table: Table) -> Vec<Command> {
        let km = keymap();
        let mut out = Vec::new();
        for c in keys.chars() {
            if let Resolution::Command(cmd) = p.feed(Key::char(c), &km, table) {
                out.push(cmd);
            }
        }
        out
    }

    fn one(keys: &str) -> Command {
        let mut p = Pending::default();
        let cmds = run(&mut p, keys, Table::Normal);
        assert_eq!(cmds.len(), 1, "{keys:?} should resolve exactly once: {cmds:?}");
        cmds.into_iter().next().unwrap()
    }

    #[test]
    fn a_bare_motion_moves() {
        let cmd = one("w");
        assert_eq!(cmd.action, Action::Move(Motion::WordForward { big: false }));
        assert_eq!(cmd.count, 1);
    }

    #[test]
    fn counts_multiply_across_the_operator() {
        let cmd = one("2d3w");
        assert_eq!(
            cmd.action,
            Action::Operator {
                op: Operator::Delete,
                target: Target::Motion(Motion::WordForward { big: false }),
            }
        );
        assert_eq!(cmd.count, 6, "2 × 3");
    }

    #[test]
    fn a_doubled_operator_is_linewise() {
        let cmd = one("dd");
        assert_eq!(
            cmd.action,
            Action::Operator { op: Operator::Delete, target: Target::Line }
        );
        // `3dd` is three lines.
        assert_eq!(one("3dd").count, 3);
    }

    #[test]
    fn a_register_survives_the_count_and_operator() {
        let cmd = one("\"a2dd");
        assert_eq!(cmd.register, Some('a'));
        assert_eq!(cmd.count, 2);
        assert_eq!(
            cmd.action,
            Action::Operator { op: Operator::Delete, target: Target::Line }
        );
    }

    #[test]
    fn text_objects_need_an_operator_in_normal_mode() {
        assert_eq!(
            one("diw").action,
            Action::Operator {
                op: Operator::Delete,
                target: Target::Object { key: 'w', around: false },
            }
        );
        // Bare `i` is Insert, not an object.
        assert_eq!(one("i").action, Action::Insert);
        assert_eq!(one("a").action, Action::Append);
    }

    #[test]
    fn find_takes_the_next_key_whatever_it_is_bound_to() {
        assert_eq!(
            one("fd").action,
            Action::Move(Motion::FindChar { target: 'd', forward: true, till: false }),
            "`fd` seeks a `d`; it must not start a delete"
        );
        assert_eq!(
            one("dt,").action,
            Action::Operator {
                op: Operator::Delete,
                target: Target::Motion(Motion::FindChar {
                    target: ',',
                    forward: true,
                    till: true
                }),
            }
        );
    }

    #[test]
    fn g_is_a_prefix_for_motions_and_writer_verbs() {
        assert_eq!(one("gg").action, Action::Move(Motion::BufferStart));
        assert_eq!(one("5gg").action, Action::Move(Motion::GotoLine(4)));
        assert_eq!(one("gb").action, Action::ToggleBold);
        assert_eq!(one("g3").action, Action::SetHeading(3));
        assert_eq!(
            one("gUiw").action,
            Action::Operator {
                op: Operator::Uppercase,
                target: Target::Object { key: 'w', around: false },
            }
        );
    }

    #[test]
    fn a_count_before_goto_is_the_line_number() {
        assert_eq!(one("42G").action, Action::Move(Motion::GotoLine(41)));
        assert_eq!(one("42G").count, 1, "the count was spent on the target");
        assert_eq!(one("G").action, Action::Move(Motion::BufferEnd));
    }

    #[test]
    fn visual_operators_fire_on_the_selection() {
        let mut p = Pending::default();
        let cmds = run(&mut p, "d", Table::Visual);
        assert_eq!(
            cmds[0].action,
            Action::Operator { op: Operator::Delete, target: Target::Selection }
        );

        // `u`/`U` are the case operators in Visual, not undo.
        let mut p = Pending::default();
        assert_eq!(
            run(&mut p, "u", Table::Visual)[0].action,
            Action::Operator { op: Operator::Lowercase, target: Target::Selection }
        );

        // An object SELECTS rather than operating.
        let mut p = Pending::default();
        assert_eq!(
            run(&mut p, "i(", Table::Visual)[0].action,
            Action::SelectObject { key: '(', around: false }
        );
    }

    #[test]
    fn a_dangling_operator_swallows_a_direct_action() {
        let mut p = Pending::default();
        let cmds = run(&mut p, "dp", Table::Normal);
        assert!(cmds.is_empty(), "`dp` is not a command: {cmds:?}");
        assert!(p.is_clean(), "and it leaves nothing pending");
    }

    /// The point of the relocation: config can move an OPERATOR, and it keeps
    /// its whole grammar — counts, motions, objects, the doubled form — which
    /// the old action-only overlay could not express at all.
    #[test]
    fn config_can_rebind_the_grammar_itself() {
        let mut cfg = KeysConfig::default();
        cfg.normal.insert("s".into(), "operator_delete".into());
        cfg.normal.insert("m".into(), "word_forward".into());
        let (km, warnings) = Keymap::from_config(&cfg, " ");
        assert!(warnings.is_empty(), "{warnings:?}");

        let mut p = Pending::default();
        let fire = |p: &mut Pending, keys: &str| -> Option<Command> {
            let mut last = None;
            for c in keys.chars() {
                if let Resolution::Command(cmd) = p.feed(Key::char(c), &km, Table::Normal) {
                    last = Some(cmd);
                }
            }
            last
        };

        // The rebound operator takes a rebound motion, with counts.
        let cmd = fire(&mut p, "2s3m").expect("s + m resolves");
        assert_eq!(
            cmd.action,
            Action::Operator {
                op: Operator::Delete,
                target: Target::Motion(Motion::WordForward { big: false }),
            }
        );
        assert_eq!(cmd.count, 6);

        // …its doubled form works on the new key, not on `d`.
        let cmd = fire(&mut p, "ss").expect("ss is the linewise form");
        assert_eq!(
            cmd.action,
            Action::Operator { op: Operator::Delete, target: Target::Line }
        );

        // …and it takes text objects.
        let cmd = fire(&mut p, "siw").expect("s + object");
        assert_eq!(
            cmd.action,
            Action::Operator {
                op: Operator::Delete,
                target: Target::Object { key: 'w', around: false },
            }
        );
    }

    #[test]
    fn escape_clears_a_half_typed_command() {
        let mut p = Pending::default();
        let km = keymap();
        p.feed(Key::char('2'), &km, Table::Normal);
        p.feed(Key::char('d'), &km, Table::Normal);
        assert!(!p.is_clean());
        let esc = Key { code: KeyCode::Esc, ctrl: false, alt: false, shift: false };
        match p.feed(esc, &km, Table::Normal) {
            Resolution::Command(cmd) => assert_eq!(cmd.action, Action::NormalMode),
            _ => panic!("Esc resolves to NormalMode"),
        }
        assert!(p.is_clean());
    }
}
