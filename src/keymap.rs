//! Every key Kika understands, declared once. The same records drive
//! dispatch, the help menu and the bottom bar, so they cannot drift apart.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::confirm_question::Answer;
use crate::features::branches::{self, remotes, tags};
use crate::features::commits::CommitFilesList;
use crate::features::custom_commands::{CommandTable, CustomCommand};
use crate::features::diff::{self, renderers};
use crate::features::{
    blame, command_log, commit, commits, conflicts, custom_commands, files, operation,
    pull_requests, repos, review, stash, submodules, sync, worktrees,
};
use crate::intent::{AppIntent, Scroll};
use crate::layout::{Panel, SidePanel};
use crate::model::{AppState, Focus};
use kika_git::remotes::RemoteName;
use kika_git::repo_state::{Operation, resumable};

/// Why a key or menu item cannot run, shown to the user. Most reasons are
/// fixed text; some name a branch or a commit, which `Notice` makes safe.
pub type Reason = crate::notice::Notice;

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
    pub(crate) fn overlaps(&self, other: &KeyPattern) -> bool {
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
            KeyCode::Right => "→".to_string(),
            KeyCode::BackTab => "⇧tab".to_string(),
            // Some platforms call it Delete, and the bar is short of room.
            KeyCode::Backspace => "bksp".to_string(),
            KeyCode::PageDown => "PgDn".to_string(),
            KeyCode::PageUp => "PgUp".to_string(),
            other => other.to_string(),
        };
        match self.mods {
            Mods::Exact(mods) if mods.contains(KeyModifiers::CONTROL) => format!("ctrl-{key}"),
            Mods::Exact(mods) if mods.contains(KeyModifiers::ALT) => format!("alt-{key}"),
            _ => key,
        }
    }
}

/// Read a key as a config file writes it: the names [`KeyPattern::label`]
/// uses where they are ASCII, any letter case, plus `up`, `down`, `left` and
/// `right` for the arrows. A `ctrl-` prefix wants exactly Ctrl, an `alt-`
/// prefix exactly Alt (Option, where the terminal sends it as Meta).
pub fn parse_key(text: &str) -> Result<KeyPattern, String> {
    for (prefix, mods) in [
        ("ctrl-", KeyModifiers::CONTROL),
        ("alt-", KeyModifiers::ALT),
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            if rest.chars().any(|c| c.is_uppercase()) {
                return Err(format!(
                    "bad key \"{text}\": write {prefix} keys in lower case"
                ));
            }
            let key = parse_key(rest).map_err(|_| bad_key(text))?;
            return Ok(KeyPattern::exact(key.code, mods));
        }
    }
    let mut chars = text.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return if printable(c) {
            Ok(char_pattern(c))
        } else {
            Err(bad_key(text))
        };
    }
    let code = match text.to_ascii_lowercase().as_str() {
        "space" => return Ok(char_pattern(' ')),
        "esc" => KeyCode::Esc,
        "enter" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        // Terminals report Shift+Tab as a key of its own.
        "shift-tab" => KeyCode::BackTab,
        "bksp" => KeyCode::Backspace,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "pgup" => KeyCode::PageUp,
        "pgdn" => KeyCode::PageDown,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        _ => return Err(bad_key(text)),
    };
    Ok(KeyPattern::any(code))
}

/// Says what would have been accepted, since there is no list in Kika.
fn bad_key(text: &str) -> String {
    format!(
        "bad key \"{}\": use one character, or space, esc, enter, tab, shift-tab, bksp, up, \
         down, left, right, pgup, pgdn, home or end, with an optional ctrl- or alt- prefix",
        text.escape_debug()
    )
}

/// A character a key can type and the help menu can show. Control and
/// format characters (bidi overrides, zero-width marks) could never be
/// pressed, and would garble the help a repository file shows.
fn printable(c: char) -> bool {
    c.is_ascii_graphic()
        || ('\u{a1}'..='\u{ff}').contains(&c) && c != '\u{ad}'
        || c.is_alphanumeric()
}

/// A typed character as the built-in bindings declare it: keys typed
/// without Shift on a US layout want no modifiers, so Ctrl or Alt with them
/// stays free; the rest need Shift (or more), so any modifiers match.
fn char_pattern(c: char) -> KeyPattern {
    let unshifted = c.is_ascii_lowercase() || c.is_ascii_digit() || " `-=[]\\;',./".contains(c);
    if unshifted {
        KeyPattern::exact(KeyCode::Char(c), KeyModifiers::NONE)
    } else {
        KeyPattern::any(KeyCode::Char(c))
    }
}

/// Where a binding applies. Keys are looked up in the focused scope, then
/// each parent, so a panel can override a global key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyScope {
    Global,
    Workspace,
    Files,
    /// The Branches panel with focus, showing local branches.
    Branches,
    /// The Branches panel with focus, showing remotes and their branches.
    Remotes,
    /// The Branches panel with focus, showing tags.
    Tags,
    /// The Commits panel, with a cursor on the log.
    Commits,
    /// The Commits panel showing one commit's files.
    CommitFiles,
    /// The Stash panel, with a cursor on the stash list.
    Stash,
    /// The Stash panel showing one stash's files.
    StashFiles,
    /// The Status panel with focus: controls for the operation in progress.
    Status,
    /// The Diff pane with focus, working on hunks.
    Diff,
    /// The Diff pane picking changed lines inside a hunk.
    Lines,
    /// A blame of the selected file, with a cursor on its lines.
    Blame,
    /// The commit opened from a blame line.
    BlameCommit,
    /// A conflicted file's blocks, with a cursor on one.
    Conflict,
    Review,
    /// Review mode's files screen with the file list focused.
    ReviewFiles,
    /// Review mode's files screen with the diff focused.
    ReviewDiff,
    /// One review thread read in full over the files screen.
    ReviewThread,
    /// Picking the diff line or lines a pending comment goes on.
    ReviewPick,
    /// Typing a pending comment over the files screen.
    ReviewComment,
    /// The list of pending comments over the files screen.
    ReviewPending,
    /// "Delete this comment?" or "Discard this comment?" on top of the
    /// popup stack.
    ReviewCommentConfirm,
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
    /// "Delete this branch?" on top of the popup stack.
    DeleteBranch,
    /// "Remove this remote?" on top of the popup stack.
    RemoveRemote,
    /// "Delete this tag?" on top of the popup stack.
    DeleteTag,
    /// "Delete these branches, tags or remotes?" over a range, on top of
    /// the popup stack.
    DeleteRange,
    /// "Reset hard and lose every uncommitted change?" on top of the popup
    /// stack.
    ResetHard,
    /// "Leave these commits behind?" on top of the popup stack.
    LeaveCommits,
    /// "Drop this stash?" on top of the popup stack.
    DropStash,
    /// "Abort the operation in progress?" on top of the popup stack.
    AbortConfirm,
    /// "Skip the stopped commit and lose the work since the stop?" on top
    /// of the popup stack.
    SkipConfirm,
    /// "Fold every fixup! commit?" on top of the popup stack.
    AutosquashConfirm,
    /// "Rewrite the history with the custom patch?" on top of the popup
    /// stack.
    PatchConfirm,
    /// "Drop these commits?" over a log range, on top of the popup stack.
    DropRangeConfirm,
    /// The submodule list on top of the popup stack.
    Submodules,
    /// The worktree list on top of the popup stack.
    Worktrees,
    /// "Remove this worktree?" on top of the popup stack.
    RemoveWorktree,
    /// The recent repositories on top of the popup stack.
    Recent,
    /// Git or ssh asking for a username, password or passphrase.
    Credential,
    /// The Commits panel showing `HEAD`'s reflog.
    Reflog,
    /// "Undo this?" or "Redo this?" on top of the popup stack.
    Travel,
    /// Typing a list filter or a diff search on the bottom bar.
    Input,
    /// The git commands Kika ran, on top of the popup stack.
    CommandLog,
    /// A list to pick one row from, on top of the popup stack.
    PickList,
    /// "Create this pull request?" on top of the popup stack.
    CreatePullRequest,
    /// "Merge this pull request?" on top of the popup stack.
    MergePullRequest,
    /// "Check out this pull request?" on top of the popup stack.
    CheckoutPullRequest,
}

impl KeyScope {
    /// The commit and popup scopes have no parent, so `q`, `?` and `Esc`
    /// never quit or open help while one is open: they are typed or cancel.
    /// Those that take text or can stay open bind Ctrl+C themselves, so
    /// there is always a way out.
    pub fn parent(self) -> Option<KeyScope> {
        match self {
            KeyScope::Files
            | KeyScope::Branches
            | KeyScope::Remotes
            | KeyScope::Tags
            | KeyScope::Commits
            | KeyScope::CommitFiles
            | KeyScope::Stash
            | KeyScope::StashFiles
            | KeyScope::Status
            | KeyScope::Diff
            | KeyScope::Blame
            | KeyScope::BlameCommit
            | KeyScope::Conflict
            | KeyScope::Reflog => Some(KeyScope::Workspace),
            KeyScope::Lines => Some(KeyScope::Diff),
            KeyScope::ReviewFiles => Some(KeyScope::Review),
            KeyScope::ReviewDiff => Some(KeyScope::ReviewFiles),
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
            | KeyScope::DiscardConfirm
            | KeyScope::DeleteBranch
            | KeyScope::RemoveRemote
            | KeyScope::DeleteTag
            | KeyScope::DeleteRange
            | KeyScope::ResetHard
            | KeyScope::LeaveCommits
            | KeyScope::DropStash
            | KeyScope::AbortConfirm
            | KeyScope::SkipConfirm
            | KeyScope::AutosquashConfirm
            | KeyScope::PatchConfirm
            | KeyScope::DropRangeConfirm
            | KeyScope::Submodules
            | KeyScope::Worktrees
            | KeyScope::RemoveWorktree
            | KeyScope::Recent
            | KeyScope::Credential
            | KeyScope::Travel
            | KeyScope::Input
            | KeyScope::CommandLog
            | KeyScope::PickList
            | KeyScope::CreatePullRequest
            | KeyScope::MergePullRequest
            | KeyScope::CheckoutPullRequest
            | KeyScope::ReviewThread
            | KeyScope::ReviewPick
            | KeyScope::ReviewComment
            | KeyScope::ReviewPending
            | KeyScope::ReviewCommentConfirm => None,
        }
    }

    /// The help menu heading.
    pub fn title(self) -> &'static str {
        match self {
            KeyScope::Global => "Global",
            KeyScope::Workspace => "Workspace",
            KeyScope::Files => "Files",
            KeyScope::Branches => "Branches",
            KeyScope::Remotes => "Remotes",
            KeyScope::Tags => "Tags",
            KeyScope::Commits => "Commits",
            KeyScope::CommitFiles => "Commit files",
            KeyScope::Stash => "Stash",
            KeyScope::StashFiles => "Stash files",
            KeyScope::Status => "Status",
            KeyScope::Diff => "Diff",
            KeyScope::Lines => "Lines",
            KeyScope::Blame => "Blame",
            KeyScope::BlameCommit => "Commit",
            KeyScope::Conflict => "Conflict",
            KeyScope::Review => "Review",
            KeyScope::ReviewFiles => "Pull request files",
            KeyScope::ReviewDiff => "Pull request diff",
            KeyScope::ReviewThread => "Review thread",
            KeyScope::ReviewPick => "Pick lines to comment on",
            KeyScope::ReviewComment => "Pending comment",
            KeyScope::ReviewPending => "Pending review",
            KeyScope::ReviewCommentConfirm => "Pending comment question",
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
            KeyScope::DeleteBranch => "Delete branch",
            KeyScope::RemoveRemote => "Remove remote",
            KeyScope::DeleteTag => "Delete tag",
            KeyScope::DeleteRange => "Delete selection",
            KeyScope::ResetHard => "Reset hard",
            KeyScope::LeaveCommits => "Leave commits behind",
            KeyScope::DropStash => "Drop stash",
            KeyScope::AbortConfirm => "Abort",
            KeyScope::SkipConfirm => "Skip",
            KeyScope::AutosquashConfirm => "Autosquash",
            KeyScope::PatchConfirm => "Custom patch",
            KeyScope::DropRangeConfirm => "Drop commits",
            KeyScope::Submodules => "Submodules",
            KeyScope::Worktrees => "Worktrees",
            KeyScope::RemoveWorktree => "Remove worktree",
            KeyScope::Recent => "Recent repositories",
            KeyScope::Credential => "Git is asking",
            KeyScope::Reflog => "Reflog",
            KeyScope::Travel => "Undo or redo",
            KeyScope::Input => "Filter or search",
            KeyScope::CommandLog => "Command log",
            KeyScope::PickList => "Pick one",
            KeyScope::CreatePullRequest => "Create pull request",
            KeyScope::MergePullRequest => "Merge pull request",
            KeyScope::CheckoutPullRequest => "Check out pull request",
        }
    }

    /// What the bottom bar says before the key hints, for scopes whose
    /// keys behave unlike everywhere else.
    pub fn bar_note(self) -> Option<&'static str> {
        match self {
            KeyScope::CommitEditor
            | KeyScope::Credential
            | KeyScope::Input
            | KeyScope::ReviewComment => Some("q and ? are typed"),
            _ => None,
        }
    }

    /// This scope and its parents, nearest first.
    pub(crate) fn chain(self) -> impl Iterator<Item = KeyScope> {
        std::iter::successors(Some(self), |scope| scope.parent())
    }
}

/// One entry in the keymap.
#[derive(Debug, Clone)]
pub struct Binding {
    /// `<scope>.<action>`, the name a config file uses to remap it.
    pub id: &'static str,
    /// The built-in keys; a [`Keymap`] may replace them.
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
    pub disabled: fn(&AppState) -> Option<Reason>,
}

impl Binding {
    /// Whether the built-in keys match; tests ask about the defaults.
    #[cfg(test)]
    pub(crate) fn matches(&self, key: KeyPress) -> bool {
        self.keys.iter().any(|pattern| pattern.matches(key))
    }
}

/// A config file's `[keys]` table: binding id to the keys it should have.
/// An empty list disables the binding.
pub type KeysTable = BTreeMap<String, Vec<String>>;

/// How far a `[keys]` table is trusted. A repository file arrives with a
/// clone, so whoever wrote it is not necessarily the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    Global,
    Repo,
}

/// Bindings a repository file may not change: they quit, start other
/// programs (an editor, a diff renderer, a merge tool), reach the network,
/// destroy or write work, answer a question, create a repository or switch
/// to another one. Moving
/// one onto an innocent key, taking quit away, or letting the key that
/// asks also answer, would let a cloned repository trap or trick the user.
pub const REPO_LOCKED: &[&str] = &[
    "global.quit",
    "commit_editor.quit",
    "commit_loading.quit",
    "menu.quit",
    "prompt.quit",
    "error.quit",
    "discard_confirm.quit",
    "submodules.quit",
    "files.open",
    "diff.open",
    "lines.open",
    "diff.edit_hunk",
    "diff.renderer",
    "commit_editor.edit_externally",
    "submodules.init",
    "submodules.update",
    "files.discard",
    "diff.discard",
    "lines.discard",
    "commit_discard.discard",
    "confirm.yes",
    "confirm.no",
    "discard_confirm.yes",
    "discard_confirm.no",
    "recent.quit",
    "recent.open",
    "recent.init",
    "branches.delete",
    "branches.merge",
    "status.continue",
    "status.skip",
    "status.abort",
    "status.bisect_good",
    "status.bisect_bad",
    "commits.bisect",
    "delete_branch.yes",
    "delete_branch.force",
    "delete_branch.no",
    "delete_branch.quit",
    "workspace.fetch",
    "workspace.pull",
    "workspace.push",
    "credential.answer",
    "credential.cancel",
    "credential.quit",
    "files.mergetool",
    "conflict.mergetool",
    "conflict.ours",
    "conflict.theirs",
    "conflict.both",
    "commits.revert",
    "commits.reset",
    "reset_hard.yes",
    "reset_hard.no",
    "reset_hard.quit",
    "leave_commits.yes",
    "leave_commits.no",
    "leave_commits.quit",
    "files.stash",
    "stash.files",
    "stash.apply",
    "stash.pop",
    "stash.drop",
    "stash.rename",
    "drop_stash.yes",
    "drop_stash.no",
    "drop_stash.quit",
    "abort_confirm.yes",
    "abort_confirm.no",
    "abort_confirm.quit",
    "skip_confirm.yes",
    "skip_confirm.no",
    "skip_confirm.quit",
    "remotes.remove",
    "remove_remote.yes",
    "remove_remote.no",
    "remove_remote.quit",
    "tags.delete",
    "tags.push",
    "delete_tag.yes",
    "delete_tag.no",
    "delete_tag.quit",
    "delete_range.yes",
    "delete_range.no",
    "delete_range.quit",
    "commits.squash",
    "commits.fixup",
    "commits.drop",
    "commits.reword",
    "commits.move_up",
    "commits.move_down",
    "commits.edit",
    "commits.break",
    "commits.pick",
    "commits.create_fixup",
    "commits.autosquash",
    "autosquash_confirm.yes",
    "autosquash_confirm.no",
    "autosquash_confirm.quit",
    "workspace.patch_menu",
    "patch_confirm.yes",
    "patch_confirm.no",
    "patch_confirm.quit",
    "drop_range_confirm.yes",
    "drop_range_confirm.no",
    "drop_range_confirm.quit",
    "worktrees.open",
    "worktrees.add",
    "worktrees.remove",
    "worktrees.quit",
    "worktrees.escape",
    "remove_worktree.yes",
    "remove_worktree.no",
    "remove_worktree.quit",
    "workspace.undo",
    "workspace.redo",
    "travel.yes",
    "travel.no",
    "travel.quit",
    "branches.pull_request",
    "branches.refresh_pull_requests",
    "pick_list.quit",
    "pick_list.pick",
    "workspace.review",
    "review.open",
    "review.refresh",
    "review.open_check",
    "review.back",
    "review.files",
    "review_files.viewed",
    "create_pull_request.yes",
    "create_pull_request.no",
    "create_pull_request.quit",
    "review.merge",
    "merge_pr.yes",
    "merge_pr.no",
    "merge_pr.quit",
    "review.checkout",
    "checkout_pr.yes",
    "checkout_pr.no",
    "checkout_pr.quit",
    "review_thread.quit",
    "review_pick.quit",
    "review_comment.quit",
    "review_pending.quit",
    "review_comment_confirm.yes",
    "review_comment_confirm.no",
    "review_comment_confirm.quit",
    "command_log.quit",
    "command_log.escape",
    "commits.paste",
    "files.escape",
    "commits.escape",
    "branches.escape",
    "remotes.escape",
    "tags.escape",
    "reflog.escape",
    "stash.escape",
    "input.quit",
    "commit_files.escape",
    "stash_files.escape",
    "blame.escape",
    "blame_commit.escape",
];

/// The keys in force: the registry's, except where the user's config
/// changed them, plus the user's own commands. Only the changes are stored,
/// so the state stays cheap to clone and compare.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Keymap {
    overrides: BTreeMap<&'static str, Vec<KeyPattern>>,
    /// From the global file only; a repository layer copies them unchanged.
    pub commands: Vec<CustomCommand>,
}

/// Who a key belongs to, for messages.
#[derive(Clone, Copy)]
enum Owner<'a> {
    Binding(&'a Binding),
    Command(usize),
}

impl Owner<'_> {
    /// Tells owners apart: a binding by its id, a command by its place.
    fn id(self) -> (usize, &'static str) {
        match self {
            Owner::Binding(b) => (0, b.id),
            Owner::Command(i) => (i + 1, ""),
        }
    }
}

impl std::fmt::Display for Owner<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Owner::Binding(b) => write!(f, "\"{}\"", b.id),
            Owner::Command(i) => write!(f, "command {}", i + 1),
        }
    }
}

/// A key press that two owners of one scope answer to, or one owner
/// through two of its keys.
#[derive(Clone, Copy)]
struct Clash<'a> {
    a: Owner<'a>,
    b: Owner<'a>,
    /// `b`'s key.
    key: KeyPattern,
    /// Where `a`'s and `b`'s keys stand in [`Keymap::owned_keys`], so the
    /// first clash reported is always the same one.
    at: (usize, usize),
}

impl std::fmt::Display for Clash<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let key = self.key.label();
        if self.a.id() == self.b.id() {
            write!(f, "key \"{key}\" is bound twice to {}", self.a)
        } else {
            write!(
                f,
                "key \"{key}\" is bound to both {} and {}",
                self.a, self.b
            )
        }
    }
}

impl Keymap {
    /// The keys `b` answers to.
    pub fn keys<'a>(&'a self, b: &'a Binding) -> &'a [KeyPattern] {
        self.overrides.get(b.id).map_or(b.keys, Vec::as_slice)
    }

    /// `base` with `layer` applied on top, and one message per entry of
    /// `layer` dropped for a key clash. A dropped entry's binding keeps its
    /// keys from `base`; the rest of `layer` still applies, so one stale
    /// line does not cost the user every other key. Fails, leaving nothing
    /// applied, on an unknown id or key, quit left without keys, or a
    /// repository layer reaching past its trust. Those checks run on the
    /// keymap after the drops, so no drop can get past them.
    pub fn build(
        base: &Keymap,
        layer: &KeysTable,
        trust: Trust,
    ) -> Result<(Keymap, Vec<String>), String> {
        Self::build_layer(base, layer, trust).map(|(keymap, dropped, _)| (keymap, dropped))
    }

    /// [`Keymap::build`], with the ids whose change `layer` made and kept
    /// after the drops: the global file's commands give way to those.
    pub fn build_layer(
        base: &Keymap,
        layer: &KeysTable,
        trust: Trust,
    ) -> Result<(Keymap, Vec<String>, BTreeSet<&'static str>), String> {
        let mut keymap = base.clone();
        let mut layer_ids = BTreeSet::new();
        for (id, texts) in layer {
            let binding = registry()
                .iter()
                .find(|b| b.id == id)
                .ok_or_else(|| format!("unknown binding \"{id}\""))?;
            if trust == Trust::Repo && REPO_LOCKED.contains(&binding.id) {
                return Err(format!(
                    "\"{id}\" can only be changed in the global config file"
                ));
            }
            let keys = texts
                .iter()
                .map(|text| parse_key(text).map_err(|e| format!("\"{id}\": {e}")))
                .collect::<Result<Vec<_>, _>>()?;
            keymap.overrides.insert(binding.id, keys);
            layer_ids.insert(binding.id);
        }
        let dropped = keymap.drop_clashes(base, &mut layer_ids)?;
        if trust == Trust::Repo {
            keymap.check_locked_not_hidden(base)?;
        }
        let quit = registry().iter().find(|b| b.id == "global.quit");
        if quit.is_some_and(|b| keymap.keys(b).is_empty()) {
            return Err("\"global.quit\" needs at least one key".to_string());
        }
        Ok((keymap, dropped, layer_ids))
    }

    /// Undo the overrides in `layer_ids`, the ones the layer being built
    /// made, that take part in a key clash, until none is left; each goes
    /// back to its keys in `base`. A side the layer did not set is never
    /// touched. Every pass drops at least one override, and with none left
    /// the keymap is `base`, which has no clash, so this ends. A clash with
    /// no side in `layer_ids` means `base` was not clash-free: fail closed.
    fn drop_clashes(
        &mut self,
        base: &Keymap,
        layer_ids: &mut BTreeSet<&'static str>,
    ) -> Result<Vec<String>, String> {
        let mut messages = Vec::new();
        loop {
            let mut dropping = BTreeSet::new();
            for clash in self.clashes() {
                for side in [clash.a, clash.b] {
                    let Owner::Binding(binding) = side else {
                        continue;
                    };
                    if layer_ids.contains(binding.id) && dropping.insert(binding.id) {
                        messages.push(format!("{clash}; the change to {side} was dropped"));
                    }
                }
            }
            if dropping.is_empty() {
                // No clash left, or one `base` itself had: refuse that.
                return self.check_scopes().map(|()| messages);
            }
            for id in dropping {
                layer_ids.remove(id);
                match base.overrides.get(id) {
                    Some(keys) => self.overrides.insert(id, keys.clone()),
                    None => self.overrides.remove(id),
                };
            }
        }
    }

