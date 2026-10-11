//! Every key Kika understands, declared once. The same records drive
//! dispatch, the help menu and the bottom bar, so they cannot drift apart.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::git::diff::{DiffSide, MAX_CONTEXT, MIN_CONTEXT};
use crate::git::patch::{FileKind, PatchError};
use crate::git::status::{EntryKind, Head};
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
    /// A blame of the selected file, with a cursor on its lines.
    Blame,
    /// The commit opened from a blame line.
    BlameCommit,
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
    /// Reading the last commit's message to amend it.
    CommitLoading,
    /// A menu on top of the popup stack.
    Menu,
    /// A yes/no question on top of the popup stack.
    Confirm,
    /// A one-line text prompt on top of the popup stack.
    Prompt,
    /// An error on top of the popup stack.
    Error,
    /// "Discard this work?" on top of the popup stack.
    DiscardConfirm,
}

impl KeyScope {
    /// The commit and popup scopes have no parent, so `q`, `?` and `Esc`
    /// never quit or open help while one is open: they are typed or cancel.
    /// Those that take text or can stay open bind Ctrl+C themselves, so
    /// there is always a way out.
    pub fn parent(self) -> Option<KeyScope> {
        match self {
            KeyScope::Files | KeyScope::Diff | KeyScope::Blame | KeyScope::BlameCommit => {
                Some(KeyScope::Workspace)
            }
            KeyScope::Lines => Some(KeyScope::Diff),
            KeyScope::Workspace | KeyScope::Review | KeyScope::Help => Some(KeyScope::Global),
            KeyScope::Global
            | KeyScope::CommitConfirm
            | KeyScope::CommitEditor
            | KeyScope::CommitError
            | KeyScope::CommitDiscard
            | KeyScope::CommitLoading
            | KeyScope::Menu
            | KeyScope::Confirm
            | KeyScope::Prompt
            | KeyScope::Error
            | KeyScope::DiscardConfirm => None,
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
            KeyScope::Blame => "Blame",
            KeyScope::BlameCommit => "Commit",
            KeyScope::Review => "Review",
            KeyScope::Help => "Help",
            KeyScope::CommitConfirm => "Stage all and commit",
            KeyScope::CommitEditor => "Commit message",
            KeyScope::CommitError => "Commit failed",
            KeyScope::CommitDiscard => "Discard message",
            KeyScope::CommitLoading => "Loading the last commit",
            KeyScope::Menu => "Menu",
            KeyScope::Confirm => "Question",
            KeyScope::Prompt => "Prompt",
            KeyScope::Error => "Error",
            KeyScope::DiscardConfirm => "Discard",
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

/// A diff made with `-w` can differ from the file, so a patch built from it
/// can fail to apply or stage the wrong lines.
pub(crate) const WHITESPACE_HIDDEN: &str = "Whitespace is hidden; press w to stage";

/// The Diff pane shows a `-w` diff, which hunk and line keys cannot apply.
fn whitespace_hidden(state: &AppState) -> Option<&'static str> {
    let in_diff = matches!(state.focus, Focus::Diff(_));
    (in_diff && state.diff_options.ignore_whitespace).then_some(WHITESPACE_HIDDEN)
}

/// The diff keys of the Workspace scope reach the blame through its
/// parent, but the diff they would change is hidden behind it.
const BLAME_OPEN: &str = "Close the blame first";

fn blame_open(state: &AppState) -> Option<&'static str> {
    matches!(state.focus, Focus::Blame(_)).then_some(BLAME_OPEN)
}

fn most_context(state: &AppState) -> Option<&'static str> {
    blame_open(state).or_else(|| {
        (state.diff_options.context >= MAX_CONTEXT).then_some("Already at the most context")
    })
}

fn least_context(state: &AppState) -> Option<&'static str> {
    blame_open(state).or_else(|| {
        (state.diff_options.context <= MIN_CONTEXT).then_some("Already at the least context")
    })
}

