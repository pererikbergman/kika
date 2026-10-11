//! Every key Kika understands, declared once. The same records drive
//! dispatch, the help menu and the bottom bar, so they cannot drift apart.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::git::diff::DiffSide;
use crate::git::patch::{FileKind, PatchError};
use crate::git::status::EntryKind;
use crate::hunk_view::NO_CHANGES;
use crate::intent::{AppIntent, Scroll};
use crate::menu::MenuId;
use crate::model::{AppState, CommitFlow, Focus};

/// A key as pressed: just the code and modifiers, without the event kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyPress {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyPress {
    pub const fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }
}

impl From<KeyEvent> for KeyPress {
    fn from(key: KeyEvent) -> Self {
        Self::new(key.code, key.modifiers)
    }
}

/// Which modifiers a pattern accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mods {
    /// Any modifiers. Terminals send uppercase letters with SHIFT, and
    /// arrow keys should not care about it either.
    Any,
    Exact(KeyModifiers),
}

/// One key a binding answers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyPattern {
    pub code: KeyCode,
    pub mods: Mods,
}

impl KeyPattern {
    /// `code` with any modifiers.
    pub const fn any(code: KeyCode) -> Self {
        Self {
            code,
            mods: Mods::Any,
        }
    }

    /// `code` with exactly `mods`.
    pub const fn exact(code: KeyCode, mods: KeyModifiers) -> Self {
        Self {
            code,
            mods: Mods::Exact(mods),
        }
    }

    pub fn matches(&self, key: KeyPress) -> bool {
        self.code == key.code
            && match self.mods {
                Mods::Any => true,
                Mods::Exact(mods) => mods == key.modifiers,
            }
    }

    /// Whether some key press matches both patterns, so one shadows the other.
    fn overlaps(&self, other: &KeyPattern) -> bool {
        self.code == other.code
            && match (self.mods, other.mods) {
                (Mods::Exact(a), Mods::Exact(b)) => a == b,
                _ => true,
            }
    }

    /// How the key is written in the bar and the help menu.
    pub fn label(&self) -> String {
        let key = match self.code {
            KeyCode::Char(' ') => "space".to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Esc => "esc".to_string(),
            // Some platforms call it Return; the bar should not depend on that.
            KeyCode::Enter => "enter".to_string(),
            KeyCode::Down => "↓".to_string(),
            KeyCode::Up => "↑".to_string(),
            KeyCode::Left => "←".to_string(),
            KeyCode::PageDown => "PgDn".to_string(),
            KeyCode::PageUp => "PgUp".to_string(),
            other => other.to_string(),
        };
        match self.mods {
            Mods::Exact(mods) if mods.contains(KeyModifiers::CONTROL) => format!("ctrl-{key}"),
            _ => key,
        }
    }
}

/// Where a binding applies. Keys are looked up in the focused scope, then
/// each parent, so a panel can override a global key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyScope {
    Global,
    Workspace,
    Files,
    /// The Diff pane with focus, working on hunks.
    Diff,
    /// The Diff pane picking changed lines inside a hunk.
    Lines,
    Review,
    Help,
    /// "Stage everything and commit?"
    CommitConfirm,
    /// Typing the commit message.
    CommitEditor,
    /// A failed commit's output.
    CommitError,
    /// "Discard the typed message?"
    CommitDiscard,
    /// A menu on top of the popup stack.
    Menu,
    /// A yes/no question on top of the popup stack.
    Confirm,
    /// A one-line text prompt on top of the popup stack.
    Prompt,
    /// An error on top of the popup stack.
    Error,
}

impl KeyScope {
    /// The commit and popup scopes have no parent, so `q`, `?` and `Esc`
    /// never quit or open help while one is open: they are typed or cancel.
    /// Those that take text or can stay open bind Ctrl+C themselves, so
    /// there is always a way out.
    pub fn parent(self) -> Option<KeyScope> {
        match self {
            KeyScope::Files | KeyScope::Diff => Some(KeyScope::Workspace),
            KeyScope::Lines => Some(KeyScope::Diff),
            KeyScope::Workspace | KeyScope::Review | KeyScope::Help => Some(KeyScope::Global),
            KeyScope::Global
            | KeyScope::CommitConfirm
            | KeyScope::CommitEditor
            | KeyScope::CommitError
            | KeyScope::CommitDiscard
            | KeyScope::Menu
            | KeyScope::Confirm
            | KeyScope::Prompt
            | KeyScope::Error => None,
        }
    }