    /// `self` with the global file's `[[commands]]`, in place of any it
    /// had, and one message per command dropped. A command on a key that
    /// a binding in `kept`, the ones the same file's `[keys]` moved, holds
    /// in the command's scope is dropped alone: the user's newer intent is
    /// usually the binding, and one stale command should not cost the rest
    /// of the file. A dropped command binds nothing, so it can neither hide
    /// nor move a locked binding. The commands left must then pass every
    /// other check: the file fails on a table that does not parse, a
    /// command that would hide a locked binding in an enclosing scope, such
    /// as `q` hiding quit, or any other clash in a command's scope.
    /// Messages number commands by their place in the file, dropped ones
    /// included.
    pub fn with_commands(
        self,
        tables: &[CommandTable],
        kept: &BTreeSet<&'static str>,
    ) -> Result<(Keymap, Vec<String>), String> {
        let commands = tables
            .iter()
            .enumerate()
            .map(|(i, table)| CustomCommand::parse(table, i + 1))
            .collect::<Result<Vec<_>, _>>()?;
        let keymap = Keymap { commands, ..self };
        let dropping = keymap.binding_clashes(kept);
        let mut messages = Vec::new();
        let mut numbers = Vec::new();
        let mut commands = Vec::new();
        for (i, (command, clashes)) in keymap.commands.into_iter().zip(dropping).enumerate() {
            match clashes {
                Some(clashes) => messages.push(format!(
                    "{} (\"{}\"); command {} was dropped",
                    clashes.join(", and "),
                    command.description,
                    i + 1
                )),
                None => {
                    numbers.push(i);
                    commands.push(command);
                }
            }
        }
        let keymap = Keymap {
            commands,
            overrides: keymap.overrides,
        };
        keymap.check_locked_commands(&numbers)?;
        if let Some(clash) = keymap.clashes().first() {
            // Named by their place in the file, not in what is left.
            let renumber = |owner| match owner {
                Owner::Command(i) => Owner::Command(numbers[i]),
                binding => binding,
            };
            let clash = Clash {
                a: renumber(clash.a),
                b: renumber(clash.b),
                ..*clash
            };
            return Err(clash.to_string());
        }
        Ok((keymap, messages))
    }

    /// For each command that one of its keys puts on a binding in `kept`,
    /// in its scope, every key of it that a binding of its scope holds,
    /// one clash each, so the message says all it loses; `None` for the
    /// others. Bindings are indexed by scope and key code and a repeated
    /// key is listed once, so the work and the messages stay linear in the
    /// number of keys.
    fn binding_clashes(&self, kept: &BTreeSet<&'static str>) -> Vec<Option<Vec<String>>> {
        let mut held: HashMap<(KeyScope, KeyCode), Vec<(&KeyPattern, &Binding)>> = HashMap::new();
        for binding in registry() {
            for key in self.keys(binding) {
                held.entry((binding.scope, key.code))
                    .or_default()
                    .push((key, binding));
            }
        }
        let mut dropping = Vec::with_capacity(self.commands.len());
        for (i, command) in self.commands.iter().enumerate() {
            let mut on_kept = false;
            let mut listed = HashSet::new();
            let mut clashes = Vec::new();
            for key in &command.keys {
                let holders = held.get(&(command.scope, key.code)).into_iter().flatten();
                for &(_, binding) in holders.filter(|(k, _)| k.overlaps(key)) {
                    on_kept |= kept.contains(binding.id);
                    if listed.insert((key.label(), binding.id)) {
                        let clash = Clash {
                            a: Owner::Binding(binding),
                            b: Owner::Command(i),
                            key: *key,
                            // Only shown, never ordered against another clash.
                            at: (0, 0),
                        };
                        clashes.push(clash.to_string());
                    }
                }
            }
            dropping.push(on_kept.then_some(clashes));
        }
        dropping
    }

    /// No command key may hide a locked binding of an enclosing scope.
    /// `numbers` holds each command's place in the file, from 0.
    fn check_locked_commands(&self, numbers: &[usize]) -> Result<(), String> {
        for (command, n) in self.commands.iter().zip(numbers) {
            let locked = registry().iter().filter(|b| REPO_LOCKED.contains(&b.id));
            for binding in locked.filter(|b| command.scope.chain().any(|s| s == b.scope)) {
                let hidden = self
                    .keys(binding)
                    .iter()
                    .find(|k| command.keys.iter().any(|key| key.overlaps(k)));
                if let Some(key) = hidden {
                    return Err(format!(
                        "command {}: key \"{}\" would hide \"{}\"",
                        n + 1,
                        key.label(),
                        binding.id
                    ));
                }
            }
        }
        Ok(())
    }

    /// Every key in force with its owner and scope: the registry's, then
    /// the commands'.
    fn owned_keys(&self) -> Vec<(Owner<'_>, KeyScope, &KeyPattern)> {
        let builtin = registry().iter().flat_map(|b| {
            self.keys(b)
                .iter()
                .map(move |k| (Owner::Binding(b), b.scope, k))
        });
        let commands = self
            .commands
            .iter()
            .enumerate()
            .flat_map(|(i, c)| c.keys.iter().map(move |k| (Owner::Command(i), c.scope, k)));
        builtin.chain(commands).collect()
    }

    /// No key press may match two bindings or commands of one scope.
    fn check_scopes(&self) -> Result<(), String> {
        match self.clashes().first() {
            Some(clash) => Err(clash.to_string()),
            None => Ok(()),
        }
    }

    /// The first clash of every owner that has one, in order. Each owner
    /// appears in at least one and there is at most one per owner, so the
    /// work and the result stay small however many keys a file lists:
    /// keys are compared only within their scope and key code, and a key
    /// an owner already has is not stored again.
    fn clashes(&self) -> Vec<Clash<'_>> {
        type Seen<'a> = Vec<(usize, Owner<'a>, KeyPattern)>;
        let mut groups: HashMap<(KeyScope, KeyCode), Seen<'_>> = HashMap::new();
        let mut first: BTreeMap<(usize, &str), Clash<'_>> = BTreeMap::new();
        for (j, (owner, scope, key)) in self.owned_keys().into_iter().enumerate() {
            let seen = groups.entry((scope, key.code)).or_default();
            let mut repeated = false;
            for &(i, other, k) in seen.iter() {
                if k.overlaps(key) {
                    let clash = Clash {
                        a: other,
                        b: owner,
                        key: *key,
                        at: (i, j),
                    };
                    for side in [other, owner] {
                        first
                            .entry(side.id())
                            .and_modify(|c| {
                                if clash.at < c.at {
                                    *c = clash;
                                }
                            })
                            .or_insert(clash);
                    }
                }
                repeated |= other.id() == owner.id() && k == *key;
            }
            if !repeated {
                seen.push((j, owner, *key));
            }
        }
        let mut clashes: Vec<Clash<'_>> = first.into_values().collect();
        clashes.sort_by_key(|c| c.at);
        clashes.dedup_by_key(|c| c.at);
        clashes
    }

    /// A key a repository file adds in a nearer scope would take a locked
    /// binding's key away from it, as surely as removing it would.
    fn check_locked_not_hidden(&self, base: &Keymap) -> Result<(), String> {
        for binding in registry() {
            let added = self
                .keys(binding)
                .iter()
                .filter(|k| !base.keys(binding).contains(k));
            for key in added {
                let hidden = registry().iter().find(|locked| {
                    REPO_LOCKED.contains(&locked.id)
                        && binding.scope.chain().skip(1).any(|s| s == locked.scope)
                        && self.keys(locked).iter().any(|k| k.overlaps(key))
                });
                if let Some(locked) = hidden {
                    return Err(format!(
                        "\"{}\": key \"{}\" would hide \"{}\"",
                        binding.id,
                        key.label(),
                        locked.id
                    ));
                }
            }
        }
        Ok(())
    }
}

/// A keymap from one global `[keys]` table, for tests elsewhere.
#[cfg(test)]
pub(crate) fn remap(entries: &[(&str, &[&str])]) -> Keymap {
    let table: KeysTable = entries
        .iter()
        .map(|(id, keys)| (id.to_string(), keys.iter().map(|k| k.to_string()).collect()))
        .collect();
    let (keymap, dropped) =
        Keymap::build(&Keymap::default(), &table, Trust::Global).expect("a valid layer");
    assert_eq!(dropped, Vec::<String>::new(), "a layer without clashes");
    keymap
}

/// What a key press does from a given focus.
#[derive(Debug)]
pub enum Resolution<'a> {
    Run(&'a Binding),
    /// The user's command at this index in [`Keymap::commands`].
    Command(usize),
    /// The key is bound but cannot run; the reason is shown to the user.
    Disabled(Reason),
    Unbound,
}

/// Find the binding for `key`: the focused scope first, then each parent.
/// Within a scope the built-in bindings come before the user's commands,
/// though a valid keymap never lets the two share a key.
/// A bound but disabled key stops the search, so a panel's refusal is never
/// silently replaced by a global binding of the same key.
pub fn resolve<'a>(
    bindings: &'a [Binding],
    keymap: &Keymap,
    focus: KeyScope,
    state: &AppState,
    key: KeyPress,
) -> Resolution<'a> {
    for scope in focus.chain() {
        let found = bindings
            .iter()
            .find(|b| b.scope == scope && keymap.keys(b).iter().any(|p| p.matches(key)));
        if let Some(binding) = found {
            return match (binding.disabled)(state) {
                Some(reason) => Resolution::Disabled(reason),
                None => Resolution::Run(binding),
            };
        }
        let command = keymap
            .commands
            .iter()
            .position(|c| c.scope == scope && c.keys.iter().any(|p| p.matches(key)));
        if let Some(index) = command {
            return Resolution::Command(index);
        }
    }
    Resolution::Unbound
}

/// The bindings reachable from `focus`, nearest scope first and in table
/// order within a scope, each with why it is disabled. A binding whose keys
/// are all taken by a nearer scope can never run, so it is left out.
/// A binding the keymap left without keys can never run either.
pub fn available<'a>(
    bindings: &'a [Binding],
    keymap: &Keymap,
    focus: KeyScope,
    state: &AppState,
) -> Vec<(&'a Binding, Option<Reason>)> {
    let mut claimed: Vec<KeyPattern> = Vec::new();
    let mut out = Vec::new();
    for scope in focus.chain() {
        let in_scope: Vec<&Binding> = bindings.iter().filter(|b| b.scope == scope).collect();
        for binding in &in_scope {
            let shadowed = keymap
                .keys(binding)
                .iter()
                .all(|key| claimed.iter().any(|c| c.overlaps(key)));
            if !shadowed {
                out.push((*binding, (binding.disabled)(state)));
            }
        }
        claimed.extend(in_scope.iter().flat_map(|b| keymap.keys(b).iter().copied()));
        claimed.extend(commands_in(keymap, scope).flat_map(|c| c.keys.iter().copied()));
    }
    out
}

/// The user's commands reachable from `focus`, nearest scope first and in
/// file order within a scope. Like [`available`], it leaves out a command
/// whose keys are all taken by a nearer scope.
pub fn available_commands(keymap: &Keymap, focus: KeyScope) -> Vec<&CustomCommand> {
    let mut claimed: Vec<KeyPattern> = Vec::new();
    let mut out = Vec::new();
    for scope in focus.chain() {
        for command in commands_in(keymap, scope) {
            let shadowed = command
                .keys
                .iter()
                .all(|key| claimed.iter().any(|c| c.overlaps(key)));
            if !shadowed {
                out.push(command);
            }
        }
        let builtin = registry().iter().filter(|b| b.scope == scope);
        claimed.extend(builtin.flat_map(|b| keymap.keys(b).iter().copied()));
        claimed.extend(commands_in(keymap, scope).flat_map(|c| c.keys.iter().copied()));
    }
    out
}

fn commands_in(keymap: &Keymap, scope: KeyScope) -> impl Iterator<Item = &CustomCommand> {
    keymap.commands.iter().filter(move |c| c.scope == scope)
}

/// For bindings and menu items that always run.
pub(crate) fn never(_: &AppState) -> Option<Reason> {
    None
}

pub(crate) const OP_RUNNING: &str = "An operation is running";

/// A diff made with `-w` can differ from the file, so a patch built from it
/// can fail to apply or stage the wrong lines.
pub(crate) const WHITESPACE_HIDDEN: &str = "Whitespace is hidden; press w to stage";

/// The Diff pane shows a `-w` diff, which hunk and line keys cannot apply.
pub(crate) fn whitespace_hidden(state: &AppState) -> Option<Reason> {
    let in_diff = state.focus == Focus::Diff;
    (in_diff && state.diff_options.ignore_whitespace).then_some(WHITESPACE_HIDDEN.into())
}

/// A diff tool draws the files, not the patch, so nothing in its view can
/// be staged, discarded or edited.
pub const DISPLAY_ONLY: &str = "The diff tool view is display-only: press R for the built-in diff";

/// The Diff pane shows a tool's rows, which hunk and line keys cannot act on.
pub(crate) fn display_only(state: &AppState) -> Option<Reason> {
    let in_diff = state.focus == Focus::Diff;
    (in_diff && state.display_only()).then_some(DISPLAY_ONLY.into())
}

/// The diff keys of the Workspace scope reach the blame through its
/// parent, but the diff they would change is hidden behind it.
const BLAME_OPEN: &str = "Close the blame first";

pub(crate) fn blame_open(state: &AppState) -> Option<Reason> {
    matches!(state.focus, Focus::Blame(_)).then_some(BLAME_OPEN.into())
}

/// Enter has no commit to open before the log has rows.
/// Why a key that acts on a log commit does nothing: there is none, or
/// the cursor is on the rebase's todo list above the log.
pub(crate) fn no_commit(state: &AppState) -> Option<Reason> {
    if state.commits.todo_selected.is_some() {
        return Some("Select a commit in the log first".into());
    }
    state
        .commits
        .list
        .rows
        .is_empty()
        .then_some("No commits".into())
}

/// Why nothing can be copied from the Commits panel.
pub(crate) fn no_commit_selected(state: &AppState) -> Option<Reason> {
    let row = state.commits.list.rows.get(state.commits.list.selected);
    row.is_none().then_some("No commit selected".into())
}

/// Why the diff cannot take focus: with a commit's files open, the hunks
/// are its selected file's, picked for the custom patch, so the filter must
/// show that file; otherwise the selected working-tree file's.
fn focus_diff_disabled(state: &AppState) -> Option<Reason> {
    match &state.commits.open {
        Some(open) if state.focus == Focus::Commits => list_guard(state, false)
            .or_else(|| commit_source_disabled(state, open))
            .or_else(|| {
                open.selected_file()
                    .is_none()
                    .then_some("No file selected".into())
            }),
        _ => no_file_selected(state),
    }
}

/// Why changes cannot be picked from the open commit's files: only one
/// commit, with a parent and no second one.
pub(crate) fn commit_source_disabled(state: &AppState, open: &CommitFilesList) -> Option<Reason> {
    if open.against.is_some() {
        return Some("Only one commit's changes can be picked, not a comparison".into());
    }
    if open.range.parent.is_none() {
        return Some("Cannot pick from the first commit".into());
    }
    let merge = state
        .commits
        .list
        .rows
        .iter()
        .any(|row| row.oid == open.range.oid.as_str() && row.parents.len() > 1);
    merge.then_some("Cannot pick from a merge commit".into())
}

pub(crate) fn no_file_selected(state: &AppState) -> Option<Reason> {
    let selected = state.files.selected_row();
    selected.is_none().then_some("No file selected".into())
}

/// Why a key that works on the working tree does nothing in a commit's
/// change: the commit is history, not the worktree.
pub(crate) const NOT_IN_COMMIT: &str = "Not in a commit's change";

/// The Diff pane shows a commit's change, which has no sides, nothing to
/// discard and no hunk to edit before staging.
pub(crate) fn in_commit(state: &AppState) -> Option<Reason> {
    (state.hunk_view().is_some_and(|view| view.commit.is_some())).then_some(NOT_IN_COMMIT.into())
}

/// Why `d` cannot ask right now. What it would discard, and why that is
/// refused, is the model's call once it runs.
pub(crate) fn discard_disabled(state: &AppState) -> Option<Reason> {
    if let Some(reason) = in_commit(state) {
        return Some(reason);
    }
    if state.op_is_running() {
        return Some(OP_RUNNING.into());
    }
    if let Some(reason) = display_only(state).or_else(|| whitespace_hidden(state)) {
        return Some(reason);
    }
    match state.hunk_view() {
        Some(view) if view.loading() => Some("Loading…".into()),
        _ => no_file_selected(state),
    }
}

/// Why `o` cannot open the editor right now.
pub(crate) fn open_disabled(state: &AppState) -> Option<Reason> {
    if state.op_is_running() {
        return Some(OP_RUNNING.into());
    }
    no_file_selected(state)
}

/// Why no branch action can run right now: one needs a selected branch
/// the filter shows and no range, and git one operation at a time.
pub(crate) fn branch_action_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| branch_selected_disabled(state))
}

/// [`branch_action_disabled`] without the filter and range guard.
pub(crate) fn branch_selected_disabled(state: &AppState) -> Option<Reason> {
    if state.op_is_running() {
        Some(OP_RUNNING.into())
    } else if state.branches.selected_branch().is_none() {
        Some("No branch selected".into())
    } else {
        None
    }
}

/// Why nothing can start now: git one operation at a time.
pub(crate) fn op_running(state: &AppState) -> Option<Reason> {
    state.op_is_running().then_some(OP_RUNNING.into())
}

/// Why the Tags tab has nothing to act on.
pub(crate) fn tag_selected_disabled(state: &AppState) -> Option<Reason> {
    state
        .branches
        .tags
        .selected_row()
        .is_none()
        .then_some("No tag".into())
}

/// Why the selected tag cannot be pushed: a push needs the slot, a tag,
/// and a remote Kika can choose without asking.
pub(crate) fn push_tag_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false)
        .or_else(|| remote_disabled(state))
        .or_else(|| tag_selected_disabled(state))
        .or_else(|| push_tag_remote(state).err().map(Reason::from))
}

/// The remote a tag is pushed to: the current branch's upstream's, else
/// the only configured one. Never a picker; the status bar names it.
pub(crate) fn push_tag_remote(state: &AppState) -> Result<RemoteName, &'static str> {
    let configured: Vec<&RemoteName> = state
        .branches
        .remotes
        .remotes()
        .filter(|r| r.configured)
        .map(|r| &r.name)
        .collect();
    let upstream = state.branches.current().and_then(|b| b.upstream.as_deref());
    // A remote's name may hold `/`, so the longest one the upstream
    // starts with owns it.
    let owner = upstream.and_then(|upstream| {
        configured
            .iter()
            .filter(|name| {
                upstream
                    .strip_prefix(name.display().as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
            })
            .max_by_key(|name| name.0.len())
    });
    match (owner, configured.as_slice()) {
        (Some(name), _) => Ok((*name).clone()),
        (None, [only]) => Ok((*only).clone()),
        (None, []) => Err("No remote"),
        (None, _) => Err("Several remotes and no upstream to choose from"),
    }
}

/// The operation continue, skip and abort act on, if one is in progress.
pub(crate) fn resumable_operation(state: &AppState) -> Option<Operation> {
    state
        .branch
        .as_ref()
        .and_then(|b| resumable(&b.in_progress))
}

/// Why continue or abort cannot run: one at a time, and only with a
/// merge, rebase, cherry-pick or revert to act on.
pub(crate) fn resume_disabled(state: &AppState) -> Option<Reason> {
    if state.op_is_running() {
        Some(OP_RUNNING.into())
    } else if resumable_operation(state).is_none() {
        Some("Nothing in progress".into())
    } else {
        None
    }
}

pub(crate) fn skip_disabled(state: &AppState) -> Option<Reason> {
    resume_disabled(state).or_else(|| {
        resumable_operation(state)
            .is_some_and(|op| !op.can_skip())
            .then_some("A merge cannot be skipped".into())
    })
}

/// Another operation in progress would mix with this one's commits, and
/// a bisect's `HEAD` is the commit it is testing, not one to build on.
/// The bisect is checked here, not in [`resumable_operation`], which
/// leaves it out on purpose; the operation to finish is named first.
pub(crate) fn in_progress_disabled(state: &AppState) -> Option<Reason> {
    resumable_operation(state)
        .is_some()
        .then_some("Finish or abort the operation in progress".into())
        .or_else(|| crate::bisect::head_move_disabled(state))
}

/// A range spans log rows; the todo list above the log has none.
pub(crate) fn commits_range_disabled(state: &AppState) -> Option<Reason> {
    state
        .commits
        .todo_selected
        .is_some()
        .then_some("Select a commit in the log first".into())
}

/// Why no fetch, pull or push may start: one needs a repository, and git
/// one operation at a time.
pub(crate) fn remote_disabled(state: &AppState) -> Option<Reason> {
    if state.repo.is_none() {
        Some("Not in a repository".into())
    } else if state.op_is_running() || state.remote.is_some() {
        Some(REMOTE_BUSY.into())
    } else {
        None
    }
}

pub(crate) const REMOTE_BUSY: &str = "Another operation is running";

/// Why an action on the selected row cannot run: the filter hides it.
pub(crate) const FILTER_HIDES_ROW: &str = "No row matches the filter";

/// Why an action on one row cannot run while a range is selected.
pub(crate) const CLEAR_RANGE_FIRST: &str = "Clear the selection first (esc)";

/// Why an action on a list's selected row cannot run now: the filter
/// hides that row, or a range is selected and the action is not one that
/// applies to a range (`range_ok`). Chained in front of the action's own
/// reason, so these come first.
pub(crate) fn list_guard(state: &AppState, range_ok: bool) -> Option<Reason> {
    let branches = &state.branches;
    let (hidden, range) = match state.panel_focus() {
        KeyScope::Branches => (
            branches.list.selected_hidden(),
            branches.list.range_anchor.is_some(),
        ),
        KeyScope::Remotes => (
            branches.remotes.list.selected_hidden(),
            branches.remotes.list.range_anchor.is_some(),
        ),
        KeyScope::Tags => (
            branches.tags.selected_hidden(),
            branches.tags.range_anchor.is_some(),
        ),
        KeyScope::Files => (
            state.files.selected_hidden(),
            state.files.range_anchor.is_some(),
        ),
        KeyScope::Stash => (
            state.stash.list.selected_hidden(),
            state.stash.list.range_anchor.is_some(),
        ),
        // The filter hides log rows only; a todo row above the log shows.
        KeyScope::Commits => (
            state.commits.todo_selected.is_none() && state.commits.list.selected_hidden(),
            state.commits.list.range_anchor.is_some(),
        ),
        // No range: nothing acts on a range of a commit's files.
        KeyScope::CommitFiles => (
            state
                .commits
                .open
                .as_ref()
                .is_some_and(|open| open.list.selected_hidden()),
            false,
        ),
        _ => return None,
    };
    if hidden {
        Some(FILTER_HIDES_ROW.into())
    } else if range && !range_ok {
        Some(CLEAR_RANGE_FIRST.into())
    } else {
        None
    }
}

/// Why a rewrite cannot run on a log narrowed to a path or an author.
pub(crate) const CLEAR_LOG_FILTER: &str = "Clear the log filter first";

pub(crate) fn no_branch_selected(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| {
        let selected = state.branches.selected_branch();
        selected.is_none().then_some("No branch selected".into())
    })
}

/// The search works on the built-in diff's hunks, outside line mode.
fn search_disabled(state: &AppState) -> Option<Reason> {
    match state.hunk_view() {
        Some(view) if view.lines.is_some() => Some("Leave line mode first".into()),
        _ if state.rendered_shown().is_some() => Some("Search works in the built-in diff".into()),
        _ => None,
    }
}

fn search_step_disabled(state: &AppState) -> Option<Reason> {
    search_disabled(state).or_else(|| {
        let searched = state.hunk_view().is_some_and(|view| view.search.is_some());
        (!searched).then_some("Nothing searched yet (/)".into())
    })
}

/// The preview search works on the built-in diff of a loaded file.
fn preview_search_disabled(state: &AppState) -> Option<Reason> {
    if state.rendered_shown().is_some() {
        Some("Search works in the built-in diff".into())
    } else if state.diff.path.is_none() {
        Some("No file selected".into())
    } else {
        None
    }
}

fn preview_step_disabled(state: &AppState) -> Option<Reason> {
    preview_search_disabled(state).or_else(|| {
        let searched = state.diff.search.is_some();
        (!searched).then_some("Nothing searched yet (ctrl-f)".into())
    })
}

/// The blame, or the commit opened from it, once its lines are in.
fn pane_search_disabled(state: &AppState) -> Option<Reason> {
    let Focus::Blame(view) = &state.focus else {
        return None;
    };
    let (loaded, error, empty) = match &view.commit {
        Some(commit) => (commit.loaded, &commit.error, commit.lines.is_empty()),
        None => (view.loaded, &view.error, view.lines.is_empty()),
    };
    if !loaded {
        Some("Loading…".into())
    } else if error.is_some() || empty {
        Some("Nothing to search".into())
    } else {
        None
    }
}