/// Why `b` cannot blame the selected file. An untracked or deleted file
/// has no lines git can blame.
pub(crate) fn blame_disabled(state: &AppState) -> Option<&'static str> {
    match state.selected.and_then(|i| state.files.get(i)) {
        None => Some("No file selected"),
        Some(file) if file.kind == EntryKind::Untracked => Some("Untracked file: nothing to blame"),
        Some(file) if file.worktree == 'D' || file.index == 'D' => {
            Some("The file is deleted: nothing to blame")
        }
        Some(_) => None,
    }
}

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
    if let Some(reason) = whitespace_hidden(state) {
        return Some(reason);
    }
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

/// Why the last commit cannot be amended right now. A clean tree is
/// fine: amending then only changes the message.
pub(crate) fn amend_disabled(state: &AppState) -> Option<&'static str> {
    let no_commit = match &state.branch {
        None => true,
        Some(branch) => matches!(branch.head, Head::Unborn(_)),
    };
    if state.op_seq.is_some() {
        Some(OP_RUNNING)
    } else if no_commit {
        Some("No commit to amend")
    } else if state.files.iter().any(|f| f.kind == EntryKind::Conflicted) {
        Some("Resolve conflicts before amending")
    } else {
        None
    }
}

/// Why `d` cannot ask right now. What it would discard, and why that is
/// refused, is the model's call once it runs.
pub(crate) fn discard_disabled(state: &AppState) -> Option<&'static str> {
    if state.op_seq.is_some() {
        return Some(OP_RUNNING);
    }
    if let Some(reason) = whitespace_hidden(state) {
        return Some(reason);
    }
    match &state.focus {
        Focus::Diff(view) if view.loading() => Some("Loading…"),
        _ => no_file_selected(state),
    }
}

/// Why `o` cannot open the editor right now.
fn open_disabled(state: &AppState) -> Option<&'static str> {
    if state.op_seq.is_some() {
        return Some(OP_RUNNING);
    }
    no_file_selected(state)
}