    /// The help menu heading.
    pub fn title(self) -> &'static str {
        match self {
            KeyScope::Global => "Global",
            KeyScope::Workspace => "Workspace",
            KeyScope::Files => "Files",
            KeyScope::Diff => "Diff",
            KeyScope::Lines => "Lines",
            KeyScope::Review => "Review",
            KeyScope::Help => "Help",
            KeyScope::CommitConfirm => "Stage all and commit",
            KeyScope::CommitEditor => "Commit message",
            KeyScope::CommitError => "Commit failed",
            KeyScope::CommitDiscard => "Discard message",
            KeyScope::Menu => "Menu",
            KeyScope::Confirm => "Question",
            KeyScope::Prompt => "Prompt",
            KeyScope::Error => "Error",
        }
    }

    /// What the bottom bar says before the key hints, for scopes whose
    /// keys behave unlike everywhere else.
    pub fn bar_note(self) -> Option<&'static str> {
        match self {
            KeyScope::CommitEditor => Some("q and ? are typed"),
            _ => None,
        }
    }

    /// This scope and its parents, nearest first.
    fn chain(self) -> impl Iterator<Item = KeyScope> {
        std::iter::successors(Some(self), |scope| scope.parent())
    }
}

/// One entry in the keymap.
#[derive(Debug)]
pub struct Binding {
    pub keys: &'static [KeyPattern],
    pub scope: KeyScope,
    pub intent: AppIntent,
    /// A sentence for the help menu.
    pub description: &'static str,
    /// A word or two for the bottom bar.
    pub label: &'static str,
    /// Whether the bottom bar shows it; the help menu always does.
    pub bar: bool,
    /// Why the binding cannot run right now, if it cannot. Pure.
    pub disabled: fn(&AppState) -> Option<&'static str>,
}

impl Binding {
    fn matches(&self, key: KeyPress) -> bool {
        self.keys.iter().any(|pattern| pattern.matches(key))
    }
}

/// What a key press does from a given focus.
#[derive(Debug)]
pub enum Resolution<'a> {
    Run(&'a Binding),
    /// The key is bound but cannot run; the reason is shown to the user.
    Disabled(&'static str),
    Unbound,
}

/// Find the binding for `key`: the focused scope first, then each parent.
/// A bound but disabled key stops the search, so a panel's refusal is never
/// silently replaced by a global binding of the same key.
pub fn resolve<'a>(
    bindings: &'a [Binding],
    focus: KeyScope,
    state: &AppState,
    key: KeyPress,
) -> Resolution<'a> {
    for scope in focus.chain() {
        let found = bindings.iter().find(|b| b.scope == scope && b.matches(key));
        if let Some(binding) = found {
            return match (binding.disabled)(state) {
                Some(reason) => Resolution::Disabled(reason),
                None => Resolution::Run(binding),
            };
        }
    }
    Resolution::Unbound
}

/// The bindings reachable from `focus`, nearest scope first and in table
/// order within a scope, each with why it is disabled. A binding whose keys
/// are all taken by a nearer scope can never run, so it is left out.
pub fn available<'a>(
    bindings: &'a [Binding],
    focus: KeyScope,
    state: &AppState,
) -> Vec<(&'a Binding, Option<&'static str>)> {
    let mut claimed: Vec<KeyPattern> = Vec::new();
    let mut out = Vec::new();
    for scope in focus.chain() {
        let in_scope: Vec<&Binding> = bindings.iter().filter(|b| b.scope == scope).collect();
        for binding in &in_scope {
            let shadowed = binding
                .keys
                .iter()
                .all(|key| claimed.iter().any(|c| c.overlaps(key)));
            if !shadowed {
                out.push((*binding, (binding.disabled)(state)));
            }
        }
        claimed.extend(in_scope.iter().flat_map(|b| b.keys.iter().copied()));
    }
    out
}

/// For bindings and menu items that always run.
pub(crate) fn never(_: &AppState) -> Option<&'static str> {
    None
}

const OP_RUNNING: &str = "An operation is running";

pub(crate) fn stage_disabled(state: &AppState) -> Option<&'static str> {
    if state.op_seq.is_some() {
        return Some(OP_RUNNING);
    }
    match state.selected.and_then(|i| state.files.get(i)) {
        None => Some("No file selected"),
        Some(file) if file.kind == EntryKind::Conflicted => {
            Some("Resolve the conflict before staging this file")
        }
        Some(_) => None,
    }
}

fn no_file_selected(state: &AppState) -> Option<&'static str> {
    let selected = state.selected.and_then(|i| state.files.get(i));
    selected.is_none().then_some("No file selected")
}

/// Why the selected hunk cannot be staged or unstaged right now.
pub(crate) fn hunk_disabled(state: &AppState) -> Option<&'static str> {
    if state.op_seq.is_some() {
        return Some(OP_RUNNING);
    }
    let Focus::Diff(view) = &state.focus else {
        return Some(NO_CHANGES);
    };
    if view.loading() {
        return Some("Loading…");
    }
    let entry = state.selected.and_then(|i| state.files.get(i));
    match entry.map(|e| e.kind) {
        Some(EntryKind::Conflicted) => return Some("Resolve the conflict first"),
        Some(EntryKind::Renamed | EntryKind::Copied) => {
            return Some(PatchError::Renamed.message());
        }
        _ => {}
    }
    match &view.patch {
        Some(patch) if patch.kind == FileKind::Binary => Some(PatchError::Binary.message()),
        Some(patch) if !patch.hunks.is_empty() => None,
        _ => Some(NO_CHANGES),
    }
}