fn pane_step_disabled(state: &AppState) -> Option<Reason> {
    pane_search_disabled(state).or_else(|| {
        let searched = match &state.focus {
            Focus::Blame(view) => match &view.commit {
                Some(commit) => commit.search.is_some(),
                None => view.search.is_some(),
            },
            _ => false,
        };
        (!searched).then_some("Nothing searched yet (/)".into())
    })
}

pub(crate) const NONE: KeyModifiers = KeyModifiers::NONE;

// Chunk 1: core
static CORE_GLOBAL: &[Binding] = &[
    Binding {
        id: "global.help",
        keys: &[KeyPattern::any(KeyCode::Char('?'))],
        scope: KeyScope::Global,
        intent: AppIntent::OpenHelp,
        description: "Show the keys for this panel",
        label: "keys",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "global.quit",
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
        id: "workspace.refresh",
        keys: &[KeyPattern::exact(KeyCode::Char('r'), NONE)],
        scope: KeyScope::Workspace,
        intent: AppIntent::RefreshStatus,
        description: "Reload the changed files and the branches",
        label: "refresh",
        bar: true,
        disabled: never,
    },
];

// Chunk 3: core
static CORE_SCROLL: &[Binding] = &[
    Binding {
        id: "workspace.scroll_down",
        keys: &[KeyPattern::any(KeyCode::Char('J'))],
        scope: KeyScope::Workspace,
        intent: AppIntent::ScrollDiff(Scroll::LineDown),
        description: "Scroll the diff down a line",
        label: "",
        bar: false,
        disabled: blame_open,
    },
    Binding {
        id: "workspace.scroll_up",
        keys: &[KeyPattern::any(KeyCode::Char('K'))],
        scope: KeyScope::Workspace,
        intent: AppIntent::ScrollDiff(Scroll::LineUp),
        description: "Scroll the diff up a line",
        label: "",
        bar: false,
        disabled: blame_open,
    },
    Binding {
        id: "workspace.page_down",
        keys: &[KeyPattern::any(KeyCode::PageDown)],
        scope: KeyScope::Workspace,
        intent: AppIntent::ScrollDiff(Scroll::PageDown),
        description: "Scroll the diff down a page",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "workspace.page_up",
        keys: &[KeyPattern::any(KeyCode::PageUp)],
        scope: KeyScope::Workspace,
        intent: AppIntent::ScrollDiff(Scroll::PageUp),
        description: "Scroll the diff up a page",
        label: "",
        bar: false,
        disabled: never,
    },
];

// Chunk 6: core
static CORE_FOCUS: &[Binding] = &[
    // Moving between panels and zooming. Out of the bar, which is full at
    // 80 columns; the help menu lists them.
    Binding {
        id: "workspace.focus_status",
        keys: &[KeyPattern::exact(KeyCode::Char('1'), NONE)],
        scope: KeyScope::Workspace,
        intent: AppIntent::FocusPanel(Panel::Side(SidePanel::Status)),
        description: "Focus the Status panel",
        label: "status",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "workspace.focus_files",
        keys: &[KeyPattern::exact(KeyCode::Char('2'), NONE)],
        scope: KeyScope::Workspace,
        intent: AppIntent::FocusPanel(Panel::Side(SidePanel::Files)),
        description: "Focus the Files panel",
        label: "files",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "workspace.focus_branches",
        keys: &[KeyPattern::exact(KeyCode::Char('3'), NONE)],
        scope: KeyScope::Workspace,
        intent: AppIntent::FocusPanel(Panel::Side(SidePanel::Branches)),
        description: "Focus the Branches panel",
        label: "branches",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "workspace.focus_commits",
        keys: &[KeyPattern::exact(KeyCode::Char('4'), NONE)],
        scope: KeyScope::Workspace,
        intent: AppIntent::FocusPanel(Panel::Side(SidePanel::Commits)),
        description: "Focus the Commits panel",
        label: "commits",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "workspace.focus_stash",
        keys: &[KeyPattern::exact(KeyCode::Char('5'), NONE)],
        scope: KeyScope::Workspace,
        intent: AppIntent::FocusPanel(Panel::Side(SidePanel::Stash)),
        description: "Focus the Stash panel",
        label: "stash",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "workspace.focus_diff",
        keys: &[
            KeyPattern::exact(KeyCode::Char('0'), NONE),
            KeyPattern::exact(KeyCode::Char('l'), NONE),
            KeyPattern::any(KeyCode::Right),
        ],
        scope: KeyScope::Workspace,
        intent: AppIntent::FocusPanel(Panel::Diff),
        description: "Focus the diff, to work on the selected file's hunks, or pick lines of a commit's file",
        label: "diff",
        bar: false,
        disabled: focus_diff_disabled,
    },
    Binding {
        id: "workspace.next_panel",
        keys: &[KeyPattern::exact(KeyCode::Tab, NONE)],
        scope: KeyScope::Workspace,
        intent: AppIntent::CycleFocus { back: false },
        description: "Focus the next panel",
        label: "next panel",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "workspace.prev_panel",
        keys: &[KeyPattern::any(KeyCode::BackTab)],
        scope: KeyScope::Workspace,
        intent: AppIntent::CycleFocus { back: true },
        description: "Focus the previous panel",
        label: "previous panel",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "workspace.zoom",
        keys: &[KeyPattern::exact(KeyCode::Char('z'), NONE)],
        scope: KeyScope::Workspace,
        intent: AppIntent::CycleScreenMode,
        description: "Zoom the focused panel: normal, half, full",
        label: "zoom",
        bar: false,
        disabled: never,
    },
];

