//! `Action` is the single vocabulary of the editor. SPEC.md §7.2.
//!
//! Config strings map to these names. Everything that mutates the buffer is an
//! `Action`, dispatched from one place — which is what makes undo grouping,
//! dirty tracking, and (post-v1) macro recording tractable.

use crate::text::motion::Motion;

/// What an operator applies to. An object and a motion are two ways of naming
/// a range, so they reach the same `operate_*` helpers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Motion(Motion),
    /// `iw`, `a(` — the object's key and whether it takes its delimiters.
    Object { key: char, around: bool },
    /// The doubled form: `dd`, `yy`, `>>`.
    Line,
    /// Visual mode — whatever is selected.
    Selection,
}

/// Where a panel starts looking. Deliberately NOT a project root: Shoin edits
/// prose, and a directory of notes is not a checkout — `.git` has no more to
/// say about where a text begins than any other dotfile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Root {
    /// The edited file's own directory — the near view.
    File,
    /// `$HOME` — everything you might have written.
    Home,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    // --- movement ---
    Move(Motion),
    /// `H`/`M`/`L` and friends resolve against the viewport, which only the app
    /// can see; the grammar emits the motion and `app` finishes it.
    SelectObject { key: char, around: bool },
    SwapSelectionEnds,

    // --- mode changes ---
    Insert,
    Append,
    InsertLineStart,
    AppendLineEnd,
    OpenBelow,
    OpenAbove,
    Visual,
    VisualLine,
    NormalMode,
    Command,
    SearchForward,
    SearchBackward,

    // --- operators (applied to a motion, an object, a line, or a selection) ---
    Operator { op: Operator, target: Target },

    // --- direct edits ---
    DeleteChar,
    DeleteCharBack,
    /// `D` / `C` — to the end of the line.
    DeleteToEol,
    ChangeToEol,
    /// `Y` — the whole line, without waiting for a motion.
    YankLine,
    /// `n` / `N`.
    SearchNext { reverse: bool },
    /// `;` / `,`.
    RepeatFind { reverse: bool },
    /// `*`.
    SearchWordUnderCursor,
    /// `gp` — a new paragraph below, in Insert.
    AppendParagraph,
    /// `<C-w>` window commands, by their second key.
    Window(char),
    /// `<leader>sv`/`<leader>ss` — split the focused pane, or close it if the
    /// window is already split (toggle symmetry, docs/history/IDEAS.md).
    ToggleSplit { vertical: bool },
    ClosePane,
    OnlyPane,
    PasteAfter,
    PasteBefore,
    ReplaceChar(char),
    JoinLines,
    ToggleCase,
    Undo,
    Redo,
    Repeat,

    // --- writer verbs (SPEC.md §7.3) ---
    ToggleBold,
    ToggleItalic,
    ToggleHighlight,
    ToggleCode,
    InsertLink,
    ToggleTask,
    SetHeading(u8),
    ClearHeading,

    // --- app ---
    Save,
    SaveStayInsert,
    Quit { force: bool },
    WriteQuit,
    CycleFocus,
    ToggleTypewriter,
    ToggleZen,
    /// Flip `layout.conceal` — live preview vs. every line raw.
    ToggleConceal,
    /// Toggle the file-tree pane, rooted at the edited file's own folder or at
    /// `$HOME`. Inside the tree, `-` and `+` move the root from there.
    FileTree { root: Root },
    /// Toggle the fuzzy file finder overlay, rooted at the edited file's own
    /// directory or at `$HOME`.
    FindFile { root: Root },
    /// The same overlay over the open buffers.
    FindBuffer,

    /// Recognized key, deliberately does nothing.
    Nop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operator {
    Delete,
    Change,
    Yank,
    Indent,
    Outdent,
    Lowercase,
    Uppercase,
}

impl Action {
    /// Parse the action name used in `shoin.conf` `[keys.*]` tables.
    pub fn from_config_name(name: &str) -> Option<Action> {
        Some(match name {
            "" | "nop" | "none" => Action::Nop,
            "save" | "write" => Action::Save,
            "save_stay_insert" => Action::SaveStayInsert,
            "save_quit" | "write_quit" | "wq" => Action::WriteQuit,
            "quit" => Action::Quit { force: false },
            "quit!" | "force_quit" => Action::Quit { force: true },
            "insert" => Action::Insert,
            "append" => Action::Append,
            "insert_line_start" => Action::InsertLineStart,
            "append_line_end" => Action::AppendLineEnd,
            "open_below" => Action::OpenBelow,
            "open_above" => Action::OpenAbove,
            "normal_mode" | "escape" => Action::NormalMode,
            "visual" => Action::Visual,
            "visual_line" => Action::VisualLine,
            "command" => Action::Command,
            "search_forward" | "search" => Action::SearchForward,
            "search_backward" => Action::SearchBackward,
            "undo" => Action::Undo,
            "redo" => Action::Redo,
            "repeat" => Action::Repeat,
            "paste_after" | "paste" => Action::PasteAfter,
            "paste_before" => Action::PasteBefore,
            "delete_char" => Action::DeleteChar,
            "join_lines" => Action::JoinLines,
            "toggle_bold" | "bold" => Action::ToggleBold,
            "toggle_italic" | "italic" => Action::ToggleItalic,
            "toggle_highlight" | "highlight" => Action::ToggleHighlight,
            "toggle_code" | "code" => Action::ToggleCode,
            "insert_link" | "link" => Action::InsertLink,
            "toggle_task" | "task" => Action::ToggleTask,
            "clear_heading" => Action::ClearHeading,
            "cycle_focus" | "focus" => Action::CycleFocus,
            "toggle_typewriter" | "typewriter" => Action::ToggleTypewriter,
            "toggle_conceal" | "conceal" => Action::ToggleConceal,
            "toggle_zen" | "zen" => Action::ToggleZen,
            "file_tree" | "file_explorer" | "tree" => Action::FileTree { root: Root::File },
            "file_tree_home" | "file_explorer_home" => Action::FileTree { root: Root::Home },
            "find_file" | "fuzzy_find" | "files" => Action::FindFile { root: Root::File },
            "find_file_home" | "fuzzy_find_home" => Action::FindFile { root: Root::Home },
            "find_buffer" | "buffers" | "switch_buffer" => Action::FindBuffer,
            "split_vertical" | "vsplit" => Action::ToggleSplit { vertical: true },
            "split_horizontal" | "split" => Action::ToggleSplit { vertical: false },
            "close_pane" => Action::ClosePane,
            "only_pane" | "only" => Action::OnlyPane,
            _ => {
                if let Some(n) = name.strip_prefix("heading_") {
                    return n.parse::<u8>().ok().filter(|n| (1..=6).contains(n)).map(Action::SetHeading);
                }
                return None;
            }
        })
    }
}