/// An untracked file has only one side to show.
fn side_disabled(state: &AppState) -> Option<&'static str> {
    match &state.focus {
        Focus::Diff(view) if view.side == DiffSide::Untracked => {
            Some("Untracked file: nothing is staged")
        }
        _ => None,
    }
}

pub(crate) fn stage_all_disabled(state: &AppState) -> Option<&'static str> {
    if state.op_seq.is_some() {
        Some(OP_RUNNING)
    } else if state.files.is_empty() {
        Some("No changes to stage")
    } else if state.files.iter().any(|f| f.kind == EntryKind::Conflicted) {
        Some(crate::git::ops::CONFLICTS_REFUSAL)
    } else {
        None
    }
}

pub(crate) fn commit_disabled(state: &AppState) -> Option<&'static str> {
    if state.op_seq.is_some() {
        Some(OP_RUNNING)
    } else if state.files.is_empty() {
        Some("Nothing to commit")
    } else if state.files.iter().any(|f| f.kind == EntryKind::Conflicted) {
        Some("Resolve conflicts before committing")
    } else {
        None
    }
}

/// The editor's keys wait while the commit runs, so the message cannot change.
fn commit_editor_disabled(state: &AppState) -> Option<&'static str> {
    let running = matches!(state.commit, CommitFlow::Running(_)) || state.op_seq.is_some();
    running.then_some(OP_RUNNING)
}

const NONE: KeyModifiers = KeyModifiers::NONE;