/// Why `e` cannot edit the selected hunk right now. What kind of change
/// can be edited is the model's call once it runs.
fn edit_hunk_disabled(state: &AppState) -> Option<&'static str> {
    if state.op_seq.is_some() {
        return Some(OP_RUNNING);
    }
    let Focus::Diff(view) = &state.focus else {
        return Some(NO_CHANGES);
    };
    if let Some(reason) = whitespace_hidden(state) {
        return Some(reason);
    }
    if view.lines.is_some() {
        return Some("Press Esc to edit the whole hunk");
    }
    if view.loading() {
        return Some("Loading…");
    }
    match &view.patch {
        Some(patch) if !patch.hunks.is_empty() => None,
        _ => Some(NO_CHANGES),
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
        disabled: blame_open,
    },
    Binding {
        keys: &[KeyPattern::any(KeyCode::Char('K'))],
        scope: KeyScope::Workspace,
        intent: AppIntent::ScrollDiff(Scroll::LineUp),
        description: "Scroll the diff up a line",
        label: "",
        bar: false,
        disabled: blame_open,
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
    // Diff settings, in Workspace so they work from Files, Diff and Lines.
    // Left out of the bar until the layout review (#10).
    Binding {
        keys: &[
            KeyPattern::any(KeyCode::Char('+')),
            KeyPattern::exact(KeyCode::Char('='), NONE),
        ],
        scope: KeyScope::Workspace,
        intent: AppIntent::MoreContext,
        description: "Show more context lines in the diff",
        label: "more context",
        bar: false,
        disabled: most_context,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('-'), NONE)],
        scope: KeyScope::Workspace,
        intent: AppIntent::LessContext,
        description: "Show fewer context lines in the diff",
        label: "less context",
        bar: false,
        disabled: least_context,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('w'), NONE)],
        scope: KeyScope::Workspace,
        intent: AppIntent::ToggleWhitespace,
        description: "Hide or show whitespace-only changes",
        label: "whitespace",
        bar: false,
        disabled: blame_open,
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
        keys: &[KeyPattern::any(KeyCode::Char('A'))],
        scope: KeyScope::Files,
        intent: AppIntent::OpenAmend,
        description: "Amend the last commit with the staged changes, or reword it",
        label: "amend",
        // Left out of the bar until the layout review (#10).
        bar: false,
        disabled: amend_disabled,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::Discard,
        description: "Discard the selected file's unstaged changes, or delete it if untracked",
        label: "discard",
        bar: true,
        disabled: discard_disabled,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('o'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::OpenInEditor,
        description: "Open the selected file in your editor",
        label: "open in editor",
        // Left out of the bar until the layout review (#10).
        bar: false,
        disabled: open_disabled,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('b'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::OpenBlame,
        description: "Blame the selected file",
        label: "blame",
        bar: false,
        disabled: blame_disabled,
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
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::Diff,
        intent: AppIntent::Discard,
        description: "Discard the selected hunk from the working tree",
        label: "discard",
        bar: true,
        disabled: discard_disabled,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('o'), NONE)],
        scope: KeyScope::Diff,
        intent: AppIntent::OpenInEditor,
        description: "Open the file in your editor at the selected hunk",
        label: "open in editor",
        // Left out of the bar until the layout review (#10).
        bar: false,
        disabled: open_disabled,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('e'), NONE)],
        scope: KeyScope::Diff,
        intent: AppIntent::EditHunk,
        description: "Edit the selected hunk in your editor, then stage it",
        label: "edit hunk",
        // Left out of the bar until the layout review (#10).
        bar: false,
        disabled: edit_hunk_disabled,
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
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::Lines,
        intent: AppIntent::Discard,
        description: "Discard the selected lines from the working tree",
        label: "discard",
        bar: true,
        disabled: discard_disabled,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('o'), NONE)],
        scope: KeyScope::Lines,
        intent: AppIntent::OpenInEditor,
        description: "Open the file in your editor at the selected line",
        label: "open in editor",
        // Left out of the bar until the layout review (#10).
        bar: false,
        disabled: open_disabled,
    },
    Binding {
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Blame,
        intent: AppIntent::BlameNext,
        description: "Move to the next line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Blame,
        intent: AppIntent::BlamePrev,
        description: "Move to the previous line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::any(KeyCode::PageDown)],
        scope: KeyScope::Blame,
        intent: AppIntent::BlamePageDown,
        description: "Move down a page",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::any(KeyCode::PageUp)],
        scope: KeyScope::Blame,
        intent: AppIntent::BlamePageUp,
        description: "Move up a page",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Blame,
        intent: AppIntent::OpenBlameCommit,
        description: "Show the commit that last changed this line",
        label: "commit",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::any(KeyCode::Esc),
            KeyPattern::exact(KeyCode::Char('h'), NONE),
            KeyPattern::any(KeyCode::Left),
        ],
        scope: KeyScope::Blame,
        intent: AppIntent::CloseBlame,
        description: "Back to the file list",
        label: "back",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::BlameCommit,
        intent: AppIntent::ScrollCommit(Scroll::LineDown),
        description: "Scroll the commit down a line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::BlameCommit,
        intent: AppIntent::ScrollCommit(Scroll::LineUp),
        description: "Scroll the commit up a line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::any(KeyCode::PageDown)],
        scope: KeyScope::BlameCommit,
        intent: AppIntent::ScrollCommit(Scroll::PageDown),
        description: "Scroll the commit down a page",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::any(KeyCode::PageUp)],
        scope: KeyScope::BlameCommit,
        intent: AppIntent::ScrollCommit(Scroll::PageUp),
        description: "Scroll the commit up a page",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::any(KeyCode::Esc),
            KeyPattern::exact(KeyCode::Char('h'), NONE),
            KeyPattern::any(KeyCode::Left),
        ],
        scope: KeyScope::BlameCommit,
        intent: AppIntent::CloseBlameCommit,
        description: "Back to the blame, at the same line",
        label: "back",
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
    // Out of the bar, which is full at 80 columns; the help menu lists them.
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('n'), KeyModifiers::CONTROL)],
        scope: KeyScope::CommitEditor,
        intent: AppIntent::ToggleNoVerify,
        description: "Skip the pre-commit and commit-msg hooks for this commit",
        label: "skip hooks",
        bar: false,
        disabled: commit_editor_disabled,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('e'), KeyModifiers::CONTROL)],
        scope: KeyScope::CommitEditor,
        intent: AppIntent::EditCommitExternally,
        description: "Write the message in your editor, then commit when it closes",
        label: "editor",
        bar: false,
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
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::CommitLoading,
        intent: AppIntent::CancelCommit,
        description: "Do not amend",
        label: "cancel",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::CommitLoading,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
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
    // Only `y` throws work away. Enter is left unbound on purpose: it
    // answers yes elsewhere, and a discard cannot be undone.
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::DiscardConfirm,
        intent: AppIntent::ConfirmYes,
        description: "Yes, throw it away",
        label: "discard",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::DiscardConfirm,
        intent: AppIntent::ConfirmNo,
        description: "No, keep it",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::DiscardConfirm,
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
    fn b_blames_from_files_and_the_blame_scopes_have_their_own_keys() {
        use AppIntent::*;
        let none = KeyModifiers::NONE;
        let b = registry()
            .iter()
            .find(|b| b.scope == KeyScope::Files && b.matches(plain('b')))
            .expect("b is bound in Files");
        assert_eq!(b.intent, OpenBlame);
        assert_eq!(b.label, "blame");
        assert!(!b.bar);
        assert_eq!(from(KeyScope::Diff, plain('b')), None);

        for scope in [KeyScope::Blame, KeyScope::BlameCommit] {
            assert_eq!(scope.parent(), Some(KeyScope::Workspace));
        }
        assert_eq!(KeyScope::Blame.title(), "Blame");
        assert_eq!(KeyScope::BlameCommit.title(), "Commit");

        let blame = |k| from(KeyScope::Blame, k);
        assert_eq!(blame(plain('j')), Some(BlameNext));
        assert_eq!(blame(key(KeyCode::Down, none)), Some(BlameNext));
        assert_eq!(blame(plain('k')), Some(BlamePrev));
        assert_eq!(blame(key(KeyCode::Up, none)), Some(BlamePrev));
        assert_eq!(blame(key(KeyCode::PageDown, none)), Some(BlamePageDown));
        assert_eq!(blame(key(KeyCode::PageUp, none)), Some(BlamePageUp));
        assert_eq!(blame(key(KeyCode::Enter, none)), Some(OpenBlameCommit));
        for k in [
            key(KeyCode::Esc, none),
            plain('h'),
            key(KeyCode::Left, none),
        ] {
            assert_eq!(blame(k), Some(CloseBlame), "{k:?}");
        }
        assert_eq!(blame(plain('q')), Some(Quit));
        // File and hunk keys do not reach the blame.
        for c in [' ', 'a', 'c', 'd', 'o', 'b', 'v', 't', 'e'] {
            assert_eq!(blame(plain(c)), None, "{c:?}");
        }

        let commit = |k| from(KeyScope::BlameCommit, k);
        assert_eq!(commit(plain('j')), Some(ScrollCommit(Scroll::LineDown)));
        assert_eq!(commit(plain('k')), Some(ScrollCommit(Scroll::LineUp)));
        assert_eq!(
            commit(key(KeyCode::PageDown, none)),
            Some(ScrollCommit(Scroll::PageDown))
        );
        for k in [
            key(KeyCode::Esc, none),
            plain('h'),
            key(KeyCode::Left, none),
        ] {
            assert_eq!(commit(k), Some(CloseBlameCommit), "{k:?}");
        }
        assert_eq!(commit(key(KeyCode::Enter, none)), None);
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
    fn shift_a_amends_unless_an_op_runs_there_is_no_commit_or_there_are_conflicts() {
        use crate::git::status::{BranchStatus, Head};

        let a = |state: &AppState| {
            let k = key(KeyCode::Char('A'), KeyModifiers::SHIFT);
            match resolve(registry(), KeyScope::Files, state, k) {
                Resolution::Run(binding) => Ok(binding.intent.clone()),
                Resolution::Disabled(reason) => Err(reason),
                Resolution::Unbound => panic!("A is unbound"),
            }
        };
        let branch = |head| BranchStatus {
            head,
            upstream: None,
            ahead_behind: None,
            in_progress: vec![],
        };
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(
            a(&state),
            Err("No commit to amend"),
            "before the first load"
        );
        state.branch = Some(branch(Head::Unborn("main".into())));
        assert_eq!(a(&state), Err("No commit to amend"));

        // A clean tree with commits: nothing staged is a message-only amend.
        state.branch = Some(branch(Head::Branch("main".into())));
        assert!(state.files.is_empty());
        assert_eq!(a(&state), Ok(AppIntent::OpenAmend));
        state.branch = Some(branch(Head::Detached("abc".into())));
        assert_eq!(a(&state), Ok(AppIntent::OpenAmend));

        state.op_seq = Some(1);
        assert_eq!(a(&state), Err(OP_RUNNING));
        state.op_seq = None;
        state.files = vec![crate::git::FileEntry {
            path: "a.rs".into(),
            orig_path: None,
            index: 'U',
            worktree: 'U',
            kind: EntryKind::Conflicted,
        }];
        assert_eq!(a(&state), Err("Resolve conflicts before amending"));
    }

    #[test]
    fn the_commit_editor_binds_skip_hooks_and_the_external_editor() {
        use AppIntent::*;
        let ctrl = |c| key(KeyCode::Char(c), KeyModifiers::CONTROL);
        let editor = |k| from(KeyScope::CommitEditor, k);
        assert_eq!(editor(ctrl('n')), Some(ToggleNoVerify));
        assert_eq!(editor(ctrl('e')), Some(EditCommitExternally));
        assert_eq!(editor(plain('n')), None, "n is typed");
        assert_eq!(editor(plain('e')), None, "e is typed");

        let mut state = AppState::new(Context::Workspace);
        state.commit = CommitFlow::Running(crate::commit_editor::CommitEditor::new(false));
        state.op_seq = Some(1);
        for c in ['n', 'e'] {
            assert!(matches!(
                resolve(registry(), KeyScope::CommitEditor, &state, ctrl(c)),
                Resolution::Disabled(OP_RUNNING)
            ));
        }
    }

    #[test]
    fn loading_the_last_commit_only_cancels_or_quits() {
        use AppIntent::*;
        assert_eq!(KeyScope::CommitLoading.parent(), None);
        let loading = |k| from(KeyScope::CommitLoading, k);
        assert_eq!(
            loading(key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(CancelCommit)
        );
        assert_eq!(
            loading(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Quit)
        );
        for k in [plain('q'), plain('?'), plain('A'), plain(' ')] {
            assert_eq!(loading(k), None, "{k:?}");
        }
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
    fn d_asks_to_discard_and_only_y_answers_yes() {
        use AppIntent::*;
        let none = KeyModifiers::NONE;
        for scope in [KeyScope::Files, KeyScope::Diff, KeyScope::Lines] {
            assert_eq!(from(scope, plain('d')), Some(Discard), "{scope:?}");
            assert_eq!(
                from(scope, key(KeyCode::Char('d'), KeyModifiers::CONTROL)),
                None,
                "{scope:?}"
            );
        }

        assert_eq!(KeyScope::DiscardConfirm.parent(), None);
        assert_eq!(KeyScope::DiscardConfirm.title(), "Discard");
        let confirm = |k| from(KeyScope::DiscardConfirm, k);
        assert_eq!(confirm(plain('y')), Some(ConfirmYes));
        assert_eq!(confirm(plain('n')), Some(ConfirmNo));
        assert_eq!(confirm(key(KeyCode::Esc, none)), Some(ConfirmNo));
        assert_eq!(
            confirm(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Quit)
        );
        assert_eq!(
            confirm(key(KeyCode::Enter, none)),
            None,
            "Enter never discards"
        );
        assert_eq!(confirm(key(KeyCode::Char('Y'), KeyModifiers::SHIFT)), None);
        for k in [plain('q'), plain('?'), plain('d'), plain(' ')] {
            assert_eq!(confirm(k), None, "{k:?}");
        }
    }

    #[test]
    fn o_opens_the_editor_from_files_diff_and_lines_and_e_edits_a_hunk_in_the_diff() {
        use AppIntent::*;
        for scope in [KeyScope::Files, KeyScope::Diff, KeyScope::Lines] {
            assert_eq!(from(scope, plain('o')), Some(OpenInEditor), "{scope:?}");
            assert_eq!(
                from(scope, key(KeyCode::Char('o'), KeyModifiers::CONTROL)),
                None,
                "{scope:?}"
            );
        }
        let bound = |scope, k| {
            registry()
                .iter()
                .find(|b| b.scope == scope && b.matches(k))
                .map(|b| b.intent.clone())
        };
        assert_eq!(bound(KeyScope::Diff, plain('e')), Some(EditHunk));
        assert_eq!(bound(KeyScope::Files, plain('e')), None);
        assert_eq!(bound(KeyScope::Lines, plain('e')), None);
        assert_eq!(
            bound(
                KeyScope::Diff,
                key(KeyCode::Char('e'), KeyModifiers::CONTROL)
            ),
            None
        );
        assert_eq!(from(KeyScope::Files, plain('e')), None);
    }

    #[test]
    fn o_and_e_say_why_they_cannot_run() {
        use crate::git::diff::DiffSide;
        use crate::git::patch::parse;
        use crate::hunk_view::{HunkView, LineCursor};

        let reason = |state: &AppState, scope, c| match resolve(registry(), scope, state, plain(c))
        {
            Resolution::Disabled(reason) => Some(reason),
            Resolution::Run(_) => None,
            Resolution::Unbound => panic!("{c} is unbound in {scope:?}"),
        };
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(
            reason(&state, KeyScope::Files, 'o'),
            Some("No file selected")
        );
        state.op_seq = Some(1);
        assert_eq!(reason(&state, KeyScope::Files, 'o'), Some(OP_RUNNING));
        assert_eq!(reason(&state, KeyScope::Diff, 'e'), Some(OP_RUNNING));
        state.op_seq = None;

        let mut view = HunkView::new("a.rs".into(), DiffSide::Worktree);
        view.seq = Some(3);
        state.focus = Focus::Diff(view.clone());
        assert_eq!(reason(&state, KeyScope::Diff, 'e'), Some("Loading…"));
        view.seq = None;
        state.focus = Focus::Diff(view.clone());
        assert_eq!(reason(&state, KeyScope::Diff, 'e'), Some(NO_CHANGES));
        view.patch =
            Some(parse(b"diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-a\n+b\n").unwrap());
        state.focus = Focus::Diff(view.clone());
        assert_eq!(reason(&state, KeyScope::Diff, 'e'), None);
        view.lines = Some(LineCursor {
            anchor: 0,
            cursor: 0,
        });
        state.focus = Focus::Diff(view);
        assert_eq!(
            reason(&state, KeyScope::Lines, 'e'),
            Some("Press Esc to edit the whole hunk")
        );
    }

    #[test]
    fn d_is_disabled_while_an_op_runs_or_without_a_file() {
        let d = |state: &AppState| match resolve(registry(), KeyScope::Files, state, plain('d')) {
            Resolution::Disabled(reason) => Some(reason),
            Resolution::Run(_) => None,
            Resolution::Unbound => panic!("d is unbound"),
        };
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(d(&state), Some("No file selected"));
        state.op_seq = Some(1);
        assert_eq!(d(&state), Some("An operation is running"));
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

    #[test]
    fn diff_option_keys_work_from_files_diff_and_lines() {
        use AppIntent::*;
        for scope in [KeyScope::Files, KeyScope::Diff, KeyScope::Lines] {
            assert_eq!(from(scope, plain('+')), Some(MoreContext), "{scope:?}");
            assert_eq!(
                from(scope, key(KeyCode::Char('+'), KeyModifiers::SHIFT)),
                Some(MoreContext),
                "{scope:?}"
            );
            assert_eq!(from(scope, plain('=')), Some(MoreContext), "{scope:?}");
            assert_eq!(from(scope, plain('-')), Some(LessContext), "{scope:?}");
            assert_eq!(from(scope, plain('w')), Some(ToggleWhitespace), "{scope:?}");
        }
        for k in [plain('+'), plain('='), plain('-'), plain('w')] {
            let binding = registry().iter().find(|b| b.matches(k)).expect("bound");
            assert_eq!(binding.scope, KeyScope::Workspace, "{k:?}");
            assert!(!binding.bar, "{k:?} stays out of the bar");
        }
    }

    #[test]
    fn context_keys_are_disabled_at_the_limits() {
        let reason =
            |state: &AppState, c| match resolve(registry(), KeyScope::Files, state, plain(c)) {
                Resolution::Disabled(reason) => Some(reason),
                Resolution::Run(_) => None,
                Resolution::Unbound => panic!("{c} is unbound"),
            };
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(reason(&state, '+'), None);
        assert_eq!(reason(&state, '-'), None);
        state.diff_options.context = MAX_CONTEXT;
        assert_eq!(reason(&state, '+'), Some("Already at the most context"));
        assert_eq!(reason(&state, '='), Some("Already at the most context"));
        assert_eq!(reason(&state, '-'), None);
        state.diff_options.context = MIN_CONTEXT;
        assert_eq!(reason(&state, '-'), Some("Already at the least context"));
        assert_eq!(reason(&state, '+'), None);
    }

    #[test]
    fn hidden_whitespace_disables_the_patch_keys_in_the_diff_only() {
        use crate::git::patch::parse;
        use crate::hunk_view::{HunkView, LineCursor};

        let mut state = AppState::new(Context::Workspace);
        state.files = vec![crate::git::FileEntry {
            path: "a.rs".into(),
            orig_path: None,
            index: '.',
            worktree: 'M',
            kind: EntryKind::Changed,
        }];
        state.selected = Some(0);
        state.diff_options.ignore_whitespace = true;
        let reason = |state: &AppState, scope, c| match resolve(registry(), scope, state, plain(c))
        {
            Resolution::Disabled(reason) => Some(reason),
            Resolution::Run(_) => None,
            Resolution::Unbound => panic!("{c} is unbound in {scope:?}"),
        };
        // Whole-file keys are unaffected.
        assert_eq!(reason(&state, KeyScope::Files, ' '), None);
        assert_eq!(reason(&state, KeyScope::Files, 'd'), None);

        let mut view = HunkView::new("a.rs".into(), DiffSide::Worktree);
        view.patch =
            Some(parse(b"diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-a\n+b\n").unwrap());
        state.focus = Focus::Diff(view.clone());
        for c in [' ', 'v', 'd', 'e'] {
            assert_eq!(
                reason(&state, KeyScope::Diff, c),
                Some(WHITESPACE_HIDDEN),
                "{c:?}"
            );
        }
        view.lines = Some(LineCursor {
            anchor: 0,
            cursor: 0,
        });
        state.focus = Focus::Diff(view);
        for c in [' ', 'd'] {
            assert_eq!(
                reason(&state, KeyScope::Lines, c),
                Some(WHITESPACE_HIDDEN),
                "{c:?}"
            );
        }
        state.diff_options.ignore_whitespace = false;
        assert_eq!(reason(&state, KeyScope::Lines, ' '), None);
    }
}