// Chunk 41: core
static CORE_POPUPS: &[Binding] = &[
    Binding {
        id: "help.close",
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
    // Out of the bar; the help popup's border says how to scroll.
    Binding {
        id: "help.down",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Help,
        intent: AppIntent::ScrollHelp(Scroll::LineDown),
        description: "Scroll the list down",
        label: "scroll",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "help.up",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Help,
        intent: AppIntent::ScrollHelp(Scroll::LineUp),
        description: "Scroll the list up",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "help.page_down",
        keys: &[KeyPattern::any(KeyCode::PageDown)],
        scope: KeyScope::Help,
        intent: AppIntent::ScrollHelp(Scroll::PageDown),
        description: "Scroll the list down a page",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "help.page_up",
        keys: &[KeyPattern::any(KeyCode::PageUp)],
        scope: KeyScope::Help,
        intent: AppIntent::ScrollHelp(Scroll::PageUp),
        description: "Scroll the list up a page",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "menu.next",
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
        id: "menu.prev",
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
        id: "menu.select",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Menu,
        intent: AppIntent::MenuSelect,
        description: "Run the highlighted item",
        label: "select",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "menu.back",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::Menu,
        intent: AppIntent::PopPopup,
        description: "Close this menu and go back to the one below",
        label: "back",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "menu.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::Menu,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "confirm.yes",
        keys: Answer::YOrEnter.keys(),
        scope: KeyScope::Confirm,
        intent: AppIntent::ConfirmYes,
        description: "Yes",
        label: "yes",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "confirm.no",
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
        id: "prompt.ok",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Prompt,
        intent: AppIntent::SubmitPrompt,
        description: "Use the typed text",
        label: "ok",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "prompt.cancel",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::Prompt,
        intent: AppIntent::PopPopup,
        description: "Close without using the text",
        label: "cancel",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "prompt.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::Prompt,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];

// Chunk 43: core
static CORE_ERROR: &[Binding] = &[
    Binding {
        id: "error.close",
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
        id: "error.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::Error,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];

// Chunk 56: branches
static BRANCHES_DELETE_RANGE_CONFIRM: &[Binding] = &[
    // Only `y` deletes a range: Enter is left unbound, as for one branch.
    Binding {
        id: "delete_range.yes",
        keys: Answer::YOnly.keys(),
        scope: KeyScope::DeleteRange,
        intent: AppIntent::ConfirmDeleteRange,
        description: "Yes, delete what the question names",
        label: "delete",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "delete_range.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::DeleteRange,
        intent: AppIntent::ConfirmNo,
        description: "No, keep them all",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "delete_range.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::DeleteRange,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];

// Chunk 72: core
static CORE_FILTER_SEARCH: &[Binding] = &[
    // List filters, ranges and the diff search.
    Binding {
        id: "files.filter",
        keys: &[KeyPattern::any(KeyCode::Char('/'))],
        scope: KeyScope::Files,
        intent: AppIntent::OpenFilter,
        description: "Filter the files by typed text",
        label: "filter",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "files.range",
        keys: &[KeyPattern::any(KeyCode::Char('V'))],
        scope: KeyScope::Files,
        intent: AppIntent::ToggleRange,
        description: "Select a range of files from here, for stage and discard; again to clear it",
        label: "range",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "files.escape",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::Files,
        intent: AppIntent::ListEscape,
        description: "Clear the selection, else the filter, else the diff search, else quit Kika",
        label: "clear",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "commits.filter",
        keys: &[KeyPattern::any(KeyCode::Char('/'))],
        scope: KeyScope::Commits,
        intent: AppIntent::OpenFilter,
        description: "Filter the loaded commits by typed text",
        label: "filter",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "commits.range",
        keys: &[KeyPattern::any(KeyCode::Char('V'))],
        scope: KeyScope::Commits,
        intent: AppIntent::ToggleRange,
        description: "Select a range of commits from here, for copy, squash, fixup and drop; again to clear it",
        label: "range",
        bar: false,
        disabled: commits_range_disabled,
    },
    Binding {
        id: "commits.escape",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::Commits,
        intent: AppIntent::ListEscape,
        description: "Clear the selection, else the filter, else the diff mark, else the log filter, else quit Kika",
        label: "clear",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "branches.filter",
        keys: &[KeyPattern::any(KeyCode::Char('/'))],
        scope: KeyScope::Branches,
        intent: AppIntent::OpenFilter,
        description: "Filter the local branches by typed text",
        label: "filter",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "branches.range",
        keys: &[KeyPattern::any(KeyCode::Char('V'))],
        scope: KeyScope::Branches,
        intent: AppIntent::ToggleRange,
        description: "Select a range from here, for delete; again to clear it",
        label: "range",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "branches.escape",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::Branches,
        intent: AppIntent::ListEscape,
        description: "Clear the selection, else the filter, else quit Kika",
        label: "clear",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "remotes.filter",
        keys: &[KeyPattern::any(KeyCode::Char('/'))],
        scope: KeyScope::Remotes,
        intent: AppIntent::OpenFilter,
        description: "Filter the remotes and their branches by typed text",
        label: "filter",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "remotes.range",
        keys: &[KeyPattern::any(KeyCode::Char('V'))],
        scope: KeyScope::Remotes,
        intent: AppIntent::ToggleRange,
        description: "Select a range from here, for remove; again to clear it",
        label: "range",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "remotes.escape",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::Remotes,
        intent: AppIntent::ListEscape,
        description: "Clear the selection, else the filter, else quit Kika",
        label: "clear",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "tags.filter",
        keys: &[KeyPattern::any(KeyCode::Char('/'))],
        scope: KeyScope::Tags,
        intent: AppIntent::OpenFilter,
        description: "Filter the tags by typed text",
        label: "filter",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "tags.range",
        keys: &[KeyPattern::any(KeyCode::Char('V'))],
        scope: KeyScope::Tags,
        intent: AppIntent::ToggleRange,
        description: "Select a range from here, for delete; again to clear it",
        label: "range",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "tags.escape",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::Tags,
        intent: AppIntent::ListEscape,
        description: "Clear the selection, else the filter, else quit Kika",
        label: "clear",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "diff.search",
        keys: &[KeyPattern::any(KeyCode::Char('/'))],
        scope: KeyScope::Diff,
        intent: AppIntent::OpenSearch,
        description: "Search the diff for typed text",
        label: "search",
        bar: false,
        disabled: search_disabled,
    },
    Binding {
        id: "diff.search_next",
        keys: &[KeyPattern::exact(KeyCode::Char('n'), NONE)],
        scope: KeyScope::Diff,
        intent: AppIntent::SearchNext,
        description: "Go to the next match of the search",
        label: "next match",
        bar: false,
        disabled: search_step_disabled,
    },
    Binding {
        id: "diff.search_prev",
        keys: &[KeyPattern::any(KeyCode::Char('N'))],
        scope: KeyScope::Diff,
        intent: AppIntent::SearchPrev,
        description: "Go to the previous match of the search",
        label: "previous match",
        bar: false,
        disabled: search_step_disabled,
    },
    Binding {
        id: "diff.escape",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::Diff,
        intent: AppIntent::DiffEscape,
        description: "Clear the search, else go back to the side panel",
        label: "back",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "files.search_diff",
        keys: &[KeyPattern::exact(KeyCode::Char('f'), KeyModifiers::CONTROL)],
        scope: KeyScope::Files,
        intent: AppIntent::OpenPreviewSearch,
        description: "Search the selected file's diff for typed text",
        label: "search diff",
        bar: false,
        disabled: preview_search_disabled,
    },
    Binding {
        id: "files.search_next",
        keys: &[KeyPattern::exact(KeyCode::Char('n'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::SearchNext,
        description: "Go to the next match of the diff search",
        label: "next match",
        bar: false,
        disabled: preview_step_disabled,
    },
    Binding {
        id: "files.search_prev",
        keys: &[KeyPattern::any(KeyCode::Char('N'))],
        scope: KeyScope::Files,
        intent: AppIntent::SearchPrev,
        description: "Go to the previous match of the diff search",
        label: "previous match",
        bar: false,
        disabled: preview_step_disabled,
    },
    Binding {
        id: "commit_files.search",
        keys: &[KeyPattern::exact(KeyCode::Char('f'), KeyModifiers::CONTROL)],
        scope: KeyScope::CommitFiles,
        intent: AppIntent::OpenPreviewSearch,
        description: "Search the selected file's change for typed text",
        label: "search diff",
        bar: false,
        disabled: preview_search_disabled,
    },
    Binding {
        id: "commit_files.search_next",
        keys: &[KeyPattern::exact(KeyCode::Char('n'), NONE)],
        scope: KeyScope::CommitFiles,
        intent: AppIntent::SearchNext,
        description: "Go to the next match of the diff search",
        label: "next match",
        bar: false,
        disabled: preview_step_disabled,
    },
    Binding {
        id: "commit_files.search_prev",
        keys: &[KeyPattern::any(KeyCode::Char('N'))],
        scope: KeyScope::CommitFiles,
        intent: AppIntent::SearchPrev,
        description: "Go to the previous match of the diff search",
        label: "previous match",
        bar: false,
        disabled: preview_step_disabled,
    },
    Binding {
        id: "commit_files.filter",
        keys: &[KeyPattern::any(KeyCode::Char('/'))],
        scope: KeyScope::CommitFiles,
        intent: AppIntent::OpenFilter,
        description: "Filter the commit's files by typed text",
        label: "filter",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "commit_files.escape",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::CommitFiles,
        intent: AppIntent::PaneEscape,
        description: "Clear the filter, else the diff search, else go back to the log, at the same commit",
        label: "back",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "stash_files.search",
        keys: &[KeyPattern::exact(KeyCode::Char('f'), KeyModifiers::CONTROL)],
        scope: KeyScope::StashFiles,
        intent: AppIntent::OpenPreviewSearch,
        description: "Search the selected file's change for typed text",
        label: "search diff",
        bar: false,
        disabled: preview_search_disabled,
    },
    Binding {
        id: "stash_files.search_next",
        keys: &[KeyPattern::exact(KeyCode::Char('n'), NONE)],
        scope: KeyScope::StashFiles,
        intent: AppIntent::SearchNext,
        description: "Go to the next match of the diff search",
        label: "next match",
        bar: false,
        disabled: preview_step_disabled,
    },
    Binding {
        id: "stash_files.search_prev",
        keys: &[KeyPattern::any(KeyCode::Char('N'))],
        scope: KeyScope::StashFiles,
        intent: AppIntent::SearchPrev,
        description: "Go to the previous match of the diff search",
        label: "previous match",
        bar: false,
        disabled: preview_step_disabled,
    },
    Binding {
        id: "stash_files.escape",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::StashFiles,
        intent: AppIntent::PaneEscape,
        description: "Clear the diff search, else go back to the stash list, at the same stash",
        label: "back",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "blame.search",
        keys: &[KeyPattern::any(KeyCode::Char('/'))],
        scope: KeyScope::Blame,
        intent: AppIntent::OpenSearch,
        description: "Search the blamed lines' text for typed text",
        label: "search",
        bar: false,
        disabled: pane_search_disabled,
    },
    Binding {
        id: "blame.search_next",
        keys: &[KeyPattern::exact(KeyCode::Char('n'), NONE)],
        scope: KeyScope::Blame,
        intent: AppIntent::SearchNext,
        description: "Go to the next match of the search",
        label: "next match",
        bar: false,
        disabled: pane_step_disabled,
    },
    Binding {
        id: "blame.search_prev",
        keys: &[KeyPattern::any(KeyCode::Char('N'))],
        scope: KeyScope::Blame,
        intent: AppIntent::SearchPrev,
        description: "Go to the previous match of the search",
        label: "previous match",
        bar: false,
        disabled: pane_step_disabled,
    },
    Binding {
        id: "blame.escape",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::Blame,
        intent: AppIntent::PaneEscape,
        description: "Clear the search, else go back to the file list",
        label: "back",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "blame_commit.search",
        keys: &[KeyPattern::any(KeyCode::Char('/'))],
        scope: KeyScope::BlameCommit,
        intent: AppIntent::OpenSearch,
        description: "Search the commit for typed text",
        label: "search",
        bar: false,
        disabled: pane_search_disabled,
    },
    Binding {
        id: "blame_commit.search_next",
        keys: &[KeyPattern::exact(KeyCode::Char('n'), NONE)],
        scope: KeyScope::BlameCommit,
        intent: AppIntent::SearchNext,
        description: "Go to the next match of the search",
        label: "next match",
        bar: false,
        disabled: pane_step_disabled,
    },
    Binding {
        id: "blame_commit.search_prev",
        keys: &[KeyPattern::any(KeyCode::Char('N'))],
        scope: KeyScope::BlameCommit,
        intent: AppIntent::SearchPrev,
        description: "Go to the previous match of the search",
        label: "previous match",
        bar: false,
        disabled: pane_step_disabled,
    },
    Binding {
        id: "blame_commit.escape",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::BlameCommit,
        intent: AppIntent::PaneEscape,
        description: "Clear the search, else go back to the blame, at the same line",
        label: "back",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "input.accept",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Input,
        intent: AppIntent::InputAccept,
        description: "Keep the typed text; a search goes to its match",
        label: "done",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "input.cancel",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::Input,
        intent: AppIntent::InputCancel,
        description: "Clear the typed text and close",
        label: "clear",
        bar: true,
        disabled: never,
    },
];

// Chunk 78: core
#[rustfmt::skip]
static CORE_INPUT_QUIT: &[Binding] = &[
    Binding {
        id: "input.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::Input,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];

/// Every chunk in registry order. The order is behaviour: `resolve` takes
/// the first match in a scope, and help and the bottom bar list bindings in
/// this order. A feature moving out of core changes only its paths here.
static CHUNKS: &[&[Binding]] = &[
    CORE_GLOBAL,
    sync::keys::SYNC_WORKSPACE,
    CORE_SCROLL,
    submodules::keys::SUBMODULES_WORKSPACE,
    diff::keys::DIFF_WORKSPACE,
    CORE_FOCUS,
    commits::custom_patch::keys::CUSTOM_PATCH_WORKSPACE,
    custom_commands::keys::CUSTOM_COMMANDS_WORKSPACE,
    commits::reflog::keys::REFLOG_WORKSPACE,
    files::keys::FILES_NAV,
    commits::keys::COMMITS_KEYS,
    commits::rewrite::keys::REWRITE_COMMITS,
    commits::rewrite::keys::REWRITE_TODO_COMMITS,
    commits::keys::COMMITS_SWITCH_TAB,
    commits::copy::keys::COPY_COMMITS,
    commits::reflog::keys::REFLOG_KEYS,
    commits::keys::COMMITS_FILES_NAV,
    commits::custom_patch::keys::CUSTOM_PATCH_COMMIT_FILES,
    commits::keys::COMMITS_FILES_BACK,
    stash::keys::STASH_KEYS,
    files::keys::FILES_STAGE,
    commit::keys::COMMIT_FROM_FILES,
    files::keys::FILES_DISCARD_OPEN,
    submodules::keys::SUBMODULES_FILES,
    worktrees::keys::WORKTREES_WORKSPACE,
    command_log::keys::COMMAND_LOG_WORKSPACE,
    repos::keys::REPOS_WORKSPACE,
    review::keys::REVIEW_KEYS,
    review::keys::REVIEW_COMMENT_KEYS,
    blame::keys::BLAME_FILES,
    diff::keys::DIFF_FILES,
    conflicts::keys::CONFLICTS_FILES,
    files::keys::FILES_IGNORE,
    diff::keys::DIFF_KEYS,
    renderers::keys::RENDERERS_DIFF,
    diff::keys::DIFF_LINES,
    blame::keys::BLAME_KEYS,
    conflicts::keys::CONFLICTS_KEYS,
    blame::keys::BLAME_COMMIT,
    files::keys::FILES_MENU,
    stash::keys::STASH_FROM_FILES,
    commit::keys::COMMIT_KEYS,
    CORE_POPUPS,
    sync::keys::SYNC_CREDENTIAL,
    CORE_ERROR,
    files::keys::FILES_DISCARD_CONFIRM,
    branches::keys::BRANCHES_KEYS,
    branches::keys::BRANCHES_CHANGE,
    operation::keys::OPERATION_BRANCHES,
    pull_requests::keys::PULL_REQUESTS_BRANCHES,
    branches::keys::BRANCHES_UPSTREAM,
    branches::keys::BRANCHES_TABS,
    remotes::keys::REMOTES_KEYS,
    tags::keys::TAGS_KEYS,
    branches::keys::BRANCHES_AT_HEAD,
    commits::bisect::keys::BISECT_COMMITS,
    operation::keys::OPERATION_STATUS,
    commits::bisect::keys::BISECT_STATUS,
    branches::keys::BRANCHES_DELETE_CONFIRM,
    tags::keys::TAGS_DELETE_CONFIRM,
    BRANCHES_DELETE_RANGE_CONFIRM,
    remotes::keys::REMOTES_REMOVE_CONFIRM,
    commits::keys::COMMITS_RESET_HARD_CONFIRM,
    commits::rewrite::keys::REWRITE_LEAVE_COMMITS_CONFIRM,
    stash::keys::STASH_DROP_CONFIRM,
    operation::keys::OPERATION_ABORT_CONFIRM,
    operation::keys::OPERATION_SKIP_CONFIRM,
    commits::rewrite::keys::REWRITE_AUTOSQUASH_CONFIRM,
    commits::custom_patch::keys::CUSTOM_PATCH_CONFIRM,
    commits::rewrite::keys::REWRITE_DROP_RANGE_CONFIRM,
    commits::reflog::keys::REFLOG_TRAVEL_CONFIRM,
    submodules::keys::SUBMODULES_KEYS,
    worktrees::keys::WORKTREES_KEYS,
    command_log::keys::COMMAND_LOG_KEYS,
    worktrees::keys::WORKTREES_REMOVE_CONFIRM,
    repos::keys::REPOS_RECENT,
    pull_requests::keys::PULL_REQUESTS_PICK_CREATE,
    review::keys::REVIEW_MERGE_CONFIRM,
    review::keys::REVIEW_CHECKOUT_CONFIRM,
    review::keys::REVIEW_COMMENT_CONFIRM,
    CORE_FILTER_SEARCH,
    commits::keys::COMMITS_LOG_FILTER,
    commits::compare::keys::COMPARE_COMMITS,
    commits::reflog::keys::REFLOG_LOG_FILTER,
    commits::compare::keys::COMPARE_BRANCHES,
    commits::keys::COMMITS_FILES_LOG,
    CORE_INPUT_QUIT,
];

/// The application's keymap: `CHUNKS` flattened once, so callers keep a
/// plain slice while the chunks can live with the features that own them.
pub fn registry() -> &'static [Binding] {
    static BINDINGS: LazyLock<Vec<Binding>> = LazyLock::new(|| {
        CHUNKS
            .iter()
            .flat_map(|chunk| chunk.iter().cloned())
            .collect()
    });
    &BINDINGS[..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::branches::BranchesIntent;
    use crate::features::branches::remotes::RemotesIntent;
    use crate::features::branches::tags::TagsIntent;
    use crate::features::commit::CommitIntent;
    use crate::features::commits::CommitsIntent;
    use crate::features::commits::bisect::BisectIntent;
    use crate::features::commits::copy::CopyIntent;
    use crate::features::commits::custom_patch::CustomPatchIntent;
    use crate::features::commits::custom_patch::keys::patch_file_disabled;
    use crate::features::commits::reflog::ReflogIntent;
    use crate::features::commits::rewrite::RewriteIntent;
    use crate::features::commits::rewrite::keys::{
        commits_drop_disabled, commits_fixup_disabled, commits_reword_disabled,
        commits_squash_disabled,
    };
    use crate::features::conflicts::ConflictsIntent;
    use crate::features::custom_commands::{OutputMode, Panel, test_table};
    use crate::features::diff::DiffIntent;
    use crate::features::diff::renderers::RenderersIntent;
    use crate::features::files::FilesIntent;
    use crate::features::pull_requests::PullRequestsIntent;
    use crate::features::repos::ReposIntent;
    use crate::features::review::ReviewIntent;
    use crate::features::stash::StashIntent;
    use crate::features::submodules::SubmodulesIntent;
    use crate::features::sync::SyncIntent;
    use crate::features::worktrees::WorktreesIntent;
    use crate::menu::MenuId;
    use crate::model::Context;
    use crate::test_support::{binding, build, from, key, layer, load_list, load_remotes, plain};
    use kika_git::bisect::BisectMark;
    use kika_git::commit_files::{CommitId, CommitRange};
    use kika_git::rebase_plan::{Direction, Edit};
    use kika_git::status::{EntryKind, Head};
    use kika_git::version;

    fn off(_: &AppState) -> Option<Reason> {
        Some("off".into())
    }

    const X: &[KeyPattern] = &[KeyPattern::any(KeyCode::Char('x'))];
    const Y: &[KeyPattern] = &[KeyPattern::any(KeyCode::Char('y'))];
    const Z: &[KeyPattern] = &[KeyPattern::any(KeyCode::Char('z'))];
    const W: &[KeyPattern] = &[KeyPattern::any(KeyCode::Char('w'))];
    const V: &[KeyPattern] = &[KeyPattern::any(KeyCode::Char('v'))];

    fn bind(keys: &'static [KeyPattern], scope: KeyScope, intent: AppIntent) -> Binding {
        Binding {
            id: "test.binding",
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
            bind(X, KeyScope::Files, Files(FilesIntent::Next)),
            Binding {
                disabled: off,
                ..bind(V, KeyScope::Files, Files(FilesIntent::Prev))
            },
            bind(W, KeyScope::Review, Files(FilesIntent::ToggleStage)),
        ]
    }

    fn run(bindings: &[Binding], focus: KeyScope, state: &AppState, key: KeyPress) -> AppIntent {
        match resolve(bindings, &Keymap::default(), focus, state, key) {
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
            AppIntent::Files(FilesIntent::Next)
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
            resolve(
                &table,
                &Keymap::default(),
                KeyScope::Files,
                &state,
                plain('w')
            ),
            Resolution::Unbound
        ));
        assert_eq!(
            run(&table, KeyScope::Review, &state, plain('w')),
            AppIntent::Files(FilesIntent::ToggleStage)
        );
    }

    #[test]
    fn a_disabled_binding_does_not_fall_through_to_its_parent() {
        let table = table();
        let state = AppState::new(Context::Workspace);
        assert!(matches!(
            resolve(
                &table,
                &Keymap::default(),
                KeyScope::Files,
                &state,
                plain('v')
            ),
            Resolution::Disabled(ref reason) if reason == "off"
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
        let listed: Vec<(KeyScope, char, Option<String>)> =
            available(&table, &Keymap::default(), KeyScope::Files, &state)
                .into_iter()
                .map(|(b, reason)| {
                    let KeyCode::Char(c) = b.keys[0].code else {
                        unreachable!()
                    };
                    (b.scope, c, reason.map(String::from))
                })
                .collect();
        assert_eq!(
            listed,
            [
                (KeyScope::Files, 'x', None),
                (KeyScope::Files, 'v', Some("off".to_string())),
                (KeyScope::Workspace, 'y', None),
                (KeyScope::Global, 'z', None),
            ]
        );
    }

    #[test]
    fn a_partly_shadowed_binding_stays_available() {
        let state = AppState::new(Context::Workspace);
        let help = available(registry(), &Keymap::default(), KeyScope::Help, &state);
        let intents: Vec<&AppIntent> = help.iter().map(|(b, _)| &b.intent).collect();
        // Esc is taken by CloseHelp, but q still quits; `?` to open is gone.
        assert_eq!(
            intents,
            [
                &AppIntent::CloseHelp,
                &AppIntent::ScrollHelp(Scroll::LineDown),
                &AppIntent::ScrollHelp(Scroll::LineUp),
                &AppIntent::ScrollHelp(Scroll::PageDown),
                &AppIntent::ScrollHelp(Scroll::PageUp),
                &AppIntent::Quit
            ]
        );
    }

    #[test]
    fn a_repository_layer_cannot_put_enter_back_on_the_abort_or_skip_question() {
        for id in [
            "abort_confirm.yes",
            "abort_confirm.no",
            "abort_confirm.quit",
            "skip_confirm.yes",
            "skip_confirm.no",
            "skip_confirm.quit",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
        }
        assert_eq!(
            build(&[("abort_confirm.yes", &["enter"])], Trust::Repo).unwrap_err(),
            r#""abort_confirm.yes" can only be changed in the global config file"#
        );
        assert_eq!(
            build(&[("skip_confirm.yes", &["enter"])], Trust::Repo).unwrap_err(),
            r#""skip_confirm.yes" can only be changed in the global config file"#
        );
        assert_eq!(
            build(&[("discard_confirm.yes", &["enter"])], Trust::Repo).unwrap_err(),
            r#""discard_confirm.yes" can only be changed in the global config file"#
        );
    }

    #[test]
    fn the_commits_panel_moves_its_cursor_and_falls_back_to_the_workspace() {
        use AppIntent::*;
        assert_eq!(KeyScope::Commits.parent(), Some(KeyScope::Workspace));
        assert_eq!(KeyScope::Commits.title(), "Commits");
        let commits = |k| from(KeyScope::Commits, k);
        let none = KeyModifiers::NONE;
        assert_eq!(commits(plain('j')), Some(Commits(CommitsIntent::Next)));
        assert_eq!(
            commits(key(KeyCode::Down, none)),
            Some(Commits(CommitsIntent::Next))
        );
        assert_eq!(commits(plain('k')), Some(Commits(CommitsIntent::Prev)));
        assert_eq!(
            commits(key(KeyCode::Up, none)),
            Some(Commits(CommitsIntent::Prev))
        );
        assert_eq!(commits(plain('r')), Some(RefreshStatus));
        assert_eq!(commits(plain('q')), Some(Quit));
    }

    #[test]
    fn enter_opens_a_commits_files_where_esc_goes_back_instead_of_quitting() {
        use AppIntent::*;
        let none = KeyModifiers::NONE;
        assert_eq!(
            from(KeyScope::Commits, key(KeyCode::Enter, none)),
            Some(Commits(CommitsIntent::OpenFiles))
        );
        assert_eq!(KeyScope::CommitFiles.parent(), Some(KeyScope::Workspace));
        assert_eq!(KeyScope::CommitFiles.title(), "Commit files");
        let files = |k| from(KeyScope::CommitFiles, k);
        // Esc clears a search first, then goes back.
        assert_eq!(files(key(KeyCode::Esc, none)), Some(PaneEscape));
        for k in [plain('h'), key(KeyCode::Left, none)] {
            assert_eq!(files(k), Some(Commits(CommitsIntent::CloseFiles)), "{k:?}");
        }
        assert_eq!(files(plain('j')), Some(Commits(CommitsIntent::FileNext)));
        assert_eq!(
            files(key(KeyCode::Down, none)),
            Some(Commits(CommitsIntent::FileNext))
        );
        assert_eq!(files(plain('k')), Some(Commits(CommitsIntent::FilePrev)));
        assert_eq!(
            files(key(KeyCode::Up, none)),
            Some(Commits(CommitsIntent::FilePrev))
        );
        assert_eq!(files(plain('q')), Some(Quit));
        assert_eq!(files(plain('J')), Some(ScrollDiff(Scroll::LineDown)));
    }

    #[test]
    fn existing_keys_keep_their_meaning_from_the_files_panel() {
        use AppIntent::*;
        let files = |k| from(KeyScope::Files, k);
        let none = KeyModifiers::NONE;
        let shift = KeyModifiers::SHIFT;
        assert_eq!(files(plain('q')), Some(Quit));
        // Esc clears a range or a filter first, and quits with neither.
        assert_eq!(files(key(KeyCode::Esc, none)), Some(ListEscape));
        assert_eq!(
            files(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Quit)
        );
        assert_eq!(files(plain('r')), Some(RefreshStatus));
        assert_eq!(files(plain('j')), Some(Files(FilesIntent::Next)));
        assert_eq!(
            files(key(KeyCode::Down, none)),
            Some(Files(FilesIntent::Next))
        );
        assert_eq!(files(plain('k')), Some(Files(FilesIntent::Prev)));
        assert_eq!(
            files(key(KeyCode::Up, none)),
            Some(Files(FilesIntent::Prev))
        );
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
        assert_eq!(files(plain(' ')), Some(Files(FilesIntent::ToggleStage)));
        assert_eq!(files(plain('a')), Some(Files(FilesIntent::ToggleStageAll)));
        assert_eq!(files(plain('c')), Some(Commit(CommitIntent::Open)));
        assert_eq!(files(key(KeyCode::Char('a'), KeyModifiers::CONTROL)), None);
        assert_eq!(files(plain('x')), None);
    }

    #[test]
    fn review_has_no_workspace_keys_but_still_quits() {
        let review = |k| from(KeyScope::Review, k);
        assert_eq!(
            review(plain('r')),
            Some(AppIntent::Review(ReviewIntent::Refresh))
        );
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
        let c = |state: &AppState| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::Files,
            state,
            plain('c'),
        ) {
            Resolution::Run(binding) => Ok(binding.intent.clone()),
            Resolution::Disabled(reason) => Err(reason),
            Resolution::Unbound | Resolution::Command(_) => panic!("c is unbound"),
        };
        let mut state = AppState::new(Context::Workspace);
        state.hold_op(1);
        assert_eq!(c(&state), Err("An operation is running".into()));
        state.free_op();
        assert_eq!(c(&state), Err("Nothing to commit".into()));
        let file = |kind| kika_git::FileEntry {
            path: "a.rs".into(),
            orig_path: None,
            index: 'U',
            worktree: 'U',
            kind,
        };
        state.files.rows = vec![file(EntryKind::Changed), file(EntryKind::Conflicted)];
        assert_eq!(c(&state), Err("Resolve conflicts before committing".into()));
        state.hold_op(1);
        assert_eq!(c(&state), Err("An operation is running".into()));
        state.free_op();
        state.files.rows.pop();
        assert_eq!(c(&state), Ok(AppIntent::Commit(CommitIntent::Open)));
    }

    #[test]
    fn shift_a_amends_unless_an_op_runs_there_is_no_commit_or_there_are_conflicts() {
        use kika_git::status::{BranchStatus, Head};

        let a = |state: &AppState| {
            let k = key(KeyCode::Char('A'), KeyModifiers::SHIFT);
            match resolve(registry(), &Keymap::default(), KeyScope::Files, state, k) {
                Resolution::Run(binding) => Ok(binding.intent.clone()),
                Resolution::Disabled(reason) => Err(reason),
                Resolution::Unbound | Resolution::Command(_) => panic!("A is unbound"),
            }
        };
        let branch = |head| BranchStatus {
            head,
            upstream: None,
            ahead_behind: None,
            in_progress: vec![],
            bisect: None,
        };
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(
            a(&state),
            Err("No commit to amend".into()),
            "before the first load"
        );
        state.branch = Some(branch(Head::Unborn("main".into())));
        assert_eq!(a(&state), Err("No commit to amend".into()));

        // A clean tree with commits: nothing staged is a message-only amend.
        state.branch = Some(branch(Head::Branch("main".into())));
        assert!(state.files.rows.is_empty());
        assert_eq!(a(&state), Ok(AppIntent::Commit(CommitIntent::OpenAmend)));
        state.branch = Some(branch(Head::Detached("abc".into())));
        assert_eq!(a(&state), Ok(AppIntent::Commit(CommitIntent::OpenAmend)));

        state.hold_op(1);
        assert_eq!(a(&state), Err(OP_RUNNING.into()));
        state.free_op();
        state.files.rows = vec![kika_git::FileEntry {
            path: "a.rs".into(),
            orig_path: None,
            index: 'U',
            worktree: 'U',
            kind: EntryKind::Conflicted,
        }];
        assert_eq!(a(&state), Err("Resolve conflicts before amending".into()));
    }

    #[test]
    fn loading_the_last_commit_only_cancels_or_quits() {
        use AppIntent::*;
        assert_eq!(KeyScope::CommitLoading.parent(), None);
        let loading = |k| from(KeyScope::CommitLoading, k);
        assert_eq!(
            loading(key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(Commit(CommitIntent::Cancel))
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
        assert_eq!(
            confirm(plain('y')),
            Some(Commit(CommitIntent::ConfirmStageAll))
        );
        assert_eq!(confirm(enter), Some(Commit(CommitIntent::ConfirmStageAll)));
        assert_eq!(confirm(plain('n')), Some(Commit(CommitIntent::Cancel)));
        assert_eq!(confirm(esc), Some(Commit(CommitIntent::Cancel)));

        let editor = |k| from(KeyScope::CommitEditor, k);
        assert_eq!(editor(ctrl('s')), Some(Commit(CommitIntent::Submit)));
        assert_eq!(editor(esc), Some(Commit(CommitIntent::Cancel)));
        assert_eq!(
            editor(key(KeyCode::Tab, none)),
            Some(Commit(CommitIntent::ToggleField))
        );
        assert_eq!(editor(enter), None, "Enter is typed into the message");

        let error = |k| from(KeyScope::CommitError, k);
        assert_eq!(error(esc), Some(Commit(CommitIntent::DismissError)));
        assert_eq!(error(enter), Some(Commit(CommitIntent::DismissError)));

        assert_eq!(editor(ctrl('c')), Some(Commit(CommitIntent::Interrupt)));

        let discard = |k| from(KeyScope::CommitDiscard, k);
        assert_eq!(discard(plain('y')), Some(Commit(CommitIntent::Discard)));
        assert_eq!(discard(plain('n')), Some(Commit(CommitIntent::Resume)));
        assert_eq!(discard(esc), Some(Commit(CommitIntent::Resume)));
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
        assert_eq!(
            bound(KeyScope::Diff, plain('e')),
            Some(Diff(DiffIntent::EditHunk))
        );
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
    fn d_is_disabled_while_an_op_runs_or_without_a_file() {
        let d = |state: &AppState| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::Files,
            state,
            plain('d'),
        ) {
            Resolution::Disabled(reason) => Some(reason),
            Resolution::Run(_) => None,
            Resolution::Unbound | Resolution::Command(_) => panic!("d is unbound"),
        };
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(d(&state), Some("No file selected".into()));
        state.hold_op(1);
        assert_eq!(d(&state), Some("An operation is running".into()));
    }

    #[test]
    fn the_branches_panel_has_its_own_keys_and_reaches_the_workspace_ones() {
        use kika_git::branches::{Branch, Track};
        let mut state = AppState::new(Context::Workspace);
        let branch = |name: &str, head| Branch {
            name: name.into(),
            head,
            upstream: None,
            track: Track::None,
            merged: true,
        };
        load_list(
            &mut state.branches.list,
            Ok(vec![branch("main", true), branch("topic", false)]),
        );
        state.branches.list.move_by(1);
        let run = |state: &AppState, key| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::Branches,
            state,
            key,
        ) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            other => panic!("{key:?}: {other:?}"),
        };
        let shift = |c| key(KeyCode::Char(c), KeyModifiers::SHIFT);
        assert_eq!(
            run(&state, plain('j')),
            Some(AppIntent::Branches(BranchesIntent::Next))
        );
        assert_eq!(
            run(&state, key(KeyCode::Up, KeyModifiers::NONE)),
            Some(AppIntent::Branches(BranchesIntent::Prev))
        );
        assert_eq!(
            run(&state, plain(' ')),
            Some(AppIntent::Branches(BranchesIntent::Checkout))
        );
        assert_eq!(
            run(&state, plain('n')),
            Some(AppIntent::Branches(BranchesIntent::New))
        );
        assert_eq!(
            run(&state, shift('R')),
            Some(AppIntent::Branches(BranchesIntent::Rename))
        );
        assert_eq!(
            run(&state, plain('d')),
            Some(AppIntent::Branches(BranchesIntent::Delete))
        );
        // `r` still reloads, and the digits still move focus.
        assert_eq!(run(&state, plain('r')), Some(AppIntent::RefreshStatus));
        assert_eq!(
            run(&state, plain('2')),
            Some(AppIntent::FocusPanel(crate::layout::Panel::Side(
                SidePanel::Files
            )))
        );
        assert_eq!(
            run(&state, shift('J')),
            Some(AppIntent::ScrollDiff(Scroll::LineDown))
        );

        state.branches.list.move_by(-1);
        for c in [' ', 'd'] {
            assert!(matches!(
                resolve(
                    registry(),
                    &Keymap::default(),
                    KeyScope::Branches,
                    &state,
                    plain(c)
                ),
                Resolution::Disabled(_)
            ));
        }
    }

    #[test]
    fn the_remotes_tab_takes_its_own_keys_and_the_workspace_keys_still_reach_it() {
        use kika_git::remotes::{Remote, RemoteBranch};
        let mut state =
            AppState::new(Context::Workspace).with_location(Some("/r".into()), "/r".into());
        load_remotes(
            &mut state.branches.remotes.list,
            Ok(vec![Remote {
                name: "origin".into(),
                url: Some("/srv".into()),
                configured: true,
                branches: vec![RemoteBranch {
                    remote: "origin".into(),
                    short: b"main".to_vec(),
                    refname: b"refs/remotes/origin/main".to_vec(),
                }],
            }]),
        );
        let resolve_in = |state: &AppState, key| {
            resolve(
                registry(),
                &Keymap::default(),
                KeyScope::Remotes,
                state,
                key,
            )
        };
        let run = |state: &AppState, key| match resolve_in(state, key) {
            Resolution::Run(binding) => binding.intent.clone(),
            other => panic!("{key:?}: {other:?}"),
        };
        let shift = |c| key(KeyCode::Char(c), KeyModifiers::SHIFT);
        assert_eq!(
            run(&state, plain('j')),
            AppIntent::Branches(BranchesIntent::Remotes(RemotesIntent::Next))
        );
        assert_eq!(
            run(&state, key(KeyCode::Up, KeyModifiers::NONE)),
            AppIntent::Branches(BranchesIntent::Remotes(RemotesIntent::Prev))
        );
        assert_eq!(
            run(&state, plain('a')),
            AppIntent::Branches(BranchesIntent::Remotes(RemotesIntent::Add))
        );
        assert_eq!(
            run(&state, shift('R')),
            AppIntent::Branches(BranchesIntent::Remotes(RemotesIntent::Rename))
        );
        assert_eq!(
            run(&state, plain('d')),
            AppIntent::Branches(BranchesIntent::Remotes(RemotesIntent::Remove))
        );
        assert_eq!(
            run(&state, plain('[')),
            AppIntent::Branches(BranchesIntent::PrevTab)
        );
        assert_eq!(
            run(&state, plain(']')),
            AppIntent::Branches(BranchesIntent::SwitchTab)
        );
        assert_eq!(run(&state, plain('r')), AppIntent::RefreshStatus);
        assert_eq!(run(&state, plain('f')), AppIntent::Sync(SyncIntent::Fetch));
        // A header row is a remote, not a branch to check out.
        assert!(matches!(
            resolve_in(&state, plain(' ')),
            Resolution::Disabled(ref reason) if reason == "Select a remote branch"
        ));
        state.branches.remotes.list.move_by(1);
        assert_eq!(
            run(&state, plain(' ')),
            AppIntent::Branches(BranchesIntent::Remotes(RemotesIntent::Track))
        );
        assert_eq!(KeyScope::Remotes.parent(), Some(KeyScope::Workspace));

        let branches = |key| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::Branches,
            &state,
            key,
        ) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            Resolution::Disabled(_) => None,
            other => panic!("{key:?}: {other:?}"),
        };
        assert_eq!(
            branches(plain('[')),
            Some(AppIntent::Branches(BranchesIntent::PrevTab))
        );
        assert_eq!(
            branches(plain(']')),
            Some(AppIntent::Branches(BranchesIntent::SwitchTab))
        );
        // No branch is selected, so both are refused.
        assert_eq!(branches(plain('u')), None);
        assert_eq!(branches(shift('U')), None);
    }

    fn tag_ref(name: &str) -> kika_git::tags::TagRef {
        let oid = "1".repeat(40);
        kika_git::tags::TagRef {
            name: name.into(),
            target: CommitId::parse(&oid).unwrap(),
            target_short: oid[..7].into(),
            ref_oid: oid,
            annotated: false,
            date: "d".into(),
            subject: "s".into(),
        }
    }

    fn remote_named(name: &str, configured: bool) -> kika_git::remotes::Remote {
        kika_git::remotes::Remote {
            name: name.into(),
            url: configured.then(|| "/srv".into()),
            configured,
            branches: vec![],
        }
    }

    #[test]
    fn the_tags_tab_moves_deletes_pushes_and_switches_tabs() {
        let mut state =
            AppState::new(Context::Workspace).with_location(Some("/r".into()), "/r".into());
        let resolve_in = |state: &AppState, key| {
            resolve(registry(), &Keymap::default(), KeyScope::Tags, state, key)
        };
        let shift_p = key(KeyCode::Char('P'), KeyModifiers::SHIFT);
        let disabled = |state: &AppState, key| match resolve_in(state, key) {
            Resolution::Disabled(reason) => reason,
            other => panic!("{key:?}: {other:?}"),
        };
        assert_eq!(disabled(&state, plain('d')), "No tag");
        assert_eq!(disabled(&state, shift_p), "No tag");

        load_list(&mut state.branches.tags, Ok(vec![tag_ref("v1")]));
        let run = |state: &AppState, key| match resolve_in(state, key) {
            Resolution::Run(binding) => binding.intent.clone(),
            other => panic!("{key:?}: {other:?}"),
        };
        assert_eq!(
            run(&state, plain('j')),
            AppIntent::Branches(BranchesIntent::Tags(TagsIntent::Next))
        );
        assert_eq!(
            run(&state, key(KeyCode::Down, KeyModifiers::NONE)),
            AppIntent::Branches(BranchesIntent::Tags(TagsIntent::Next))
        );
        assert_eq!(
            run(&state, plain('k')),
            AppIntent::Branches(BranchesIntent::Tags(TagsIntent::Prev))
        );
        assert_eq!(
            run(&state, key(KeyCode::Up, KeyModifiers::NONE)),
            AppIntent::Branches(BranchesIntent::Tags(TagsIntent::Prev))
        );
        assert_eq!(
            run(&state, plain('d')),
            AppIntent::Branches(BranchesIntent::Tags(TagsIntent::Delete))
        );
        assert_eq!(
            run(&state, plain(']')),
            AppIntent::Branches(BranchesIntent::SwitchTab)
        );
        assert_eq!(
            run(&state, plain('[')),
            AppIntent::Branches(BranchesIntent::PrevTab)
        );
        assert_eq!(run(&state, plain('f')), AppIntent::Sync(SyncIntent::Fetch));
        assert_eq!(KeyScope::Tags.parent(), Some(KeyScope::Workspace));

        // `P` is the tag's push here, never the branch's.
        assert_eq!(disabled(&state, shift_p), "No remote");
        load_remotes(
            &mut state.branches.remotes.list,
            Ok(vec![
                remote_named("fork", true),
                remote_named("origin", true),
                remote_named("stray", false),
            ]),
        );
        assert_eq!(
            disabled(&state, shift_p),
            "Several remotes and no upstream to choose from"
        );
        load_list(
            &mut state.branches.list,
            Ok(vec![kika_git::branches::Branch {
                name: "main".into(),
                head: true,
                upstream: Some("origin/main".into()),
                track: kika_git::branches::Track::None,
                merged: true,
            }]),
        );
        assert_eq!(
            run(&state, shift_p),
            AppIntent::Branches(BranchesIntent::Tags(TagsIntent::Push))
        );
        assert_eq!(push_tag_remote(&state), Ok("origin".into()));
        state.remote = Some(crate::remote_run::RemoteRun::new(
            kika_git::remote::RemoteOp::Fetch,
            1,
            None,
        ));
        assert_eq!(disabled(&state, shift_p), REMOTE_BUSY);
    }

    #[test]
    fn a_tag_goes_to_the_upstreams_remote_or_the_only_one() {
        let mut state = AppState::new(Context::Workspace);
        let branch = |upstream: Option<&str>| kika_git::branches::Branch {
            name: "main".into(),
            head: true,
            upstream: upstream.map(Into::into),
            track: kika_git::branches::Track::None,
            merged: true,
        };
        load_remotes(
            &mut state.branches.remotes.list,
            Ok(vec![
                remote_named("a", true),
                remote_named("a/b", true),
                remote_named("only-group", false),
            ]),
        );
        // A name with `/` owns the upstream it is the longest prefix of.
        load_list(&mut state.branches.list, Ok(vec![branch(Some("a/b/main"))]));
        assert_eq!(push_tag_remote(&state), Ok("a/b".into()));
        load_list(&mut state.branches.list, Ok(vec![branch(Some("a/main"))]));
        assert_eq!(push_tag_remote(&state), Ok("a".into()));
        // An upstream no remote owns, such as a local branch, chooses none.
        load_list(&mut state.branches.list, Ok(vec![branch(Some("main2"))]));
        assert!(push_tag_remote(&state).is_err());
        load_remotes(
            &mut state.branches.remotes.list,
            Ok(vec![remote_named("solo", true)]),
        );
        assert_eq!(push_tag_remote(&state), Ok("solo".into()));
    }

    #[test]
    fn the_commits_panel_checks_out_reverts_and_resets_the_selected_commit() {
        use AppIntent::*;
        let commits = |k| from(KeyScope::Commits, k);
        assert_eq!(commits(plain(' ')), Some(Commits(CommitsIntent::Checkout)));
        assert_eq!(
            commits(key(KeyCode::Char('R'), KeyModifiers::SHIFT)),
            Some(Commits(CommitsIntent::Revert))
        );
        assert_eq!(
            commits(key(KeyCode::Char('X'), KeyModifiers::SHIFT)),
            Some(OpenMenu(MenuId::Reset))
        );
    }

    /// A workspace with a change, a commit and another branch to act on,
    /// and a bisect in progress.
    fn bisecting() -> AppState {
        let mut state = AppState::new(Context::Workspace);
        state.files.rows = vec![kika_git::FileEntry {
            path: "a.rs".into(),
            orig_path: None,
            index: 'M',
            worktree: '.',
            kind: EntryKind::Changed,
        }];
        state.files.selected = 0;
        state.commits.list.rows = vec![kika_git::log::CommitRow {
            oid: "abc".into(),
            short_oid: "abc".into(),
            parents: vec!["p".into()],
            author: "Ann".into(),
            time: 0,
            subject: "s".into(),
            subject_altered: false,
        }];
        load_list(
            &mut state.branches.list,
            Ok(vec![kika_git::branches::Branch {
                name: "topic".into(),
                head: false,
                upstream: None,
                track: kika_git::branches::Track::None,
                merged: false,
            }]),
        );
        state.branch = Some(kika_git::status::BranchStatus {
            head: Head::Detached("abc".into()),
            upstream: None,
            ahead_behind: None,
            in_progress: vec![Operation::Bisect],
            bisect: None,
        });
        state
    }

    #[test]
    fn during_a_bisect_the_bar_and_help_say_to_reset_it_first() {
        let state = bisecting();
        let reason = |scope, k| match resolve(registry(), &Keymap::default(), scope, &state, k) {
            Resolution::Disabled(reason) => Some(reason),
            other => panic!("{k:?} in {scope:?}: {other:?}"),
        };
        let reset_first = Some(crate::bisect::RESET_FIRST.into());
        let shift = |c| key(KeyCode::Char(c), KeyModifiers::SHIFT);
        for (scope, k) in [
            (KeyScope::Commits, plain(' ')),
            (KeyScope::Commits, shift('R')),
            (KeyScope::Commits, shift('X')),
            (KeyScope::Files, plain('c')),
            (KeyScope::Files, shift('A')),
            (KeyScope::Branches, plain(' ')),
            (KeyScope::Branches, plain('n')),
            (KeyScope::Branches, plain('m')),
        ] {
            assert_eq!(reason(scope, k), reset_first, "{k:?} in {scope:?}");
        }
        let help = available(registry(), &Keymap::default(), KeyScope::Branches, &state);
        let listed = |id| {
            help.iter()
                .find(|(b, _)| b.id == id)
                .map(|(_, r)| r.clone())
        };
        assert_eq!(listed("branches.new"), Some(reset_first.clone()));
        // Renaming moves nothing.
        assert_eq!(listed("branches.rename"), Some(None));
    }

    #[test]
    fn the_commits_panel_squashes_fixes_up_drops_and_rewords() {
        let ids = ['a', 'b', 'c'].map(|d| d.to_string().repeat(40));
        let mut state = AppState::new(Context::Workspace);
        state.commits.list.rows = (0..2)
            .map(|i| kika_git::log::CommitRow {
                oid: ids[i].clone(),
                short_oid: "x".into(),
                parents: vec![ids[i + 1].clone()],
                author: "Ann".into(),
                time: 0,
                subject: "s".into(),
                subject_altered: false,
            })
            .collect();
        state.commits.tips = Some(vec![]);
        state.git_version = Some(version::BREAK);
        let on = |state: &AppState, key| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::Commits,
            state,
            key,
        ) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            other => panic!("{key:?}: {other:?}"),
        };
        assert_eq!(
            on(&state, plain('s')),
            Some(AppIntent::Commits(CommitsIntent::Rewrite(
                RewriteIntent::Edit(Edit::Squash)
            )))
        );
        assert_eq!(
            on(&state, key(KeyCode::Char('F'), KeyModifiers::SHIFT)),
            Some(AppIntent::Commits(CommitsIntent::Rewrite(
                RewriteIntent::Edit(Edit::Fixup)
            )))
        );
        assert_eq!(
            on(&state, plain('d')),
            Some(AppIntent::Commits(CommitsIntent::Rewrite(
                RewriteIntent::Edit(Edit::Drop)
            )))
        );
        assert_eq!(
            on(&state, plain('e')),
            Some(AppIntent::Commits(CommitsIntent::Rewrite(
                RewriteIntent::Edit(Edit::Reword)
            )))
        );
        assert_eq!(
            on(&state, key(KeyCode::Char('E'), KeyModifiers::SHIFT)),
            Some(AppIntent::Commits(CommitsIntent::Rewrite(
                RewriteIntent::Edit(Edit::Stop)
            )))
        );
        assert_eq!(
            on(&state, plain('b')),
            Some(AppIntent::Commits(CommitsIntent::Rewrite(
                RewriteIntent::InsertBreak
            )))
        );
        let alt = KeyModifiers::ALT;
        let (up, down) = (
            Some(AppIntent::Commits(CommitsIntent::Rewrite(
                RewriteIntent::Move(Direction::Up),
            ))),
            Some(AppIntent::Commits(CommitsIntent::Rewrite(
                RewriteIntent::Move(Direction::Down),
            ))),
        );
        state.commits.list.selected = 1;
        assert_eq!(on(&state, key(KeyCode::Char('k'), alt)), up);
        assert_eq!(on(&state, key(KeyCode::Up, alt)), up);
        state.commits.list.selected = 0;
        assert_eq!(on(&state, key(KeyCode::Char('j'), alt)), down);
        assert_eq!(on(&state, key(KeyCode::Down, alt)), down);
        // Plain arrows still move the cursor.
        assert_eq!(
            on(&state, key(KeyCode::Down, KeyModifiers::NONE)),
            Some(AppIntent::Commits(CommitsIntent::Next))
        );
        assert_eq!(
            on(&state, key(KeyCode::Up, KeyModifiers::NONE)),
            Some(AppIntent::Commits(CommitsIntent::Prev))
        );
        // Only plain arrows move the cursor now: Alt+arrow moves the
        // commit, and Shift+arrow is left unbound.
        for code in [KeyCode::Down, KeyCode::Up] {
            assert!(matches!(
                resolve(
                    registry(),
                    &Keymap::default(),
                    KeyScope::Commits,
                    &state,
                    key(code, KeyModifiers::SHIFT)
                ),
                Resolution::Unbound
            ));
        }
        // A log row is a pick already; `p` stays pull.
        assert!(matches!(
            resolve(
                registry(),
                &Keymap::default(),
                KeyScope::Commits,
                &state,
                plain('u')
            ),
            Resolution::Disabled(ref reason) if reason == "Already a pick"
        ));
        assert!(matches!(
            resolve(
                registry(),
                &Keymap::default(),
                KeyScope::Commits,
                &state,
                plain('p')
            ),
            Resolution::Run(Binding {
                id: "workspace.pull",
                ..
            }) | Resolution::Disabled(_)
        ));
        for id in [
            "commits.squash",
            "commits.fixup",
            "commits.drop",
            "commits.reword",
            "commits.move_up",
            "commits.move_down",
            "commits.edit",
            "commits.break",
            "commits.pick",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
            assert!(build(&[(id, &["Z"])], Trust::Repo).is_err(), "{id}");
        }
    }

    #[test]
    fn the_hard_reset_question_has_no_enter_and_n_or_esc_keeps_the_work() {
        let state = AppState::new(Context::Workspace);
        let resolve_in = |key| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::ResetHard,
            &state,
            key,
        ) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            Resolution::Unbound => None,
            other => panic!("{key:?}: {other:?}"),
        };
        let none = KeyModifiers::NONE;
        assert_eq!(resolve_in(key(KeyCode::Enter, none)), None);
        assert_eq!(resolve_in(plain('q')), None);
        assert_eq!(
            resolve_in(plain('y')),
            Some(AppIntent::Commits(CommitsIntent::ConfirmResetHard))
        );
        assert_eq!(resolve_in(plain('n')), Some(AppIntent::ConfirmNo));
        assert_eq!(
            resolve_in(key(KeyCode::Esc, none)),
            Some(AppIntent::ConfirmNo)
        );
        assert_eq!(
            resolve_in(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(AppIntent::Quit)
        );
        assert_eq!(KeyScope::ResetHard.parent(), None);
        assert_eq!(KeyScope::ResetHard.title(), "Reset hard");
    }

    #[test]
    fn o_opens_the_pull_request_menu_in_branches_alone_and_help_lists_it() {
        // Nothing else in Branches or the scopes around it has `o`.
        let o = KeyPattern::exact(KeyCode::Char('o'), NONE);
        let holders: Vec<&str> = registry()
            .iter()
            .filter(|b| KeyScope::Branches.chain().any(|scope| scope == b.scope))
            .filter(|b| b.keys.iter().any(|k| k.overlaps(&o)))
            .map(|b| b.id)
            .collect();
        assert_eq!(holders, ["branches.pull_request"]);
        assert!(Keymap::build(&Keymap::default(), &KeysTable::default(), Trust::Global).is_ok());

        let mut state = AppState::new(Context::Workspace);
        let listed = |state: &AppState| {
            available(registry(), &Keymap::default(), KeyScope::Branches, state)
                .into_iter()
                .find(|(b, _)| b.id == "branches.pull_request")
                .map(|(_, disabled)| disabled)
        };
        assert_eq!(listed(&state), Some(Some("No branch selected".into())));
        load_list(
            &mut state.branches.list,
            Ok(vec![kika_git::branches::Branch {
                name: "topic".into(),
                head: false,
                upstream: None,
                track: kika_git::branches::Track::None,
                merged: false,
            }]),
        );
        assert_eq!(listed(&state), Some(None));
        match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::Branches,
            &state,
            plain('o'),
        ) {
            Resolution::Run(binding) => {
                assert_eq!(binding.intent, AppIntent::OpenMenu(MenuId::PullRequest))
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn shift_b_refreshes_pull_requests_in_branches_and_says_why_when_it_cannot() {
        let shift_b = KeyPattern::any(KeyCode::Char('B'));
        let holders: Vec<&str> = registry()
            .iter()
            .filter(|b| KeyScope::Branches.chain().any(|scope| scope == b.scope))
            .filter(|b| b.keys.iter().any(|k| k.overlaps(&shift_b)))
            .map(|b| b.id)
            .collect();
        assert_eq!(holders, ["branches.refresh_pull_requests"]);

        let press = |state: &AppState| {
            resolve(
                registry(),
                &Keymap::default(),
                KeyScope::Branches,
                state,
                KeyPress::new(KeyCode::Char('B'), KeyModifiers::SHIFT),
            )
        };
        let mut state = AppState::new(Context::Workspace);
        match press(&state) {
            Resolution::Disabled(reason) => assert_eq!(
                reason,
                "Name GitHub hosts under [forge] in the global config"
            ),
            other => panic!("{other:?}"),
        }
        state.forge =
            kika_forge::ForgeState::new(vec![kika_forge::ForgeHost::parse("github.com").unwrap()]);
        state.forge.repo = Some(kika_forge::ForgeRepo {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "widgets".into(),
        });
        match press(&state) {
            Resolution::Run(binding) => {
                assert_eq!(
                    binding.intent,
                    AppIntent::PullRequests(PullRequestsIntent::Refresh)
                )
            }
            other => panic!("{other:?}"),
        }

        // Help lists it with what it does, greyed out with why when off.
        let listed = |state: &AppState| {
            available(registry(), &Keymap::default(), KeyScope::Branches, state)
                .into_iter()
                .find(|(b, _)| b.id == "branches.refresh_pull_requests")
                .map(|(b, disabled)| (b.description, disabled))
        };
        assert_eq!(
            listed(&state),
            Some(("Ask GitHub again for the branches' pull requests", None))
        );
        state.forge.in_flight = true;
        assert_eq!(
            listed(&state),
            Some((
                "Ask GitHub again for the branches' pull requests",
                Some("Pull requests are already being refreshed".into())
            ))
        );
    }

    #[test]
    fn a_repository_layer_cannot_move_or_empty_the_pull_request_refresh() {
        let id = "branches.refresh_pull_requests";
        assert!(REPO_LOCKED.contains(&id));
        let expected = format!("{id:?} can only be changed in the global config file");
        assert_eq!(build(&[(id, &["r"])], Trust::Repo).unwrap_err(), expected);
        assert_eq!(build(&[(id, &[])], Trust::Repo).unwrap_err(), expected);
        assert!(build(&[(id, &["ctrl-b"])], Trust::Global).is_ok());
    }

    #[test]
    fn the_create_question_has_no_enter_and_the_pick_list_closes_on_esc() {
        let state = AppState::new(Context::Workspace);
        let resolve_in =
            |scope, key| match resolve(registry(), &Keymap::default(), scope, &state, key) {
                Resolution::Run(binding) => Some(binding.intent.clone()),
                Resolution::Unbound => None,
                other => panic!("{key:?}: {other:?}"),
            };
        let none = KeyModifiers::NONE;
        let ask = KeyScope::CreatePullRequest;
        assert_eq!(resolve_in(ask, key(KeyCode::Enter, none)), None);
        assert_eq!(resolve_in(ask, plain('q')), None);
        assert_eq!(
            resolve_in(ask, plain('y')),
            Some(AppIntent::PullRequests(PullRequestsIntent::ConfirmCreate))
        );
        assert_eq!(resolve_in(ask, plain('n')), Some(AppIntent::ConfirmNo));
        assert_eq!(
            resolve_in(ask, key(KeyCode::Esc, none)),
            Some(AppIntent::ConfirmNo)
        );
        assert_eq!(ask.parent(), None);
        let pick = KeyScope::PickList;
        assert_eq!(
            resolve_in(pick, key(KeyCode::Enter, none)),
            Some(AppIntent::PullRequests(PullRequestsIntent::PickSelect))
        );
        assert_eq!(
            resolve_in(pick, plain('j')),
            Some(AppIntent::PullRequests(PullRequestsIntent::PickNext))
        );
        assert_eq!(
            resolve_in(pick, key(KeyCode::Esc, none)),
            Some(AppIntent::PopPopup)
        );
        assert_eq!(pick.parent(), None);
        for id in [
            "branches.pull_request",
            "create_pull_request.yes",
            "create_pull_request.no",
            "create_pull_request.quit",
            "pick_list.quit",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
        }
    }

    #[test]
    fn only_y_answers_the_merge_question_and_its_keys_are_locked() {
        let state = AppState::new(Context::Workspace);
        let resolve_in =
            |scope, key| match resolve(registry(), &Keymap::default(), scope, &state, key) {
                Resolution::Run(binding) => Some(binding.intent.clone()),
                Resolution::Unbound => None,
                other => panic!("{key:?}: {other:?}"),
            };
        let none = KeyModifiers::NONE;
        let ask = KeyScope::MergePullRequest;
        assert_eq!(
            resolve_in(ask, plain('y')),
            Some(AppIntent::Review(ReviewIntent::ConfirmMergePr))
        );
        assert_eq!(resolve_in(ask, key(KeyCode::Enter, none)), None);
        assert_eq!(resolve_in(ask, plain('q')), None);
        assert_eq!(resolve_in(ask, plain('Y')), None);
        assert_eq!(resolve_in(ask, plain('n')), Some(AppIntent::ConfirmNo));
        assert_eq!(
            resolve_in(ask, key(KeyCode::Esc, none)),
            Some(AppIntent::ConfirmNo)
        );
        assert_eq!(
            resolve_in(ask, key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(AppIntent::Quit)
        );
        assert_eq!(ask.parent(), None);
        let merge = registry().iter().find(|b| b.id == "review.merge").unwrap();
        assert_eq!(merge.scope, KeyScope::Review);
        assert_eq!(merge.intent, AppIntent::Review(ReviewIntent::OpenMergePr));
        assert!(merge.matches(key(KeyCode::Char('M'), KeyModifiers::SHIFT)));
        for id in [
            "review.merge",
            "merge_pr.yes",
            "merge_pr.no",
            "merge_pr.quit",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
            let expected = format!("{id:?} can only be changed in the global config file");
            assert_eq!(
                build(&[(id, &["enter"])], Trust::Repo).unwrap_err(),
                expected
            );
        }
        assert!(build(&[("merge_pr.yes", &["Y"])], Trust::Global).is_ok());
    }

    #[test]
    fn only_y_deletes_or_discards_a_pending_comment_and_its_answers_are_locked() {
        let state = AppState::new(Context::Workspace);
        let resolve_in =
            |scope, key| match resolve(registry(), &Keymap::default(), scope, &state, key) {
                Resolution::Run(binding) => Some(binding.intent.clone()),
                Resolution::Unbound => None,
                other => panic!("{key:?}: {other:?}"),
            };
        let none = KeyModifiers::NONE;
        let ask = KeyScope::ReviewCommentConfirm;
        assert_eq!(resolve_in(ask, plain('y')), Some(AppIntent::ConfirmYes));
        // Enter, a key meant for the list underneath, keeps the comment.
        assert_eq!(resolve_in(ask, key(KeyCode::Enter, none)), None);
        assert_eq!(resolve_in(ask, plain('q')), None);
        assert_eq!(resolve_in(ask, plain('Y')), None);
        assert_eq!(resolve_in(ask, plain('n')), Some(AppIntent::ConfirmNo));
        assert_eq!(
            resolve_in(ask, key(KeyCode::Esc, none)),
            Some(AppIntent::ConfirmNo)
        );
        assert_eq!(
            resolve_in(ask, key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(AppIntent::Quit)
        );
        assert_eq!(ask.parent(), None);
        for id in [
            "review_comment_confirm.yes",
            "review_comment_confirm.no",
            "review_comment_confirm.quit",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
        }
    }

    #[test]
    fn only_y_answers_the_checkout_question_and_its_keys_are_locked() {
        let state = AppState::new(Context::Workspace);
        let resolve_in =
            |scope, key| match resolve(registry(), &Keymap::default(), scope, &state, key) {
                Resolution::Run(binding) => Some(binding.intent.clone()),
                Resolution::Unbound => None,
                other => panic!("{key:?}: {other:?}"),
            };
        let none = KeyModifiers::NONE;
        let ask = KeyScope::CheckoutPullRequest;
        assert_eq!(
            resolve_in(ask, plain('y')),
            Some(AppIntent::Review(ReviewIntent::ConfirmCheckoutPr))
        );
        assert_eq!(resolve_in(ask, key(KeyCode::Enter, none)), None);
        assert_eq!(resolve_in(ask, plain('q')), None);
        assert_eq!(resolve_in(ask, plain('Y')), None);
        assert_eq!(resolve_in(ask, plain('b')), None);
        assert_eq!(resolve_in(ask, plain('n')), Some(AppIntent::ConfirmNo));
        assert_eq!(
            resolve_in(ask, key(KeyCode::Esc, none)),
            Some(AppIntent::ConfirmNo)
        );
        assert_eq!(
            resolve_in(ask, key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(AppIntent::Quit)
        );
        assert_eq!(ask.parent(), None);
        assert_eq!(ask.title(), "Check out pull request");
        let checkout = registry()
            .iter()
            .find(|b| b.id == "review.checkout")
            .unwrap();
        assert_eq!(checkout.scope, KeyScope::Review);
        assert_eq!(checkout.intent, AppIntent::Review(ReviewIntent::CheckoutPr));
        assert!(checkout.matches(plain('b')));
        for id in [
            "review.checkout",
            "checkout_pr.yes",
            "checkout_pr.no",
            "checkout_pr.quit",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
            let expected = format!("{id:?} can only be changed in the global config file");
            assert_eq!(
                build(&[(id, &["enter"])], Trust::Repo).unwrap_err(),
                expected
            );
        }
        // The chunk sits right after the merge question's.
        let at = |chunk: &[Binding]| CHUNKS.iter().position(|c| std::ptr::eq(*c, chunk));
        assert_eq!(
            at(review::keys::REVIEW_CHECKOUT_CONFIRM),
            at(review::keys::REVIEW_MERGE_CONFIRM).map(|i| i + 1)
        );
    }

    #[test]
    fn undo_redo_and_the_travel_answers_are_locked() {
        for id in [
            "workspace.undo",
            "workspace.redo",
            "travel.yes",
            "travel.no",
            "travel.quit",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
            assert!(build(&[(id, &["F"])], Trust::Repo).is_err(), "{id}");
            assert!(build(&[(id, &["ctrl-u"])], Trust::Global).is_ok(), "{id}");
        }
        // Nor can a repository file hide them behind a nearer key.
        assert!(build(&[("files.stage", &["ctrl-z"])], Trust::Repo).is_err());
        assert!(build(&[("blame.next", &["ctrl-r"])], Trust::Repo).is_err());
    }

    #[test]
    fn ctrl_z_and_ctrl_r_undo_and_redo_from_every_workspace_panel() {
        let mut state = AppState::new(Context::Workspace);
        state.repo = Some("/repo".into());
        let ctrl = |c| key(KeyCode::Char(c), KeyModifiers::CONTROL);
        for scope in [
            KeyScope::Workspace,
            KeyScope::Files,
            KeyScope::Branches,
            KeyScope::Remotes,
            KeyScope::Commits,
            KeyScope::CommitFiles,
            KeyScope::Reflog,
            KeyScope::Status,
            KeyScope::Diff,
            KeyScope::Lines,
            KeyScope::Blame,
            KeyScope::BlameCommit,
            KeyScope::Conflict,
        ] {
            let run = |k| match resolve(registry(), &Keymap::default(), scope, &state, k) {
                Resolution::Run(binding) => Some(binding.intent.clone()),
                other => panic!("{scope:?} {k:?}: {other:?}"),
            };
            assert_eq!(
                run(ctrl('z')),
                Some(AppIntent::Commits(CommitsIntent::Reflog(
                    ReflogIntent::Undo
                ))),
                "{scope:?}"
            );
            assert_eq!(
                run(ctrl('r')),
                Some(AppIntent::Commits(CommitsIntent::Reflog(
                    ReflogIntent::Redo
                ))),
                "{scope:?}"
            );
        }
        // Plain `z` still zooms and plain `r` still refreshes.
        assert_eq!(
            from(KeyScope::Files, plain('z')),
            Some(AppIntent::CycleScreenMode)
        );
        assert_eq!(
            from(KeyScope::Files, plain('r')),
            Some(AppIntent::RefreshStatus)
        );
        // The bar keeps its room; help lists them.
        assert!(!binding("workspace.undo").bar && !binding("workspace.redo").bar);
        assert_eq!(binding("workspace.undo").keys[0].label(), "ctrl-z");
        assert_eq!(binding("workspace.redo").keys[0].label(), "ctrl-r");
    }

    #[test]
    fn revert_reset_and_the_hard_reset_answers_are_locked_but_checkout_is_not() {
        for id in [
            "commits.revert",
            "commits.reset",
            "reset_hard.yes",
            "reset_hard.no",
            "reset_hard.quit",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
            assert!(build(&[(id, &["F"])], Trust::Repo).is_err(), "{id}");
        }
        assert!(!REPO_LOCKED.contains(&"commits.checkout"));
        assert!(build(&[("commits.checkout", &["Z"])], Trust::Repo).is_ok());
    }

    #[test]
    fn the_filter_range_and_search_keys_clash_with_nothing_their_scope_reaches() {
        use KeyScope::*;
        let new: &[(&str, &[KeyScope])] = &[
            ("files.filter", &[Files, Workspace, Global]),
            ("files.range", &[Files, Workspace, Global]),
            ("files.escape", &[Files, Workspace, Global]),
            ("commits.filter", &[Commits, Workspace, Global]),
            ("commits.range", &[Commits, Workspace, Global]),
            ("commits.escape", &[Commits, Workspace, Global]),
            // Line mode reaches the Diff keys too.
            ("diff.search", &[Lines, Diff, Workspace, Global]),
            ("diff.search_next", &[Lines, Diff, Workspace, Global]),
            ("diff.search_prev", &[Lines, Diff, Workspace, Global]),
            ("diff.escape", &[Diff, Workspace, Global]),
            ("input.accept", &[Input]),
            ("input.cancel", &[Input]),
            ("input.quit", &[Input]),
        ];
        for (id, reached) in new {
            let new = binding(id);
            for other in registry()
                .iter()
                .filter(|b| b.id != *id && reached.contains(&b.scope))
            {
                // Esc in a list or the diff clears first and only then
                // quits or goes back; taking it from quit is the point.
                if id.ends_with(".escape") && other.id == "global.quit" {
                    continue;
                }
                for a in new.keys {
                    for b in other.keys {
                        assert!(!a.overlaps(b), "{id} {a:?} clashes with {}", other.id);
                    }
                }
            }
        }
    }

    #[test]
    fn the_preview_blame_and_commit_search_keys_clash_with_nothing_their_scope_reaches() {
        use KeyScope::*;
        let mut new: Vec<(String, &[KeyScope])> = Vec::new();
        for id in [
            "files.search_diff",
            "files.search_next",
            "files.search_prev",
        ] {
            new.push((id.to_string(), &[Files, Workspace, Global]));
        }
        let panes: [(&str, &[KeyScope]); 4] = [
            ("commit_files", &[CommitFiles, Workspace, Global]),
            ("stash_files", &[StashFiles, Workspace, Global]),
            ("blame", &[Blame, Workspace, Global]),
            ("blame_commit", &[BlameCommit, Workspace, Global]),
        ];
        for (pane, reached) in panes {
            for action in ["search", "search_next", "search_prev", "escape"] {
                new.push((format!("{pane}.{action}"), reached));
            }
        }
        for (id, reached) in &new {
            let new = binding(id);
            assert_eq!(new.scope, reached[0], "{id}");
            for other in registry()
                .iter()
                .filter(|b| b.id != id && reached.contains(&b.scope))
            {
                // Esc clears a search first and only then closes the pane;
                // taking it from quit is the point.
                if id.ends_with(".escape") && other.id == "global.quit" {
                    continue;
                }
                for a in new.keys {
                    for b in other.keys {
                        assert!(!a.overlaps(b), "{id} {a:?} clashes with {}", other.id);
                    }
                }
            }
        }
        // ctrl-f in the Commits log is a sibling scope's, so no clash.
        let ctrl_f = key(KeyCode::Char('f'), KeyModifiers::CONTROL);
        assert!(binding("commits.create_fixup").matches(ctrl_f));
        for id in [
            "files.search_diff",
            "commit_files.search",
            "stash_files.search",
        ] {
            assert!(binding(id).matches(ctrl_f), "{id}");
            assert_eq!(binding(id).intent, AppIntent::OpenPreviewSearch, "{id}");
        }
        for id in ["blame.search", "blame_commit.search"] {
            assert!(binding(id).matches(plain('/')), "{id}");
            assert_eq!(binding(id).intent, AppIntent::OpenSearch, "{id}");
        }
    }

    #[test]
    fn the_new_pane_escapes_are_locked_and_the_search_keys_are_not() {
        for pane in ["commit_files", "stash_files", "blame", "blame_commit"] {
            // Unbound, Esc would fall through to quit.
            let id = format!("{pane}.escape");
            assert!(REPO_LOCKED.contains(&id.as_str()), "{id}");
            assert!(build(&[(&id, &["Z"])], Trust::Repo).is_err(), "{id}");
            assert!(build(&[(&id, &[])], Trust::Repo).is_err(), "{id}");
            for action in ["search", "search_next", "search_prev"] {
                let id = format!("{pane}.{action}");
                assert!(!REPO_LOCKED.contains(&id.as_str()), "{id}");
                assert!(build(&[(&id, &["Z"])], Trust::Repo).is_ok(), "{id}");
            }
        }
        for id in [
            "files.search_diff",
            "files.search_next",
            "files.search_prev",
        ] {
            assert!(!REPO_LOCKED.contains(&id), "{id}");
            assert!(build(&[(id, &["Z"])], Trust::Repo).is_ok(), "{id}");
        }
    }

    #[test]
    fn the_log_query_and_diff_mark_keys_clash_with_nothing_their_scope_reaches() {
        use KeyScope::*;
        let new: &[(&str, &[KeyScope])] = &[
            ("commits.filter_path", &[Commits, Workspace, Global]),
            ("commits.filter_author", &[Commits, Workspace, Global]),
            ("commits.mark", &[Commits, Workspace, Global]),
            ("reflog.filter_path", &[Reflog, Workspace, Global]),
            ("reflog.filter_author", &[Reflog, Workspace, Global]),
            ("commit_files.filter", &[CommitFiles, Workspace, Global]),
            ("branches.mark", &[Branches, Workspace, Global]),
            ("branches.diff", &[Branches, Workspace, Global]),
            ("files.log", &[Files, Workspace, Global]),
        ];
        for (id, reached) in new {
            let new = binding(id);
            assert!(!REPO_LOCKED.contains(id), "{id} quits nothing");
            for other in registry()
                .iter()
                .filter(|b| b.id != *id && reached.contains(&b.scope))
            {
                for a in new.keys {
                    for b in other.keys {
                        assert!(!a.overlaps(b), "{id} {a:?} clashes with {}", other.id);
                    }
                }
            }
        }
    }

    #[test]
    fn rewrites_wait_for_the_log_filter_and_the_reflog_has_none() {
        let ids = ['a', 'b', 'c'].map(|d| d.to_string().repeat(40));
        let mut state = AppState::new(Context::Workspace);
        state.commits.list.rows = (0..2)
            .map(|i| kika_git::log::CommitRow {
                oid: ids[i].clone(),
                short_oid: "x".into(),
                parents: vec![ids[i + 1].clone()],
                author: "Ann".into(),
                time: 0,
                subject: "s".into(),
                subject_altered: false,
            })
            .collect();
        state.commits.tips = Some(vec![]);
        let all = [
            commits_squash_disabled,
            commits_fixup_disabled,
            commits_drop_disabled,
            commits_reword_disabled,
        ];
        for disabled in all {
            assert_eq!(disabled(&state), None);
        }
        state.commits.query.author = Some("ann".into());
        for disabled in all {
            assert_eq!(disabled(&state).as_deref(), Some(CLEAR_LOG_FILTER));
        }
        // Moves and stops plan from the same rows, so they wait too.
        for id in [
            "commits.move_up",
            "commits.move_down",
            "commits.edit",
            "commits.break",
        ] {
            assert_eq!(
                (binding(id).disabled)(&state).as_deref(),
                Some(CLEAR_LOG_FILTER),
                "{id}"
            );
        }

        state.focus = Focus::Commits;
        state.commits.tab = crate::features::commits::CommitsTab::Reflog;
        let reflog = |c| resolve(registry(), &Keymap::default(), KeyScope::Reflog, &state, c);
        for c in ['L', 'A'] {
            assert!(
                matches!(
                    reflog(key(KeyCode::Char(c), KeyModifiers::SHIFT)),
                    Resolution::Disabled(ref why) if why == "Filters work on the log tab"
                ),
                "{c}"
            );
        }
    }

    #[test]
    fn the_new_bindings_that_can_quit_are_locked() {
        for id in ["files.escape", "commits.escape", "input.quit"] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
            assert!(build(&[(id, &["Z"])], Trust::Repo).is_err(), "{id}");
        }
        for id in ["files.filter", "files.range", "diff.search", "input.accept"] {
            assert!(!REPO_LOCKED.contains(&id), "{id}");
        }
    }

    #[test]
    fn the_branch_lists_new_keys_are_remappable_but_escape_and_the_range_question_are_locked() {
        for id in [
            "delete_range.yes",
            "delete_range.no",
            "delete_range.quit",
            // Esc with nothing to clear quits, as files.escape does.
            "branches.escape",
            "remotes.escape",
            "tags.escape",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
            assert!(build(&[(id, &["Z"])], Trust::Repo).is_err(), "{id}");
        }
        for scope in ["branches", "remotes", "tags"] {
            for action in ["filter", "range"] {
                let id = format!("{scope}.{action}");
                assert!(!REPO_LOCKED.contains(&id.as_str()), "{id}");
                assert!(build(&[(&id, &["Z"])], Trust::Repo).is_ok(), "{id}");
            }
        }
    }

    #[test]
    fn the_commit_files_filter_is_remappable_but_its_escape_is_locked() {
        // Esc with nothing to clear goes back to the log; unbound it would
        // fall through to quit.
        assert!(REPO_LOCKED.contains(&"commit_files.escape"));
        assert!(build(&[("commit_files.escape", &["Z"])], Trust::Repo).is_err());
        assert!(!REPO_LOCKED.contains(&"commit_files.filter"));
        assert!(build(&[("commit_files.filter", &["Z"])], Trust::Repo).is_ok());
        assert_eq!(
            from(KeyScope::CommitFiles, plain('/')),
            Some(AppIntent::OpenFilter)
        );
        // No range: nothing acts on a range of a commit's files.
        assert_ne!(
            from(KeyScope::CommitFiles, plain('V')),
            Some(AppIntent::ToggleRange)
        );
    }

    #[test]
    fn a_filter_hiding_the_commit_file_disables_picking_it_and_focusing_its_diff() {
        use kika_git::commit_files::CommitFile;
        let mut state = AppState::new(Context::Workspace);
        state.focus = Focus::Commits;
        let range = CommitRange {
            oid: CommitId::parse("abc").unwrap(),
            parent: CommitId::parse("def"),
        };
        let mut open = CommitFilesList::begin(range, "abc".into(), 1);
        let file = CommitFile {
            status: 'M',
            path: "src/lib.rs".into(),
            orig_path: None,
        };
        load_list(&mut open.list, Ok(vec![file]));
        state.commits.open = Some(open);
        assert_eq!(state.panel_focus(), KeyScope::CommitFiles);
        assert_eq!(patch_file_disabled(&state), None);
        assert_eq!(focus_diff_disabled(&state), None);

        state
            .commits
            .open
            .as_mut()
            .unwrap()
            .list
            .set_filter("zzz".into());
        assert_eq!(
            patch_file_disabled(&state).as_deref(),
            Some(FILTER_HIDES_ROW)
        );
        assert_eq!(
            focus_diff_disabled(&state).as_deref(),
            Some(FILTER_HIDES_ROW)
        );
        assert_eq!(list_guard(&state, true).as_deref(), Some(FILTER_HIDES_ROW));
    }

    #[test]
    fn only_y_answers_the_range_question() {
        let state = AppState::new(Context::Workspace);
        let scope = KeyScope::DeleteRange;
        assert_eq!(scope.parent(), None);
        let ran = |k| run(registry(), scope, &state, k);
        assert_eq!(ran(plain('y')), AppIntent::ConfirmDeleteRange);
        assert_eq!(ran(plain('n')), AppIntent::ConfirmNo);
        assert_eq!(
            ran(key(KeyCode::Esc, KeyModifiers::NONE)),
            AppIntent::ConfirmNo
        );
        assert_eq!(
            ran(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            AppIntent::Quit
        );
        for k in [
            key(KeyCode::Enter, KeyModifiers::NONE),
            key(KeyCode::Char('Y'), KeyModifiers::SHIFT),
            key(KeyCode::Char('D'), KeyModifiers::SHIFT),
            plain('q'),
        ] {
            assert!(
                matches!(
                    resolve(registry(), &Keymap::default(), scope, &state, k),
                    Resolution::Unbound
                ),
                "{k:?}"
            );
        }
    }

    /// The Branches panel focused on `tab`, with a branch, a remote with
    /// a branch, and a tag loaded, and nothing running.
    fn branch_lists(tab: crate::features::branches::BranchesTab) -> AppState {
        use kika_git::branches::{Branch, Track};
        let mut state = AppState::new(Context::Workspace);
        state.focus = Focus::Branches;
        state.branches.tab = tab;
        state.diff_mark = Some(crate::diff_mark::DiffMark::Branch("other".into()));
        load_list(
            &mut state.branches.list,
            Ok(vec![Branch {
                name: "topic".into(),
                head: false,
                upstream: Some("origin/topic".into()),
                track: Track::None,
                merged: true,
            }]),
        );
        load_remotes(
            &mut state.branches.remotes.list,
            Ok(vec![kika_git::remotes::Remote {
                name: "origin".into(),
                url: Some("/srv/origin".into()),
                configured: true,
                branches: vec![kika_git::remotes::RemoteBranch {
                    remote: "origin".into(),
                    short: b"topic".to_vec(),
                    refname: b"refs/remotes/origin/topic".to_vec(),
                }],
            }]),
        );
        let oid = "a".repeat(40);
        load_list(
            &mut state.branches.tags,
            Ok(vec![kika_git::tags::TagRef {
                name: "v1".into(),
                target: CommitId::parse(&oid).unwrap(),
                target_short: oid[..7].into(),
                ref_oid: oid,
                annotated: false,
                date: "2026-10-01".into(),
                subject: "s".into(),
            }]),
        );
        state
    }

    /// The row actions of `scope` and the range delete it has.
    fn row_actions(scope: KeyScope) -> (Vec<&'static str>, &'static str) {
        match scope {
            KeyScope::Branches => (
                vec![
                    "branches.checkout",
                    "branches.new",
                    "branches.rename",
                    "branches.merge",
                    "branches.pull_request",
                    "branches.set_upstream",
                    "branches.unset_upstream",
                    "branches.mark",
                    "branches.diff",
                ],
                "branches.delete",
            ),
            KeyScope::Remotes => (vec!["remotes.checkout", "remotes.rename"], "remotes.remove"),
            _ => (vec!["tags.push"], "tags.delete"),
        }
    }

    #[test]
    fn a_range_in_the_branch_lists_leaves_only_d_and_a_free_of_row_actions() {
        use crate::features::branches::BranchesTab;
        for tab in [BranchesTab::Local, BranchesTab::Remotes, BranchesTab::Tags] {
            let mut state = branch_lists(tab);
            let scope = state.panel_focus();
            let (actions, delete) = row_actions(scope);
            // A row action may be off for its own reason without a range;
            // with one, the range is the reason given.
            let reason = |state: &AppState, id: &str| (binding(id).disabled)(state);
            state.update(AppIntent::ToggleRange);
            for id in &actions {
                assert_eq!(
                    reason(&state, id).as_deref(),
                    Some(CLEAR_RANGE_FIRST),
                    "{id}"
                );
            }
            assert_eq!(reason(&state, delete), None, "{delete} works on a range");
            assert_eq!(reason(&state, "remotes.add"), None);
            state.hold_op(1);
            assert_eq!(reason(&state, delete).as_deref(), Some(OP_RUNNING));
        }
    }

    #[test]
    fn a_filter_that_hides_every_row_in_the_branch_lists_refuses_row_actions() {
        use crate::features::branches::BranchesTab;
        for tab in [BranchesTab::Local, BranchesTab::Remotes, BranchesTab::Tags] {
            let mut state = branch_lists(tab);
            match tab {
                BranchesTab::Local => state.branches.list.filter = "zzz".into(),
                BranchesTab::Remotes => state.branches.remotes.list.filter = "zzz".into(),
                BranchesTab::Tags => state.branches.tags.filter = "zzz".into(),
            }
            let (actions, delete) = row_actions(state.panel_focus());
            for id in actions.iter().chain([&delete]) {
                assert_eq!(
                    (binding(id).disabled)(&state).as_deref(),
                    Some(FILTER_HIDES_ROW),
                    "{id}"
                );
            }
            assert_eq!((binding("remotes.add").disabled)(&state), None);
        }
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
        assert_eq!(label(KeyPattern::any(KeyCode::Right)), "→");
        assert_eq!(label(KeyPattern::any(KeyCode::BackTab)), "⇧tab");
    }

    #[test]
    fn focus_and_zoom_keys_work_from_every_workspace_panel() {
        use crate::layout::Panel::{Diff, Side};
        use AppIntent::*;
        let none = KeyModifiers::NONE;
        let shift = KeyModifiers::SHIFT;
        for scope in [
            KeyScope::Workspace,
            KeyScope::Files,
            KeyScope::Diff,
            KeyScope::Lines,
            KeyScope::Blame,
            KeyScope::BlameCommit,
        ] {
            let run = |k| from(scope, k);
            assert_eq!(run(plain('1')), Some(FocusPanel(Side(SidePanel::Status))));
            assert_eq!(run(plain('2')), Some(FocusPanel(Side(SidePanel::Files))));
            assert_eq!(run(plain('3')), Some(FocusPanel(Side(SidePanel::Branches))));
            assert_eq!(run(plain('4')), Some(FocusPanel(Side(SidePanel::Commits))));
            assert_eq!(run(plain('5')), Some(FocusPanel(Side(SidePanel::Stash))));
            assert_eq!(run(plain('0')), Some(FocusPanel(Diff)), "{scope:?}");
            assert_eq!(
                run(key(KeyCode::Tab, none)),
                Some(CycleFocus { back: false })
            );
            assert_eq!(
                run(key(KeyCode::BackTab, shift)),
                Some(CycleFocus { back: true })
            );
            assert_eq!(run(plain('z')), Some(CycleScreenMode), "{scope:?}");
        }
        // `l` and `→` reach the diff from the side panels; in the diff,
        // `h` and `←` go back.
        for scope in [KeyScope::Workspace, KeyScope::Files] {
            assert_eq!(from(scope, plain('l')), Some(FocusPanel(Diff)));
            assert_eq!(
                from(scope, key(KeyCode::Right, none)),
                Some(FocusPanel(Diff))
            );
            assert_eq!(from(scope, plain('h')), None, "{scope:?}");
        }
        for scope in [KeyScope::Diff, KeyScope::Lines] {
            assert_eq!(from(scope, plain('h')), Some(FocusLeft));
            assert_eq!(from(scope, key(KeyCode::Left, none)), Some(FocusLeft));
        }
        // List movement stays list movement.
        assert_eq!(
            from(KeyScope::Files, plain('j')),
            Some(Files(FilesIntent::Next))
        );
        assert_eq!(from(KeyScope::Workspace, plain('j')), None);
        // None of them are in the bar, which is full at 80 columns.
        for c in ['0', '1', '2', '3', '4', '5', 'l', 'z'] {
            let binding = registry()
                .iter()
                .find(|b| b.scope == KeyScope::Workspace && b.matches(plain(c)))
                .expect("bound");
            assert!(!binding.bar, "{c}");
        }
    }

    #[test]
    fn the_diff_keys_say_why_with_no_file_selected() {
        let state = AppState::new(Context::Workspace);
        for k in [
            plain('0'),
            plain('l'),
            key(KeyCode::Right, KeyModifiers::NONE),
        ] {
            assert!(matches!(
                resolve(registry(), &Keymap::default(), KeyScope::Files, &state, k),
                Resolution::Disabled(ref reason) if reason == "No file selected"
            ));
        }
    }

    /// The `<scope>` part of a binding id.
    fn scope_name(scope: KeyScope) -> &'static str {
        match scope {
            KeyScope::Global => "global",
            KeyScope::Workspace => "workspace",
            KeyScope::Files => "files",
            KeyScope::Branches => "branches",
            KeyScope::Remotes => "remotes",
            KeyScope::Tags => "tags",
            KeyScope::Commits => "commits",
            KeyScope::CommitFiles => "commit_files",
            KeyScope::Stash => "stash",
            KeyScope::StashFiles => "stash_files",
            KeyScope::Status => "status",
            KeyScope::Diff => "diff",
            KeyScope::Lines => "lines",
            KeyScope::Blame => "blame",
            KeyScope::BlameCommit => "blame_commit",
            KeyScope::Conflict => "conflict",
            KeyScope::Review => "review",
            KeyScope::ReviewFiles => "review_files",
            KeyScope::ReviewDiff => "review_diff",
            KeyScope::ReviewThread => "review_thread",
            KeyScope::ReviewPick => "review_pick",
            KeyScope::ReviewComment => "review_comment",
            KeyScope::ReviewPending => "review_pending",
            KeyScope::ReviewCommentConfirm => "review_comment_confirm",
            KeyScope::Help => "help",
            KeyScope::CommitConfirm => "commit_confirm",
            KeyScope::CommitEditor => "commit_editor",
            KeyScope::CommitError => "commit_error",
            KeyScope::CommitDiscard => "commit_discard",
            KeyScope::CommitLoading => "commit_loading",
            KeyScope::Menu => "menu",
            KeyScope::Confirm => "confirm",
            KeyScope::Prompt => "prompt",
            KeyScope::Error => "error",
            KeyScope::DiscardConfirm => "discard_confirm",
            KeyScope::DeleteBranch => "delete_branch",
            KeyScope::RemoveRemote => "remove_remote",
            KeyScope::DeleteTag => "delete_tag",
            KeyScope::DeleteRange => "delete_range",
            KeyScope::ResetHard => "reset_hard",
            KeyScope::LeaveCommits => "leave_commits",
            KeyScope::DropStash => "drop_stash",
            KeyScope::AbortConfirm => "abort_confirm",
            KeyScope::SkipConfirm => "skip_confirm",
            KeyScope::AutosquashConfirm => "autosquash_confirm",
            KeyScope::PatchConfirm => "patch_confirm",
            KeyScope::DropRangeConfirm => "drop_range_confirm",
            KeyScope::Submodules => "submodules",
            KeyScope::Worktrees => "worktrees",
            KeyScope::RemoveWorktree => "remove_worktree",
            KeyScope::Recent => "recent",
            KeyScope::Credential => "credential",
            KeyScope::Reflog => "reflog",
            KeyScope::Travel => "travel",
            KeyScope::Input => "input",
            KeyScope::CommandLog => "command_log",
            KeyScope::PickList => "pick_list",
            KeyScope::CreatePullRequest => "create_pull_request",
            KeyScope::MergePullRequest => "merge_pr",
            KeyScope::CheckoutPullRequest => "checkout_pr",
        }
    }

    #[test]
    fn binding_ids_are_unique_and_name_their_scope() {
        let mut seen = std::collections::HashSet::new();
        for b in registry() {
            assert!(seen.insert(b.id), "{} is used twice", b.id);
            let (scope, action) = b.id.split_once('.').expect("<scope>.<action>");
            assert_eq!(scope, scope_name(b.scope), "{}", b.id);
            let snake = |c: char| c.is_ascii_lowercase() || c == '_';
            assert!(!action.is_empty() && action.chars().all(snake), "{}", b.id);
        }
    }

    #[test]
    fn parse_key_reads_back_every_ascii_label_in_the_registry() {
        let mut checked = 0;
        for b in registry() {
            for key in b.keys {
                let label = key.label();
                if !label.is_ascii() {
                    continue;
                }
                // Some defaults accept any modifiers on a plain letter (`q`
                // quits with Shift too), so the patterns can differ there;
                // the key, its label and how it is shown must not.
                let parsed = parse_key(&label).unwrap_or_else(|e| panic!("{}: {e}", b.id));
                assert_eq!(parsed.label(), label, "{}", b.id);
                assert_eq!(parsed.code, key.code, "{}", b.id);
                assert!(parsed.overlaps(key), "{} {label:?}", b.id);
                checked += 1;
            }
        }
        assert!(checked > 50, "only {checked} labels checked");
    }

    #[test]
    fn parse_key_reads_the_names_a_user_writes() {
        let none = KeyModifiers::NONE;
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(
            parse_key("x"),
            Ok(KeyPattern::exact(KeyCode::Char('x'), none))
        );
        assert_eq!(parse_key("X"), Ok(KeyPattern::any(KeyCode::Char('X'))));
        assert_eq!(
            parse_key("space"),
            Ok(KeyPattern::exact(KeyCode::Char(' '), none))
        );
        assert_eq!(
            parse_key("ctrl-s"),
            Ok(KeyPattern::exact(KeyCode::Char('s'), ctrl))
        );
        assert_eq!(
            parse_key("ctrl-up"),
            Ok(KeyPattern::exact(KeyCode::Up, ctrl))
        );
        let alt = KeyModifiers::ALT;
        assert_eq!(
            parse_key("alt-k"),
            Ok(KeyPattern::exact(KeyCode::Char('k'), alt))
        );
        assert_eq!(
            parse_key("alt-down"),
            Ok(KeyPattern::exact(KeyCode::Down, alt))
        );
        assert_eq!(KeyPattern::exact(KeyCode::Char('j'), alt).label(), "alt-j");
        assert!(parse_key("alt-K").is_err());
        for (name, code) in [
            ("up", KeyCode::Up),
            ("down", KeyCode::Down),
            ("left", KeyCode::Left),
            ("right", KeyCode::Right),
            ("PgDn", KeyCode::PageDown),
            ("pgup", KeyCode::PageUp),
            ("home", KeyCode::Home),
            ("end", KeyCode::End),
            ("tab", KeyCode::Tab),
            ("shift-tab", KeyCode::BackTab),
        ] {
            assert_eq!(parse_key(name), Ok(KeyPattern::any(code)), "{name}");
        }
        assert_eq!(parse_key("ä"), Ok(KeyPattern::any(KeyCode::Char('ä'))));
        assert_eq!(parse_key("§"), Ok(KeyPattern::any(KeyCode::Char('§'))));
        for bad in [
            "",
            "ctrl-",
            "ctrl-S",
            "shift-x",
            "f1",
            "xy",
            " ",
            "\t",
            "\u{1b}",
            "\u{202e}",
            "\u{200b}",
            "\u{ad}",
            "ctrl-\u{202e}",
        ] {
            let err = parse_key(bad).unwrap_err();
            assert!(err.starts_with("bad key"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn a_remap_replaces_the_keys_and_an_empty_list_disables() {
        let keymap = build(
            &[("files.stage", &["x", "ctrl-x"]), ("files.discard", &[])],
            Trust::Global,
        )
        .unwrap()
        .0;
        let none = KeyModifiers::NONE;
        assert_eq!(
            keymap.keys(binding("files.stage")),
            [
                KeyPattern::exact(KeyCode::Char('x'), none),
                KeyPattern::exact(KeyCode::Char('x'), KeyModifiers::CONTROL),
            ]
        );
        assert!(keymap.keys(binding("files.discard")).is_empty());
        assert_eq!(
            keymap.keys(binding("files.commit")),
            binding("files.commit").keys
        );

        let state = AppState::new(Context::Workspace);
        let files = KeyScope::Files;
        assert!(matches!(
            resolve(registry(), &keymap, files, &state, plain('d')),
            Resolution::Unbound
        ));
        assert!(matches!(
            resolve(registry(), &keymap, files, &state, plain(' ')),
            Resolution::Unbound
        ));
        let listed = available(registry(), &keymap, files, &state);
        assert!(!listed.iter().any(|(b, _)| b.id == "files.discard"));
    }

    #[test]
    fn a_later_layer_overrides_an_earlier_one_binding_by_binding() {
        let global = build(
            &[("files.stage", &["x"]), ("files.commit", &["C"])],
            Trust::Global,
        )
        .unwrap()
        .0;
        let (merged, dropped) =
            Keymap::build(&global, &layer(&[("files.stage", &["y"])]), Trust::Repo).unwrap();
        assert_eq!(dropped, Vec::<String>::new());
        assert_eq!(merged.keys(binding("files.stage")), [char_pattern('y')]);
        assert_eq!(merged.keys(binding("files.commit")), [char_pattern('C')]);
    }

    #[test]
    fn a_clashing_entry_is_dropped_alone_and_keeps_its_base_keys() {
        let (keymap, dropped) = build(
            &[("files.stage", &["c"]), ("files.blame", &["Z"])],
            Trust::Global,
        )
        .unwrap();
        assert_eq!(
            dropped,
            [
                r#"key "c" is bound to both "files.stage" and "files.commit"; the change to "files.stage" was dropped"#
            ]
        );
        assert_eq!(
            keymap.keys(binding("files.stage")),
            binding("files.stage").keys
        );
        assert_eq!(keymap.keys(binding("files.commit")), [char_pattern('c')]);
        assert_eq!(keymap.keys(binding("files.blame")), [char_pattern('Z')]);

        // A key listed twice clashes with itself.
        let (keymap, dropped) = build(&[("files.stage", &["x", "x"])], Trust::Global).unwrap();
        assert_eq!(
            dropped,
            [r#"key "x" is bound twice to "files.stage"; the change to "files.stage" was dropped"#]
        );
        assert_eq!(keymap, Keymap::default());
    }

    #[test]
    fn two_entries_moved_onto_one_key_are_both_dropped() {
        let (keymap, dropped) = build(
            &[("files.stage", &["Z"]), ("files.blame", &["Z"])],
            Trust::Repo,
        )
        .unwrap();
        let message = |id: &str| {
            format!(
                r#"key "Z" is bound to both "files.stage" and "files.blame"; the change to "{id}" was dropped"#
            )
        };
        assert_eq!(dropped, [message("files.stage"), message("files.blame")]);
        assert_eq!(keymap, Keymap::default());
    }

    #[test]
    fn a_dropped_entry_back_on_its_base_keys_drops_the_entry_now_on_them() {
        // Stage leaves space for `c`, which commit holds, and blame takes
        // space. Stage goes back to space, so blame must go too.
        let (keymap, dropped) = build(
            &[
                ("files.stage", &["c"]),
                ("files.blame", &["space"]),
                ("files.hunks", &["Z"]),
            ],
            Trust::Repo,
        )
        .unwrap();
        assert_eq!(dropped.len(), 2, "{dropped:?}");
        assert!(dropped[0].ends_with(r#"the change to "files.stage" was dropped"#));
        assert!(
            dropped[1].starts_with(r#"key "space" is bound to both"#),
            "{dropped:?}"
        );
        assert!(dropped[1].ends_with(r#"the change to "files.blame" was dropped"#));
        assert_eq!(
            keymap.keys(binding("files.stage")),
            binding("files.stage").keys
        );
        assert_eq!(
            keymap.keys(binding("files.blame")),
            binding("files.blame").keys
        );
        assert_eq!(keymap.keys(binding("files.hunks")), [char_pattern('Z')]);
        assert!(keymap.clashes().is_empty());
    }

    #[test]
    fn a_repository_entry_falls_back_to_the_global_keys_and_never_drops_them() {
        let global = build(&[("files.stage", &["x"])], Trust::Global).unwrap().0;
        // The repository moves commit onto the global stage key.
        let (keymap, dropped) =
            Keymap::build(&global, &layer(&[("files.commit", &["x"])]), Trust::Repo).unwrap();
        assert_eq!(dropped.len(), 1, "{dropped:?}");
        assert!(dropped[0].ends_with(r#"the change to "files.commit" was dropped"#));
        assert_eq!(keymap, global);
        // Its own stage entry clashing goes back to the global `x`.
        let (keymap, _) =
            Keymap::build(&global, &layer(&[("files.stage", &["c"])]), Trust::Repo).unwrap();
        assert_eq!(keymap.keys(binding("files.stage")), [char_pattern('x')]);
    }

    #[test]
    fn the_quit_and_locked_checks_run_after_the_drops() {
        // A droppable clash does not save a file that hides quit.
        assert_eq!(
            build(
                &[("files.blame", &["c"]), ("files.stage", &["q"])],
                Trust::Repo
            )
            .unwrap_err(),
            r#""files.stage": key "q" would hide "global.quit""#
        );
        assert!(
            build(
                &[("files.blame", &["c"]), ("files.open", &["x"])],
                Trust::Repo
            )
            .is_err()
        );
        assert_eq!(
            build(
                &[("files.blame", &["c"]), ("global.quit", &[])],
                Trust::Global
            )
            .unwrap_err(),
            r#""global.quit" needs at least one key"#
        );
        // A clash the layer did not make is never dropped: it fails closed.
        let clashing = Keymap {
            overrides: [("files.stage", vec![char_pattern('c')])].into(),
            ..Keymap::default()
        };
        assert_eq!(
            Keymap::build(&clashing, &layer(&[("files.blame", &["Z"])]), Trust::Repo).unwrap_err(),
            r#"key "c" is bound to both "files.stage" and "files.commit""#
        );
        // Quit moved onto a taken key goes back to its own, never to none.
        let (keymap, dropped) = build(&[("global.quit", &["?"])], Trust::Global).unwrap();
        assert_eq!(dropped.len(), 1, "{dropped:?}");
        assert_eq!(
            keymap.keys(binding("global.quit")),
            binding("global.quit").keys
        );
    }

    /// A repository file may hold 64 KiB of keys. Repeating one key, in one
    /// entry or across many, must neither store every pair of clashing keys
    /// nor take long: the scan keeps one clash per owner.
    #[test]
    fn thousands_of_copies_of_one_key_are_dropped_quickly_with_bounded_work() {
        let copies = vec!["x"; 16_000];
        let files: Vec<&str> = registry()
            .iter()
            .filter(|b| b.scope == KeyScope::Files && !REPO_LOCKED.contains(&b.id))
            .map(|b| b.id)
            .collect();
        assert!(files.len() > 10, "{files:?}");
        let few = vec!["x"; 500];
        let mut entries: Vec<(&str, &[&str])> = vec![(files[0], &copies)];
        entries.extend(files[1..].iter().map(|id| (*id, few.as_slice())));

        // Every key the layer set, stored as given, before any drop.
        let mut flooded = Keymap::default();
        for (id, keys) in &entries {
            let keys = keys.iter().map(|k| parse_key(k).unwrap()).collect();
            flooded.overrides.insert(binding(id).id, keys);
        }
        let started = std::time::Instant::now();
        let clashes = flooded.clashes();
        assert!(clashes.len() <= files.len(), "{}", clashes.len());
        assert!(flooded.check_scopes().is_err());

        let (keymap, dropped) = build(&entries, Trust::Repo).unwrap();
        assert_eq!(dropped.len(), files.len(), "{dropped:?}");
        assert!(dropped[0].contains("is bound"), "{dropped:?}");
        assert_eq!(keymap, Keymap::default());
        let took = started.elapsed();
        assert!(took < std::time::Duration::from_secs(2), "{took:?}");
    }

    /// Every unlocked binding moved onto each key a locked binding or a
    /// neighbour holds, with the next binding moved there too so drops
    /// cascade: whatever is kept has no clash, quit still has keys, and a
    /// repository layer leaves every locked binding its keys and in sight.
    #[test]
    fn no_run_of_drops_unbinds_quit_or_uncovers_a_locked_key() {
        let quit = binding("global.quit");
        let ids: Vec<&str> = registry().iter().map(|b| b.id).collect();
        let mut drops = 0;
        for (i, id) in ids.iter().enumerate() {
            let next = ids[(i + 1) % ids.len()];
            for key in ["q", "esc", "enter", "space", "c"] {
                for trust in [Trust::Global, Trust::Repo] {
                    let entries: &[(&str, &[&str])] = &[(id, &[key]), (next, &[key])];
                    let Ok((keymap, dropped)) = build(entries, trust) else {
                        continue;
                    };
                    drops += dropped.len();
                    assert!(keymap.clashes().is_empty(), "{entries:?}");
                    assert!(!keymap.keys(quit).is_empty(), "{entries:?}");
                    if trust == Trust::Repo {
                        let base = Keymap::default();
                        assert_eq!(keymap.check_locked_not_hidden(&base), Ok(()), "{entries:?}");
                        for locked in registry().iter().filter(|b| REPO_LOCKED.contains(&b.id)) {
                            assert_eq!(keymap.keys(locked), locked.keys, "{entries:?}");
                        }
                    }
                }
            }
        }
        assert!(drops > 1000, "the sweep drops entries: {drops}");
    }

    #[test]
    fn a_broken_layer_says_what_is_wrong() {
        let err = |entries: &[(&str, &[&str])]| build(entries, Trust::Global).unwrap_err();
        assert_eq!(
            err(&[("files.stag", &["s"])]),
            r#"unknown binding "files.stag""#
        );
        let bad = err(&[("files.stage", &["shift-s"])]);
        assert!(
            bad.starts_with(r#""files.stage": bad key "shift-s": use one character, or space"#),
            "{bad}"
        );
        assert_eq!(
            err(&[("global.quit", &[])]),
            r#""global.quit" needs at least one key"#
        );
        // The same key in different scopes is fine: the nearer one wins.
        assert!(build(&[("files.stage", &["q"])], Trust::Global).is_ok());
    }

    #[test]
    fn a_repository_layer_cannot_touch_locked_bindings() {
        for id in REPO_LOCKED {
            assert!(
                registry().iter().any(|b| b.id == *id),
                "{id} is not a binding"
            );
            let moved = build(&[(id, &["Z"])], Trust::Repo).unwrap_err();
            assert_eq!(
                moved,
                format!("\"{id}\" can only be changed in the global config file")
            );
            assert!(build(&[(id, &[])], Trust::Repo).is_err(), "{id}");
            if *id != "global.quit" {
                assert!(build(&[(id, &["Z"])], Trust::Global).is_ok(), "{id}");
                assert!(build(&[(id, &[])], Trust::Global).is_ok(), "{id}");
            }
        }
        assert!(build(&[("global.quit", &["Q"])], Trust::Global).is_ok());
        assert!(build(&[("files.stage", &["x"])], Trust::Repo).is_ok());
    }

    #[test]
    fn every_binding_that_quits_starts_a_program_destroys_work_or_answers_is_locked() {
        use AppIntent::*;
        // The feature that owns a nested intent, by its top-level variant.
        fn nested_feature(intent: &AppIntent) -> Option<&'static str> {
            match intent {
                Branches(_) => Some("branches"),
                Commits(_) => Some("commits"),
                Commit(_) => Some("commit"),
                Blame(_) => Some("blame"),
                Conflicts(_) => Some("conflicts"),
                CustomCommands(_) => Some("custom_commands"),
                Sync(_) => Some("sync"),
                Operation(_) => Some("operation"),
                Repos(_) => Some("repos"),
                Files(_) => Some("files"),
                Diff(_) => Some("diff"),
                Stash(_) => Some("stash"),
                Submodules(_) => Some("submodules"),
                Worktrees(_) => Some("worktrees"),
                PullRequests(_) => Some("pull_requests"),
                Review(_) => Some("review"),
                _ => None,
            }
        }
        let mut nested_risky = Vec::new();
        for b in registry() {
            let risky = matches!(
                b.intent,
                Quit | Commit(CommitIntent::Interrupt)
                    | OpenInEditor
                    | Diff(DiffIntent::EditHunk)
                    | Diff(DiffIntent::Renderers(RenderersIntent::Cycle))
                    | Commit(CommitIntent::EditExternally)
                    | Submodules(SubmodulesIntent::Init)
                    | Submodules(SubmodulesIntent::Update)
                    | Discard
                    | Commit(CommitIntent::Discard)
                    | ConfirmYes
                    | ConfirmNo
                    | Repos(ReposIntent::Init)
                    | Repos(ReposIntent::Select)
                    | Branches(BranchesIntent::Delete)
                    | Branches(BranchesIntent::ConfirmDelete { .. })
                    | Sync(SyncIntent::Fetch)
                    | Sync(SyncIntent::Pull)
                    | Sync(SyncIntent::Push)
                    | Sync(SyncIntent::ForcePush)
                    | SubmitCredential
                    | CredentialAnswered(_)
                    | OpenMenu(MenuId::Merge)
                    | ContinueOperation
                    | SkipOperation
                    | AbortOperation
                    | Conflicts(ConflictsIntent::Pick(_))
                    | Conflicts(ConflictsIntent::OpenMergeTool)
                    | Commits(CommitsIntent::Revert)
                    | Commits(CommitsIntent::Copy(CopyIntent::Paste))
                    | OpenMenu(MenuId::Reset)
                    | Commits(CommitsIntent::ConfirmResetHard)
                    | Commits(CommitsIntent::Rewrite(RewriteIntent::ConfirmLeaveCommits))
                    | OpenMenu(MenuId::Stash)
                    | Stash(StashIntent::OpenFiles)
                    | Stash(StashIntent::Apply)
                    | Stash(StashIntent::Pop)
                    | Stash(StashIntent::Drop)
                    | Stash(StashIntent::Rename)
                    | Stash(StashIntent::ConfirmDrop)
                    | Branches(BranchesIntent::Remotes(RemotesIntent::Remove))
                    | Branches(BranchesIntent::Remotes(RemotesIntent::ConfirmRemove))
                    | ConfirmDeleteRange
                    | Branches(BranchesIntent::Tags(TagsIntent::Delete))
                    | Branches(BranchesIntent::Tags(TagsIntent::ConfirmDelete))
                    | Branches(BranchesIntent::Tags(TagsIntent::Push))
                    | Commits(CommitsIntent::Rewrite(RewriteIntent::Edit(_)))
                    | Commits(CommitsIntent::Rewrite(RewriteIntent::Move(_)))
                    | Commits(CommitsIntent::Rewrite(RewriteIntent::InsertBreak))
                    | Commits(CommitsIntent::Rewrite(RewriteIntent::CreateFixup))
                    | Commits(CommitsIntent::Rewrite(RewriteIntent::Autosquash))
                    | Commits(CommitsIntent::Bisect(BisectIntent::MarkHead(_)))
                    | OpenMenu(MenuId::Bisect)
                    | Worktrees(WorktreesIntent::Open)
                    | Worktrees(WorktreesIntent::Add)
                    | Worktrees(WorktreesIntent::Remove)
                    | Worktrees(WorktreesIntent::ConfirmRemove)
                    | Commits(CommitsIntent::Reflog(ReflogIntent::Undo))
                    | Commits(CommitsIntent::Reflog(ReflogIntent::Redo))
                    | Commits(CommitsIntent::Reflog(ReflogIntent::ConfirmTravel))
                    | ListEscape
                    | OpenMenu(MenuId::PullRequest)
                    | PullRequests(PullRequestsIntent::ConfirmCreate)
                    // It asks GitHub for the badges again.
                    | PullRequests(PullRequestsIntent::Refresh)
                    | Commits(CommitsIntent::CustomPatch(CustomPatchIntent::OpenMenu))
                    // Review mode reaches GitHub or starts the browser, and
                    // a pick may open a check's page.
                    | Review(ReviewIntent::Enter)
                    | Review(ReviewIntent::OpenPr)
                    | Review(ReviewIntent::Refresh)
                    | Review(ReviewIntent::OpenCheckDetails)
                    // A merge reaches GitHub and cannot be undone.
                    | Review(ReviewIntent::OpenMergePr)
                    | Review(ReviewIntent::ConfirmMergePr)
                    // A checkout moves HEAD and may fetch the head.
                    | Review(ReviewIntent::CheckoutPr)
                    | Review(ReviewIntent::ConfirmCheckoutPr)
                    // It may fetch the pull request's commits.
                    | Review(ReviewIntent::OpenFiles)
                    // A viewed mark reaches GitHub.
                    | Review(ReviewIntent::ToggleViewed)
                    | PullRequests(PullRequestsIntent::PickSelect)
                    // Esc falls through to quit without it.
                    | Review(ReviewIntent::CloseDetail)
                    | PaneEscape
            );
            assert_eq!(risky, REPO_LOCKED.contains(&b.id), "{}", b.id);
            if risky && let Some(feature) = nested_feature(&b.intent) {
                nested_risky.push((feature, b.id));
            }
        }
        // `use AppIntent::*` lets a stale bare name in the pattern above
        // bind anything, so pin the nested bindings it must match, per
        // feature and in registry order.
        let risky_in = |feature| {
            nested_risky
                .iter()
                .filter(|(f, _)| *f == feature)
                .map(|(_, id)| *id)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            risky_in("branches"),
            [
                "branches.delete",
                "remotes.remove",
                "tags.delete",
                "tags.push",
                "delete_branch.yes",
                "delete_branch.force",
                "delete_tag.yes",
                "remove_remote.yes",
            ]
        );
        assert_eq!(
            risky_in("commits"),
            [
                "workspace.patch_menu",
                "workspace.undo",
                "workspace.redo",
                "commits.revert",
                "commits.squash",
                "commits.fixup",
                "commits.create_fixup",
                "commits.autosquash",
                "commits.drop",
                "commits.reword",
                "commits.move_up",
                "commits.move_down",
                "commits.edit",
                "commits.break",
                "commits.pick",
                "commits.paste",
                "status.bisect_good",
                "status.bisect_bad",
                "reset_hard.yes",
                "leave_commits.yes",
                "travel.yes",
            ]
        );
        assert_eq!(
            risky_in("commit"),
            [
                "commit_editor.edit_externally",
                "commit_editor.quit",
                "commit_discard.discard",
            ]
        );
        assert_eq!(risky_in("repos"), ["recent.open", "recent.init"]);
        assert_eq!(
            risky_in("sync"),
            ["workspace.fetch", "workspace.pull", "workspace.push"]
        );
        assert_eq!(
            risky_in("conflicts"),
            [
                "files.mergetool",
                "conflict.ours",
                "conflict.theirs",
                "conflict.both",
                "conflict.mergetool",
            ]
        );
        assert_eq!(risky_in("diff"), ["diff.edit_hunk", "diff.renderer"]);
        assert_eq!(
            risky_in("stash"),
            [
                "stash.files",
                "stash.apply",
                "stash.pop",
                "stash.drop",
                "stash.rename",
                "drop_stash.yes",
            ]
        );
        assert_eq!(
            risky_in("submodules"),
            ["submodules.update", "submodules.init"]
        );
        assert_eq!(
            risky_in("worktrees"),
            [
                "worktrees.open",
                "worktrees.add",
                "worktrees.remove",
                "remove_worktree.yes",
            ]
        );
        assert_eq!(
            risky_in("pull_requests"),
            [
                "branches.refresh_pull_requests",
                "pick_list.pick",
                "create_pull_request.yes",
            ]
        );
        assert_eq!(
            risky_in("review"),
            [
                "workspace.review",
                "review.open",
                "review.refresh",
                "review.open_check",
                "review.merge",
                "review.checkout",
                "review.back",
                "review.files",
                "review_files.viewed",
                "merge_pr.yes",
                "checkout_pr.yes",
            ]
        );
        for feature in ["blame", "custom_commands", "operation", "files"] {
            assert!(risky_in(feature).is_empty(), "{feature}");
        }
    }

    #[test]
    fn a_repository_layer_cannot_turn_a_navigation_key_into_a_confirmed_discard() {
        let attack: &[(&str, &[&str])] = &[
            ("files.next", &["down"]),
            ("files.discard", &["j"]),
            ("discard_confirm.yes", &["j", "y"]),
        ];
        assert_eq!(
            build(attack, Trust::Repo).unwrap_err(),
            r#""discard_confirm.yes" can only be changed in the global config file"#
        );
        assert!(build(&attack[..2], Trust::Repo).is_err());
        assert!(build(attack, Trust::Global).is_ok());
    }

    #[test]
    fn a_repository_layer_cannot_hide_a_locked_key_behind_a_nearer_one() {
        assert_eq!(
            build(&[("files.stage", &["q"])], Trust::Repo).unwrap_err(),
            r#""files.stage": key "q" would hide "global.quit""#
        );
        // Keys a binding already had are not new, so the defaults pass.
        assert!(build(&[("diff.back", &["h", "left"])], Trust::Repo).is_ok());
    }

    fn commands(tables: &[CommandTable]) -> Result<Keymap, String> {
        let (keymap, dropped) = Keymap::default().with_commands(tables, &BTreeSet::new())?;
        assert_eq!(dropped, Vec::<String>::new(), "no layer ids, no drops");
        Ok(keymap)
    }

    fn files_command(keys: &[&str]) -> CommandTable {
        test_table(Panel::Files, keys, &["true"], &[], OutputMode::None)
    }

    /// A global layer's `[keys]` and `[[commands]]`, loaded as the global
    /// file is.
    fn global_with_commands(
        entries: &[(&str, &[&str])],
        tables: &[CommandTable],
    ) -> Result<(Keymap, Vec<String>), String> {
        let (keymap, dropped, kept) =
            Keymap::build_layer(&Keymap::default(), &layer(entries), Trust::Global)?;
        assert_eq!(dropped, Vec::<String>::new(), "a layer without clashes");
        keymap.with_commands(tables, &kept)
    }

    #[test]
    fn a_command_on_a_key_the_layer_gave_a_binding_is_dropped_alone() {
        let (keymap, dropped) = global_with_commands(
            &[("files.stage", &["ctrl-t"])],
            &[
                files_command(&["ctrl-t"]),
                files_command(&["ctrl-y"]),
                files_command(&["ctrl-t"]),
            ],
        )
        .unwrap();
        assert_eq!(
            dropped,
            [
                r#"key "ctrl-t" is bound to both "files.stage" and command 1 ("Run true"); command 1 was dropped"#,
                r#"key "ctrl-t" is bound to both "files.stage" and command 3 ("Run true"); command 3 was dropped"#,
            ]
        );
        assert_eq!(
            keymap.keys(binding("files.stage")),
            [parse_key("ctrl-t").unwrap()]
        );
        let [command] = keymap.commands.as_slice() else {
            panic!("expected one command: {:?}", keymap.commands);
        };
        assert_eq!(command.keys, [parse_key("ctrl-y").unwrap()]);
    }

    #[test]
    fn a_command_clash_the_layer_did_not_make_still_fails_with_the_file_numbers() {
        let moved = &[("files.stage", &["ctrl-t"][..])];
        // Command 1 is dropped; command 3 is on a default key.
        let e = global_with_commands(
            moved,
            &[
                files_command(&["ctrl-t"]),
                files_command(&["ctrl-y"]),
                files_command(&["c"]),
            ],
        )
        .unwrap_err();
        assert_eq!(
            e,
            r#"key "c" is bound to both "files.commit" and command 3"#
        );
        // Two commands on a key no `[keys]` entry holds.
        let e = global_with_commands(
            moved,
            &[
                files_command(&["ctrl-t"]),
                files_command(&["X"]),
                files_command(&["X"]),
            ],
        )
        .unwrap_err();
        assert_eq!(e, r#"key "X" is bound to both command 2 and command 3"#);
        // A command that is left still may not hide a locked binding.
        let e = global_with_commands(moved, &[files_command(&["ctrl-t"]), files_command(&["q"])])
            .unwrap_err();
        assert_eq!(e, r#"command 2: key "q" would hide "global.quit""#);
    }

    #[test]
    fn a_dropped_command_names_every_key_it_clashed_on_and_binds_nothing() {
        let moved = &[("files.stage", &["ctrl-t"][..])];
        // `c` is a default key, `X` is free: only the bindings are named.
        let (keymap, dropped) =
            global_with_commands(moved, &[files_command(&["ctrl-t", "c", "X", "ctrl-t"])]).unwrap();
        assert_eq!(
            dropped,
            [concat!(
                r#"key "ctrl-t" is bound to both "files.stage" and command 1, "#,
                r#"and key "c" is bound to both "files.commit" and command 1 ("Run true"); "#,
                "command 1 was dropped"
            )]
        );
        assert_eq!(keymap.commands, []);
        assert_eq!(
            keymap.keys(binding("files.commit")),
            binding("files.commit").keys
        );
        // On the moved key and on quit's: dropped, so quit keeps its key.
        let (keymap, dropped) =
            global_with_commands(moved, &[files_command(&["ctrl-t", "q"])]).unwrap();
        assert_eq!(dropped.len(), 1, "{dropped:?}");
        assert_eq!(keymap.commands, []);
        let state = AppState::new(Context::Workspace);
        let q = key(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(matches!(
            resolve(registry(), &keymap, KeyScope::Files, &state, q),
            Resolution::Run(b) if b.id == "global.quit"
        ));
    }

    /// A global file may hold 64 KiB of commands, every one on the key a
    /// `[keys]` entry holds. Each is dropped once, without comparing every
    /// pair of them.
    #[test]
    fn hundreds_of_commands_on_a_moved_key_are_dropped_quickly_with_bounded_work() {
        let one = "[[commands]]\ncontext = \"files\"\nkeys = [\"ctrl-t\"]\ndescription = \"d\"\ncommand = [\"true\"]\n";
        let count = 64 * 1024 / one.len();
        assert!(count > 500, "{count}");
        let tables = vec![files_command(&["ctrl-t"]); count];
        let started = std::time::Instant::now();
        let (keymap, dropped) =
            global_with_commands(&[("files.stage", &["ctrl-t"])], &tables).unwrap();
        let took = started.elapsed();
        assert!(took < std::time::Duration::from_secs(2), "{took:?}");
        assert_eq!(dropped.len(), count);
        assert!(dropped[count - 1].ends_with(&format!("command {count} was dropped")));
        assert!(keymap.commands.is_empty());
        assert_eq!(
            keymap.keys(binding("files.stage")),
            [parse_key("ctrl-t").unwrap()]
        );
    }

    #[test]
    fn a_command_key_cannot_clash_in_its_scope() {
        assert_eq!(
            commands(&[files_command(&["c"])]).unwrap_err(),
            r#"key "c" is bound to both "files.commit" and command 1"#
        );
        assert_eq!(
            commands(&[files_command(&["X"]), files_command(&["ctrl-x", "X"])]).unwrap_err(),
            r#"key "X" is bound to both command 1 and command 2"#
        );
        // The same key in another panel is another scope.
        let diff = test_table(Panel::Diff, &["X"], &["true"], &[], OutputMode::None);
        assert!(commands(&[files_command(&["X"]), diff]).is_ok());
        // A key the global file moved away is free for a command.
        let moved = remap(&[("files.commit", &["C"])]);
        assert!(
            moved
                .with_commands(&[files_command(&["c"])], &BTreeSet::new())
                .is_ok()
        );
    }

    #[test]
    fn a_command_key_cannot_hide_a_locked_binding() {
        assert_eq!(
            commands(&[files_command(&["q"])]).unwrap_err(),
            r#"command 1: key "q" would hide "global.quit""#
        );
        assert_eq!(
            commands(&[files_command(&["ctrl-c"])]).unwrap_err(),
            r#"command 1: key "ctrl-c" would hide "global.quit""#
        );
        // An unlocked enclosing binding may be shadowed, as a remap may.
        assert!(commands(&[files_command(&["r"])]).is_ok());
    }

    #[test]
    fn a_bad_command_table_names_its_entry() {
        let e = commands(&[files_command(&["X"]), files_command(&[])]).unwrap_err();
        assert_eq!(e, "command 2: needs at least one key");
    }

    #[test]
    fn a_repository_layer_keeps_the_commands_and_drops_a_key_moved_onto_one() {
        let global = commands(&[files_command(&["X"])]).unwrap();
        let repo = Keymap::build(&global, &layer(&[("files.commit", &["C"])]), Trust::Repo);
        assert_eq!(repo.unwrap().0.commands, global.commands);
        let (keymap, dropped) = Keymap::build(
            &global,
            &layer(&[("files.stage", &["X"]), ("files.commit", &["C"])]),
            Trust::Repo,
        )
        .unwrap();
        assert_eq!(
            dropped,
            [
                r#"key "X" is bound to both "files.stage" and command 1; the change to "files.stage" was dropped"#
            ]
        );
        assert_eq!(
            keymap.keys(binding("files.stage")),
            binding("files.stage").keys
        );
        assert_eq!(keymap.keys(binding("files.commit")), [char_pattern('C')]);
        assert_eq!(keymap.commands, global.commands);
    }

    #[test]
    fn a_command_key_resolves_in_its_panel_and_the_ones_below_it() {
        let diff = test_table(Panel::Diff, &["X"], &["true"], &[], OutputMode::None);
        let keymap = commands(&[files_command(&["ctrl-t"]), diff]).unwrap();
        let state = AppState::new(Context::Workspace);
        let at = |focus, key| resolve(registry(), &keymap, focus, &state, key);
        let ctrl_t = key(KeyCode::Char('t'), KeyModifiers::CONTROL);
        let x = key(KeyCode::Char('X'), KeyModifiers::SHIFT);
        assert!(matches!(
            at(KeyScope::Files, ctrl_t),
            Resolution::Command(0)
        ));
        assert!(matches!(at(KeyScope::Diff, ctrl_t), Resolution::Unbound));
        assert!(matches!(at(KeyScope::Diff, x), Resolution::Command(1)));
        assert!(matches!(at(KeyScope::Lines, x), Resolution::Command(1)));
        assert!(matches!(at(KeyScope::Files, x), Resolution::Unbound));
        assert!(matches!(at(KeyScope::Help, ctrl_t), Resolution::Unbound));
    }

    #[test]
    fn available_commands_are_those_reachable_and_not_shadowed() {
        let diff = |keys: &[&str]| test_table(Panel::Diff, keys, &["true"], &[], OutputMode::None);
        // `J` is lines.extend_down, so in line mode this one is unreachable.
        let keymap = commands(&[files_command(&["X"]), diff(&["J"]), diff(&["Z"])]).unwrap();
        let listed = |focus| -> Vec<usize> {
            available_commands(&keymap, focus)
                .into_iter()
                .map(|c| keymap.commands.iter().position(|k| k == c).unwrap())
                .collect()
        };
        assert_eq!(listed(KeyScope::Files), [0]);
        assert_eq!(listed(KeyScope::Diff), [1, 2]);
        assert_eq!(listed(KeyScope::Lines), [2]);
        assert!(listed(KeyScope::Blame).is_empty());
    }

    #[test]
    fn shift_b_in_commits_opens_the_bisect_menu_and_g_b_mark_from_status() {
        use AppIntent::*;
        assert_eq!(
            from(
                KeyScope::Commits,
                key(KeyCode::Char('B'), KeyModifiers::SHIFT)
            ),
            Some(OpenMenu(MenuId::Bisect))
        );
        let mut state = AppState::new(Context::Workspace);
        state.branch = Some(kika_git::status::BranchStatus {
            head: Head::Detached("abc".into()),
            upstream: None,
            ahead_behind: None,
            in_progress: vec![kika_git::repo_state::Operation::Bisect],
            bisect: Some(kika_git::bisect::BisectState::default()),
        });
        let status = |c| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::Status,
            &state,
            plain(c),
        ) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            other => panic!("{c}: {other:?}"),
        };
        assert_eq!(
            status('g'),
            Some(Commits(CommitsIntent::Bisect(BisectIntent::MarkHead(
                BisectMark::Good
            ))))
        );
        assert_eq!(
            status('b'),
            Some(Commits(CommitsIntent::Bisect(BisectIntent::MarkHead(
                BisectMark::Bad
            ))))
        );
        assert_eq!(status('s'), Some(SkipOperation));
        assert_eq!(status('a'), Some(AbortOperation));
    }

    #[test]
    fn the_clipboard_and_ignore_keys_clash_with_nothing_reachable_from_their_scopes() {
        for id in ["commits.copy_menu", "files.ignore"] {
            let new = binding(id);
            for other in registry() {
                let reachable = new.scope.chain().any(|s| s == other.scope)
                    || other.scope.chain().any(|s| s == new.scope);
                if other.id == id || !reachable {
                    continue;
                }
                for key in new.keys {
                    assert!(
                        !other.keys.iter().any(|k| k.overlaps(key)),
                        "{id} clashes with {}",
                        other.id
                    );
                }
            }
        }
    }

    #[test]
    fn y_and_i_do_nothing_in_the_command_log_and_never_shadow_its_key() {
        // The log is a popup with no parent scope, so Commits' `y` and
        // Files' `i` cannot reach into it, and `@` opens it from both.
        assert_eq!(KeyScope::CommandLog.parent(), None);
        for c in ['y', 'i'] {
            assert_eq!(from(KeyScope::CommandLog, plain(c)), None, "{c}");
        }
        let at = binding("workspace.command_log");
        for id in ["commits.copy_menu", "files.ignore"] {
            let new = binding(id);
            assert!(new.scope.chain().any(|s| s == at.scope), "{id}");
            for key in new.keys {
                assert!(!at.keys.iter().any(|k| k.overlaps(key)), "{id}");
            }
        }
    }

    #[test]
    fn y_opens_the_copy_menu_and_i_ignores_only_an_untracked_file() {
        assert_eq!(
            from(KeyScope::Commits, plain('y')),
            Some(AppIntent::OpenMenu(MenuId::Copy))
        );
        let mut state = AppState::new(Context::Workspace);
        let resolve_i = |state: &AppState| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::Files,
            state,
            plain('i'),
        ) {
            Resolution::Run(binding) => Ok(binding.intent.clone()),
            Resolution::Disabled(reason) => Err(reason),
            other => panic!("{other:?}"),
        };
        assert_eq!(resolve_i(&state), Err("No files".into()));
        let file = |kind, index, worktree| kika_git::FileEntry {
            path: "a.log".into(),
            orig_path: None,
            index,
            worktree,
            kind,
        };
        state.files.rows = vec![file(EntryKind::Changed, '.', 'M')];
        state.files.selected = 0;
        assert_eq!(
            resolve_i(&state),
            Err("Only untracked files can be ignored".into())
        );
        state.files.rows = vec![file(EntryKind::Untracked, '?', '?')];
        assert_eq!(
            resolve_i(&state),
            Ok(AppIntent::Files(FilesIntent::OpenIgnore))
        );
        for id in ["commits.copy_menu", "files.ignore"] {
            assert!(!REPO_LOCKED.contains(&id), "{id}");
        }
    }

    /// Every binding id in registry order. Order is behaviour: `resolve`
    /// takes the first match and help lists bindings in this order. A new
    /// binding goes into its chunk and into this list in the same change.
    const PINNED_ORDER: &[&str] = &[
        "global.help",
        "global.quit",
        "workspace.refresh",
        "workspace.fetch",
        "workspace.pull",
        "workspace.push",
        "workspace.scroll_down",
        "workspace.scroll_up",
        "workspace.page_down",
        "workspace.page_up",
        "workspace.parent",
        "workspace.more_context",
        "workspace.less_context",
        "workspace.whitespace",
        "workspace.focus_status",
        "workspace.focus_files",
        "workspace.focus_branches",
        "workspace.focus_commits",
        "workspace.focus_stash",
        "workspace.focus_diff",
        "workspace.next_panel",
        "workspace.prev_panel",
        "workspace.zoom",
        "workspace.patch_menu",
        "workspace.cancel_command",
        "workspace.undo",
        "workspace.redo",
        "files.next",
        "files.prev",
        "commits.next",
        "commits.prev",
        "commits.open",
        "commits.checkout",
        "commits.revert",
        "commits.reset",
        "commits.squash",
        "commits.fixup",
        "commits.create_fixup",
        "commits.autosquash",
        "commits.find_base",
        "commits.drop",
        "commits.reword",
        "commits.move_up",
        "commits.move_down",
        "commits.edit",
        "commits.break",
        "commits.pick",
        "commits.switch_tab",
        "commits.copy",
        "commits.paste",
        "commits.clear_copied",
        "commits.copy_menu",
        "reflog.next",
        "reflog.prev",
        "reflog.switch_tab",
        "reflog.filter",
        "reflog.escape",
        "commit_files.next",
        "commit_files.prev",
        "commit_files.patch",
        "commit_files.back",
        "stash.next",
        "stash.prev",
        "stash.files",
        "stash.apply",
        "stash.pop",
        "stash.drop",
        "stash.rename",
        "stash.filter",
        "stash.range",
        "stash.escape",
        "stash_files.next",
        "stash_files.prev",
        "stash_files.close",
        "files.stage",
        "files.stage_all",
        "files.commit",
        "files.amend",
        "files.discard",
        "files.open",
        "files.submodules",
        "workspace.worktrees",
        "workspace.command_log",
        "workspace.recent",
        "workspace.review",
        "review.next",
        "review.prev",
        "review.open",
        "review.next_queue",
        "review.prev_queue",
        "review.filter",
        "review.refresh",
        "review.open_check",
        "review.merge",
        "review.checkout",
        "review.back",
        "review.files",
        "review.conversation",
        "review.page_down",
        "review.page_up",
        "review_files.focus",
        "review_files.viewed",
        "review_files.commit",
        "review_files.next_thread",
        "review_files.prev_thread",
        "review_files.thread",
        "review_diff.down",
        "review_diff.up",
        "review_diff.page_down",
        "review_diff.page_up",
        "review_diff.search",
        "review_diff.search_next",
        "review_diff.search_prev",
        "review_diff.escape",
        "review_thread.down",
        "review_thread.up",
        "review_thread.page_down",
        "review_thread.page_up",
        "review_thread.next",
        "review_thread.prev",
        "review_thread.close",
        "review_thread.quit",
        "review_files.comment",
        "review_files.pending",
        "review_pick.down",
        "review_pick.up",
        "review_pick.range",
        "review_pick.confirm",
        "review_pick.cancel",
        "review_pick.quit",
        "review_comment.save",
        "review_comment.cancel",
        "review_comment.quit",
        "review_pending.down",
        "review_pending.up",
        "review_pending.edit",
        "review_pending.delete",
        "review_pending.close",
        "review_pending.quit",
        "files.blame",
        "files.hunks",
        "files.mergetool",
        "files.ignore",
        "diff.next",
        "diff.prev",
        "diff.stage",
        "diff.lines",
        "diff.side",
        "diff.discard",
        "diff.open",
        "diff.edit_hunk",
        "diff.renderer",
        "diff.back",
        "lines.next",
        "lines.prev",
        "lines.extend_down",
        "lines.extend_up",
        "lines.stage",
        "lines.back",
        "lines.discard",
        "lines.open",
        "blame.next",
        "blame.prev",
        "blame.page_down",
        "blame.page_up",
        "blame.commit",
        "blame.back",
        "conflict.next",
        "conflict.prev",
        "conflict.ours",
        "conflict.theirs",
        "conflict.both",
        "conflict.mergetool",
        "conflict.back",
        "blame_commit.scroll_down",
        "blame_commit.scroll_up",
        "blame_commit.page_down",
        "blame_commit.page_up",
        "blame_commit.back",
        "files.menu",
        "files.stash",
        "commit_confirm.stage_all",
        "commit_confirm.cancel",
        "commit_editor.commit",
        "commit_editor.switch_field",
        "commit_editor.skip_hooks",
        "commit_editor.edit_externally",
        "commit_editor.cancel",
        "commit_editor.quit",
        "commit_discard.discard",
        "commit_discard.keep",
        "commit_error.back",
        "commit_loading.cancel",
        "commit_loading.quit",
        "help.close",
        "help.down",
        "help.up",
        "help.page_down",
        "help.page_up",
        "menu.next",
        "menu.prev",
        "menu.select",
        "menu.back",
        "menu.quit",
        "confirm.yes",
        "confirm.no",
        "prompt.ok",
        "prompt.cancel",
        "prompt.quit",
        "credential.answer",
        "credential.cancel",
        "credential.quit",
        "error.close",
        "error.quit",
        "discard_confirm.yes",
        "discard_confirm.no",
        "discard_confirm.quit",
        "branches.next",
        "branches.prev",
        "branches.checkout",
        "branches.new",
        "branches.rename",
        "branches.delete",
        "branches.merge",
        "branches.pull_request",
        "branches.refresh_pull_requests",
        "branches.set_upstream",
        "branches.unset_upstream",
        "branches.switch_tab",
        "branches.prev_tab",
        "remotes.next",
        "remotes.prev",
        "remotes.checkout",
        "remotes.add",
        "remotes.rename",
        "remotes.remove",
        "remotes.switch_tab",
        "remotes.prev_tab",
        "tags.next",
        "tags.prev",
        "tags.delete",
        "tags.push",
        "tags.switch_tab",
        "tags.prev_tab",
        "commits.tag",
        "commits.tag_annotated",
        "commits.branch_at_head",
        "commits.bisect",
        "status.continue",
        "status.skip",
        "status.abort",
        "status.bisect_good",
        "status.bisect_bad",
        "delete_branch.yes",
        "delete_branch.force",
        "delete_branch.no",
        "delete_branch.quit",
        "delete_tag.yes",
        "delete_tag.no",
        "delete_tag.quit",
        "delete_range.yes",
        "delete_range.no",
        "delete_range.quit",
        "remove_remote.yes",
        "remove_remote.no",
        "remove_remote.quit",
        "reset_hard.yes",
        "reset_hard.no",
        "reset_hard.quit",
        "leave_commits.yes",
        "leave_commits.no",
        "leave_commits.quit",
        "drop_stash.yes",
        "drop_stash.no",
        "drop_stash.quit",
        "abort_confirm.yes",
        "abort_confirm.no",
        "abort_confirm.quit",
        "skip_confirm.yes",
        "skip_confirm.no",
        "skip_confirm.quit",
        "autosquash_confirm.yes",
        "autosquash_confirm.no",
        "autosquash_confirm.quit",
        "patch_confirm.yes",
        "patch_confirm.no",
        "patch_confirm.quit",
        "drop_range_confirm.yes",
        "drop_range_confirm.no",
        "drop_range_confirm.quit",
        "travel.yes",
        "travel.no",
        "travel.quit",
        "submodules.next",
        "submodules.prev",
        "submodules.enter",
        "submodules.update",
        "submodules.init",
        "submodules.close",
        "submodules.quit",
        "worktrees.next",
        "worktrees.prev",
        "worktrees.open",
        "worktrees.add",
        "worktrees.remove",
        "worktrees.filter",
        "worktrees.escape",
        "worktrees.close",
        "worktrees.quit",
        "command_log.next",
        "command_log.prev",
        "command_log.page_down",
        "command_log.page_up",
        "command_log.filter",
        "command_log.escape",
        "command_log.close",
        "command_log.quit",
        "remove_worktree.yes",
        "remove_worktree.no",
        "remove_worktree.quit",
        "recent.next",
        "recent.prev",
        "recent.open",
        "recent.init",
        "recent.close",
        "recent.quit",
        "pick_list.next",
        "pick_list.prev",
        "pick_list.pick",
        "pick_list.close",
        "pick_list.quit",
        "create_pull_request.yes",
        "create_pull_request.no",
        "create_pull_request.quit",
        "merge_pr.yes",
        "merge_pr.no",
        "merge_pr.quit",
        "checkout_pr.yes",
        "checkout_pr.no",
        "checkout_pr.quit",
        "review_comment_confirm.yes",
        "review_comment_confirm.no",
        "review_comment_confirm.quit",
        "files.filter",
        "files.range",
        "files.escape",
        "commits.filter",
        "commits.range",
        "commits.escape",
        "branches.filter",
        "branches.range",
        "branches.escape",
        "remotes.filter",
        "remotes.range",
        "remotes.escape",
        "tags.filter",
        "tags.range",
        "tags.escape",
        "diff.search",
        "diff.search_next",
        "diff.search_prev",
        "diff.escape",
        "files.search_diff",
        "files.search_next",
        "files.search_prev",
        "commit_files.search",
        "commit_files.search_next",
        "commit_files.search_prev",
        "commit_files.filter",
        "commit_files.escape",
        "stash_files.search",
        "stash_files.search_next",
        "stash_files.search_prev",
        "stash_files.escape",
        "blame.search",
        "blame.search_next",
        "blame.search_prev",
        "blame.escape",
        "blame_commit.search",
        "blame_commit.search_next",
        "blame_commit.search_prev",
        "blame_commit.escape",
        "input.accept",
        "input.cancel",
        "commits.filter_path",
        "commits.filter_author",
        "commits.mark",
        "reflog.filter_path",
        "reflog.filter_author",
        "branches.mark",
        "branches.diff",
        "files.log",
        "input.quit",
    ];

    #[test]
    fn registry_order_is_pinned() {
        let ids = registry().iter().map(|b| b.id).collect::<Vec<_>>();
        assert_eq!(ids, PINNED_ORDER);
    }

    #[test]
    fn chunks_flatten_to_registry() {
        assert!(CHUNKS.iter().all(|chunk| !chunk.is_empty()));
        let total: usize = CHUNKS.iter().map(|chunk| chunk.len()).sum();
        assert_eq!(total, registry().len());
    }
}