static REGISTRY: &[Binding] = &[
    Binding {
        keys: &[KeyPattern::any(KeyCode::Char('?'))],
        scope: KeyScope::Global,
        intent: AppIntent::OpenHelp,
        description: "Show the keys for this panel",
        label: "keys",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::any(KeyCode::Char('q')),
            KeyPattern::any(KeyCode::Esc),
            KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ],
        scope: KeyScope::Global,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('r'), NONE)],
        scope: KeyScope::Workspace,
        intent: AppIntent::RefreshStatus,
        description: "Reload the list of changed files",
        label: "refresh",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::any(KeyCode::Char('J'))],
        scope: KeyScope::Workspace,
        intent: AppIntent::ScrollDiff(Scroll::LineDown),
        description: "Scroll the diff down a line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::any(KeyCode::Char('K'))],
        scope: KeyScope::Workspace,
        intent: AppIntent::ScrollDiff(Scroll::LineUp),
        description: "Scroll the diff up a line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::any(KeyCode::PageDown)],
        scope: KeyScope::Workspace,
        intent: AppIntent::ScrollDiff(Scroll::PageDown),
        description: "Scroll the diff down a page",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::any(KeyCode::PageUp)],
        scope: KeyScope::Workspace,
        intent: AppIntent::ScrollDiff(Scroll::PageUp),
        description: "Scroll the diff up a page",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Files,
        intent: AppIntent::SelectNext,
        description: "Select the next file",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Files,
        intent: AppIntent::SelectPrev,
        description: "Select the previous file",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char(' '), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::ToggleStage,
        description: "Stage the selected file, or unstage it if fully staged",
        label: "stage",
        bar: true,
        disabled: stage_disabled,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('a'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::ToggleStageAll,
        description: "Stage everything, or unstage everything if all is staged",
        label: "all",
        bar: true,
        disabled: stage_all_disabled,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('c'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::OpenCommit,
        description: "Commit the staged changes with a message",
        label: "commit",
        bar: true,
        disabled: commit_disabled,
    },
    Binding {
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Files,
        intent: AppIntent::FocusDiff,
        description: "Work on the hunks of the selected file",
        label: "hunks",
        // Left out of the bar until the layout review (#10), like `m`.
        bar: false,
        disabled: no_file_selected,
    },
    Binding {
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Diff,
        intent: AppIntent::NextHunk,
        description: "Select the next hunk",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Diff,
        intent: AppIntent::PrevHunk,
        description: "Select the previous hunk",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char(' '), NONE)],
        scope: KeyScope::Diff,
        intent: AppIntent::ToggleHunkStage,
        description: "Stage the selected hunk, or unstage it in the staged view",
        label: "hunk",
        bar: true,
        disabled: hunk_disabled,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('v'), NONE)],
        scope: KeyScope::Diff,
        intent: AppIntent::EnterLineMode,
        description: "Pick changed lines inside the selected hunk",
        label: "lines",
        bar: true,
        disabled: hunk_disabled,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('t'), NONE)],
        scope: KeyScope::Diff,
        intent: AppIntent::ToggleDiffSide,
        description: "Switch between the unstaged and the staged changes",
        label: "staged/unstaged",
        bar: true,
        disabled: side_disabled,
    },
    Binding {
        keys: &[
            KeyPattern::any(KeyCode::Esc),
            KeyPattern::exact(KeyCode::Char('h'), NONE),
            KeyPattern::any(KeyCode::Left),
        ],
        scope: KeyScope::Diff,
        intent: AppIntent::FocusFiles,
        description: "Back to the file list",
        label: "back",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::exact(KeyCode::Down, NONE),
        ],
        scope: KeyScope::Lines,
        intent: AppIntent::NextLine { extend: false },
        description: "Move to the next changed line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::exact(KeyCode::Up, NONE),
        ],
        scope: KeyScope::Lines,
        intent: AppIntent::PrevLine { extend: false },
        description: "Move to the previous changed line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::any(KeyCode::Char('J')),
            KeyPattern::exact(KeyCode::Down, KeyModifiers::SHIFT),
        ],
        scope: KeyScope::Lines,
        intent: AppIntent::NextLine { extend: true },
        description: "Grow the selection down a changed line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::any(KeyCode::Char('K')),
            KeyPattern::exact(KeyCode::Up, KeyModifiers::SHIFT),
        ],
        scope: KeyScope::Lines,
        intent: AppIntent::PrevLine { extend: true },
        description: "Grow the selection up a changed line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char(' '), NONE)],
        scope: KeyScope::Lines,
        intent: AppIntent::ToggleLinesStage,
        description: "Stage the selected lines, or unstage them in the staged view",
        label: "lines",
        bar: true,
        disabled: hunk_disabled,
    },
    Binding {
        keys: &[
            KeyPattern::any(KeyCode::Esc),
            KeyPattern::exact(KeyCode::Char('v'), NONE),
        ],
        scope: KeyScope::Lines,
        intent: AppIntent::ExitLineMode,
        description: "Back to whole hunks",
        label: "hunks",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('m'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::OpenMenu(MenuId::Actions),
        description: "Open the actions menu",
        label: "menu",
        // Left out of the bar until the layout review (#10): the bar's
        // hints for this panel are pinned by existing tests.
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::exact(KeyCode::Char('y'), NONE),
            KeyPattern::any(KeyCode::Enter),
        ],
        scope: KeyScope::CommitConfirm,
        intent: AppIntent::ConfirmStageAll,
        description: "Stage all changes, then write the message",
        label: "stage all",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::CommitConfirm,
        intent: AppIntent::CancelCommit,
        description: "Do not commit",
        label: "cancel",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('s'), KeyModifiers::CONTROL)],
        scope: KeyScope::CommitEditor,
        intent: AppIntent::SubmitCommit,
        description: "Commit with this message",
        label: "commit",
        bar: true,
        disabled: commit_editor_disabled,
    },
    Binding {
        keys: &[KeyPattern::any(KeyCode::Tab)],
        scope: KeyScope::CommitEditor,
        intent: AppIntent::ToggleCommitField,
        description: "Switch between subject and body",
        label: "subject/body",
        bar: true,
        disabled: commit_editor_disabled,
    },
    Binding {
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::CommitEditor,
        intent: AppIntent::CancelCommit,
        description: "Close without committing",
        label: "cancel",
        bar: true,
        disabled: commit_editor_disabled,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::CommitEditor,
        intent: AppIntent::Quit,
        description: "Quit Kika, stopping a commit that is running",
        label: "quit",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::CommitDiscard,
        intent: AppIntent::DiscardCommit,
        description: "Throw the typed message away",
        label: "discard",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::CommitDiscard,
        intent: AppIntent::ResumeCommit,
        description: "Back to the message",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::any(KeyCode::Esc),
            KeyPattern::any(KeyCode::Enter),
        ],
        scope: KeyScope::CommitError,
        intent: AppIntent::DismissCommitError,
        description: "Back to the message",
        label: "back",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::any(KeyCode::Char('?')),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::Help,
        intent: AppIntent::CloseHelp,
        description: "Close this menu",
        label: "close",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Menu,
        intent: AppIntent::MenuNext,
        description: "Move down the menu",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Menu,
        intent: AppIntent::MenuPrev,
        description: "Move up the menu",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Menu,
        intent: AppIntent::MenuSelect,
        description: "Run the highlighted item",
        label: "select",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::Menu,
        intent: AppIntent::PopPopup,
        description: "Close this menu and go back to the one below",
        label: "back",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::Menu,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::exact(KeyCode::Char('y'), NONE),
            KeyPattern::any(KeyCode::Enter),
        ],
        scope: KeyScope::Confirm,
        intent: AppIntent::ConfirmYes,
        description: "Yes",
        label: "yes",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::Confirm,
        intent: AppIntent::ConfirmNo,
        description: "No",
        label: "no",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Prompt,
        intent: AppIntent::SubmitPrompt,
        description: "Use the typed text",
        label: "ok",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::Prompt,
        intent: AppIntent::PopPopup,
        description: "Close without using the text",
        label: "cancel",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::Prompt,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::any(KeyCode::Esc),
            KeyPattern::any(KeyCode::Enter),
        ],
        scope: KeyScope::Error,
        intent: AppIntent::PopPopup,
        description: "Close this message",
        label: "close",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::Error,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];

