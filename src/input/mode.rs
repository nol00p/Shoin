//! Editor modes. SPEC.md §7.1.

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    Visual,
    VisualLine,
    /// The `:` line. Holds the text typed so far.
    Command(String),
    /// `/` or `?`. Holds the query typed so far.
    Search { query: String, reverse: bool },
    /// A one-line question a PANEL asked — today only the file tree's
    /// create/rename/move/delete. It is a mode rather than a flag on the panel
    /// because it has to swallow every key exactly as `:` does, and the answer
    /// belongs to the panel, not the buffer.
    Prompt(Prompt),
}

/// A pending panel question: what is being asked, of which path, and what has
/// been typed so far.
///
/// `target` is captured when the prompt OPENS. Reading it back off the tree at
/// answer time would be a bug waiting to happen — a refresh between the two can
/// move the selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub kind: PromptKind,
    pub target: PathBuf,
    pub input: String,
}

/// Named without a `Tree` prefix because the enum already says what it is —
/// `PromptKind::Delete`. Every variant belongs to the file tree today.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptKind {
    /// `a` — a name to create in `target` (a directory). A trailing `/` makes a
    /// directory instead of a file.
    Create,
    /// `r` — a new name for `target`, in the same directory.
    Rename,
    /// `m` — a new path for `target`, relative to the tree root.
    Move,
    /// `d` — a yes/no confirmation. `input` is unused; the count of entries the
    /// deletion would take is carried in the label.
    Delete { entries: usize },
    /// `:export` — confirm where the finished document goes. `input` opens
    /// pre-filled with a suggestion, and `target` is the document being
    /// exported, kept so the write can refuse to land on it.
    Export { format: crate::export::Format },
}

impl PromptKind {
    /// The floating box's title — what kind of question this is.
    pub fn title(&self) -> &'static str {
        match self {
            PromptKind::Create => " new ",
            PromptKind::Rename => " rename ",
            PromptKind::Move => " move ",
            PromptKind::Delete { .. } => " delete ",
            PromptKind::Export { .. } => " export ",
        }
    }

    /// Whether the box accepts typing. A confirmation does not.
    pub fn is_confirm(&self) -> bool {
        matches!(self, PromptKind::Delete { .. })
    }
}

impl Mode {
    /// In Insert the cursor may sit one past the last character.
    pub fn allows_past_end(&self) -> bool {
        matches!(self, Mode::Insert)
    }
}