/// The application's keymap.
pub fn registry() -> &'static [Binding] {
    REGISTRY
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Context;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyPress {
        KeyPress::new(code, modifiers)
    }

    fn plain(c: char) -> KeyPress {
        key(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn off(_: &AppState) -> Option<&'static str> {
        Some("off")
    }

    const X: &[KeyPattern] = &[KeyPattern::any(KeyCode::Char('x'))];
    const Y: &[KeyPattern] = &[KeyPattern::any(KeyCode::Char('y'))];
    const Z: &[KeyPattern] = &[KeyPattern::any(KeyCode::Char('z'))];
    const W: &[KeyPattern] = &[KeyPattern::any(KeyCode::Char('w'))];
    const V: &[KeyPattern] = &[KeyPattern::any(KeyCode::Char('v'))];

    fn bind(keys: &'static [KeyPattern], scope: KeyScope, intent: AppIntent) -> Binding {
        Binding {
            keys,
            scope,
            intent,
            description: "",
            label: "",
            bar: false,
            disabled: never,
        }
    }

    /// x: Files, Workspace and Global. y: Workspace and Global. z: Global.
    /// w: Review only. v: disabled in Files, enabled in Global.
    fn table() -> Vec<Binding> {
        use AppIntent::*;
        vec![
            bind(X, KeyScope::Global, Quit),
            bind(Y, KeyScope::Global, Quit),
            bind(Z, KeyScope::Global, Quit),
            bind(V, KeyScope::Global, Quit),
            bind(X, KeyScope::Workspace, RefreshStatus),
            bind(Y, KeyScope::Workspace, RefreshStatus),
            bind(X, KeyScope::Files, SelectNext),
            Binding {
                disabled: off,
                ..bind(V, KeyScope::Files, SelectPrev)
            },
            bind(W, KeyScope::Review, ToggleStage),
        ]
    }

    fn run(bindings: &[Binding], focus: KeyScope, state: &AppState, key: KeyPress) -> AppIntent {
        match resolve(bindings, focus, state, key) {
            Resolution::Run(binding) => binding.intent.clone(),
            other => panic!("{key:?} from {focus:?}: expected Run, got {other:?}"),
        }
    }

    #[test]
    fn the_nearest_scope_wins() {
        let table = table();
        let state = AppState::new(Context::Workspace);
        let files = KeyScope::Files;
        assert_eq!(
            run(&table, files, &state, plain('x')),
            AppIntent::SelectNext
        );
        assert_eq!(
            run(&table, files, &state, plain('y')),
            AppIntent::RefreshStatus
        );
        assert_eq!(run(&table, files, &state, plain('z')), AppIntent::Quit);
    }

    #[test]
    fn keys_of_unrelated_scopes_are_unbound() {
        let table = table();
        let state = AppState::new(Context::Workspace);
        assert!(matches!(
            resolve(&table, KeyScope::Files, &state, plain('w')),
            Resolution::Unbound
        ));
        assert_eq!(
            run(&table, KeyScope::Review, &state, plain('w')),
            AppIntent::ToggleStage
        );
    }

    #[test]
    fn a_disabled_binding_does_not_fall_through_to_its_parent() {
        let table = table();
        let state = AppState::new(Context::Workspace);
        assert!(matches!(
            resolve(&table, KeyScope::Files, &state, plain('v')),
            Resolution::Disabled("off")
        ));
        assert_eq!(
            run(&table, KeyScope::Workspace, &state, plain('v')),
            AppIntent::Quit
        );
    }

    #[test]
    fn available_lists_nearest_first_and_omits_fully_shadowed_bindings() {
        let table = table();
        let state = AppState::new(Context::Workspace);
        let listed: Vec<(KeyScope, char, Option<&str>)> =
            available(&table, KeyScope::Files, &state)
                .into_iter()
                .map(|(b, reason)| {
                    let KeyCode::Char(c) = b.keys[0].code else {
                        unreachable!()
                    };
                    (b.scope, c, reason)
                })
                .collect();
        assert_eq!(
            listed,
            [
                (KeyScope::Files, 'x', None),
                (KeyScope::Files, 'v', Some("off")),
                (KeyScope::Workspace, 'y', None),
                (KeyScope::Global, 'z', None),
            ]
        );
    }

    #[test]
    fn a_partly_shadowed_binding_stays_available() {
        let state = AppState::new(Context::Workspace);
        let help = available(registry(), KeyScope::Help, &state);
        let intents: Vec<&AppIntent> = help.iter().map(|(b, _)| &b.intent).collect();
        // Esc is taken by CloseHelp, but q still quits; `?` to open is gone.
        assert_eq!(intents, [&AppIntent::CloseHelp, &AppIntent::Quit]);
    }

    fn from(focus: KeyScope, key: KeyPress) -> Option<AppIntent> {
        // Every binding is enabled on a workspace with a stageable file selected.
        let mut state = AppState::new(Context::Workspace);
        state.files = vec![crate::git::FileEntry {
            path: "a.rs".into(),
            orig_path: None,
            index: '.',
            worktree: 'M',
            kind: EntryKind::Changed,
        }];
        state.selected = Some(0);
        match resolve(registry(), focus, &state, key) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            Resolution::Disabled(reason) => panic!("{key:?} disabled: {reason}"),
            Resolution::Unbound => None,
        }
    }

    #[test]
    fn existing_keys_keep_their_meaning_from_the_files_panel() {
        use AppIntent::*;
        let files = |k| from(KeyScope::Files, k);
        let none = KeyModifiers::NONE;
        let shift = KeyModifiers::SHIFT;
        assert_eq!(files(plain('q')), Some(Quit));
        assert_eq!(files(key(KeyCode::Esc, none)), Some(Quit));
        assert_eq!(
            files(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Quit)
        );
        assert_eq!(files(plain('r')), Some(RefreshStatus));
        assert_eq!(files(plain('j')), Some(SelectNext));
        assert_eq!(files(key(KeyCode::Down, none)), Some(SelectNext));
        assert_eq!(files(plain('k')), Some(SelectPrev));
        assert_eq!(files(key(KeyCode::Up, none)), Some(SelectPrev));
        for mods in [none, shift] {
            assert_eq!(
                files(key(KeyCode::Char('J'), mods)),
                Some(ScrollDiff(Scroll::LineDown))
            );
            assert_eq!(
                files(key(KeyCode::Char('K'), mods)),
                Some(ScrollDiff(Scroll::LineUp))
            );
        }
        assert_eq!(
            files(key(KeyCode::PageDown, none)),
            Some(ScrollDiff(Scroll::PageDown))
        );
        assert_eq!(
            files(key(KeyCode::PageUp, none)),
            Some(ScrollDiff(Scroll::PageUp))
        );
        assert_eq!(files(plain(' ')), Some(ToggleStage));
        assert_eq!(files(plain('a')), Some(ToggleStageAll));
        assert_eq!(files(plain('c')), Some(OpenCommit));
        assert_eq!(files(key(KeyCode::Char('a'), KeyModifiers::CONTROL)), None);
        assert_eq!(files(plain('x')), None);
    }

    #[test]
    fn the_diff_pane_has_its_own_keys_and_esc_goes_back() {
        use AppIntent::*;
        let none = KeyModifiers::NONE;
        assert_eq!(
            from(KeyScope::Files, key(KeyCode::Enter, none)),
            Some(FocusDiff)
        );
        let diff = |k| from(KeyScope::Diff, k);
        assert_eq!(diff(plain('j')), Some(NextHunk));
        assert_eq!(diff(key(KeyCode::Down, none)), Some(NextHunk));
        assert_eq!(diff(plain('k')), Some(PrevHunk));
        assert_eq!(diff(key(KeyCode::Up, none)), Some(PrevHunk));
        assert_eq!(diff(plain('t')), Some(ToggleDiffSide));
        for k in [
            key(KeyCode::Esc, none),
            plain('h'),
            key(KeyCode::Left, none),
        ] {
            assert_eq!(diff(k), Some(FocusFiles), "{k:?}");
        }
        // Workspace and global keys still reach through.
        assert_eq!(diff(plain('r')), Some(RefreshStatus));
        assert_eq!(diff(plain('J')), Some(ScrollDiff(Scroll::LineDown)));
        assert_eq!(diff(plain('q')), Some(Quit));
        // File keys do not.
        assert_eq!(diff(plain('a')), None);
        assert_eq!(diff(plain('c')), None);
    }

    #[test]
    fn line_mode_has_its_own_keys_and_falls_through_to_the_diff() {
        use AppIntent::*;
        let none = KeyModifiers::NONE;
        let shift = KeyModifiers::SHIFT;
        assert_eq!(KeyScope::Lines.parent(), Some(KeyScope::Diff));
        assert_eq!(KeyScope::Lines.title(), "Lines");
        let lines = |k| from(KeyScope::Lines, k);
        let next = NextLine { extend: false };
        let prev = PrevLine { extend: false };
        assert_eq!(lines(plain('j')), Some(next.clone()));
        assert_eq!(lines(key(KeyCode::Down, none)), Some(next));
        assert_eq!(lines(plain('k')), Some(prev.clone()));
        assert_eq!(lines(key(KeyCode::Up, none)), Some(prev));
        for mods in [none, shift] {
            assert_eq!(
                lines(key(KeyCode::Char('J'), mods)),
                Some(NextLine { extend: true })
            );
            assert_eq!(
                lines(key(KeyCode::Char('K'), mods)),
                Some(PrevLine { extend: true })
            );
        }
        assert_eq!(
            lines(key(KeyCode::Down, shift)),
            Some(NextLine { extend: true })
        );
        assert_eq!(
            lines(key(KeyCode::Up, shift)),
            Some(PrevLine { extend: true })
        );
        assert_eq!(lines(key(KeyCode::Esc, none)), Some(ExitLineMode));
        assert_eq!(lines(plain('v')), Some(ExitLineMode));
        // Other Diff keys, and workspace and global ones, reach through.
        assert_eq!(lines(plain('t')), Some(ToggleDiffSide));
        assert_eq!(lines(plain('h')), Some(FocusFiles));
        assert_eq!(lines(plain('r')), Some(RefreshStatus));
        assert_eq!(lines(plain('q')), Some(Quit));

        // Space and `v` share the hunk key's refusals; with no hunks here,
        // that is the same reason.
        let state = AppState::new(Context::Workspace);
        for (scope, k) in [(KeyScope::Lines, plain(' ')), (KeyScope::Diff, plain('v'))] {
            assert!(matches!(
                resolve(registry(), scope, &state, k),
                Resolution::Disabled(NO_CHANGES)
            ));
        }
        let bound = |scope, k| {
            registry()
                .iter()
                .find(|b| b.scope == scope && b.matches(k))
                .map(|b| b.intent.clone())
        };
        assert_eq!(bound(KeyScope::Lines, plain(' ')), Some(ToggleLinesStage));
        assert_eq!(bound(KeyScope::Diff, plain('v')), Some(EnterLineMode));
    }

    #[test]
    fn review_has_no_workspace_keys_but_still_quits() {
        let review = |k| from(KeyScope::Review, k);
        assert_eq!(review(plain('r')), None);
        assert_eq!(review(plain(' ')), None);
        assert_eq!(review(plain('a')), None);
        assert_eq!(review(plain('q')), Some(AppIntent::Quit));
    }

    #[test]
    fn help_closes_on_question_mark_and_esc_but_q_still_quits() {
        let help = |k| from(KeyScope::Help, k);
        assert_eq!(help(plain('?')), Some(AppIntent::CloseHelp));
        assert_eq!(
            help(key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(AppIntent::CloseHelp)
        );
        assert_eq!(help(plain('q')), Some(AppIntent::Quit));
        assert_eq!(from(KeyScope::Files, plain('?')), Some(AppIntent::OpenHelp));
    }

    #[test]
    fn c_is_disabled_while_an_op_runs_without_files_or_with_conflicts_in_that_order() {
        let c = |state: &AppState| match resolve(registry(), KeyScope::Files, state, plain('c')) {
            Resolution::Run(binding) => Ok(binding.intent.clone()),
            Resolution::Disabled(reason) => Err(reason),
            Resolution::Unbound => panic!("c is unbound"),
        };
        let mut state = AppState::new(Context::Workspace);
        state.op_seq = Some(1);
        assert_eq!(c(&state), Err("An operation is running"));
        state.op_seq = None;
        assert_eq!(c(&state), Err("Nothing to commit"));
        let file = |kind| crate::git::FileEntry {
            path: "a.rs".into(),
            orig_path: None,
            index: 'U',
            worktree: 'U',
            kind,
        };
        state.files = vec![file(EntryKind::Changed), file(EntryKind::Conflicted)];
        assert_eq!(c(&state), Err("Resolve conflicts before committing"));
        state.op_seq = Some(1);
        assert_eq!(c(&state), Err("An operation is running"));
        state.op_seq = None;
        state.files.pop();
        assert_eq!(c(&state), Ok(AppIntent::OpenCommit));
    }

    #[test]
    fn commit_popups_have_their_own_keys_and_never_reach_global_ones() {
        use AppIntent::*;
        let none = KeyModifiers::NONE;
        let esc = key(KeyCode::Esc, none);
        let enter = key(KeyCode::Enter, none);
        let ctrl = |c| key(KeyCode::Char(c), KeyModifiers::CONTROL);

        let confirm = |k| from(KeyScope::CommitConfirm, k);
        assert_eq!(confirm(plain('y')), Some(ConfirmStageAll));
        assert_eq!(confirm(enter), Some(ConfirmStageAll));
        assert_eq!(confirm(plain('n')), Some(CancelCommit));
        assert_eq!(confirm(esc), Some(CancelCommit));

        let editor = |k| from(KeyScope::CommitEditor, k);
        assert_eq!(editor(ctrl('s')), Some(SubmitCommit));
        assert_eq!(editor(esc), Some(CancelCommit));
        assert_eq!(editor(key(KeyCode::Tab, none)), Some(ToggleCommitField));
        assert_eq!(editor(enter), None, "Enter is typed into the message");

        let error = |k| from(KeyScope::CommitError, k);
        assert_eq!(error(esc), Some(DismissCommitError));
        assert_eq!(error(enter), Some(DismissCommitError));

        assert_eq!(editor(ctrl('c')), Some(Quit));

        let discard = |k| from(KeyScope::CommitDiscard, k);
        assert_eq!(discard(plain('y')), Some(DiscardCommit));
        assert_eq!(discard(plain('n')), Some(ResumeCommit));
        assert_eq!(discard(esc), Some(ResumeCommit));
        assert_eq!(discard(enter), None, "Enter never throws a message away");

        for scope in [
            KeyScope::CommitConfirm,
            KeyScope::CommitEditor,
            KeyScope::CommitError,
            KeyScope::CommitDiscard,
        ] {
            for k in [plain('q'), plain('?')] {
                assert_eq!(from(scope, k), None, "{scope:?} {k:?}");
            }
        }
    }

    #[test]
    fn popups_have_their_own_keys_and_never_reach_global_ones() {
        use AppIntent::*;
        let none = KeyModifiers::NONE;
        let esc = key(KeyCode::Esc, none);
        let enter = key(KeyCode::Enter, none);
        let ctrl_c = key(KeyCode::Char('c'), KeyModifiers::CONTROL);

        assert_eq!(
            from(KeyScope::Files, plain('m')),
            Some(OpenMenu(MenuId::Actions))
        );

        let menu = |k| from(KeyScope::Menu, k);
        assert_eq!(menu(plain('j')), Some(MenuNext));
        assert_eq!(menu(key(KeyCode::Down, none)), Some(MenuNext));
        assert_eq!(menu(plain('k')), Some(MenuPrev));
        assert_eq!(menu(key(KeyCode::Up, none)), Some(MenuPrev));
        assert_eq!(menu(enter), Some(MenuSelect));
        assert_eq!(menu(esc), Some(PopPopup));
        assert_eq!(menu(ctrl_c), Some(Quit));

        let confirm = |k| from(KeyScope::Confirm, k);
        assert_eq!(confirm(plain('y')), Some(ConfirmYes));
        assert_eq!(confirm(enter), Some(ConfirmYes));
        assert_eq!(confirm(plain('n')), Some(ConfirmNo));
        assert_eq!(confirm(esc), Some(ConfirmNo));

        let prompt = |k| from(KeyScope::Prompt, k);
        assert_eq!(prompt(enter), Some(SubmitPrompt));
        assert_eq!(prompt(esc), Some(PopPopup));
        assert_eq!(prompt(ctrl_c), Some(Quit));
        assert_eq!(prompt(plain('j')), None, "j is typed");

        let error = |k| from(KeyScope::Error, k);
        assert_eq!(error(esc), Some(PopPopup));
        assert_eq!(error(enter), Some(PopPopup));
        assert_eq!(error(ctrl_c), Some(Quit));

        for scope in [
            KeyScope::Menu,
            KeyScope::Confirm,
            KeyScope::Prompt,
            KeyScope::Error,
        ] {
            for k in [plain('q'), plain('?'), plain('r'), plain(' ')] {
                assert_eq!(from(scope, k), None, "{scope:?} {k:?}");
            }
        }
    }

    #[test]
    fn the_editor_keys_wait_while_the_commit_runs() {
        let mut state = AppState::new(Context::Workspace);
        state.commit = CommitFlow::Running(crate::commit_editor::CommitEditor::new(false));
        state.op_seq = Some(1);
        for k in [
            key(KeyCode::Char('s'), KeyModifiers::CONTROL),
            key(KeyCode::Esc, KeyModifiers::NONE),
            key(KeyCode::Tab, KeyModifiers::NONE),
        ] {
            assert!(matches!(
                resolve(registry(), KeyScope::CommitEditor, &state, k),
                Resolution::Disabled("An operation is running")
            ));
        }
        // Ctrl+C is the way out while git runs.
        let ctrl_c = key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(matches!(
            resolve(registry(), KeyScope::CommitEditor, &state, ctrl_c),
            Resolution::Run(Binding {
                intent: AppIntent::Quit,
                ..
            })
        ));
    }

    #[test]
    fn no_scope_binds_the_same_key_twice() {
        let patterns: Vec<(KeyScope, &KeyPattern)> = registry()
            .iter()
            .flat_map(|b| b.keys.iter().map(move |k| (b.scope, k)))
            .collect();
        for (i, (scope, a)) in patterns.iter().enumerate() {
            for (other, b) in &patterns[i + 1..] {
                assert!(
                    scope != other || !a.overlaps(b),
                    "{scope:?} binds {a:?} and {b:?}"
                );
            }
        }
    }

    #[test]
    fn labels_are_short_and_readable() {
        let label = |p: KeyPattern| p.label();
        assert_eq!(label(KeyPattern::any(KeyCode::Char(' '))), "space");
        assert_eq!(label(KeyPattern::any(KeyCode::Esc)), "esc");
        assert_eq!(label(KeyPattern::any(KeyCode::Enter)), "enter");
        assert_eq!(
            label(KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            "ctrl-c"
        );
        assert_eq!(label(KeyPattern::any(KeyCode::Down)), "↓");
        assert_eq!(label(KeyPattern::any(KeyCode::Up)), "↑");
        assert_eq!(label(KeyPattern::any(KeyCode::PageDown)), "PgDn");
        assert_eq!(label(KeyPattern::any(KeyCode::PageUp)), "PgUp");
        assert_eq!(label(KeyPattern::any(KeyCode::Char('J'))), "J");
    }
}
