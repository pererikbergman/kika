//! Every key Kika understands, declared once. The same records drive
//! dispatch, the help menu and the bottom bar, so they cannot drift apart.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::bisect;
use crate::commit_files::CommitFilesList;
use crate::conflict::Choice;
use crate::custom_command::{CommandTable, CustomCommand};
use crate::custom_patch::{PatchAction, PatchChoice};
use crate::diff_mark::DiffMark;
use crate::external_diff::NO_RENDERER;
use crate::git::bisect::BisectMark;
use crate::git::commit_files::{CommitId, CommitRange};
use crate::git::diff::{DiffSide, MAX_CONTEXT, MIN_CONTEXT};
use crate::git::patch::{FileKind, PatchError};
use crate::git::rebase_plan::{self, Direction, Edit, RebasePlan, TodoEntry};
use crate::git::remotes::RemoteName;
use crate::git::repo_state::{Operation, resumable};
use crate::git::status::{EntryKind, Head};
use crate::git::submodule::SubmoduleState;
use crate::git::{branch_tips, version};
use crate::hunk_view::NO_CHANGES;
use crate::intent::{AppIntent, Scroll};
use crate::layout::{Panel, SidePanel};
use crate::list_filter::LogFilterField;
use crate::menu::MenuId;
use crate::model::{AppState, CommitFlow, Focus};
use crate::popup::{DeleteBranchConfirm, Popup};
use crate::rebase_todo::Rewrite;

/// Why a key or menu item cannot run, shown to the user. Most reasons are
/// fixed text; some name a branch or a commit.
pub type Reason = std::borrow::Cow<'static, str>;
use crate::worktree_list::{
    add_worktree_disabled, open_worktree_disabled, remove_worktree_disabled, worktrees_disabled,
};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// "Reset hard and lose every uncommitted change?" on top of the popup
    /// stack.
    ResetHard,
    /// "Leave these commits behind?" on top of the popup stack.
    LeaveCommits,
    /// "Drop this stash?" on top of the popup stack.
    DropStash,
    /// "Abort the operation in progress?" on top of the popup stack.
    AbortConfirm,
    /// "Fold every fixup! commit?" on top of the popup stack.
    AutosquashConfirm,
    /// "Rewrite the history with the custom patch?" on top of the popup
    /// stack.
    PatchConfirm,
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
            | KeyScope::ResetHard
            | KeyScope::LeaveCommits
            | KeyScope::DropStash
            | KeyScope::AbortConfirm
            | KeyScope::AutosquashConfirm
            | KeyScope::PatchConfirm
            | KeyScope::Submodules
            | KeyScope::Worktrees
            | KeyScope::RemoveWorktree
            | KeyScope::Recent
            | KeyScope::Credential
            | KeyScope::Travel
            | KeyScope::Input
            | KeyScope::CommandLog
            | KeyScope::PickList
            | KeyScope::CreatePullRequest => None,
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
            KeyScope::ResetHard => "Reset hard",
            KeyScope::LeaveCommits => "Leave commits behind",
            KeyScope::DropStash => "Drop stash",
            KeyScope::AbortConfirm => "Abort",
            KeyScope::AutosquashConfirm => "Autosquash",
            KeyScope::PatchConfirm => "Custom patch",
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
        }
    }

    /// What the bottom bar says before the key hints, for scopes whose
    /// keys behave unlike everywhere else.
    pub fn bar_note(self) -> Option<&'static str> {
        match self {
            KeyScope::CommitEditor | KeyScope::Credential | KeyScope::Input => {
                Some("q and ? are typed")
            }
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
    fn matches(&self, key: KeyPress) -> bool {
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
    "remotes.remove",
    "remove_remote.yes",
    "remove_remote.no",
    "remove_remote.quit",
    "tags.delete",
    "tags.push",
    "delete_tag.yes",
    "delete_tag.no",
    "delete_tag.quit",
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
    "worktrees.open",
    "worktrees.add",
    "worktrees.remove",
    "worktrees.quit",
    "remove_worktree.yes",
    "remove_worktree.no",
    "remove_worktree.quit",
    "workspace.undo",
    "workspace.redo",
    "travel.yes",
    "travel.no",
    "travel.quit",
    "branches.pull_request",
    "pick_list.quit",
    "create_pull_request.yes",
    "create_pull_request.no",
    "create_pull_request.quit",
    "command_log.quit",
    "commits.paste",
    "files.escape",
    "commits.escape",
    "input.quit",
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
enum Owner<'a> {
    Binding(&'a Binding),
    Command(usize),
}

impl std::fmt::Display for Owner<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Owner::Binding(b) => write!(f, "\"{}\"", b.id),
            Owner::Command(i) => write!(f, "command {}", i + 1),
        }
    }
}

impl Keymap {
    /// The keys `b` answers to.
    pub fn keys<'a>(&'a self, b: &'a Binding) -> &'a [KeyPattern] {
        self.overrides.get(b.id).map_or(b.keys, Vec::as_slice)
    }

    /// `base` with `layer` applied on top. Fails, leaving nothing applied,
    /// on an unknown id or key, a key bound twice in one scope, quit left
    /// without keys, or a repository layer reaching past its trust.
    pub fn build(base: &Keymap, layer: &KeysTable, trust: Trust) -> Result<Keymap, String> {
        let mut keymap = base.clone();
        for (id, texts) in layer {
            let binding = REGISTRY
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
        }
        keymap.check_scopes()?;
        if trust == Trust::Repo {
            keymap.check_locked_not_hidden(base)?;
        }
        let quit = REGISTRY.iter().find(|b| b.id == "global.quit");
        if quit.is_some_and(|b| keymap.keys(b).is_empty()) {
            return Err("\"global.quit\" needs at least one key".to_string());
        }
        Ok(keymap)
    }

    /// `self` with the global file's `[[commands]]`, in place of any it
    /// had. Fails on a table that does not parse, a command key that
    /// clashes with another key of its scope, or one that would hide a
    /// locked binding in an enclosing scope, such as `q` hiding quit.
    pub fn with_commands(self, tables: &[CommandTable]) -> Result<Keymap, String> {
        let commands = tables
            .iter()
            .enumerate()
            .map(|(i, table)| CustomCommand::parse(table, i + 1))
            .collect::<Result<Vec<_>, _>>()?;
        let keymap = Keymap { commands, ..self };
        keymap.check_scopes()?;
        for (i, command) in keymap.commands.iter().enumerate() {
            let locked = REGISTRY.iter().filter(|b| REPO_LOCKED.contains(&b.id));
            for binding in locked.filter(|b| command.scope.chain().any(|s| s == b.scope)) {
                let hidden = keymap
                    .keys(binding)
                    .iter()
                    .find(|k| command.keys.iter().any(|key| key.overlaps(k)));
                if let Some(key) = hidden {
                    return Err(format!(
                        "command {}: key \"{}\" would hide \"{}\"",
                        i + 1,
                        key.label(),
                        binding.id
                    ));
                }
            }
        }
        Ok(keymap)
    }

    /// Every key in force with its owner and scope: the registry's, then
    /// the commands'.
    fn owned_keys(&self) -> Vec<(Owner<'_>, KeyScope, &KeyPattern)> {
        let builtin = REGISTRY.iter().flat_map(|b| {
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
        let patterns = self.owned_keys();
        for (i, (a, scope_a, ka)) in patterns.iter().enumerate() {
            for (b, scope_b, kb) in &patterns[i + 1..] {
                if scope_a == scope_b && ka.overlaps(kb) {
                    return Err(format!(
                        "key \"{}\" is bound to both {a} and {b}",
                        kb.label(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// A key a repository file adds in a nearer scope would take a locked
    /// binding's key away from it, as surely as removing it would.
    fn check_locked_not_hidden(&self, base: &Keymap) -> Result<(), String> {
        for binding in REGISTRY {
            let added = self
                .keys(binding)
                .iter()
                .filter(|k| !base.keys(binding).contains(k));
            for key in added {
                let hidden = REGISTRY.iter().find(|locked| {
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
    Keymap::build(&Keymap::default(), &table, Trust::Global).expect("a valid layer")
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
        let builtin = REGISTRY.iter().filter(|b| b.scope == scope);
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
fn whitespace_hidden(state: &AppState) -> Option<Reason> {
    let in_diff = matches!(state.focus, Focus::Diff(_));
    (in_diff && state.diff_options.ignore_whitespace).then_some(WHITESPACE_HIDDEN.into())
}

/// A diff tool draws the files, not the patch, so nothing in its view can
/// be staged, discarded or edited.
pub const DISPLAY_ONLY: &str = "The diff tool view is display-only: press R for the built-in diff";

/// The Diff pane shows a tool's rows, which hunk and line keys cannot act on.
fn display_only(state: &AppState) -> Option<Reason> {
    let in_diff = matches!(state.focus, Focus::Diff(_));
    (in_diff && state.display_only()).then_some(DISPLAY_ONLY.into())
}

/// The diff keys of the Workspace scope reach the blame through its
/// parent, but the diff they would change is hidden behind it.
const BLAME_OPEN: &str = "Close the blame first";

fn blame_open(state: &AppState) -> Option<Reason> {
    matches!(state.focus, Focus::Blame(_)).then_some(BLAME_OPEN.into())
}

/// The diff settings reach the conflict view the same way, and the diff
/// is just as hidden.
fn diff_hidden(state: &AppState) -> Option<Reason> {
    blame_open(state).or_else(|| {
        matches!(state.focus, Focus::Conflict(_)).then_some("Close the conflict view first".into())
    })
}

fn most_context(state: &AppState) -> Option<Reason> {
    diff_hidden(state).or_else(|| {
        (state.diff_options.context >= MAX_CONTEXT).then_some("Already at the most context".into())
    })
}

fn least_context(state: &AppState) -> Option<Reason> {
    diff_hidden(state).or_else(|| {
        (state.diff_options.context <= MIN_CONTEXT).then_some("Already at the least context".into())
    })
}

/// Enter has no commit to open before the log has rows.
/// Why a key that acts on a log commit does nothing: there is none, or
/// the cursor is on the rebase's todo list above the log.
fn no_commit(state: &AppState) -> Option<Reason> {
    if state.commits.todo_selected.is_some() {
        return Some("Select a commit in the log first".into());
    }
    state.commits.rows.is_empty().then_some("No commits".into())
}

/// Nothing in the Stash panel can be opened or acted on before it lists
/// an entry.
fn no_stash(state: &AppState) -> Option<Reason> {
    state
        .stash
        .entries
        .is_empty()
        .then_some("No stashes".into())
}

/// Why the selected stash cannot be applied, popped, dropped or renamed
/// now: git runs one operation at a time, and the list needs an entry.
pub(crate) fn stash_action_disabled(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
        Some(OP_RUNNING.into())
    } else {
        no_stash(state)
    }
}

/// The Stash menu has nothing to offer on a clean tree.
fn stash_menu_disabled(state: &AppState) -> Option<Reason> {
    state
        .files
        .is_empty()
        .then_some("No changes to stash".into())
}

/// Why nothing can be copied from the Commits panel.
pub(crate) fn no_commit_selected(state: &AppState) -> Option<Reason> {
    let row = state.commits.rows.get(state.commits.selected);
    row.is_none().then_some("No commit selected".into())
}

/// Why `i` cannot add the selected file to `.gitignore`. A tracked file
/// stays tracked whatever `.gitignore` says, and Kika never untracks one.
pub(crate) fn ignore_disabled(state: &AppState) -> Option<Reason> {
    if state.files.is_empty() {
        return Some("No files".into());
    }
    match state.selected.and_then(|i| state.files.get(i)) {
        Some(file) if file.kind == EntryKind::Untracked => None,
        _ => Some("Only untracked files can be ignored".into()),
    }
}

/// With no renderer configured there is nothing to cycle.
fn renderer_disabled(state: &AppState) -> Option<Reason> {
    state.renderers.is_empty().then_some(NO_RENDERER.into())
}

/// Why `b` cannot blame the selected file. An untracked or deleted file
/// has no lines git can blame.
pub(crate) fn blame_disabled(state: &AppState) -> Option<Reason> {
    match state.selected.and_then(|i| state.files.get(i)) {
        None => Some("No file selected".into()),
        Some(file) if file.kind == EntryKind::Untracked => {
            Some("Untracked file: nothing to blame".into())
        }
        Some(file) if file.worktree == 'D' || file.index == 'D' => {
            Some("The file is deleted: nothing to blame".into())
        }
        Some(_) => None,
    }
}

/// Why the conflict view cannot pick a side now: one write at a time, and
/// only in a file whose blocks were read.
pub(crate) fn pick_disabled(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
        return Some(OP_RUNNING.into());
    }
    match &state.focus {
        Focus::Conflict(view) if view.loaded.is_some() => None,
        _ => Some("Nothing to pick".into()),
    }
}

/// Why the merge tool cannot start now: in the conflict view it is for
/// that file; from Files, only for a conflicted one.
pub(crate) fn mergetool_disabled(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
        return Some(OP_RUNNING.into());
    }
    if matches!(state.focus, Focus::Conflict(_)) {
        return None;
    }
    match state.selected.and_then(|i| state.files.get(i)) {
        Some(file) if file.kind == EntryKind::Conflicted => None,
        _ => Some("Not a conflicted file".into()),
    }
}

pub(crate) fn stage_disabled(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
        return Some(OP_RUNNING.into());
    }
    match state.selected.and_then(|i| state.files.get(i)) {
        None => Some("No file selected".into()),
        Some(file) if file.kind == EntryKind::Conflicted => {
            Some("Resolve the conflict before staging this file".into())
        }
        Some(_) => None,
    }
}

fn no_command_running(state: &AppState) -> Option<Reason> {
    (!state.command_running).then_some("No command is running".into())
}

/// Why the diff cannot take focus: with a commit's files open, the hunks
/// are its selected file's, picked for the custom patch; otherwise the
/// selected working-tree file's.
fn focus_diff_disabled(state: &AppState) -> Option<Reason> {
    match &state.commits.open {
        Some(open) if state.focus == Focus::Commits => {
            commit_source_disabled(state, open).or_else(|| {
                open.selected_file()
                    .is_none()
                    .then_some("No file selected".into())
            })
        }
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
        .rows
        .iter()
        .any(|row| row.oid == open.range.oid.as_str() && row.parents.len() > 1);
    merge.then_some("Cannot pick from a merge commit".into())
}

/// Why picks from `range` cannot join the custom patch: it holds another
/// commit's.
pub(crate) fn other_commit_disabled(state: &AppState, range: &CommitRange) -> Option<Reason> {
    match &state.custom_patch {
        Some(patch) if !patch.is_from(range) => Some(patch.other_commit_notice().into()),
        _ => None,
    }
}

/// Why the selected commit file cannot be picked whole.
pub(crate) fn patch_file_disabled(state: &AppState) -> Option<Reason> {
    let Some(open) = &state.commits.open else {
        return Some("No commit open".into());
    };
    if let Some(reason) =
        commit_source_disabled(state, open).or_else(|| other_commit_disabled(state, &open.range))
    {
        return Some(reason);
    }
    match open.selected_file().map(|f| f.status) {
        None => Some("No file selected".into()),
        Some('R') => Some(PatchError::Renamed.message().into()),
        Some('C') => Some(PatchError::Copied.message().into()),
        Some(_) => None,
    }
}

fn no_file_selected(state: &AppState) -> Option<Reason> {
    let selected = state.selected.and_then(|i| state.files.get(i));
    selected.is_none().then_some("No file selected".into())
}

/// Why the selected hunk cannot be staged or unstaged right now.
pub(crate) fn hunk_disabled(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
        return Some(OP_RUNNING.into());
    }
    let Focus::Diff(view) = &state.focus else {
        return Some(NO_CHANGES.into());
    };
    if let Some(reason) = display_only(state).or_else(|| whitespace_hidden(state)) {
        return Some(reason);
    }
    if view.loading() {
        return Some("Loading…".into());
    }
    // A commit's file is not the selected working-tree one.
    let entry = state
        .selected
        .and_then(|i| state.files.get(i))
        .filter(|_| view.commit.is_none());
    match entry.map(|e| e.kind) {
        Some(EntryKind::Conflicted) => return Some("Resolve the conflict first".into()),
        Some(EntryKind::Renamed | EntryKind::Copied) => {
            return Some(PatchError::Renamed.message().into());
        }
        _ => {}
    }
    match &view.patch {
        Some(patch) if patch.kind == FileKind::Binary => Some(PatchError::Binary.message().into()),
        Some(patch) if !patch.hunks.is_empty() => None,
        _ => Some(NO_CHANGES.into()),
    }
}

/// Why a key that works on the working tree does nothing in a commit's
/// change: the commit is history, not the worktree.
pub(crate) const NOT_IN_COMMIT: &str = "Not in a commit's change";

/// The Diff pane shows a commit's change, which has no sides, nothing to
/// discard and no hunk to edit before staging.
fn in_commit(state: &AppState) -> Option<Reason> {
    matches!(&state.focus, Focus::Diff(view) if view.commit.is_some())
        .then_some(NOT_IN_COMMIT.into())
}

/// An untracked file has only one side to show.
fn side_disabled(state: &AppState) -> Option<Reason> {
    if let Some(reason) = in_commit(state) {
        return Some(reason);
    }
    match &state.focus {
        Focus::Diff(view) if view.side == DiffSide::Untracked => {
            Some("Untracked file: nothing is staged".into())
        }
        _ => None,
    }
}

pub(crate) fn stage_all_disabled(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
        Some(OP_RUNNING.into())
    } else if state.files.is_empty() {
        Some("No changes to stage".into())
    } else if state.files.iter().any(|f| f.kind == EntryKind::Conflicted) {
        Some(crate::git::ops::CONFLICTS_REFUSAL.into())
    } else {
        None
    }
}

pub(crate) fn commit_disabled(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
        Some(OP_RUNNING.into())
    } else if state.files.is_empty() {
        Some("Nothing to commit".into())
    } else if state.files.iter().any(|f| f.kind == EntryKind::Conflicted) {
        Some("Resolve conflicts before committing".into())
    } else {
        None
    }
}

/// Why the last commit cannot be amended right now. A clean tree is
/// fine: amending then only changes the message.
pub(crate) fn amend_disabled(state: &AppState) -> Option<Reason> {
    let no_commit = match &state.branch {
        None => true,
        Some(branch) => matches!(branch.head, Head::Unborn(_)),
    };
    if state.op_seq.is_some() {
        Some(OP_RUNNING.into())
    } else if no_commit {
        Some("No commit to amend".into())
    } else if state.files.iter().any(|f| f.kind == EntryKind::Conflicted) {
        Some("Resolve conflicts before amending".into())
    } else {
        None
    }
}

/// Why `d` cannot ask right now. What it would discard, and why that is
/// refused, is the model's call once it runs.
pub(crate) fn discard_disabled(state: &AppState) -> Option<Reason> {
    if let Some(reason) = in_commit(state) {
        return Some(reason);
    }
    if state.op_seq.is_some() {
        return Some(OP_RUNNING.into());
    }
    if let Some(reason) = display_only(state).or_else(|| whitespace_hidden(state)) {
        return Some(reason);
    }
    match &state.focus {
        Focus::Diff(view) if view.loading() => Some("Loading…".into()),
        _ => no_file_selected(state),
    }
}

/// Why `o` cannot open the editor right now.
fn open_disabled(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
        return Some(OP_RUNNING.into());
    }
    no_file_selected(state)
}

/// Why `e` cannot edit the selected hunk right now. What kind of change
/// can be edited is the model's call once it runs.
fn edit_hunk_disabled(state: &AppState) -> Option<Reason> {
    if let Some(reason) = in_commit(state) {
        return Some(reason);
    }
    if state.op_seq.is_some() {
        return Some(OP_RUNNING.into());
    }
    let Focus::Diff(view) = &state.focus else {
        return Some(NO_CHANGES.into());
    };
    if let Some(reason) = display_only(state).or_else(|| whitespace_hidden(state)) {
        return Some(reason);
    }
    if view.lines.is_some() {
        return Some("Press Esc to edit the whole hunk".into());
    }
    if view.loading() {
        return Some("Loading…".into());
    }
    match &view.patch {
        Some(patch) if !patch.hunks.is_empty() => None,
        _ => Some(NO_CHANGES.into()),
    }
}

/// The editor's keys wait while the commit runs, so the message cannot change.
fn commit_editor_disabled(state: &AppState) -> Option<Reason> {
    let running = matches!(state.commit, CommitFlow::Running(_)) || state.op_seq.is_some();
    running.then_some(OP_RUNNING.into())
}

fn no_submodule(state: &AppState) -> Option<Reason> {
    state
        .selected_submodule()
        .is_none()
        .then_some("No submodule selected".into())
}

/// Why `i` or `u` cannot run on the selected submodule right now.
fn submodule_op_disabled(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
        return Some(OP_RUNNING.into());
    }
    no_submodule(state)
}

/// Why the selected submodule cannot be entered: it must be surely checked
/// out, and an operation must not be running in the repository being left.
pub(crate) fn enter_submodule_disabled(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
        return Some("Wait for the operation to finish".into());
    }
    match state.selected_submodule() {
        None => Some("No submodule selected".into()),
        Some(sub) if sub.state == SubmoduleState::Uninitialized => {
            Some("Update the submodule first".into())
        }
        // Git marks a conflict before it looks for a checkout, so the
        // directory may hold none, and git there would find the parent.
        Some(sub) if sub.state == SubmoduleState::Conflicted => {
            Some("Resolve the submodule conflict first".into())
        }
        Some(_) => None,
    }
}

/// Why there is no parent to go back to right now.
pub(crate) fn leave_submodule_disabled(state: &AppState) -> Option<Reason> {
    if state.repo_stack.is_empty() {
        Some("Not in a submodule".into())
    } else if state.op_seq.is_some() {
        Some("Wait for the operation to finish".into())
    } else {
        None
    }
}

/// Why `i` cannot make the launch directory a repository: it is offered
/// only by the recent list shown outside one.
pub(crate) fn init_repo_disabled(state: &AppState) -> Option<Reason> {
    let offered = matches!(state.popups.last(), Some(Popup::Recent(list)) if list.offer_init);
    if state.repo.is_some() || !offered {
        Some("Already in a repository".into())
    } else if state.busy > 0 {
        Some("Loading…".into())
    } else {
        None
    }
}

/// Why no branch action can run right now: one needs a selected branch,
/// and git one operation at a time.
pub(crate) fn branch_action_disabled(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
        Some(OP_RUNNING.into())
    } else if state.branches.selected_branch().is_none() {
        Some("No branch selected".into())
    } else {
        None
    }
}

pub(crate) fn checkout_branch_disabled(state: &AppState) -> Option<Reason> {
    branch_action_disabled(state).or_else(|| {
        let selected = state.branches.selected_branch();
        selected
            .is_some_and(|b| b.head)
            .then_some("Already on this branch".into())
    })
}

pub(crate) fn delete_branch_disabled(state: &AppState) -> Option<Reason> {
    branch_action_disabled(state).or_else(|| {
        let selected = state.branches.selected_branch();
        selected
            .is_some_and(|b| b.head)
            .then_some("Cannot delete the current branch".into())
    })
}

/// Why the selected local branch's upstream cannot be removed: it needs
/// one.
pub(crate) fn unset_upstream_disabled(state: &AppState) -> Option<Reason> {
    branch_action_disabled(state).or_else(|| {
        let selected = state.branches.selected_branch();
        selected
            .is_some_and(|b| b.upstream.is_none())
            .then_some("No upstream to unset".into())
    })
}

/// Why no remote can be added now: git one operation at a time.
pub(crate) fn add_remote_disabled(state: &AppState) -> Option<Reason> {
    state.op_seq.is_some().then_some(OP_RUNNING.into())
}

/// Why the selected remote branch cannot be checked out: a header row
/// is a remote, not a branch.
pub(crate) fn track_disabled(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
        Some(OP_RUNNING.into())
    } else if state.branches.remotes.selected_branch().is_none() {
        Some("Select a remote branch".into())
    } else {
        None
    }
}

/// Why the selected remote cannot be renamed or removed. `remote rename`
/// and `remote remove` take no `--` in older git, so a name like an option
/// is never passed; nor is a group of refs no configured remote owns.
pub(crate) fn change_remote_disabled(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
        return Some(OP_RUNNING.into());
    }
    match state.branches.remotes.selected_remote() {
        None => Some("No remote selected".into()),
        Some(remote) if !remote.configured => Some("Not a configured remote".into()),
        Some(remote) if remote.name.0.starts_with(b"-") => {
            Some("Change a remote named like an option with git directly".into())
        }
        Some(_) => None,
    }
}

/// Why the selected commit cannot be tagged: the log needs a commit with
/// an id git can use. Not refused while an operation runs, since the
/// prompt comes first; the answer waits for it instead.
pub(crate) fn tag_commit_disabled(state: &AppState) -> Option<Reason> {
    let row = state.commits.rows.get(state.commits.selected);
    let valid = row.is_some_and(|r| CommitId::parse(&r.oid).is_some());
    (!valid).then_some("No commit".into())
}

/// Why the Tags tab has nothing to act on.
pub(crate) fn tag_selected_disabled(state: &AppState) -> Option<Reason> {
    state
        .branches
        .tags
        .selected_tag()
        .is_none()
        .then_some("No tag".into())
}

/// Why the selected tag cannot be deleted now: git one operation at a
/// time.
pub(crate) fn delete_tag_disabled(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
        Some(OP_RUNNING.into())
    } else {
        tag_selected_disabled(state)
    }
}

/// Why the selected tag cannot be pushed: a push needs the slot, a tag,
/// and a remote Kika can choose without asking.
pub(crate) fn push_tag_disabled(state: &AppState) -> Option<Reason> {
    remote_disabled(state)
        .or_else(|| tag_selected_disabled(state))
        .or_else(|| push_tag_remote(state).err().map(Reason::from))
}

/// The remote a tag is pushed to: the current branch's upstream's, else
/// the only configured one. Never a picker; the status bar names it.
pub(crate) fn push_tag_remote(state: &AppState) -> Result<RemoteName, &'static str> {
    let configured: Vec<&RemoteName> = state
        .branches
        .remotes
        .remotes
        .iter()
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

/// Why the selected branch cannot be merged or rebased onto now: not into
/// itself, and not while another operation waits to be finished.
pub(crate) fn merge_disabled(state: &AppState) -> Option<Reason> {
    branch_action_disabled(state).or_else(|| {
        if state.branches.selected_branch().is_some_and(|b| b.head) {
            Some("Already on this branch".into())
        } else if resumable_operation(state).is_some() {
            Some("Finish or abort the operation in progress".into())
        } else {
            None
        }
    })
}

/// Why continue or abort cannot run: one at a time, and only with a
/// merge, rebase, cherry-pick or revert to act on.
pub(crate) fn resume_disabled(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
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

/// Why no action can run on the selected commit now: git runs one
/// operation at a time, and the log needs a commit.
pub(crate) fn commit_action_disabled(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
        Some(OP_RUNNING.into())
    } else {
        no_commit(state)
    }
}

pub(crate) fn checkout_commit_disabled(state: &AppState) -> Option<Reason> {
    commit_action_disabled(state)
}

/// Another operation in progress would mix with this one's commits.
fn in_progress_disabled(state: &AppState) -> Option<Reason> {
    resumable_operation(state)
        .is_some()
        .then_some("Finish or abort the operation in progress".into())
}

/// Why the selected commit cannot be reverted now. A merge commit needs a
/// mainline chosen, which Kika does not ask for.
pub(crate) fn revert_disabled(state: &AppState) -> Option<Reason> {
    commit_action_disabled(state)
        .or_else(|| in_progress_disabled(state))
        .or_else(|| {
            let row = state.commits.rows.get(state.commits.selected);
            row.is_some_and(|r| r.parents.len() > 1)
                .then_some("Cannot revert a merge commit".into())
        })
}

/// Why the selected commit cannot be copied. A merge commit needs a
/// mainline chosen to be picked, which Kika does not ask for. Copying
/// runs no git, so it works while an operation runs.
pub(crate) fn copy_disabled(state: &AppState) -> Option<Reason> {
    no_commit(state).or_else(|| {
        let row = state.commits.rows.get(state.commits.selected);
        row.is_some_and(|r| r.parents.len() > 1)
            .then_some("Cannot cherry-pick a merge commit".into())
    })
}

/// Why the copied commits cannot be pasted now. Untracked files may stay:
/// git refuses to overwrite one before it changes anything. Any tracked
/// change is refused, though git would pick around some, because it would
/// mix into a later conflict.
pub(crate) fn paste_disabled(state: &AppState) -> Option<Reason> {
    let unborn = state
        .branch
        .as_ref()
        .is_none_or(|b| matches!(b.head, Head::Unborn(_)));
    if state.op_seq.is_some() {
        Some(OP_RUNNING.into())
    } else if state.copied.is_empty() {
        Some("Nothing copied".into())
    } else if unborn {
        Some("No commit to cherry-pick onto".into())
    } else if let Some(reason) = in_progress_disabled(state) {
        Some(reason)
    } else if state.files.iter().any(|f| f.kind != EntryKind::Untracked) {
        Some("Commit or stash your changes first".into())
    } else {
        None
    }
}

/// Why the selected commit or todo row cannot be rewritten now. On a log
/// row: one operation at a time, none in progress, and a history the
/// planner can rewrite. On a todo row: one write at a time, and an edit
/// the list allows.
pub(crate) fn rewrite_disabled(state: &AppState, rewrite: Rewrite) -> Option<Reason> {
    // Each acts on one row: not over a range, nor on a row the filter hides.
    if let Some(reason) = list_guard(state, false) {
        return Some(reason);
    }
    if state.commits.todo_selected.is_some() {
        return todo_disabled(state, rewrite);
    }
    // A narrowed log skips commits: the row below need not be the parent,
    // nor the one a move swaps with, and a plan made from its rows would
    // leave commits out.
    if state.commits.query.is_active() {
        return Some(CLEAR_LOG_FILTER.into());
    }
    // The neighbour a move swaps with may be one the filter hides.
    if matches!(rewrite, Rewrite::Move(_)) && !state.commits.filter.is_empty() {
        return Some(FILTER_HIDES_NEIGHBOUR.into());
    }
    commit_action_disabled(state)
        .or_else(|| in_progress_disabled(state))
        .or_else(|| log_plan(state, rewrite).err())
}

/// A range spans log rows; the todo list above the log has none.
fn commits_range_disabled(state: &AppState) -> Option<Reason> {
    state
        .commits
        .todo_selected
        .is_some()
        .then_some("Select a commit in the log first".into())
}

/// Why a log commit does not move while the log is filtered: it swaps
/// with the commit next to it in the history, which may be hidden.
pub(crate) const FILTER_HIDES_NEIGHBOUR: &str =
    "Clear the filter to move commits: the one next to it may be hidden";

/// Why `break` cannot be used: the todo command is newer than Kika's
/// minimum git.
pub(crate) const NEEDS_BREAK: &str = "Needs Git 2.20 or newer";

/// Why a rewrite waits: the branches are still being read, so whether
/// one sits on its commits is not known yet.
pub(crate) const TIPS_UNKNOWN: &str = "Reading the branches; try again in a moment";

/// Why a rewrite on an older git cannot go ahead: reading the branches
/// failed, so whether one would be left behind is not known.
fn tips_failed(error: &str) -> String {
    let first = error.lines().next().unwrap_or_default();
    format!("Could not read the branches ({first}); press r to retry")
}

/// Why a todo edit waits for the last one to be written.
pub(crate) const TODO_SAVING: &str = "Saving the todo list";

/// The plan for `rewrite` on the selected log row, as it will run. With
/// git 2.38 or newer branches stacked on the rewritten commits move along
/// (`--update-refs`), so only a move that would change what one holds is
/// refused; with an older git any stacked branch would be left behind, so
/// the rewrite is refused.
pub(crate) fn log_plan(state: &AppState, rewrite: Rewrite) -> Result<RebasePlan, Reason> {
    if rewrite == Rewrite::Break && !state.git_has(version::BREAK) {
        return Err(NEEDS_BREAK.into());
    }
    let commits = &state.commits;
    let (rows, selected) = (commits.rows.as_slice(), commits.selected);
    let plan = match rewrite {
        Rewrite::Edit(edit) => rebase_plan::plan(rows, selected, edit),
        Rewrite::Move(direction) => rebase_plan::plan_move(rows, selected, direction),
        Rewrite::Break => rebase_plan::plan_break(rows, selected),
    }?;
    // A branch on the older of the two would end up holding the other.
    let crossings: &[Crossing] = match rewrite {
        Rewrite::Move(direction) => &[Crossing {
            moved: selected,
            from: match direction {
                Direction::Up => selected,
                Direction::Down => selected + 1,
            },
            len: 1,
        }],
        _ => &[],
    };
    with_stacked_branches(state, plan, crossings)
}

/// A log commit a plan moves, `moved`, and the rows it moves past:
/// `len` of them from `from`. A branch on one of those would change what
/// it holds.
struct Crossing {
    moved: usize,
    from: usize,
    len: usize,
}

/// `plan` as it will run, checked against the branches stacked on its
/// commits. With git 2.38 or newer they move along (`--update-refs`), so
/// only a `crossings` move that would change what one holds is refused;
/// with an older git any stacked branch would be left behind, so the plan
/// is refused.
fn with_stacked_branches(
    state: &AppState,
    mut plan: RebasePlan,
    crossings: &[Crossing],
) -> Result<RebasePlan, Reason> {
    let commits = &state.commits;
    let rows = commits.rows.as_slice();
    plan.update_refs = state.git_has(version::UPDATE_REFS);
    let head = match state.branch.as_ref().map(|b| &b.head) {
        Some(Head::Branch(name)) => Some(format!("refs/heads/{name}").into_bytes()),
        _ => None,
    };
    let stacked =
        |tips, from: usize, len| branch_tips::stacked(&rows[from..], len, tips, head.as_deref());
    if !plan.update_refs {
        let tips = match (&commits.tips, &commits.tips_error) {
            (Some(tips), _) => tips,
            (None, Some(error)) => return Err(tips_failed(error).into()),
            (None, None) => return Err(TIPS_UNKNOWN.into()),
        };
        if let Some(name) = stacked(tips, 0, plan.steps.len()).first() {
            return Err(branch_tips::left_behind(name).into());
        }
    } else if let Some(tips) = &commits.tips {
        // Unknown tips are left to the sequence editor, which checks too.
        for crossing in crossings {
            if let Some(name) = stacked(tips, crossing.from, crossing.len).first() {
                let oid = &rows[crossing.moved].oid;
                return Err(rebase_plan::crossing(oid, name).into());
            }
        }
    }
    Ok(plan)
}

/// The plan that folds every `fixup!` commit in the log into its target,
/// as it will run. Each fixup moves down past the commits between it and
/// its target, so with `--update-refs` a branch on the fixup or on one of
/// those is refused.
pub(crate) fn autosquash_plan(state: &AppState) -> Result<RebasePlan, Reason> {
    let rows = state.commits.rows.as_slice();
    let pairs = rebase_plan::fixup_targets(rows)?;
    let plan = rebase_plan::plan_autosquash(rows)?;
    let crossings: Vec<Crossing> = pairs
        .into_iter()
        .map(|(fixup, target)| Crossing {
            moved: fixup,
            from: fixup,
            len: target - fixup,
        })
        .collect();
    with_stacked_branches(state, plan, &crossings)
}

/// Why the fixups cannot be folded now. Like any rewrite of the log: one
/// operation at a time, none in progress, and the whole log in view, as
/// the plan is made from its rows.
pub(crate) fn autosquash_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false)
        .or_else(|| commits_range_disabled(state))
        .or_else(|| {
            state
                .commits
                .query
                .is_active()
                .then_some(CLEAR_LOG_FILTER.into())
        })
        .or_else(|| (!state.commits.filter.is_empty()).then_some(FILTER_HIDES_NEIGHBOUR.into()))
        .or_else(|| commit_action_disabled(state))
        .or_else(|| in_progress_disabled(state))
        .or_else(|| autosquash_plan(state).err())
}

/// Why the custom patch's rewrite `choice` cannot run now, the first of:
/// nothing picked, an operation running or in progress, a tracked change
/// (it would mix into the rewrite), a narrowed log (the plan is made from
/// its rows), a source not loaded or not rewritable, and the plan itself,
/// which for a move needs the cursor on another log commit.
pub(crate) fn patch_rewrite_disabled(state: &AppState, choice: PatchChoice) -> Option<Reason> {
    patch_state_disabled(state).or_else(|| match patch_action(state, choice) {
        Ok(action) => patch_plan(state, &action).err(),
        Err(reason) => Some(reason),
    })
}

/// [`patch_rewrite_disabled`] for `action` as it was asked about, its
/// target fixed then.
pub(crate) fn patch_action_disabled(state: &AppState, action: &PatchAction) -> Option<Reason> {
    patch_state_disabled(state).or_else(|| patch_plan(state, action).err())
}

/// Why the patch is empty.
pub(crate) const PATCH_EMPTY: &str = "The custom patch is empty";

fn patch_state_disabled(state: &AppState) -> Option<Reason> {
    let Some(patch) = state.custom_patch.as_ref().filter(|p| !p.is_empty()) else {
        return Some(PATCH_EMPTY.into());
    };
    if state.op_seq.is_some() {
        return Some(OP_RUNNING.into());
    }
    if let Some(reason) = in_progress_disabled(state) {
        return Some(reason);
    }
    if state.files.iter().any(|f| f.kind != EntryKind::Untracked) {
        return Some("Commit or stash your changes first".into());
    }
    if state.commits.query.is_active() || !state.commits.filter.is_empty() {
        return Some(CLEAR_LOG_FILTER.into());
    }
    let rows = &state.commits.rows;
    let source = patch.source.oid.as_str();
    match rows.iter().find(|row| row.oid == source) {
        None => Some("The patch's commit is not loaded; move down to load more".into()),
        Some(row) if row.parents.is_empty() => Some("Cannot rewrite the first commit".into()),
        Some(row) if row.parents.len() > 1 => Some("Cannot rewrite a merge commit".into()),
        Some(_) => None,
    }
}

/// The action `choice` names now: a move goes to the commit under the
/// cursor; a new commit's message is asked for later.
pub(crate) fn patch_action(state: &AppState, choice: PatchChoice) -> Result<PatchAction, Reason> {
    Ok(match choice {
        PatchChoice::Remove => PatchAction::Remove,
        PatchChoice::ToIndex => PatchAction::ToIndex,
        PatchChoice::NewCommit => PatchAction::NewCommit(String::new()),
        PatchChoice::MoveToSelected => {
            if state.commits.todo_selected.is_some() {
                return Err("Select a commit in the log first".into());
            }
            let row = state.commits.rows.get(state.commits.selected);
            let target = row.and_then(|row| CommitId::parse(&row.oid));
            PatchAction::MoveTo(target.ok_or("No commits")?)
        }
    })
}

/// Why a move cannot go to the commit the patch comes from.
pub(crate) const TARGET_IS_SOURCE: &str =
    "The patch comes from this commit; select the commit to move it to";

/// The rebase plan for `action`, as it will run: a stop at the source,
/// and at a move's target, every other commit picked. A move to an older
/// commit stops at the target only: when git picks the source later, the
/// change is there already and drops out of it. The index gets the patch
/// checked at the last commit too, while the rebase can still be aborted.
pub(crate) fn patch_plan(state: &AppState, action: &PatchAction) -> Result<RebasePlan, Reason> {
    let patch = state.custom_patch.as_ref().ok_or(PATCH_EMPTY)?;
    let rows = state.commits.rows.as_slice();
    let row_of = |id: &CommitId| rows.iter().position(|row| row.oid == id.as_str());
    let source = row_of(&patch.source.oid).ok_or("The patch's commit is not loaded")?;
    let stops = match action {
        PatchAction::Remove | PatchAction::NewCommit(_) => vec![source],
        PatchAction::ToIndex => vec![source, 0],
        PatchAction::MoveTo(target) => {
            let target = row_of(target).ok_or("The target commit is not loaded")?;
            if target == source {
                return Err(TARGET_IS_SOURCE.into());
            }
            if target > source {
                vec![target]
            } else {
                vec![source, target]
            }
        }
    };
    let plan = rebase_plan::plan_stops(rows, &stops)?;
    with_stacked_branches(state, plan, &[])
}

/// Why the staged change cannot become a `fixup!` of the selected commit
/// now. The target is named by its id, so a narrowed log is fine.
pub(crate) fn create_fixup_disabled(state: &AppState) -> Option<Reason> {
    fixup_target_disabled(state)
        .or_else(|| in_progress_disabled(state))
        .or_else(|| nothing_staged(state))
}

/// Why the commit the staged lines come from cannot be looked for now.
/// It only reads, so an operation in progress does not stop it.
pub(crate) fn find_base_disabled(state: &AppState) -> Option<Reason> {
    fixup_target_disabled(state).or_else(|| nothing_staged(state))
}

/// One log commit selected, shown, and free to act on.
fn fixup_target_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false)
        .or_else(|| commits_range_disabled(state))
        .or_else(|| commit_action_disabled(state))
}

fn nothing_staged(state: &AppState) -> Option<Reason> {
    let staged = state.files.iter().any(|f| !matches!(f.index, '.' | '?'));
    (!staged).then_some(STAGE_THE_FIX.into())
}

/// Why a fixup cannot be made or looked for: nothing is staged.
pub(crate) const STAGE_THE_FIX: &str = "Stage the fix first";

/// The todo list after `rewrite` on the selected todo row, and where the
/// cursor goes, or why it cannot be made.
pub(crate) fn todo_edit(
    state: &AppState,
    rewrite: Rewrite,
) -> Result<(Vec<TodoEntry>, usize), Reason> {
    let commits = &state.commits;
    let (Some(todo), Some(i)) = (&commits.todo, commits.todo_selected) else {
        return Err("No todo row selected".into());
    };
    todo.edit(i, rewrite).map_err(Reason::from)
}

fn todo_disabled(state: &AppState, rewrite: Rewrite) -> Option<Reason> {
    if state.op_seq.is_some() {
        return Some(OP_RUNNING.into());
    }
    // The list shown is what was written only once it is read back.
    if state.commits.todo_write_seq.is_some() || state.commits.todo_seq.is_some() {
        return Some(TODO_SAVING.into());
    }
    if rewrite == Rewrite::Break && !state.git_has(version::BREAK) {
        return Some(NEEDS_BREAK.into());
    }
    todo_edit(state, rewrite).err()
}

fn squash_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Edit(Edit::Squash))
}

fn fixup_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Edit(Edit::Fixup))
}

fn drop_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Edit(Edit::Drop))
}

fn reword_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Edit(Edit::Reword))
}

fn stop_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Edit(Edit::Stop))
}

fn todo_pick_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Edit(Edit::Pick))
}

fn break_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Break)
}

fn move_up_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Move(Direction::Up))
}

fn move_down_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Move(Direction::Down))
}

pub(crate) fn reset_disabled(state: &AppState) -> Option<Reason> {
    commit_action_disabled(state).or_else(|| in_progress_disabled(state))
}

/// Why undo and redo cannot start now: git one operation at a time, a
/// repository to act on, and no operation half done, a bisect included,
/// whose own state a moved `HEAD` would confuse.
pub(crate) fn undo_disabled(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
        Some(OP_RUNNING.into())
    } else if state.repo.is_none() {
        Some("Not in a repository".into())
    } else if state
        .branch
        .as_ref()
        .is_some_and(|b| !b.in_progress.is_empty())
    {
        Some("Finish or abort the operation in progress".into())
    } else {
        None
    }
}

/// The delete question on top, if one is.
fn delete_question(state: &AppState) -> Option<&DeleteBranchConfirm> {
    match state.popups.last() {
        Some(Popup::DeleteBranch(confirm)) => Some(confirm),
        _ => None,
    }
}

/// `y` deletes only a merged branch; an unmerged one needs `D`.
fn plain_delete_disabled(state: &AppState) -> Option<Reason> {
    delete_question(state)
        .is_some_and(DeleteBranchConfirm::force)
        .then_some("Not fully merged: press D to force delete".into())
}

/// `D` answers only the stronger question, about an unmerged branch.
fn force_delete_disabled(state: &AppState) -> Option<Reason> {
    delete_question(state)
        .is_some_and(|c| !c.force())
        .then_some("Fully merged: press y to delete".into())
}

/// Why no fetch, pull or push may start: one needs a repository, and git
/// one operation at a time.
pub(crate) fn remote_disabled(state: &AppState) -> Option<Reason> {
    if state.repo.is_none() {
        Some("Not in a repository".into())
    } else if state.op_seq.is_some() || state.remote.is_some() {
        Some(REMOTE_BUSY.into())
    } else {
        None
    }
}

pub(crate) const REMOTE_BUSY: &str = "Another operation is running";

/// A fetch also waits for auto-fetch, which would race it for the same refs.
pub(crate) fn fetch_disabled(state: &AppState) -> Option<Reason> {
    remote_disabled(state).or_else(|| {
        state
            .watch
            .fetch_pending
            .then_some("A background fetch is running".into())
    })
}

/// Pull and push work on the current branch, so they need one.
pub(crate) fn pull_push_disabled(state: &AppState) -> Option<Reason> {
    remote_disabled(state).or_else(|| {
        let detached = state
            .branch
            .as_ref()
            .is_some_and(|b| matches!(b.head, Head::Detached(_)));
        detached.then_some("HEAD is detached".into())
    })
}

/// Why an action on the selected row cannot run: the filter hides it.
pub(crate) const FILTER_HIDES_ROW: &str = "No row matches the filter";

/// Why an action on one row cannot run while a range is selected.
pub(crate) const CLEAR_RANGE_FIRST: &str = "Clear the selection first (esc)";

/// Why a Files or Commits action on the selected row cannot run now: the
/// filter hides that row, or a range is selected and the action is not one
/// that applies to a range (`range_ok`). Chained in front of the action's
/// own reason, so these come first.
pub(crate) fn list_guard(state: &AppState, range_ok: bool) -> Option<Reason> {
    let (hidden, range) = match state.panel_focus() {
        KeyScope::Files => (state.files_selected_hidden(), state.files_anchor.is_some()),
        // The filter hides log rows only; a todo row above the log shows.
        KeyScope::Commits => (
            state.commits.todo_selected.is_none() && state.commits.selected_hidden(),
            state.commits.range_anchor.is_some(),
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

/// Space stages a range too, so long as one of its files can be staged.
pub(crate) fn files_stage_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, true).or_else(|| {
        // As the action: the range counts only where it shows.
        if state.files_anchor.is_none() || state.focus != Focus::Files {
            return stage_disabled(state);
        }
        let range = state.files_range();
        if state.op_seq.is_some() {
            Some(OP_RUNNING.into())
        } else if range
            .iter()
            .all(|&i| state.files[i].kind == EntryKind::Conflicted)
        {
            Some("Resolve the conflicts before staging these files".into())
        } else {
            None
        }
    })
}

fn files_discard_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, true).or_else(|| discard_disabled(state))
}

fn files_open_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| open_disabled(state))
}

fn files_blame_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| blame_disabled(state))
}

fn files_hunks_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| no_file_selected(state))
}

fn files_mergetool_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| mergetool_disabled(state))
}

fn commits_open_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| no_commit(state))
}

fn commits_checkout_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| checkout_commit_disabled(state))
}

fn commits_revert_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| revert_disabled(state))
}

fn commits_reset_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| reset_disabled(state))
}

fn commits_squash_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| squash_disabled(state))
}

fn commits_fixup_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| fixup_disabled(state))
}

fn commits_drop_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| drop_disabled(state))
}

fn commits_reword_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| reword_disabled(state))
}

/// The copy menu copies the selected commit's text: one row's action.
fn commits_copy_menu_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false)
}

fn files_ignore_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| ignore_disabled(state))
}

/// The bisect menu marks the selected commit, so it is one row's action.
fn commits_bisect_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false)
}

fn commits_tag_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| tag_commit_disabled(state))
}

/// `c` copies a range too, but a range of merge commits only has nothing
/// it can copy.
pub(crate) fn commits_copy_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, true).or_else(|| {
        if state.commits.range_anchor.is_none() {
            return copy_disabled(state);
        }
        let rows = &state.commits.rows;
        let range = state.commits.range();
        no_commit(state).or_else(|| {
            range
                .iter()
                .all(|&i| rows[i].parents.len() > 1)
                .then_some("Cannot cherry-pick a merge commit".into())
        })
    })
}

/// Why a rewrite cannot run on a log narrowed to a path or an author.
pub(crate) const CLEAR_LOG_FILTER: &str = "Clear the log filter first";

/// A new path or author reloads the log, which a range was made over.
fn log_filter_disabled(state: &AppState) -> Option<Reason> {
    state
        .commits
        .range_anchor
        .is_some()
        .then_some(CLEAR_RANGE_FIRST.into())
}

/// The reflog is not a log git can narrow.
fn reflog_filter_disabled(_: &AppState) -> Option<Reason> {
    Some("Filters work on the log tab".into())
}

/// A mark is a log commit, not a row of the todo list above the log.
fn commits_mark_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false)
        .or_else(|| commits_range_disabled(state))
        .or_else(|| no_commit_selected(state))
}

fn files_log_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| no_file_selected(state))
}

fn no_branch_selected(state: &AppState) -> Option<Reason> {
    let selected = state.branches.selected_branch();
    selected.is_none().then_some("No branch selected".into())
}

/// A comparison needs a mark, and another branch or commit than it.
pub(crate) fn branches_diff_disabled(state: &AppState) -> Option<Reason> {
    no_branch_selected(state).or_else(|| match &state.diff_mark {
        None => Some("Mark a commit or branch first (M)".into()),
        Some(DiffMark::Branch(name))
            if state
                .branches
                .selected_branch()
                .is_some_and(|b| b.name == *name) =>
        {
            Some("Pick another branch or commit to diff against".into())
        }
        Some(_) => None,
    })
}

/// The search works on the built-in diff's hunks, outside line mode.
fn search_disabled(state: &AppState) -> Option<Reason> {
    match &state.focus {
        Focus::Diff(view) if view.lines.is_some() => Some("Leave line mode first".into()),
        _ if state.rendered_shown().is_some() => Some("Search works in the built-in diff".into()),
        _ => None,
    }
}

fn search_step_disabled(state: &AppState) -> Option<Reason> {
    search_disabled(state).or_else(|| {
        let searched = matches!(&state.focus, Focus::Diff(view) if view.search.is_some());
        (!searched).then_some("Nothing searched yet (/)".into())
    })
}

const NONE: KeyModifiers = KeyModifiers::NONE;

static REGISTRY: &[Binding] = &[
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
    Binding {
        id: "workspace.fetch",
        keys: &[KeyPattern::exact(KeyCode::Char('f'), NONE)],
        scope: KeyScope::Workspace,
        intent: AppIntent::Fetch,
        description: "Fetch every remote",
        label: "fetch",
        bar: false,
        disabled: fetch_disabled,
    },
    Binding {
        id: "workspace.pull",
        keys: &[KeyPattern::exact(KeyCode::Char('p'), NONE)],
        scope: KeyScope::Workspace,
        intent: AppIntent::Pull,
        description: "Pull into the current branch",
        label: "pull",
        bar: false,
        disabled: pull_push_disabled,
    },
    Binding {
        id: "workspace.push",
        keys: &[KeyPattern::any(KeyCode::Char('P'))],
        scope: KeyScope::Workspace,
        intent: AppIntent::Push,
        description: "Push the current branch",
        label: "push",
        bar: false,
        disabled: pull_push_disabled,
    },
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
    Binding {
        id: "workspace.parent",
        keys: &[KeyPattern::any(KeyCode::Backspace)],
        scope: KeyScope::Workspace,
        intent: AppIntent::LeaveSubmodule,
        description: "Back to the parent repository",
        label: "parent",
        bar: true,
        disabled: leave_submodule_disabled,
    },
    // Diff settings, in Workspace so they work from Files, Diff and Lines.
    // Out of the bar, which is full at 80 columns; the help menu lists them.
    Binding {
        id: "workspace.more_context",
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
        id: "workspace.less_context",
        keys: &[KeyPattern::exact(KeyCode::Char('-'), NONE)],
        scope: KeyScope::Workspace,
        intent: AppIntent::LessContext,
        description: "Show fewer context lines in the diff",
        label: "less context",
        bar: false,
        disabled: least_context,
    },
    Binding {
        id: "workspace.whitespace",
        keys: &[KeyPattern::exact(KeyCode::Char('w'), NONE)],
        scope: KeyScope::Workspace,
        intent: AppIntent::ToggleWhitespace,
        description: "Hide or show whitespace-only changes",
        label: "whitespace",
        bar: false,
        disabled: diff_hidden,
    },
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
    // Out of the bar, which is full at 80 columns; the help menu lists it.
    Binding {
        id: "workspace.patch_menu",
        keys: &[KeyPattern::exact(KeyCode::Char('p'), KeyModifiers::CONTROL)],
        scope: KeyScope::Workspace,
        intent: AppIntent::OpenPatchMenu,
        description: "Show what can be done with the custom patch: remove, move or split it off",
        label: "patch",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "workspace.cancel_command",
        keys: &[KeyPattern::exact(KeyCode::Char('x'), KeyModifiers::CONTROL)],
        // Not `esc` or `ctrl-c`, which quit; the bar shows it only while a
        // command runs.
        scope: KeyScope::Workspace,
        intent: AppIntent::CancelCommand,
        description: "Stop the running command",
        label: "cancel command",
        bar: false,
        disabled: no_command_running,
    },
    // Raw mode turns off the terminal's own meaning of Ctrl+Z (suspend)
    // and Ctrl+R (reprint), so both reach Kika as keys on every platform.
    // Redo is not Ctrl+Y: the custom-command example in the docs, and so
    // perhaps the user's own config, binds it in the blame view, where a
    // locked redo would make that config fail to load.
    Binding {
        id: "workspace.undo",
        keys: &[KeyPattern::exact(KeyCode::Char('z'), KeyModifiers::CONTROL)],
        scope: KeyScope::Workspace,
        intent: AppIntent::Undo,
        description: "Undo the last commit, checkout, reset or rebase, saying first what moves",
        label: "undo",
        bar: false,
        disabled: undo_disabled,
    },
    Binding {
        id: "workspace.redo",
        keys: &[KeyPattern::exact(KeyCode::Char('r'), KeyModifiers::CONTROL)],
        scope: KeyScope::Workspace,
        intent: AppIntent::Redo,
        description: "Redo what the last undo reversed, saying first what moves",
        label: "redo",
        bar: false,
        disabled: undo_disabled,
    },
    Binding {
        id: "files.next",
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
        id: "files.prev",
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
        id: "commits.next",
        // Not any modifiers on the arrow: Alt+Down moves the commit.
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::exact(KeyCode::Down, NONE),
        ],
        scope: KeyScope::Commits,
        intent: AppIntent::CommitNext,
        description: "Select the next commit",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "commits.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::exact(KeyCode::Up, NONE),
        ],
        scope: KeyScope::Commits,
        intent: AppIntent::CommitPrev,
        description: "Select the previous commit",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "commits.open",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Commits,
        intent: AppIntent::OpenCommitFiles,
        description: "Show the files the selected commit changed, or those changed from the diff mark to it",
        label: "files",
        bar: true,
        disabled: commits_open_disabled,
    },
    Binding {
        id: "commits.checkout",
        keys: &[KeyPattern::exact(KeyCode::Char(' '), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::CheckoutCommit,
        description: "Check out the selected commit as a detached HEAD, asking first",
        label: "checkout",
        bar: true,
        disabled: commits_checkout_disabled,
    },
    Binding {
        id: "commits.revert",
        keys: &[KeyPattern::any(KeyCode::Char('R'))],
        scope: KeyScope::Commits,
        intent: AppIntent::RevertCommit,
        description: "Make a commit that undoes the selected one",
        label: "revert",
        bar: true,
        disabled: commits_revert_disabled,
    },
    Binding {
        id: "commits.reset",
        keys: &[KeyPattern::any(KeyCode::Char('X'))],
        scope: KeyScope::Commits,
        intent: AppIntent::OpenMenu(MenuId::Reset),
        description: "Reset the current branch to the selected commit: soft, mixed or hard",
        label: "reset",
        bar: true,
        disabled: commits_reset_disabled,
    },
    Binding {
        id: "commits.squash",
        keys: &[KeyPattern::exact(KeyCode::Char('s'), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::RewriteCommit(Edit::Squash),
        description: "Squash the selected commit into the one below, combining their messages",
        label: "squash",
        bar: true,
        disabled: commits_squash_disabled,
    },
    // `f` is fetch.
    Binding {
        id: "commits.fixup",
        keys: &[KeyPattern::any(KeyCode::Char('F'))],
        scope: KeyScope::Commits,
        intent: AppIntent::RewriteCommit(Edit::Fixup),
        description: "Fold the selected commit into the one below, keeping that one's message",
        label: "fixup",
        bar: true,
        disabled: commits_fixup_disabled,
    },
    Binding {
        id: "commits.create_fixup",
        keys: &[KeyPattern::exact(KeyCode::Char('f'), KeyModifiers::CONTROL)],
        scope: KeyScope::Commits,
        intent: AppIntent::CreateFixup,
        description: "Commit the staged change as a fixup! of the selected commit",
        label: "create fixup",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: create_fixup_disabled,
    },
    Binding {
        id: "commits.autosquash",
        keys: &[KeyPattern::any(KeyCode::Char('S'))],
        scope: KeyScope::Commits,
        intent: AppIntent::Autosquash,
        description: "Fold every fixup! commit into the commit it fixes, after asking",
        label: "autosquash",
        bar: false,
        disabled: autosquash_disabled,
    },
    Binding {
        id: "commits.find_base",
        keys: &[KeyPattern::exact(KeyCode::Char('b'), KeyModifiers::CONTROL)],
        scope: KeyScope::Commits,
        intent: AppIntent::FindFixupBase,
        description: "Select the commit that last touched the staged lines",
        label: "find fixup base",
        bar: false,
        disabled: find_base_disabled,
    },
    Binding {
        id: "commits.drop",
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::RewriteCommit(Edit::Drop),
        description: "Drop the selected commit and its change from the history",
        label: "drop",
        bar: true,
        disabled: commits_drop_disabled,
    },
    // `r` is refresh.
    Binding {
        id: "commits.reword",
        keys: &[KeyPattern::exact(KeyCode::Char('e'), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::RewriteCommit(Edit::Reword),
        description: "Write a new message for the selected commit",
        label: "reword",
        bar: true,
        disabled: commits_reword_disabled,
    },
    // `J` and `K` scroll the diff from the Workspace.
    Binding {
        id: "commits.move_up",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), KeyModifiers::ALT),
            KeyPattern::exact(KeyCode::Up, KeyModifiers::ALT),
        ],
        scope: KeyScope::Commits,
        intent: AppIntent::MoveCommit(Direction::Up),
        description: "Move the selected commit or todo row up, above the newer one",
        label: "move up",
        bar: true,
        disabled: move_up_disabled,
    },
    Binding {
        id: "commits.move_down",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), KeyModifiers::ALT),
            KeyPattern::exact(KeyCode::Down, KeyModifiers::ALT),
        ],
        scope: KeyScope::Commits,
        intent: AppIntent::MoveCommit(Direction::Down),
        description: "Move the selected commit or todo row down, below the older one",
        label: "move down",
        bar: true,
        disabled: move_down_disabled,
    },
    Binding {
        id: "commits.edit",
        keys: &[KeyPattern::any(KeyCode::Char('E'))],
        scope: KeyScope::Commits,
        intent: AppIntent::RewriteCommit(Edit::Stop),
        description: "Stop the rebase at the selected commit, to amend it, then continue from Status",
        label: "edit",
        bar: true,
        disabled: stop_disabled,
    },
    Binding {
        id: "commits.break",
        keys: &[KeyPattern::exact(KeyCode::Char('b'), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::InsertBreak,
        description: "Stop the rebase right after the selected commit, or toggle the stop after a todo row",
        label: "break",
        bar: true,
        disabled: break_disabled,
    },
    // `p` is pull; a pick is git's "use commit".
    Binding {
        id: "commits.pick",
        keys: &[KeyPattern::exact(KeyCode::Char('u'), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::RewriteCommit(Edit::Pick),
        description: "Pick the selected todo row as it is, undoing its other action",
        label: "pick",
        bar: true,
        disabled: todo_pick_disabled,
    },
    Binding {
        id: "commits.switch_tab",
        keys: &[
            KeyPattern::exact(KeyCode::Char('['), NONE),
            KeyPattern::exact(KeyCode::Char(']'), NONE),
        ],
        scope: KeyScope::Commits,
        intent: AppIntent::SwitchCommitsTab,
        description: "Show HEAD's reflog: everywhere HEAD has been",
        label: "reflog",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "commits.copy",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::CopyCommit,
        description: "Add the selected commit to the copied set, or take it out",
        label: "copy",
        bar: true,
        disabled: commits_copy_disabled,
    },
    Binding {
        id: "commits.paste",
        keys: &[KeyPattern::exact(KeyCode::Char('v'), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::PasteCommits,
        description: "Cherry-pick the copied commits onto the current branch, oldest first",
        label: "paste",
        bar: true,
        disabled: paste_disabled,
    },
    Binding {
        id: "commits.clear_copied",
        keys: &[KeyPattern::any(KeyCode::Char('C'))],
        scope: KeyScope::Commits,
        intent: AppIntent::ClearCopied,
        description: "Clear the copied set",
        label: "clear",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "commits.copy_menu",
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::OpenMenu(MenuId::Copy),
        description: "Copy hash or message (the terminal must allow OSC 52; in tmux, set-clipboard on)",
        label: "clipboard",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: commits_copy_menu_disabled,
    },
    Binding {
        id: "reflog.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Reflog,
        intent: AppIntent::ReflogNext,
        description: "Select the next reflog entry",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "reflog.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Reflog,
        intent: AppIntent::ReflogPrev,
        description: "Select the previous reflog entry",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "reflog.switch_tab",
        keys: &[
            KeyPattern::exact(KeyCode::Char('['), NONE),
            KeyPattern::exact(KeyCode::Char(']'), NONE),
        ],
        scope: KeyScope::Reflog,
        intent: AppIntent::SwitchCommitsTab,
        description: "Show the log",
        label: "log",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "commit_files.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::CommitFiles,
        intent: AppIntent::CommitFileNext,
        description: "Select the next file and show its change",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "commit_files.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::CommitFiles,
        intent: AppIntent::CommitFilePrev,
        description: "Select the previous file and show its change",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "commit_files.patch",
        keys: &[KeyPattern::exact(KeyCode::Char(' '), NONE)],
        scope: KeyScope::CommitFiles,
        intent: AppIntent::PatchToggleFile,
        description: "Pick the selected file's whole change for the custom patch, or drop it",
        label: "patch",
        bar: true,
        disabled: patch_file_disabled,
    },
    Binding {
        id: "commit_files.back",
        keys: &[
            KeyPattern::any(KeyCode::Esc),
            KeyPattern::exact(KeyCode::Char('h'), NONE),
            KeyPattern::any(KeyCode::Left),
        ],
        scope: KeyScope::CommitFiles,
        intent: AppIntent::CloseCommitFiles,
        description: "Back to the log, at the same commit",
        label: "back",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "stash.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Stash,
        intent: AppIntent::StashNext,
        description: "Select the next stash",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "stash.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Stash,
        intent: AppIntent::StashPrev,
        description: "Select the previous stash",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "stash.files",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Stash,
        intent: AppIntent::OpenStashFiles,
        description: "Show the files the selected stash holds",
        label: "files",
        bar: true,
        disabled: no_stash,
    },
    Binding {
        id: "stash.apply",
        keys: &[KeyPattern::exact(KeyCode::Char(' '), NONE)],
        scope: KeyScope::Stash,
        intent: AppIntent::ApplyStash,
        description: "Apply the selected stash to the worktree and keep it",
        label: "apply",
        bar: true,
        disabled: stash_action_disabled,
    },
    Binding {
        id: "stash.pop",
        keys: &[KeyPattern::exact(KeyCode::Char('g'), NONE)],
        scope: KeyScope::Stash,
        intent: AppIntent::PopStash,
        description: "Apply the selected stash and drop it; a conflict keeps it",
        label: "pop",
        bar: true,
        disabled: stash_action_disabled,
    },
    Binding {
        id: "stash.drop",
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::Stash,
        intent: AppIntent::DropStash,
        description: "Drop the selected stash, asking first",
        label: "drop",
        bar: true,
        disabled: stash_action_disabled,
    },
    Binding {
        id: "stash.rename",
        keys: &[KeyPattern::any(KeyCode::Char('R'))],
        scope: KeyScope::Stash,
        intent: AppIntent::RenameStash,
        description: "Give the selected stash a new message; it moves to stash@{0}",
        label: "rename",
        bar: true,
        disabled: stash_action_disabled,
    },
    Binding {
        id: "stash_files.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::StashFiles,
        intent: AppIntent::StashFileNext,
        description: "Select the next file and show its change",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "stash_files.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::StashFiles,
        intent: AppIntent::StashFilePrev,
        description: "Select the previous file and show its change",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "stash_files.close",
        keys: &[
            KeyPattern::any(KeyCode::Esc),
            KeyPattern::any(KeyCode::Backspace),
        ],
        scope: KeyScope::StashFiles,
        intent: AppIntent::CloseStashFiles,
        description: "Back to the stash list, at the same stash",
        label: "back",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "files.stage",
        keys: &[KeyPattern::exact(KeyCode::Char(' '), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::ToggleStage,
        description: "Stage the selected file, or unstage it if fully staged",
        label: "stage",
        bar: true,
        disabled: files_stage_disabled,
    },
    Binding {
        id: "files.stage_all",
        keys: &[KeyPattern::exact(KeyCode::Char('a'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::ToggleStageAll,
        description: "Stage everything, or unstage everything if all is staged",
        label: "all",
        bar: true,
        disabled: stage_all_disabled,
    },
    Binding {
        id: "files.commit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::OpenCommit,
        description: "Commit the staged changes with a message",
        label: "commit",
        bar: true,
        disabled: commit_disabled,
    },
    Binding {
        id: "files.amend",
        keys: &[KeyPattern::any(KeyCode::Char('A'))],
        scope: KeyScope::Files,
        intent: AppIntent::OpenAmend,
        description: "Amend the last commit with the staged changes, or reword it",
        label: "amend",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: amend_disabled,
    },
    Binding {
        id: "files.discard",
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::Discard,
        description: "Discard the selected file's unstaged changes, or delete it if untracked",
        label: "discard",
        bar: true,
        disabled: files_discard_disabled,
    },
    Binding {
        id: "files.open",
        keys: &[KeyPattern::exact(KeyCode::Char('o'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::OpenInEditor,
        description: "Open the selected file in your editor",
        label: "open in editor",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: files_open_disabled,
    },
    Binding {
        id: "files.submodules",
        keys: &[KeyPattern::any(KeyCode::Char('S'))],
        scope: KeyScope::Files,
        intent: AppIntent::OpenSubmodules,
        description: "Show submodules",
        label: "submodules",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "workspace.worktrees",
        keys: &[KeyPattern::any(KeyCode::Char('W'))],
        scope: KeyScope::Workspace,
        intent: AppIntent::OpenWorktrees,
        description: "Show worktrees, to switch to, add or remove one",
        label: "worktrees",
        bar: false,
        disabled: worktrees_disabled,
    },
    // `@` arrives as itself whatever it takes to type it: Shift on most
    // layouts, AltGr (reported as Ctrl+Alt on Windows) or Option on
    // others, so it matches with any modifiers.
    Binding {
        id: "workspace.command_log",
        keys: &[KeyPattern::any(KeyCode::Char('@'))],
        scope: KeyScope::Workspace,
        intent: AppIntent::OpenCommandLog,
        description: "Show the git commands Kika ran",
        label: "command log",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "workspace.recent",
        keys: &[KeyPattern::any(KeyCode::Char('O'))],
        // From the Diff pane too, so switching away can keep it focused.
        scope: KeyScope::Workspace,
        intent: AppIntent::OpenRecent,
        description: "Show recent repositories, to switch to one",
        label: "recent",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "files.blame",
        keys: &[KeyPattern::exact(KeyCode::Char('b'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::OpenBlame,
        description: "Blame the selected file",
        label: "blame",
        bar: false,
        disabled: files_blame_disabled,
    },
    Binding {
        id: "files.hunks",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Files,
        intent: AppIntent::FocusDiff,
        description: "Work on the hunks of the selected file, or resolve its conflicts",
        label: "hunks",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: files_hunks_disabled,
    },
    Binding {
        id: "files.mergetool",
        keys: &[KeyPattern::any(KeyCode::Char('M'))],
        scope: KeyScope::Files,
        intent: AppIntent::OpenMergeTool,
        description: "Resolve the selected conflicted file in your merge tool",
        label: "merge tool",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: files_mergetool_disabled,
    },
    Binding {
        id: "files.ignore",
        keys: &[KeyPattern::exact(KeyCode::Char('i'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::OpenIgnore,
        description: "Add to .gitignore",
        label: "ignore",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: files_ignore_disabled,
    },
    Binding {
        id: "diff.next",
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
        id: "diff.prev",
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
        id: "diff.stage",
        keys: &[KeyPattern::exact(KeyCode::Char(' '), NONE)],
        scope: KeyScope::Diff,
        intent: AppIntent::ToggleHunkStage,
        description: "Stage the selected hunk, or unstage it in the staged view; in a commit's change, pick it for the custom patch or drop it",
        label: "hunk",
        bar: true,
        disabled: hunk_disabled,
    },
    Binding {
        id: "diff.lines",
        keys: &[KeyPattern::exact(KeyCode::Char('v'), NONE)],
        scope: KeyScope::Diff,
        intent: AppIntent::EnterLineMode,
        description: "Pick changed lines inside the selected hunk",
        label: "lines",
        bar: true,
        disabled: hunk_disabled,
    },
    Binding {
        id: "diff.side",
        keys: &[KeyPattern::exact(KeyCode::Char('t'), NONE)],
        scope: KeyScope::Diff,
        intent: AppIntent::ToggleDiffSide,
        description: "Switch between the unstaged and the staged changes",
        label: "staged/unstaged",
        bar: true,
        disabled: side_disabled,
    },
    Binding {
        id: "diff.discard",
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::Diff,
        intent: AppIntent::Discard,
        description: "Discard the selected hunk from the working tree",
        label: "discard",
        bar: true,
        disabled: discard_disabled,
    },
    Binding {
        id: "diff.open",
        keys: &[KeyPattern::exact(KeyCode::Char('o'), NONE)],
        scope: KeyScope::Diff,
        intent: AppIntent::OpenInEditor,
        description: "Open the file in your editor at the selected hunk",
        label: "open in editor",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: open_disabled,
    },
    Binding {
        id: "diff.edit_hunk",
        keys: &[KeyPattern::exact(KeyCode::Char('e'), NONE)],
        scope: KeyScope::Diff,
        intent: AppIntent::EditHunk,
        description: "Edit the selected hunk in your editor, then stage it",
        label: "edit hunk",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: edit_hunk_disabled,
    },
    Binding {
        id: "diff.renderer",
        keys: &[KeyPattern::any(KeyCode::Char('R'))],
        scope: KeyScope::Diff,
        intent: AppIntent::CycleRenderer,
        description: "Cycle diff renderer",
        label: "renderer",
        bar: false,
        disabled: renderer_disabled,
    },
    Binding {
        id: "diff.back",
        // Esc is `diff.escape`, which clears a search first and stands in
        // for this one in the bar.
        keys: &[
            KeyPattern::exact(KeyCode::Char('h'), NONE),
            KeyPattern::any(KeyCode::Left),
        ],
        scope: KeyScope::Diff,
        intent: AppIntent::FocusLeft,
        description: "Back to the side panel last focused",
        label: "back",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "lines.next",
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
        id: "lines.prev",
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
        id: "lines.extend_down",
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
        id: "lines.extend_up",
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
        id: "lines.stage",
        keys: &[KeyPattern::exact(KeyCode::Char(' '), NONE)],
        scope: KeyScope::Lines,
        intent: AppIntent::ToggleLinesStage,
        description: "Stage the selected lines, or unstage them in the staged view; in a commit's change, pick them for the custom patch or drop them",
        label: "lines",
        bar: true,
        disabled: hunk_disabled,
    },
    Binding {
        id: "lines.back",
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
        id: "lines.discard",
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::Lines,
        intent: AppIntent::Discard,
        description: "Discard the selected lines from the working tree",
        label: "discard",
        bar: true,
        disabled: discard_disabled,
    },
    Binding {
        id: "lines.open",
        keys: &[KeyPattern::exact(KeyCode::Char('o'), NONE)],
        scope: KeyScope::Lines,
        intent: AppIntent::OpenInEditor,
        description: "Open the file in your editor at the selected line",
        label: "open in editor",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: open_disabled,
    },
    Binding {
        id: "blame.next",
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
        id: "blame.prev",
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
        id: "blame.page_down",
        keys: &[KeyPattern::any(KeyCode::PageDown)],
        scope: KeyScope::Blame,
        intent: AppIntent::BlamePageDown,
        description: "Move down a page",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "blame.page_up",
        keys: &[KeyPattern::any(KeyCode::PageUp)],
        scope: KeyScope::Blame,
        intent: AppIntent::BlamePageUp,
        description: "Move up a page",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "blame.commit",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Blame,
        intent: AppIntent::OpenBlameCommit,
        description: "Show the commit that last changed this line",
        label: "commit",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "blame.back",
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
        id: "conflict.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Conflict,
        intent: AppIntent::ConflictNext,
        description: "Move to the next conflict",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "conflict.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Conflict,
        intent: AppIntent::ConflictPrev,
        description: "Move to the previous conflict",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "conflict.ours",
        keys: &[KeyPattern::exact(KeyCode::Char('o'), NONE)],
        scope: KeyScope::Conflict,
        intent: AppIntent::PickConflict(Choice::Ours),
        description: "Keep our side of this conflict and write the file",
        label: "ours",
        bar: true,
        disabled: pick_disabled,
    },
    Binding {
        id: "conflict.theirs",
        keys: &[KeyPattern::exact(KeyCode::Char('t'), NONE)],
        scope: KeyScope::Conflict,
        intent: AppIntent::PickConflict(Choice::Theirs),
        description: "Keep their side of this conflict and write the file",
        label: "theirs",
        bar: true,
        disabled: pick_disabled,
    },
    Binding {
        id: "conflict.both",
        keys: &[KeyPattern::exact(KeyCode::Char('b'), NONE)],
        scope: KeyScope::Conflict,
        intent: AppIntent::PickConflict(Choice::Both),
        description: "Keep both sides of this conflict, ours first, and write the file",
        label: "both",
        bar: true,
        disabled: pick_disabled,
    },
    Binding {
        id: "conflict.mergetool",
        keys: &[KeyPattern::any(KeyCode::Char('M'))],
        scope: KeyScope::Conflict,
        intent: AppIntent::OpenMergeTool,
        description: "Resolve this file in your merge tool instead",
        label: "merge tool",
        bar: true,
        disabled: mergetool_disabled,
    },
    Binding {
        id: "conflict.back",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::Conflict,
        intent: AppIntent::FocusLeft,
        description: "Back to the file list; picks already written stay",
        label: "back",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "blame_commit.scroll_down",
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
        id: "blame_commit.scroll_up",
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
        id: "blame_commit.page_down",
        keys: &[KeyPattern::any(KeyCode::PageDown)],
        scope: KeyScope::BlameCommit,
        intent: AppIntent::ScrollCommit(Scroll::PageDown),
        description: "Scroll the commit down a page",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "blame_commit.page_up",
        keys: &[KeyPattern::any(KeyCode::PageUp)],
        scope: KeyScope::BlameCommit,
        intent: AppIntent::ScrollCommit(Scroll::PageUp),
        description: "Scroll the commit up a page",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "blame_commit.back",
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
        id: "files.menu",
        keys: &[KeyPattern::exact(KeyCode::Char('m'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::OpenMenu(MenuId::Actions),
        description: "Open the actions menu",
        label: "menu",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "files.stash",
        keys: &[KeyPattern::exact(KeyCode::Char('s'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::OpenMenu(MenuId::Stash),
        description: "Stash changes: all, with untracked files, staged only, or this file",
        label: "stash",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: stash_menu_disabled,
    },
    Binding {
        id: "commit_confirm.stage_all",
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
        id: "commit_confirm.cancel",
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
        id: "commit_editor.commit",
        keys: &[KeyPattern::exact(KeyCode::Char('s'), KeyModifiers::CONTROL)],
        scope: KeyScope::CommitEditor,
        intent: AppIntent::SubmitCommit,
        description: "Commit with this message",
        label: "commit",
        bar: true,
        disabled: commit_editor_disabled,
    },
    Binding {
        id: "commit_editor.switch_field",
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
        id: "commit_editor.skip_hooks",
        keys: &[KeyPattern::exact(KeyCode::Char('n'), KeyModifiers::CONTROL)],
        scope: KeyScope::CommitEditor,
        intent: AppIntent::ToggleNoVerify,
        description: "Skip the pre-commit and commit-msg hooks for this commit",
        label: "skip hooks",
        bar: false,
        disabled: commit_editor_disabled,
    },
    Binding {
        id: "commit_editor.edit_externally",
        keys: &[KeyPattern::exact(KeyCode::Char('e'), KeyModifiers::CONTROL)],
        scope: KeyScope::CommitEditor,
        intent: AppIntent::EditCommitExternally,
        description: "Write the message in your editor, then commit when it closes",
        label: "editor",
        bar: false,
        disabled: commit_editor_disabled,
    },
    Binding {
        id: "commit_editor.cancel",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::CommitEditor,
        intent: AppIntent::CancelCommit,
        description: "Close without committing",
        label: "cancel",
        bar: true,
        disabled: commit_editor_disabled,
    },
    Binding {
        id: "commit_editor.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::CommitEditor,
        intent: AppIntent::InterruptCommit,
        description: "Close, asking before a typed message is lost; quit Kika while a commit runs",
        label: "stop",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "commit_discard.discard",
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::CommitDiscard,
        intent: AppIntent::DiscardCommit,
        description: "Throw the typed message away",
        label: "discard",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "commit_discard.keep",
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
        id: "commit_error.back",
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
        id: "commit_loading.cancel",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::CommitLoading,
        intent: AppIntent::CancelCommit,
        description: "Do not amend",
        label: "cancel",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "commit_loading.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::CommitLoading,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
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
    Binding {
        id: "credential.answer",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Credential,
        intent: AppIntent::SubmitCredential,
        description: "Send the typed answer to git",
        label: "send",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "credential.cancel",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::Credential,
        intent: AppIntent::CredentialAnswered(None),
        description: "Cancel the question, which fails the operation",
        label: "cancel",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "credential.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::Credential,
        intent: AppIntent::Quit,
        description: "Cancel the question and quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
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
    // Only `y` throws work away. Enter is left unbound on purpose: it
    // answers yes elsewhere, and a discard cannot be undone.
    Binding {
        id: "discard_confirm.yes",
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::DiscardConfirm,
        intent: AppIntent::ConfirmYes,
        description: "Yes, throw it away",
        label: "discard",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "discard_confirm.no",
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
        id: "discard_confirm.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::DiscardConfirm,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "branches.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Branches,
        intent: AppIntent::BranchNext,
        description: "Select the next branch",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "branches.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Branches,
        intent: AppIntent::BranchPrev,
        description: "Select the previous branch",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "branches.checkout",
        keys: &[KeyPattern::exact(KeyCode::Char(' '), NONE)],
        scope: KeyScope::Branches,
        intent: AppIntent::CheckoutBranch,
        description: "Check out the selected branch",
        label: "checkout",
        bar: true,
        disabled: checkout_branch_disabled,
    },
    Binding {
        id: "branches.new",
        keys: &[KeyPattern::exact(KeyCode::Char('n'), NONE)],
        scope: KeyScope::Branches,
        intent: AppIntent::NewBranch,
        description: "Create a branch at the selected one and check it out",
        label: "new",
        bar: true,
        disabled: branch_action_disabled,
    },
    // Not `r`, which reloads everywhere in the workspace.
    Binding {
        id: "branches.rename",
        keys: &[KeyPattern::any(KeyCode::Char('R'))],
        scope: KeyScope::Branches,
        intent: AppIntent::RenameBranch,
        description: "Rename the selected branch",
        label: "rename",
        bar: true,
        disabled: branch_action_disabled,
    },
    Binding {
        id: "branches.delete",
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::Branches,
        intent: AppIntent::DeleteBranch,
        description: "Delete the selected branch, asking first",
        label: "delete",
        bar: true,
        disabled: delete_branch_disabled,
    },
    Binding {
        id: "branches.merge",
        keys: &[KeyPattern::exact(KeyCode::Char('m'), NONE)],
        scope: KeyScope::Branches,
        intent: AppIntent::OpenMenu(MenuId::Merge),
        description: "Merge the selected branch into the current one, or rebase onto it",
        label: "merge",
        bar: true,
        disabled: merge_disabled,
    },
    Binding {
        id: "branches.pull_request",
        keys: &[KeyPattern::exact(KeyCode::Char('o'), NONE)],
        scope: KeyScope::Branches,
        intent: AppIntent::OpenMenu(MenuId::PullRequest),
        description: "Open, copy or create the selected branch's pull request",
        label: "pull request",
        bar: false,
        disabled: no_branch_selected,
    },
    Binding {
        id: "branches.set_upstream",
        keys: &[KeyPattern::exact(KeyCode::Char('u'), NONE)],
        scope: KeyScope::Branches,
        intent: AppIntent::SetUpstream,
        description: "Set the remote branch the selected branch tracks",
        label: "upstream",
        bar: true,
        disabled: branch_action_disabled,
    },
    Binding {
        id: "branches.unset_upstream",
        keys: &[KeyPattern::any(KeyCode::Char('U'))],
        scope: KeyScope::Branches,
        intent: AppIntent::UnsetUpstream,
        description: "Stop the selected branch tracking its upstream",
        label: "unset upstream",
        bar: false,
        disabled: unset_upstream_disabled,
    },
    Binding {
        id: "branches.switch_tab",
        keys: &[KeyPattern::exact(KeyCode::Char(']'), NONE)],
        scope: KeyScope::Branches,
        intent: AppIntent::SwitchBranchesTab,
        description: "Show the remotes and their branches",
        label: "remotes",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "branches.prev_tab",
        keys: &[KeyPattern::exact(KeyCode::Char('['), NONE)],
        scope: KeyScope::Branches,
        intent: AppIntent::PrevBranchesTab,
        description: "Show the tags",
        label: "tags",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "remotes.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Remotes,
        intent: AppIntent::RemoteNext,
        description: "Select the next remote or remote branch",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "remotes.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Remotes,
        intent: AppIntent::RemotePrev,
        description: "Select the previous remote or remote branch",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "remotes.checkout",
        keys: &[KeyPattern::exact(KeyCode::Char(' '), NONE)],
        scope: KeyScope::Remotes,
        intent: AppIntent::TrackRemoteBranch,
        description: "Check out the selected remote branch as a new branch tracking it",
        label: "checkout",
        bar: true,
        disabled: track_disabled,
    },
    Binding {
        id: "remotes.add",
        keys: &[KeyPattern::exact(KeyCode::Char('a'), NONE)],
        scope: KeyScope::Remotes,
        intent: AppIntent::AddRemote,
        description: "Add a remote; fetch to see its branches",
        label: "add",
        bar: true,
        disabled: add_remote_disabled,
    },
    // Not `r`, which reloads everywhere in the workspace.
    Binding {
        id: "remotes.rename",
        keys: &[KeyPattern::any(KeyCode::Char('R'))],
        scope: KeyScope::Remotes,
        intent: AppIntent::RenameRemote,
        description: "Rename the selected remote",
        label: "rename",
        bar: true,
        disabled: change_remote_disabled,
    },
    Binding {
        id: "remotes.remove",
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::Remotes,
        intent: AppIntent::RemoveRemote,
        description: "Remove the selected remote and its remote-tracking branches, asking first",
        label: "remove",
        bar: true,
        disabled: change_remote_disabled,
    },
    Binding {
        id: "remotes.switch_tab",
        keys: &[KeyPattern::exact(KeyCode::Char(']'), NONE)],
        scope: KeyScope::Remotes,
        intent: AppIntent::SwitchBranchesTab,
        description: "Show the tags",
        label: "tags",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "remotes.prev_tab",
        keys: &[KeyPattern::exact(KeyCode::Char('['), NONE)],
        scope: KeyScope::Remotes,
        intent: AppIntent::PrevBranchesTab,
        description: "Show the local branches",
        label: "local",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "tags.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Tags,
        intent: AppIntent::TagNext,
        description: "Select the next tag",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "tags.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Tags,
        intent: AppIntent::TagPrev,
        description: "Select the previous tag",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "tags.delete",
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::Tags,
        intent: AppIntent::DeleteTag,
        description: "Delete the selected tag here, not on any remote, asking first",
        label: "delete",
        bar: true,
        disabled: delete_tag_disabled,
    },
    // Shadows `workspace.push` on purpose: here `P` pushes the tag.
    Binding {
        id: "tags.push",
        keys: &[KeyPattern::any(KeyCode::Char('P'))],
        scope: KeyScope::Tags,
        intent: AppIntent::PushTag,
        description: "Push the selected tag to the upstream's remote, or the only remote",
        label: "push tag",
        bar: true,
        disabled: push_tag_disabled,
    },
    Binding {
        id: "tags.switch_tab",
        keys: &[KeyPattern::exact(KeyCode::Char(']'), NONE)],
        scope: KeyScope::Tags,
        intent: AppIntent::SwitchBranchesTab,
        description: "Show the local branches",
        label: "local",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "tags.prev_tab",
        keys: &[KeyPattern::exact(KeyCode::Char('['), NONE)],
        scope: KeyScope::Tags,
        intent: AppIntent::PrevBranchesTab,
        description: "Show the remotes and their branches",
        label: "remotes",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "commits.tag",
        keys: &[KeyPattern::exact(KeyCode::Char('t'), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::NewTag,
        description: "Tag the selected commit with a lightweight tag",
        label: "tag",
        bar: true,
        disabled: commits_tag_disabled,
    },
    Binding {
        id: "commits.tag_annotated",
        keys: &[KeyPattern::any(KeyCode::Char('T'))],
        scope: KeyScope::Commits,
        intent: AppIntent::NewAnnotatedTag,
        description: "Tag the selected commit with an annotated tag and a message",
        label: "annotated tag",
        bar: false,
        disabled: commits_tag_disabled,
    },
    Binding {
        id: "commits.bisect",
        keys: &[KeyPattern::any(KeyCode::Char('B'))],
        scope: KeyScope::Commits,
        intent: AppIntent::OpenMenu(MenuId::Bisect),
        description: "Mark the selected commit good, bad or skipped for a bisect, or reset it",
        label: "bisect",
        bar: true,
        disabled: commits_bisect_disabled,
    },
    Binding {
        id: "status.continue",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), NONE)],
        scope: KeyScope::Status,
        intent: AppIntent::ContinueOperation,
        description: "Continue the merge, rebase, cherry-pick or revert once conflicts are resolved",
        label: "continue",
        bar: true,
        disabled: bisect::continue_disabled,
    },
    Binding {
        id: "status.skip",
        keys: &[KeyPattern::exact(KeyCode::Char('s'), NONE)],
        scope: KeyScope::Status,
        intent: AppIntent::SkipOperation,
        description: "Skip the commit the rebase, cherry-pick or revert stopped on, or the one a bisect is testing",
        label: "skip",
        bar: true,
        disabled: bisect::skip_disabled,
    },
    Binding {
        id: "status.abort",
        keys: &[KeyPattern::exact(KeyCode::Char('a'), NONE)],
        scope: KeyScope::Status,
        intent: AppIntent::AbortOperation,
        description: "Abort the operation in progress, or reset the bisect, asking first",
        label: "abort",
        bar: true,
        disabled: bisect::abort_disabled,
    },
    Binding {
        id: "status.bisect_good",
        keys: &[KeyPattern::exact(KeyCode::Char('g'), NONE)],
        scope: KeyScope::Status,
        intent: AppIntent::BisectHead(BisectMark::Good),
        description: "Mark the commit the bisect is testing good",
        label: "good",
        bar: true,
        disabled: bisect::head_good_disabled,
    },
    Binding {
        id: "status.bisect_bad",
        keys: &[KeyPattern::exact(KeyCode::Char('b'), NONE)],
        scope: KeyScope::Status,
        intent: AppIntent::BisectHead(BisectMark::Bad),
        description: "Mark the commit the bisect is testing bad",
        label: "bad",
        bar: true,
        disabled: bisect::head_bad_disabled,
    },
    // As with discarding, Enter is left unbound: a delete cannot be undone.
    Binding {
        id: "delete_branch.yes",
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::DeleteBranch,
        intent: AppIntent::ConfirmDeleteBranch { force: false },
        description: "Yes, delete the merged branch",
        label: "delete",
        bar: true,
        disabled: plain_delete_disabled,
    },
    Binding {
        id: "delete_branch.force",
        keys: &[KeyPattern::any(KeyCode::Char('D'))],
        scope: KeyScope::DeleteBranch,
        intent: AppIntent::ConfirmDeleteBranch { force: true },
        description: "Yes, delete the branch and its unmerged commits",
        label: "force delete",
        bar: true,
        disabled: force_delete_disabled,
    },
    Binding {
        id: "delete_branch.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::DeleteBranch,
        intent: AppIntent::ConfirmNo,
        description: "No, keep the branch",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "delete_branch.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::DeleteBranch,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    // As with deleting a branch, Enter is left unbound: a deleted tag
    // cannot be listed again to undo it.
    Binding {
        id: "delete_tag.yes",
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::DeleteTag,
        intent: AppIntent::ConfirmDeleteTag,
        description: "Yes, delete the tag here",
        label: "delete",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "delete_tag.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::DeleteTag,
        intent: AppIntent::ConfirmNo,
        description: "No, keep the tag",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "delete_tag.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::DeleteTag,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    // As with deleting a branch, Enter is left unbound: the remote's
    // branches go with it.
    Binding {
        id: "remove_remote.yes",
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::RemoveRemote,
        intent: AppIntent::ConfirmRemoveRemote,
        description: "Yes, remove the remote and its remote-tracking branches",
        label: "remove",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "remove_remote.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::RemoveRemote,
        intent: AppIntent::ConfirmNo,
        description: "No, keep the remote",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "remove_remote.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::RemoveRemote,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    // As with deleting a branch, Enter is left unbound: a hard reset
    // cannot be undone.
    Binding {
        id: "reset_hard.yes",
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::ResetHard,
        intent: AppIntent::ConfirmResetHard,
        description: "Yes, reset and throw away every uncommitted change",
        label: "reset",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "reset_hard.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::ResetHard,
        intent: AppIntent::ConfirmNo,
        description: "No, keep the changes",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "reset_hard.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::ResetHard,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    // As with a hard reset, Enter is left unbound: commits left behind
    // are only in the reflog.
    Binding {
        id: "leave_commits.yes",
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::LeaveCommits,
        intent: AppIntent::ConfirmLeaveCommits,
        description: "Yes, move HEAD and leave the commits behind",
        label: "leave",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "leave_commits.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::LeaveCommits,
        intent: AppIntent::ConfirmNo,
        description: "No, stay where HEAD is",
        label: "stay",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "leave_commits.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::LeaveCommits,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    // As with a hard reset, Enter is left unbound: a dropped stash is gone
    // from the list.
    Binding {
        id: "drop_stash.yes",
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::DropStash,
        intent: AppIntent::ConfirmDropStash,
        description: "Yes, drop the stash",
        label: "drop",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "drop_stash.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::DropStash,
        intent: AppIntent::ConfirmNo,
        description: "No, keep the stash",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "drop_stash.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::DropStash,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    // As with discarding, Enter is left unbound: aborting loses resolved
    // conflicts.
    Binding {
        id: "abort_confirm.yes",
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::AbortConfirm,
        intent: AppIntent::ConfirmYes,
        description: "Yes, abort the operation",
        label: "abort",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "abort_confirm.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::AbortConfirm,
        intent: AppIntent::ConfirmNo,
        description: "No, keep the operation going",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "abort_confirm.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::AbortConfirm,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    // Enter is left unbound here too: autosquash rewrites commits the user
    // may not be looking at.
    Binding {
        id: "autosquash_confirm.yes",
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::AutosquashConfirm,
        intent: AppIntent::ConfirmYes,
        description: "Yes, fold every fixup! commit into its target",
        label: "fold",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "autosquash_confirm.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::AutosquashConfirm,
        intent: AppIntent::ConfirmNo,
        description: "No, leave the history as it is",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "autosquash_confirm.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::AutosquashConfirm,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    // Enter is left unbound here too: a custom patch rewrites commits the
    // user may not be looking at.
    Binding {
        id: "patch_confirm.yes",
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::PatchConfirm,
        intent: AppIntent::ConfirmYes,
        description: "Yes, rewrite the history with the custom patch",
        label: "rewrite",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "patch_confirm.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::PatchConfirm,
        intent: AppIntent::ConfirmNo,
        description: "No, leave the history as it is",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "patch_confirm.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::PatchConfirm,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    // Enter is left unbound here too: an undo can move a branch away from
    // commits, so only a deliberate y answers.
    Binding {
        id: "travel.yes",
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::Travel,
        intent: AppIntent::ConfirmTravel,
        description: "Yes, do what the question says",
        label: "yes",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "travel.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::Travel,
        intent: AppIntent::ConfirmNo,
        description: "No, leave everything where it is",
        label: "no",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "travel.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::Travel,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "submodules.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Submodules,
        intent: AppIntent::SubmoduleNext,
        description: "Select the next submodule",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "submodules.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Submodules,
        intent: AppIntent::SubmodulePrev,
        description: "Select the previous submodule",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "submodules.enter",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Submodules,
        intent: AppIntent::EnterSubmodule,
        description: "Work in the selected submodule",
        label: "enter",
        bar: true,
        disabled: enter_submodule_disabled,
    },
    Binding {
        id: "submodules.update",
        keys: &[KeyPattern::exact(KeyCode::Char('u'), NONE)],
        scope: KeyScope::Submodules,
        intent: AppIntent::UpdateSubmodule,
        description: "Clone or check out the selected submodule at the recorded commit",
        label: "update",
        bar: true,
        disabled: submodule_op_disabled,
    },
    Binding {
        id: "submodules.init",
        keys: &[KeyPattern::exact(KeyCode::Char('i'), NONE)],
        scope: KeyScope::Submodules,
        intent: AppIntent::InitSubmodule,
        description: "Record the selected submodule's URL without cloning it",
        label: "init",
        bar: true,
        disabled: submodule_op_disabled,
    },
    Binding {
        id: "submodules.close",
        keys: &[
            KeyPattern::any(KeyCode::Esc),
            KeyPattern::exact(KeyCode::Char('q'), NONE),
        ],
        scope: KeyScope::Submodules,
        intent: AppIntent::PopPopup,
        description: "Close the list",
        label: "close",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "submodules.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::Submodules,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "worktrees.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Worktrees,
        intent: AppIntent::WorktreeNext,
        description: "Select the next worktree",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "worktrees.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Worktrees,
        intent: AppIntent::WorktreePrev,
        description: "Select the previous worktree",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "worktrees.open",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Worktrees,
        intent: AppIntent::OpenWorktree,
        description: "Switch to the selected worktree",
        label: "switch",
        bar: true,
        disabled: open_worktree_disabled,
    },
    Binding {
        id: "worktrees.add",
        keys: &[KeyPattern::exact(KeyCode::Char('a'), NONE)],
        scope: KeyScope::Worktrees,
        intent: AppIntent::AddWorktree,
        description: "Add a worktree for the local branch selected in the Branches panel",
        label: "add",
        bar: true,
        disabled: add_worktree_disabled,
    },
    Binding {
        id: "worktrees.remove",
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::Worktrees,
        intent: AppIntent::RemoveWorktree,
        description: "Remove the selected linked worktree",
        label: "remove",
        bar: true,
        disabled: remove_worktree_disabled,
    },
    Binding {
        id: "worktrees.close",
        keys: &[
            KeyPattern::any(KeyCode::Esc),
            KeyPattern::exact(KeyCode::Char('q'), NONE),
        ],
        scope: KeyScope::Worktrees,
        intent: AppIntent::PopPopup,
        description: "Close the list",
        label: "close",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "worktrees.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::Worktrees,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "command_log.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::CommandLog,
        intent: AppIntent::CommandLogNext,
        description: "Select the next command",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "command_log.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::CommandLog,
        intent: AppIntent::CommandLogPrev,
        description: "Select the previous command",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "command_log.page_down",
        keys: &[KeyPattern::any(KeyCode::PageDown)],
        scope: KeyScope::CommandLog,
        intent: AppIntent::CommandLogPageDown,
        description: "Move a page down",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "command_log.page_up",
        keys: &[KeyPattern::any(KeyCode::PageUp)],
        scope: KeyScope::CommandLog,
        intent: AppIntent::CommandLogPageUp,
        description: "Move a page up",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "command_log.close",
        keys: &[
            KeyPattern::any(KeyCode::Esc),
            KeyPattern::exact(KeyCode::Char('q'), NONE),
        ],
        scope: KeyScope::CommandLog,
        intent: AppIntent::PopPopup,
        description: "Close the log",
        label: "close",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "command_log.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::CommandLog,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    // Enter is left unbound: the worktree's directory goes with it.
    Binding {
        id: "remove_worktree.yes",
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::RemoveWorktree,
        intent: AppIntent::ConfirmRemoveWorktree,
        description: "Yes, remove the worktree and its directory",
        label: "remove",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "remove_worktree.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::RemoveWorktree,
        intent: AppIntent::ConfirmNo,
        description: "No, keep the worktree",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "remove_worktree.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::RemoveWorktree,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "recent.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Recent,
        intent: AppIntent::RecentNext,
        description: "Select the next row",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "recent.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Recent,
        intent: AppIntent::RecentPrev,
        description: "Select the previous row",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "recent.open",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Recent,
        intent: AppIntent::RecentSelect,
        description: "Open the selected repository",
        label: "open",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "recent.init",
        keys: &[KeyPattern::exact(KeyCode::Char('i'), NONE)],
        scope: KeyScope::Recent,
        intent: AppIntent::InitRepo,
        description: "Make the directory Kika started in a repository",
        label: "init",
        bar: true,
        disabled: init_repo_disabled,
    },
    Binding {
        id: "recent.close",
        keys: &[
            KeyPattern::any(KeyCode::Esc),
            KeyPattern::exact(KeyCode::Char('q'), NONE),
        ],
        scope: KeyScope::Recent,
        intent: AppIntent::PopPopup,
        description: "Close the list",
        label: "close",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "recent.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::Recent,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "pick_list.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::PickList,
        intent: AppIntent::PickNext,
        description: "Select the next row",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "pick_list.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::PickList,
        intent: AppIntent::PickPrev,
        description: "Select the previous row",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "pick_list.pick",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::PickList,
        intent: AppIntent::PickSelect,
        description: "Take the selected row",
        label: "pick",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "pick_list.close",
        keys: &[
            KeyPattern::any(KeyCode::Esc),
            KeyPattern::exact(KeyCode::Char('q'), NONE),
        ],
        scope: KeyScope::PickList,
        intent: AppIntent::PopPopup,
        description: "Close the list and stop",
        label: "close",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "pick_list.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::PickList,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    // Creating a pull request reaches GitHub, so Enter is left unbound.
    Binding {
        id: "create_pull_request.yes",
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::CreatePullRequest,
        intent: AppIntent::ConfirmCreatePullRequest,
        description: "Yes, create the pull request on GitHub",
        label: "create",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "create_pull_request.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::CreatePullRequest,
        intent: AppIntent::ConfirmNo,
        description: "No, create nothing",
        label: "cancel",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "create_pull_request.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::CreatePullRequest,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
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
        description: "Clear the selection, else the filter, else quit Kika",
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
        description: "Select a range of commits from here, for copy; again to clear it",
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
    // The log query and the diff mark.
    Binding {
        id: "commits.filter_path",
        keys: &[KeyPattern::any(KeyCode::Char('L'))],
        scope: KeyScope::Commits,
        intent: AppIntent::OpenLogFilter(LogFilterField::Path),
        description: "Show only the commits that changed a typed path, across renames",
        label: "log of path",
        bar: false,
        disabled: log_filter_disabled,
    },
    Binding {
        id: "commits.filter_author",
        keys: &[KeyPattern::any(KeyCode::Char('A'))],
        scope: KeyScope::Commits,
        intent: AppIntent::OpenLogFilter(LogFilterField::Author),
        description: "Show only the commits by a typed author",
        label: "log by author",
        bar: false,
        disabled: log_filter_disabled,
    },
    Binding {
        id: "commits.mark",
        keys: &[KeyPattern::any(KeyCode::Char('M'))],
        scope: KeyScope::Commits,
        intent: AppIntent::ToggleDiffMark,
        description: "Mark the selected commit to diff against (enter on another); again to clear",
        label: "mark for diff",
        bar: false,
        disabled: commits_mark_disabled,
    },
    Binding {
        id: "reflog.filter_path",
        keys: &[KeyPattern::any(KeyCode::Char('L'))],
        scope: KeyScope::Reflog,
        intent: AppIntent::OpenLogFilter(LogFilterField::Path),
        description: "Show only the commits that changed a typed path, across renames",
        label: "log of path",
        bar: false,
        disabled: reflog_filter_disabled,
    },
    Binding {
        id: "reflog.filter_author",
        keys: &[KeyPattern::any(KeyCode::Char('A'))],
        scope: KeyScope::Reflog,
        intent: AppIntent::OpenLogFilter(LogFilterField::Author),
        description: "Show only the commits by a typed author",
        label: "log by author",
        bar: false,
        disabled: reflog_filter_disabled,
    },
    Binding {
        id: "branches.mark",
        keys: &[KeyPattern::any(KeyCode::Char('M'))],
        scope: KeyScope::Branches,
        intent: AppIntent::ToggleDiffMark,
        description: "Mark the selected branch to diff against (enter on another); again to clear",
        label: "mark for diff",
        bar: false,
        disabled: no_branch_selected,
    },
    Binding {
        id: "branches.diff",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Branches,
        intent: AppIntent::OpenDiff,
        description: "Show what changed from the diff mark to the selected branch",
        label: "diff with mark",
        bar: false,
        disabled: branches_diff_disabled,
    },
    Binding {
        id: "files.log",
        keys: &[KeyPattern::any(KeyCode::Char('L'))],
        scope: KeyScope::Files,
        intent: AppIntent::LogOfFile,
        description: "Show the selected file's history in the Commits panel",
        label: "log of file",
        bar: false,
        disabled: files_log_disabled,
    },
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

/// The application's keymap.
pub fn registry() -> &'static [Binding] {
    REGISTRY
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::custom_command::{OutputMode, Panel, test_table};
    use crate::model::Context;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyPress {
        KeyPress::new(code, modifiers)
    }

    fn plain(c: char) -> KeyPress {
        key(KeyCode::Char(c), KeyModifiers::NONE)
    }

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
            bind(X, KeyScope::Files, SelectNext),
            Binding {
                disabled: off,
                ..bind(V, KeyScope::Files, SelectPrev)
            },
            bind(W, KeyScope::Review, ToggleStage),
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
            AppIntent::ToggleStage
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

    fn from(focus: KeyScope, key: KeyPress) -> Option<AppIntent> {
        // Every binding is enabled on a workspace with a stageable file
        // selected, a commit in the log and a stash in the list.
        let mut state = AppState::new(Context::Workspace);
        state.stash.entries = vec![crate::git::stash::StashEntry {
            index: 0,
            oid: crate::git::commit_files::CommitId::parse("a0").unwrap(),
            base: crate::git::commit_files::CommitId::parse("b0").unwrap(),
            untracked: None,
            subject: "On main: wip".into(),
        }];
        state.files = vec![crate::git::FileEntry {
            path: "a.rs".into(),
            orig_path: None,
            index: '.',
            worktree: 'M',
            kind: EntryKind::Changed,
        }];
        state.selected = Some(0);
        state.commits.rows = vec![crate::git::log::CommitRow {
            oid: "abc".into(),
            short_oid: "abc".into(),
            parents: vec![],
            author: "Ann".into(),
            time: 0,
            subject: "s".into(),
            subject_altered: false,
        }];
        match resolve(registry(), &Keymap::default(), focus, &state, key) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            Resolution::Disabled(reason) => panic!("{key:?} disabled: {reason}"),
            Resolution::Unbound | Resolution::Command(_) => None,
        }
    }

    #[test]
    fn the_stash_panel_opens_applies_pops_drops_and_renames_the_selected_stash() {
        use AppIntent::*;
        assert_eq!(KeyScope::Stash.parent(), Some(KeyScope::Workspace));
        assert_eq!(KeyScope::Stash.title(), "Stash");
        let stash = |k| from(KeyScope::Stash, k);
        let none = KeyModifiers::NONE;
        assert_eq!(stash(key(KeyCode::Enter, none)), Some(OpenStashFiles));
        assert_eq!(stash(plain(' ')), Some(ApplyStash));
        assert_eq!(stash(plain('g')), Some(PopStash));
        assert_eq!(stash(plain('d')), Some(DropStash));
        assert_eq!(
            stash(key(KeyCode::Char('R'), KeyModifiers::SHIFT)),
            Some(RenameStash)
        );
        assert_eq!(stash(plain('j')), Some(StashNext));
        assert_eq!(stash(key(KeyCode::Down, none)), Some(StashNext));
        assert_eq!(stash(plain('k')), Some(StashPrev));
        assert_eq!(stash(key(KeyCode::Up, none)), Some(StashPrev));
        assert_eq!(stash(plain('q')), Some(Quit));

        assert_eq!(KeyScope::StashFiles.parent(), Some(KeyScope::Workspace));
        assert_eq!(KeyScope::StashFiles.title(), "Stash files");
        let files = |k| from(KeyScope::StashFiles, k);
        assert_eq!(files(key(KeyCode::Esc, none)), Some(CloseStashFiles));
        assert_eq!(files(key(KeyCode::Backspace, none)), Some(CloseStashFiles));
        assert_eq!(files(plain('j')), Some(StashFileNext));
        assert_eq!(files(key(KeyCode::Up, none)), Some(StashFilePrev));
        assert_eq!(files(plain('J')), Some(ScrollDiff(Scroll::LineDown)));

        assert_eq!(
            from(KeyScope::Files, plain('s')),
            Some(OpenMenu(MenuId::Stash))
        );
    }

    #[test]
    fn stash_actions_say_why_they_cannot_run() {
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(stash_action_disabled(&state), Some("No stashes".into()));
        assert_eq!(no_stash(&state), Some("No stashes".into()));
        assert_eq!(
            stash_menu_disabled(&state),
            Some("No changes to stash".into())
        );
        state.stash.entries = vec![crate::git::stash::StashEntry {
            index: 0,
            oid: crate::git::commit_files::CommitId::parse("a0").unwrap(),
            base: crate::git::commit_files::CommitId::parse("b0").unwrap(),
            untracked: None,
            subject: String::new(),
        }];
        assert_eq!(stash_action_disabled(&state), None);
        state.op_seq = Some(1);
        assert_eq!(stash_action_disabled(&state), Some(OP_RUNNING.into()));
        assert_eq!(no_stash(&state), None);
    }

    #[test]
    fn the_drop_stash_question_has_no_enter_and_n_or_esc_keeps_it() {
        let state = AppState::new(Context::Workspace);
        let resolve_in = |key| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::DropStash,
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
        assert_eq!(resolve_in(plain('y')), Some(AppIntent::ConfirmDropStash));
        assert_eq!(resolve_in(plain('n')), Some(AppIntent::ConfirmNo));
        assert_eq!(
            resolve_in(key(KeyCode::Esc, none)),
            Some(AppIntent::ConfirmNo)
        );
        assert_eq!(
            resolve_in(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(AppIntent::Quit)
        );
        assert_eq!(KeyScope::DropStash.parent(), None);
        assert_eq!(KeyScope::DropStash.title(), "Drop stash");
    }

    #[test]
    fn the_abort_question_has_no_enter_and_n_or_esc_keeps_the_operation() {
        let state = AppState::new(Context::Workspace);
        let resolve_in = |key| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::AbortConfirm,
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
        assert_eq!(resolve_in(plain('y')), Some(AppIntent::ConfirmYes));
        assert_eq!(resolve_in(plain('n')), Some(AppIntent::ConfirmNo));
        assert_eq!(
            resolve_in(key(KeyCode::Esc, none)),
            Some(AppIntent::ConfirmNo)
        );
        assert_eq!(
            resolve_in(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(AppIntent::Quit)
        );
        assert_eq!(KeyScope::AbortConfirm.parent(), None);
        assert_eq!(KeyScope::AbortConfirm.title(), "Abort");

        let enter = key(KeyCode::Enter, none);
        for b in registry()
            .iter()
            .filter(|b| b.scope == KeyScope::AbortConfirm)
        {
            assert!(!b.keys.iter().any(|k| k.matches(enter)), "{}", b.id);
        }
    }

    #[test]
    fn a_repository_layer_cannot_put_enter_back_on_the_abort_question() {
        for id in [
            "abort_confirm.yes",
            "abort_confirm.no",
            "abort_confirm.quit",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
        }
        assert_eq!(
            build(&[("abort_confirm.yes", &["enter"])], Trust::Repo).unwrap_err(),
            r#""abort_confirm.yes" can only be changed in the global config file"#
        );
        assert_eq!(
            build(&[("discard_confirm.yes", &["enter"])], Trust::Repo).unwrap_err(),
            r#""discard_confirm.yes" can only be changed in the global config file"#
        );
    }

    #[test]
    fn the_stash_actions_and_answers_are_locked_but_moving_around_is_not() {
        for id in [
            "files.stash",
            "stash.files",
            "stash.apply",
            "stash.pop",
            "stash.drop",
            "stash.rename",
            "drop_stash.yes",
            "drop_stash.no",
            "drop_stash.quit",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
            assert!(build(&[(id, &["F"])], Trust::Repo).is_err(), "{id}");
        }
        for id in [
            "stash.next",
            "stash.prev",
            "stash_files.next",
            "stash_files.prev",
            "stash_files.close",
        ] {
            assert!(!REPO_LOCKED.contains(&id), "{id}");
            assert!(build(&[(id, &["F"])], Trust::Repo).is_ok(), "{id}");
        }
    }

    #[test]
    fn the_commits_panel_moves_its_cursor_and_falls_back_to_the_workspace() {
        use AppIntent::*;
        assert_eq!(KeyScope::Commits.parent(), Some(KeyScope::Workspace));
        assert_eq!(KeyScope::Commits.title(), "Commits");
        let commits = |k| from(KeyScope::Commits, k);
        let none = KeyModifiers::NONE;
        assert_eq!(commits(plain('j')), Some(CommitNext));
        assert_eq!(commits(key(KeyCode::Down, none)), Some(CommitNext));
        assert_eq!(commits(plain('k')), Some(CommitPrev));
        assert_eq!(commits(key(KeyCode::Up, none)), Some(CommitPrev));
        assert_eq!(commits(plain('r')), Some(RefreshStatus));
        assert_eq!(commits(plain('q')), Some(Quit));
    }

    #[test]
    fn enter_opens_a_commits_files_where_esc_goes_back_instead_of_quitting() {
        use AppIntent::*;
        let none = KeyModifiers::NONE;
        assert_eq!(
            from(KeyScope::Commits, key(KeyCode::Enter, none)),
            Some(OpenCommitFiles)
        );
        assert_eq!(KeyScope::CommitFiles.parent(), Some(KeyScope::Workspace));
        assert_eq!(KeyScope::CommitFiles.title(), "Commit files");
        let files = |k| from(KeyScope::CommitFiles, k);
        for k in [
            key(KeyCode::Esc, none),
            plain('h'),
            key(KeyCode::Left, none),
        ] {
            assert_eq!(files(k), Some(CloseCommitFiles), "{k:?}");
        }
        assert_eq!(files(plain('j')), Some(CommitFileNext));
        assert_eq!(files(key(KeyCode::Down, none)), Some(CommitFileNext));
        assert_eq!(files(plain('k')), Some(CommitFilePrev));
        assert_eq!(files(key(KeyCode::Up, none)), Some(CommitFilePrev));
        assert_eq!(files(plain('q')), Some(Quit));
        assert_eq!(files(plain('J')), Some(ScrollDiff(Scroll::LineDown)));
    }

    #[test]
    fn the_status_panel_controls_the_operation_and_falls_back_to_the_workspace() {
        use AppIntent::*;
        assert_eq!(KeyScope::Status.parent(), Some(KeyScope::Workspace));
        assert_eq!(KeyScope::Status.title(), "Status");
        // A rebase is in progress, so all three keys are enabled.
        let mut state = AppState::new(Context::Workspace);
        state.branch = Some(crate::git::status::BranchStatus {
            head: Head::Branch("main".into()),
            upstream: None,
            ahead_behind: None,
            in_progress: vec![Operation::Rebase],
            bisect: None,
        });
        let status = |c| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::Status,
            &state,
            plain(c),
        ) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            _ => None,
        };
        assert_eq!(status('c'), Some(ContinueOperation));
        assert_eq!(status('s'), Some(SkipOperation));
        assert_eq!(status('a'), Some(AbortOperation));
        assert_eq!(status('r'), Some(RefreshStatus));
        let merge = registry().iter().find(|b| b.id == "branches.merge");
        assert_eq!(merge.map(|b| &b.intent), Some(&OpenMenu(MenuId::Merge)));
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
        for k in [plain('h'), key(KeyCode::Left, none)] {
            assert_eq!(diff(k), Some(FocusLeft), "{k:?}");
        }
        // Esc clears a search first, and goes back with none.
        assert_eq!(diff(key(KeyCode::Esc, none)), Some(DiffEscape));
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
        assert_eq!(lines(plain('h')), Some(FocusLeft));
        assert_eq!(lines(plain('r')), Some(RefreshStatus));
        assert_eq!(lines(plain('q')), Some(Quit));

        // Space and `v` share the hunk key's refusals; with no hunks here,
        // that is the same reason.
        let state = AppState::new(Context::Workspace);
        for (scope, k) in [(KeyScope::Lines, plain(' ')), (KeyScope::Diff, plain('v'))] {
            assert!(matches!(
                resolve(registry(), &Keymap::default(), scope, &state, k),
                Resolution::Disabled(ref reason) if reason == NO_CHANGES
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
        state.op_seq = Some(1);
        assert_eq!(c(&state), Err("An operation is running".into()));
        state.op_seq = None;
        assert_eq!(c(&state), Err("Nothing to commit".into()));
        let file = |kind| crate::git::FileEntry {
            path: "a.rs".into(),
            orig_path: None,
            index: 'U',
            worktree: 'U',
            kind,
        };
        state.files = vec![file(EntryKind::Changed), file(EntryKind::Conflicted)];
        assert_eq!(c(&state), Err("Resolve conflicts before committing".into()));
        state.op_seq = Some(1);
        assert_eq!(c(&state), Err("An operation is running".into()));
        state.op_seq = None;
        state.files.pop();
        assert_eq!(c(&state), Ok(AppIntent::OpenCommit));
    }

    #[test]
    fn shift_a_amends_unless_an_op_runs_there_is_no_commit_or_there_are_conflicts() {
        use crate::git::status::{BranchStatus, Head};

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
        assert!(state.files.is_empty());
        assert_eq!(a(&state), Ok(AppIntent::OpenAmend));
        state.branch = Some(branch(Head::Detached("abc".into())));
        assert_eq!(a(&state), Ok(AppIntent::OpenAmend));

        state.op_seq = Some(1);
        assert_eq!(a(&state), Err(OP_RUNNING.into()));
        state.op_seq = None;
        state.files = vec![crate::git::FileEntry {
            path: "a.rs".into(),
            orig_path: None,
            index: 'U',
            worktree: 'U',
            kind: EntryKind::Conflicted,
        }];
        assert_eq!(a(&state), Err("Resolve conflicts before amending".into()));
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
                resolve(
                    registry(),
                    &Keymap::default(),
                    KeyScope::CommitEditor,
                    &state,
                    ctrl(c)
                ),
                Resolution::Disabled(ref reason) if reason == OP_RUNNING
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

        assert_eq!(editor(ctrl('c')), Some(InterruptCommit));

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
    fn the_submodule_list_has_its_own_keys_and_never_reaches_global_ones() {
        use crate::git::submodule::{Submodule, SubmoduleState};
        use crate::popup::Popup;
        use crate::submodule_list::SubmoduleList;
        use AppIntent::*;

        let none = KeyModifiers::NONE;
        let shift = KeyModifiers::SHIFT;
        assert_eq!(
            from(KeyScope::Files, key(KeyCode::Char('S'), shift)),
            Some(OpenSubmodules)
        );
        assert_eq!(KeyScope::Submodules.parent(), None);
        assert_eq!(KeyScope::Submodules.title(), "Submodules");

        let mut state = AppState::new(Context::Workspace);
        let mut list = SubmoduleList::loading(0);
        list.loaded(Ok(vec![Submodule {
            path: "sub".into(),
            oid: "abc".into(),
            state: SubmoduleState::UpToDate,
        }]));
        state.popups = vec![Popup::Submodules(list)];
        assert_eq!(state.key_focus(), KeyScope::Submodules);
        let run = |k| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::Submodules,
            &state,
            k,
        ) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            Resolution::Disabled(reason) => panic!("{k:?} disabled: {reason}"),
            Resolution::Unbound | Resolution::Command(_) => None,
        };
        assert_eq!(run(plain('j')), Some(SubmoduleNext));
        assert_eq!(run(key(KeyCode::Down, none)), Some(SubmoduleNext));
        assert_eq!(run(plain('k')), Some(SubmodulePrev));
        assert_eq!(run(key(KeyCode::Up, none)), Some(SubmodulePrev));
        assert_eq!(run(key(KeyCode::Enter, none)), Some(EnterSubmodule));
        assert_eq!(run(plain('i')), Some(InitSubmodule));
        assert_eq!(run(plain('u')), Some(UpdateSubmodule));
        assert_eq!(run(key(KeyCode::Esc, none)), Some(PopPopup));
        assert_eq!(run(plain('q')), Some(PopPopup));
        assert_eq!(
            run(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Quit)
        );
        for k in [
            plain('?'),
            plain('r'),
            plain(' '),
            key(KeyCode::Backspace, none),
        ] {
            assert_eq!(run(k), None, "{k:?}");
        }
    }

    /// `@` is shifted on most layouts and AltGr (Ctrl+Alt on Windows) or
    /// Option on others; crossterm reports the character itself with
    /// whatever modifiers made it, so each of those must open the log.
    #[test]
    fn at_opens_the_command_log_however_it_is_typed_and_clashes_with_nothing() {
        use AppIntent::*;

        let at = KeyCode::Char('@');
        let binding = registry()
            .iter()
            .find(|b| b.id == "workspace.command_log")
            .unwrap();
        assert_eq!(binding.scope, KeyScope::Workspace);
        assert_eq!(binding.intent, OpenCommandLog);
        assert_eq!(binding.keys, [KeyPattern::any(at)]);
        assert_eq!(parse_key("@"), Ok(KeyPattern::any(at)));

        // Nothing in the workspace, its panels or the global scope takes it.
        for other in registry() {
            let reaches_workspace = other.scope.chain().any(|s| s == KeyScope::Workspace)
                || other.scope == KeyScope::Global;
            if other.id == binding.id || !reaches_workspace {
                continue;
            }
            for key in other.keys {
                assert!(!key.overlaps(&KeyPattern::any(at)), "{} takes @", other.id);
            }
        }

        let workspace = AppState::new(Context::Workspace);
        let alt_gr = KeyModifiers::CONTROL | KeyModifiers::ALT;
        let mods = [
            KeyModifiers::NONE,
            KeyModifiers::SHIFT,
            alt_gr,
            KeyModifiers::ALT,
        ];
        for scope in [
            KeyScope::Files,
            KeyScope::Branches,
            KeyScope::Remotes,
            KeyScope::Tags,
            KeyScope::Commits,
            KeyScope::CommitFiles,
            KeyScope::Reflog,
            KeyScope::Status,
            KeyScope::Diff,
            KeyScope::Lines,
            KeyScope::Blame,
            KeyScope::BlameCommit,
            KeyScope::Conflict,
            KeyScope::Workspace,
        ] {
            for mods in mods {
                let found = resolve(
                    registry(),
                    &Keymap::default(),
                    scope,
                    &workspace,
                    key(at, mods),
                );
                assert!(
                    matches!(found, Resolution::Run(b) if b.intent == OpenCommandLog),
                    "{scope:?} {mods:?}"
                );
            }
        }
    }

    #[test]
    fn the_command_log_has_its_own_keys_and_never_reaches_global_ones() {
        use AppIntent::*;

        assert_eq!(KeyScope::CommandLog.parent(), None);
        assert_eq!(KeyScope::CommandLog.title(), "Command log");
        let state = AppState::new(Context::Workspace);
        let none = KeyModifiers::NONE;
        let run = |k| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::CommandLog,
            &state,
            k,
        ) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            Resolution::Disabled(reason) => panic!("{k:?} disabled: {reason}"),
            Resolution::Unbound | Resolution::Command(_) => None,
        };
        assert_eq!(run(plain('j')), Some(CommandLogNext));
        assert_eq!(run(key(KeyCode::Down, none)), Some(CommandLogNext));
        assert_eq!(run(plain('k')), Some(CommandLogPrev));
        assert_eq!(run(key(KeyCode::Up, none)), Some(CommandLogPrev));
        assert_eq!(run(key(KeyCode::PageDown, none)), Some(CommandLogPageDown));
        assert_eq!(run(key(KeyCode::PageUp, none)), Some(CommandLogPageUp));
        assert_eq!(run(key(KeyCode::Esc, none)), Some(PopPopup));
        assert_eq!(run(plain('q')), Some(PopPopup));
        assert_eq!(
            run(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Quit)
        );
        for k in [
            plain('?'),
            plain('@'),
            plain('r'),
            key(KeyCode::Enter, none),
        ] {
            assert_eq!(run(k), None, "{k:?}");
        }
        assert!(REPO_LOCKED.contains(&"command_log.quit"));
    }

    #[test]
    fn the_worktree_list_has_its_own_keys_and_never_reaches_global_ones() {
        use crate::git::worktrees::parse_list;
        use crate::popup::Popup;
        use crate::worktree_list::WorktreeList;
        use AppIntent::*;

        let none = KeyModifiers::NONE;
        let shift = KeyModifiers::SHIFT;
        let mut workspace = AppState::new(Context::Workspace);
        workspace.repo = Some("/r/a".into());
        for scope in [KeyScope::Files, KeyScope::Branches, KeyScope::Diff] {
            let found = resolve(
                registry(),
                &Keymap::default(),
                scope,
                &workspace,
                key(KeyCode::Char('W'), shift),
            );
            assert!(
                matches!(found, Resolution::Run(b) if b.intent == OpenWorktrees),
                "{scope:?}"
            );
        }
        assert_eq!(KeyScope::Worktrees.parent(), None);
        assert_eq!(KeyScope::Worktrees.title(), "Worktrees");
        assert_eq!(KeyScope::RemoveWorktree.parent(), None);
        assert_eq!(KeyScope::RemoveWorktree.title(), "Remove worktree");

        let mut state = AppState::new(Context::Workspace);
        state.repo = Some("/r/a".into());
        let mut list = WorktreeList::loading(0);
        list.loaded(Ok(parse_list(
            b"worktree /r/a\0bare\0\0worktree /r/b\0detached\0\0",
            0,
        )));
        list.move_cursor(1);
        state.popups = vec![Popup::Worktrees(list)];
        assert_eq!(state.key_focus(), KeyScope::Worktrees);
        let run = |k| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::Worktrees,
            &state,
            k,
        ) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            Resolution::Disabled(reason) => panic!("{k:?} disabled: {reason}"),
            Resolution::Unbound | Resolution::Command(_) => None,
        };
        assert_eq!(run(plain('j')), Some(WorktreeNext));
        assert_eq!(run(key(KeyCode::Down, none)), Some(WorktreeNext));
        assert_eq!(run(plain('k')), Some(WorktreePrev));
        assert_eq!(run(key(KeyCode::Up, none)), Some(WorktreePrev));
        assert_eq!(run(key(KeyCode::Enter, none)), Some(OpenWorktree));
        assert_eq!(run(plain('d')), Some(RemoveWorktree));
        assert_eq!(run(key(KeyCode::Esc, none)), Some(PopPopup));
        assert_eq!(run(plain('q')), Some(PopPopup));
        assert_eq!(
            run(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Quit)
        );
        for k in [plain('?'), plain('r'), plain('W')] {
            assert_eq!(run(k), None, "{k:?}");
        }
    }

    #[test]
    fn the_remove_worktree_question_has_no_enter_and_answers_with_y_or_n() {
        let enter = key(KeyCode::Enter, KeyModifiers::NONE);
        for b in registry()
            .iter()
            .filter(|b| b.scope == KeyScope::RemoveWorktree)
        {
            assert!(!b.matches(enter), "{} answers on Enter", b.id);
        }
        assert_eq!(
            from(KeyScope::RemoveWorktree, plain('y')),
            Some(AppIntent::ConfirmRemoveWorktree)
        );
        for k in [plain('n'), key(KeyCode::Esc, KeyModifiers::NONE)] {
            assert_eq!(
                from(KeyScope::RemoveWorktree, k),
                Some(AppIntent::ConfirmNo)
            );
        }
        for id in [
            "worktrees.open",
            "worktrees.add",
            "worktrees.remove",
            "worktrees.quit",
            "remove_worktree.yes",
            "remove_worktree.no",
            "remove_worktree.quit",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
        }
    }

    #[test]
    fn backspace_leaves_a_submodule_from_every_workspace_panel_once_inside_one() {
        let backspace = key(KeyCode::Backspace, KeyModifiers::NONE);
        let mut state = AppState::new(Context::Workspace);
        for scope in [KeyScope::Files, KeyScope::Diff, KeyScope::Lines] {
            assert!(
                matches!(
                    resolve(registry(), &Keymap::default(), scope, &state, backspace),
                    Resolution::Disabled(ref reason) if reason == "Not in a submodule"
                ),
                "{scope:?}"
            );
        }
        state.repo_stack.push(crate::model::ParentRepo {
            path: "sub".into(),
            files: vec![],
            files_loaded: true,
            branch: None,
            selected: None,
        });
        for scope in [KeyScope::Files, KeyScope::Diff, KeyScope::Lines] {
            assert!(
                matches!(
                    resolve(registry(), &Keymap::default(), scope, &state, backspace),
                    Resolution::Run(Binding {
                        intent: AppIntent::LeaveSubmodule,
                        ..
                    })
                ),
                "{scope:?}"
            );
        }
        state.op_seq = Some(1);
        assert!(matches!(
            resolve(
                registry(),
                &Keymap::default(),
                KeyScope::Files,
                &state,
                backspace
            ),
            Resolution::Disabled(ref reason) if reason == "Wait for the operation to finish"
        ));
        assert_eq!(
            KeyPattern::any(KeyCode::Backspace).label(),
            "bksp",
            "the same on every platform"
        );
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

        let reason = |state: &AppState, scope, c| match resolve(
            registry(),
            &Keymap::default(),
            scope,
            state,
            plain(c),
        ) {
            Resolution::Disabled(reason) => Some(reason),
            Resolution::Run(_) => None,
            Resolution::Unbound | Resolution::Command(_) => panic!("{c} is unbound in {scope:?}"),
        };
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(
            reason(&state, KeyScope::Files, 'o'),
            Some("No file selected".into())
        );
        state.op_seq = Some(1);
        assert_eq!(
            reason(&state, KeyScope::Files, 'o'),
            Some(OP_RUNNING.into())
        );
        assert_eq!(reason(&state, KeyScope::Diff, 'e'), Some(OP_RUNNING.into()));
        state.op_seq = None;

        let mut view = HunkView::new("a.rs".into(), DiffSide::Worktree);
        view.seq = Some(3);
        state.focus = Focus::Diff(view.clone());
        assert_eq!(reason(&state, KeyScope::Diff, 'e'), Some("Loading…".into()));
        view.seq = None;
        state.focus = Focus::Diff(view.clone());
        assert_eq!(reason(&state, KeyScope::Diff, 'e'), Some(NO_CHANGES.into()));
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
            Some("Press Esc to edit the whole hunk".into())
        );
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
        state.op_seq = Some(1);
        assert_eq!(d(&state), Some("An operation is running".into()));
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
                resolve(
                    registry(),
                    &Keymap::default(),
                    KeyScope::CommitEditor,
                    &state,
                    k
                ),
                Resolution::Disabled(ref reason) if reason == "An operation is running"
            ));
        }
        // Ctrl+C is the way out while git runs.
        let ctrl_c = key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(matches!(
            resolve(
                registry(),
                &Keymap::default(),
                KeyScope::CommitEditor,
                &state,
                ctrl_c
            ),
            Resolution::Run(Binding {
                intent: AppIntent::InterruptCommit,
                ..
            })
        ));
    }

    #[test]
    fn the_branches_panel_has_its_own_keys_and_reaches_the_workspace_ones() {
        use crate::git::branches::{Branch, Track};
        let mut state = AppState::new(Context::Workspace);
        let branch = |name: &str, head| Branch {
            name: name.into(),
            head,
            upstream: None,
            track: Track::None,
            merged: true,
        };
        state
            .branches
            .loaded(Ok(vec![branch("main", true), branch("topic", false)]));
        state.branches.move_by(1);
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
        assert_eq!(run(&state, plain('j')), Some(AppIntent::BranchNext));
        assert_eq!(
            run(&state, key(KeyCode::Up, KeyModifiers::NONE)),
            Some(AppIntent::BranchPrev)
        );
        assert_eq!(run(&state, plain(' ')), Some(AppIntent::CheckoutBranch));
        assert_eq!(run(&state, plain('n')), Some(AppIntent::NewBranch));
        assert_eq!(run(&state, shift('R')), Some(AppIntent::RenameBranch));
        assert_eq!(run(&state, plain('d')), Some(AppIntent::DeleteBranch));
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

        state.branches.move_by(-1);
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
    fn the_delete_question_lets_only_the_matching_key_answer() {
        use crate::popup::DeleteBranchConfirm;
        let mut state = AppState::new(Context::Workspace);
        let ask = |force| {
            Popup::DeleteBranch(DeleteBranchConfirm {
                name: "t".into(),
                op: crate::git::ops::GitOp::DeleteBranch {
                    name: "t".into(),
                    force,
                },
            })
        };
        let resolve_in = |state: &AppState, key| {
            resolve(
                registry(),
                &Keymap::default(),
                KeyScope::DeleteBranch,
                state,
                key,
            )
        };
        let shift_d = key(KeyCode::Char('D'), KeyModifiers::SHIFT);
        state.popups = vec![ask(false)];
        assert!(matches!(resolve_in(&state, plain('y')), Resolution::Run(_)));
        assert!(matches!(
            resolve_in(&state, shift_d),
            Resolution::Disabled(_)
        ));
        state.popups = vec![ask(true)];
        assert!(matches!(
            resolve_in(&state, plain('y')),
            Resolution::Disabled(_)
        ));
        assert!(matches!(resolve_in(&state, shift_d), Resolution::Run(_)));
        assert!(matches!(
            resolve_in(&state, plain('q')),
            Resolution::Unbound
        ));
        assert_eq!(KeyScope::DeleteBranch.parent(), None);
        assert_eq!(KeyScope::Branches.parent(), Some(KeyScope::Workspace));
    }

    #[test]
    fn the_remotes_tab_takes_its_own_keys_and_the_workspace_keys_still_reach_it() {
        use crate::git::remotes::{Remote, RemoteBranch};
        let mut state =
            AppState::new(Context::Workspace).with_location(Some("/r".into()), "/r".into());
        state.branches.remotes.loaded(Ok(vec![Remote {
            name: "origin".into(),
            url: Some("/srv".into()),
            configured: true,
            branches: vec![RemoteBranch {
                remote: "origin".into(),
                short: b"main".to_vec(),
                refname: b"refs/remotes/origin/main".to_vec(),
            }],
        }]));
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
        assert_eq!(run(&state, plain('j')), AppIntent::RemoteNext);
        assert_eq!(
            run(&state, key(KeyCode::Up, KeyModifiers::NONE)),
            AppIntent::RemotePrev
        );
        assert_eq!(run(&state, plain('a')), AppIntent::AddRemote);
        assert_eq!(run(&state, shift('R')), AppIntent::RenameRemote);
        assert_eq!(run(&state, plain('d')), AppIntent::RemoveRemote);
        assert_eq!(run(&state, plain('[')), AppIntent::PrevBranchesTab);
        assert_eq!(run(&state, plain(']')), AppIntent::SwitchBranchesTab);
        assert_eq!(run(&state, plain('r')), AppIntent::RefreshStatus);
        assert_eq!(run(&state, plain('f')), AppIntent::Fetch);
        // A header row is a remote, not a branch to check out.
        assert!(matches!(
            resolve_in(&state, plain(' ')),
            Resolution::Disabled(ref reason) if reason == "Select a remote branch"
        ));
        state.branches.remotes.move_by(1);
        assert_eq!(run(&state, plain(' ')), AppIntent::TrackRemoteBranch);
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
        assert_eq!(branches(plain('[')), Some(AppIntent::PrevBranchesTab));
        assert_eq!(branches(plain(']')), Some(AppIntent::SwitchBranchesTab));
        // No branch is selected, so both are refused.
        assert_eq!(branches(plain('u')), None);
        assert_eq!(branches(shift('U')), None);
    }

    fn tag_ref(name: &str) -> crate::git::tags::TagRef {
        let oid = "1".repeat(40);
        crate::git::tags::TagRef {
            name: name.into(),
            target: CommitId::parse(&oid).unwrap(),
            target_short: oid[..7].into(),
            ref_oid: oid,
            annotated: false,
            date: "d".into(),
            subject: "s".into(),
        }
    }

    fn remote_named(name: &str, configured: bool) -> crate::git::remotes::Remote {
        crate::git::remotes::Remote {
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

        state.branches.tags.loaded(Ok(vec![tag_ref("v1")]));
        let run = |state: &AppState, key| match resolve_in(state, key) {
            Resolution::Run(binding) => binding.intent.clone(),
            other => panic!("{key:?}: {other:?}"),
        };
        assert_eq!(run(&state, plain('j')), AppIntent::TagNext);
        assert_eq!(
            run(&state, key(KeyCode::Down, KeyModifiers::NONE)),
            AppIntent::TagNext
        );
        assert_eq!(run(&state, plain('k')), AppIntent::TagPrev);
        assert_eq!(
            run(&state, key(KeyCode::Up, KeyModifiers::NONE)),
            AppIntent::TagPrev
        );
        assert_eq!(run(&state, plain('d')), AppIntent::DeleteTag);
        assert_eq!(run(&state, plain(']')), AppIntent::SwitchBranchesTab);
        assert_eq!(run(&state, plain('[')), AppIntent::PrevBranchesTab);
        assert_eq!(run(&state, plain('f')), AppIntent::Fetch);
        assert_eq!(KeyScope::Tags.parent(), Some(KeyScope::Workspace));

        // `P` is the tag's push here, never the branch's.
        assert_eq!(disabled(&state, shift_p), "No remote");
        state.branches.remotes.loaded(Ok(vec![
            remote_named("fork", true),
            remote_named("origin", true),
            remote_named("stray", false),
        ]));
        assert_eq!(
            disabled(&state, shift_p),
            "Several remotes and no upstream to choose from"
        );
        state.branches.loaded(Ok(vec![crate::git::branches::Branch {
            name: "main".into(),
            head: true,
            upstream: Some("origin/main".into()),
            track: crate::git::branches::Track::None,
            merged: true,
        }]));
        assert_eq!(run(&state, shift_p), AppIntent::PushTag);
        assert_eq!(push_tag_remote(&state), Ok("origin".into()));
        state.remote = Some(crate::remote_run::RemoteRun::new(
            crate::git::remote::RemoteOp::Fetch,
            1,
            None,
        ));
        assert_eq!(disabled(&state, shift_p), REMOTE_BUSY);
    }

    #[test]
    fn a_tag_goes_to_the_upstreams_remote_or_the_only_one() {
        let mut state = AppState::new(Context::Workspace);
        let branch = |upstream: Option<&str>| crate::git::branches::Branch {
            name: "main".into(),
            head: true,
            upstream: upstream.map(Into::into),
            track: crate::git::branches::Track::None,
            merged: true,
        };
        state.branches.remotes.loaded(Ok(vec![
            remote_named("a", true),
            remote_named("a/b", true),
            remote_named("only-group", false),
        ]));
        // A name with `/` owns the upstream it is the longest prefix of.
        state.branches.loaded(Ok(vec![branch(Some("a/b/main"))]));
        assert_eq!(push_tag_remote(&state), Ok("a/b".into()));
        state.branches.loaded(Ok(vec![branch(Some("a/main"))]));
        assert_eq!(push_tag_remote(&state), Ok("a".into()));
        // An upstream no remote owns, such as a local branch, chooses none.
        state.branches.loaded(Ok(vec![branch(Some("main2"))]));
        assert!(push_tag_remote(&state).is_err());
        state
            .branches
            .remotes
            .loaded(Ok(vec![remote_named("solo", true)]));
        assert_eq!(push_tag_remote(&state), Ok("solo".into()));
    }

    #[test]
    fn t_and_shift_t_tag_the_selected_commit_and_need_a_usable_id() {
        let row = |oid: &str| crate::git::log::CommitRow {
            oid: oid.into(),
            short_oid: "abc".into(),
            parents: vec![],
            author: "Ann".into(),
            time: 0,
            subject: "s".into(),
            subject_altered: false,
        };
        let mut state = AppState::new(Context::Workspace);
        let resolve_in = |state: &AppState, key| {
            resolve(
                registry(),
                &Keymap::default(),
                KeyScope::Commits,
                state,
                key,
            )
        };
        let shift_t = key(KeyCode::Char('T'), KeyModifiers::SHIFT);
        for key in [plain('t'), shift_t] {
            assert!(matches!(
                resolve_in(&state, key),
                Resolution::Disabled(ref reason) if reason == "No commit"
            ));
        }
        state.commits.rows = vec![row("not hex")];
        assert_eq!(tag_commit_disabled(&state), Some("No commit".into()));
        state.commits.rows = vec![row(&"a".repeat(40))];
        // An operation running does not refuse the prompt; the answer waits.
        state.op_seq = Some(1);
        let ran = |key| match resolve_in(&state, key) {
            Resolution::Run(binding) => binding.intent.clone(),
            other => panic!("{key:?}: {other:?}"),
        };
        assert_eq!(ran(plain('t')), AppIntent::NewTag);
        assert_eq!(ran(shift_t), AppIntent::NewAnnotatedTag);
        assert!(!REPO_LOCKED.contains(&"commits.tag"));
        assert!(!REPO_LOCKED.contains(&"commits.tag_annotated"));
    }

    #[test]
    fn the_delete_tag_question_answers_only_to_y_n_and_esc() {
        let mut state = AppState::new(Context::Workspace);
        state.popups = vec![Popup::DeleteTag(crate::popup::DeleteTagConfirm {
            name: "v1".into(),
            op: crate::git::ops::GitOp::DeleteTag {
                name: "v1".into(),
                oid: "1".repeat(40),
            },
        })];
        let resolve_in = |key| {
            resolve(
                registry(),
                &Keymap::default(),
                KeyScope::DeleteTag,
                &state,
                key,
            )
        };
        let ran = |key| match resolve_in(key) {
            Resolution::Run(binding) => binding.intent.clone(),
            other => panic!("{key:?}: {other:?}"),
        };
        assert_eq!(ran(plain('y')), AppIntent::ConfirmDeleteTag);
        assert_eq!(ran(plain('n')), AppIntent::ConfirmNo);
        assert_eq!(
            ran(key(KeyCode::Esc, KeyModifiers::NONE)),
            AppIntent::ConfirmNo
        );
        assert_eq!(
            ran(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            AppIntent::Quit
        );
        for unbound in [key(KeyCode::Enter, KeyModifiers::NONE), plain('q')] {
            assert!(
                matches!(resolve_in(unbound), Resolution::Unbound),
                "{unbound:?}"
            );
        }
        assert_eq!(KeyScope::DeleteTag.parent(), None);
        // No binding in the scope has Enter among its keys.
        let enter = KeyPattern::any(KeyCode::Enter);
        for b in registry().iter().filter(|b| b.scope == KeyScope::DeleteTag) {
            assert!(!b.keys.iter().any(|k| k.code == enter.code), "{}", b.id);
        }
        for id in [
            "tags.delete",
            "tags.push",
            "delete_tag.yes",
            "delete_tag.no",
            "delete_tag.quit",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
            assert!(build(&[(id, &["F"])], Trust::Repo).is_err(), "{id}");
        }
        assert!(build(&[("tags.next", &["F"])], Trust::Repo).is_ok());
    }

    #[test]
    fn the_remove_remote_question_answers_only_to_y_n_and_esc() {
        let mut state = AppState::new(Context::Workspace);
        state.popups = vec![Popup::RemoveRemote(crate::popup::RemoveRemoteConfirm {
            name: "fork".into(),
            op: crate::git::ops::GitOp::RemoveRemote("fork".into()),
        })];
        let resolve_in = |key| {
            resolve(
                registry(),
                &Keymap::default(),
                KeyScope::RemoveRemote,
                &state,
                key,
            )
        };
        let ran = |key| match resolve_in(key) {
            Resolution::Run(binding) => binding.intent.clone(),
            other => panic!("{key:?}: {other:?}"),
        };
        assert_eq!(ran(plain('y')), AppIntent::ConfirmRemoveRemote);
        assert_eq!(ran(plain('n')), AppIntent::ConfirmNo);
        assert_eq!(
            ran(key(KeyCode::Esc, KeyModifiers::NONE)),
            AppIntent::ConfirmNo
        );
        assert_eq!(
            ran(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            AppIntent::Quit
        );
        for unbound in [key(KeyCode::Enter, KeyModifiers::NONE), plain('q')] {
            assert!(
                matches!(resolve_in(unbound), Resolution::Unbound),
                "{unbound:?}"
            );
        }
        assert_eq!(KeyScope::RemoveRemote.parent(), None);
        for id in [
            "remotes.remove",
            "remove_remote.yes",
            "remove_remote.no",
            "remove_remote.quit",
        ] {
            assert!(build(&[(id, &["F"])], Trust::Repo).is_err(), "{id}");
        }
        assert!(build(&[("remotes.add", &["F"])], Trust::Repo).is_ok());
    }

    #[test]
    fn the_commits_panel_checks_out_reverts_and_resets_the_selected_commit() {
        use AppIntent::*;
        let commits = |k| from(KeyScope::Commits, k);
        assert_eq!(commits(plain(' ')), Some(CheckoutCommit));
        assert_eq!(
            commits(key(KeyCode::Char('R'), KeyModifiers::SHIFT)),
            Some(RevertCommit)
        );
        assert_eq!(
            commits(key(KeyCode::Char('X'), KeyModifiers::SHIFT)),
            Some(OpenMenu(MenuId::Reset))
        );
    }

    #[test]
    fn commit_actions_say_why_they_cannot_run() {
        let row = |parents: &[&str]| crate::git::log::CommitRow {
            oid: "abc".into(),
            short_oid: "abc".into(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            author: "Ann".into(),
            time: 0,
            subject: "s".into(),
            subject_altered: false,
        };
        let mut state = AppState::new(Context::Workspace);
        for disabled in [checkout_commit_disabled, revert_disabled, reset_disabled] {
            assert_eq!(disabled(&state), Some("No commits".into()));
        }
        state.commits.rows = vec![row(&["p"])];
        for disabled in [checkout_commit_disabled, revert_disabled, reset_disabled] {
            assert_eq!(disabled(&state), None);
        }
        state.commits.rows = vec![row(&["p", "q"])];
        assert_eq!(
            revert_disabled(&state),
            Some("Cannot revert a merge commit".into())
        );
        assert_eq!(reset_disabled(&state), None);
        assert_eq!(checkout_commit_disabled(&state), None);

        state.commits.rows = vec![row(&["p"])];
        state.branch = Some(crate::git::status::BranchStatus {
            head: Head::Branch("main".into()),
            upstream: None,
            ahead_behind: None,
            in_progress: vec![Operation::Revert],
            bisect: None,
        });
        let busy = Some("Finish or abort the operation in progress");
        assert_eq!(revert_disabled(&state).as_deref(), busy);
        assert_eq!(reset_disabled(&state).as_deref(), busy);
        // Git refuses a checkout that would lose work; it may go ahead.
        assert_eq!(checkout_commit_disabled(&state), None);

        state.op_seq = Some(1);
        for disabled in [checkout_commit_disabled, revert_disabled, reset_disabled] {
            assert_eq!(disabled(&state), Some(OP_RUNNING.into()));
        }
    }

    #[test]
    fn the_commits_panel_squashes_fixes_up_drops_and_rewords() {
        use AppIntent::RewriteCommit;
        let ids = ['a', 'b', 'c'].map(|d| d.to_string().repeat(40));
        let mut state = AppState::new(Context::Workspace);
        state.commits.rows = (0..2)
            .map(|i| crate::git::log::CommitRow {
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
        assert_eq!(on(&state, plain('s')), Some(RewriteCommit(Edit::Squash)));
        assert_eq!(
            on(&state, key(KeyCode::Char('F'), KeyModifiers::SHIFT)),
            Some(RewriteCommit(Edit::Fixup))
        );
        assert_eq!(on(&state, plain('d')), Some(RewriteCommit(Edit::Drop)));
        assert_eq!(on(&state, plain('e')), Some(RewriteCommit(Edit::Reword)));
        assert_eq!(
            on(&state, key(KeyCode::Char('E'), KeyModifiers::SHIFT)),
            Some(RewriteCommit(Edit::Stop))
        );
        assert_eq!(on(&state, plain('b')), Some(AppIntent::InsertBreak));
        let alt = KeyModifiers::ALT;
        let (up, down) = (
            Some(AppIntent::MoveCommit(Direction::Up)),
            Some(AppIntent::MoveCommit(Direction::Down)),
        );
        state.commits.selected = 1;
        assert_eq!(on(&state, key(KeyCode::Char('k'), alt)), up);
        assert_eq!(on(&state, key(KeyCode::Up, alt)), up);
        state.commits.selected = 0;
        assert_eq!(on(&state, key(KeyCode::Char('j'), alt)), down);
        assert_eq!(on(&state, key(KeyCode::Down, alt)), down);
        // Plain arrows still move the cursor.
        assert_eq!(
            on(&state, key(KeyCode::Down, KeyModifiers::NONE)),
            Some(AppIntent::CommitNext)
        );
        assert_eq!(
            on(&state, key(KeyCode::Up, KeyModifiers::NONE)),
            Some(AppIntent::CommitPrev)
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
    fn rewrites_say_why_they_cannot_run() {
        let ids = ['a', 'b', 'c'].map(|d| d.to_string().repeat(40));
        let row = |i: usize, parents: Vec<String>| crate::git::log::CommitRow {
            oid: ids[i].clone(),
            short_oid: "x".into(),
            parents,
            author: "Ann".into(),
            time: 0,
            subject: "s".into(),
            subject_altered: false,
        };
        let all = [
            squash_disabled,
            fixup_disabled,
            drop_disabled,
            reword_disabled,
            stop_disabled,
        ];
        let mut state = AppState::new(Context::Workspace);
        state.commits.tips = Some(vec![]);
        for disabled in all {
            assert_eq!(disabled(&state), Some("No commits".into()));
        }
        // `a` on `b`, the root.
        state.commits.rows = vec![row(0, vec![ids[1].clone()]), row(1, vec![])];
        assert_eq!(drop_disabled(&state), None);
        assert_eq!(reword_disabled(&state), None);
        let below_is_root = Some("The commit below is the first commit");
        assert_eq!(squash_disabled(&state).as_deref(), below_is_root);
        assert_eq!(fixup_disabled(&state).as_deref(), below_is_root);
        state.commits.selected = 1;
        for disabled in all {
            assert_eq!(
                disabled(&state),
                Some("Cannot rewrite the first commit".into())
            );
        }

        state.commits.selected = 0;
        state.commits.rows[1].parents = vec![ids[2].clone()];
        for disabled in all {
            assert_eq!(disabled(&state), None);
        }
        state.branch = Some(crate::git::status::BranchStatus {
            head: Head::Branch("main".into()),
            upstream: None,
            ahead_behind: None,
            in_progress: vec![Operation::Rebase],
            bisect: None,
        });
        for disabled in all {
            assert_eq!(
                disabled(&state),
                Some("Finish or abort the operation in progress".into())
            );
        }
        state.op_seq = Some(1);
        for disabled in all {
            assert_eq!(disabled(&state), Some(OP_RUNNING.into()));
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
        assert_eq!(resolve_in(plain('y')), Some(AppIntent::ConfirmResetHard));
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
    fn the_leave_commits_question_has_no_enter_and_n_or_esc_stays() {
        let state = AppState::new(Context::Workspace);
        let resolve_in = |key| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::LeaveCommits,
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
        assert_eq!(resolve_in(plain('y')), Some(AppIntent::ConfirmLeaveCommits));
        assert_eq!(resolve_in(plain('n')), Some(AppIntent::ConfirmNo));
        assert_eq!(
            resolve_in(key(KeyCode::Esc, none)),
            Some(AppIntent::ConfirmNo)
        );
        assert_eq!(
            resolve_in(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(AppIntent::Quit)
        );
        assert_eq!(KeyScope::LeaveCommits.parent(), None);
        assert_eq!(KeyScope::LeaveCommits.title(), "Leave commits behind");

        let enter = key(KeyCode::Enter, none);
        for b in registry()
            .iter()
            .filter(|b| b.id.starts_with("leave_commits."))
        {
            assert!(!b.keys.iter().any(|k| k.matches(enter)), "{}", b.id);
        }
    }

    #[test]
    fn a_repository_layer_cannot_rebind_the_leave_commits_answers() {
        for id in [
            "leave_commits.yes",
            "leave_commits.no",
            "leave_commits.quit",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
            assert_eq!(
                build(&[(id, &["enter"])], Trust::Repo).unwrap_err(),
                format!("\"{id}\" can only be changed in the global config file")
            );
            assert!(build(&[(id, &["F"])], Trust::Repo).is_err(), "{id}");
        }
    }

    #[test]
    fn the_travel_question_has_no_enter_and_n_or_esc_leave_everything() {
        let state = AppState::new(Context::Workspace);
        let resolve_in = |key| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::Travel,
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
        assert_eq!(resolve_in(plain('y')), Some(AppIntent::ConfirmTravel));
        assert_eq!(resolve_in(plain('n')), Some(AppIntent::ConfirmNo));
        assert_eq!(
            resolve_in(key(KeyCode::Esc, none)),
            Some(AppIntent::ConfirmNo)
        );
        assert_eq!(
            resolve_in(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(AppIntent::Quit)
        );
        assert_eq!(KeyScope::Travel.parent(), None);
        assert_eq!(KeyScope::Travel.title(), "Undo or redo");
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
        state.branches.loaded(Ok(vec![crate::git::branches::Branch {
            name: "topic".into(),
            head: false,
            upstream: None,
            track: crate::git::branches::Track::None,
            merged: false,
        }]));
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
            Some(AppIntent::ConfirmCreatePullRequest)
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
            Some(AppIntent::PickSelect)
        );
        assert_eq!(resolve_in(pick, plain('j')), Some(AppIntent::PickNext));
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
            assert_eq!(run(ctrl('z')), Some(AppIntent::Undo), "{scope:?}");
            assert_eq!(run(ctrl('r')), Some(AppIntent::Redo), "{scope:?}");
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
    fn brackets_switch_the_commits_panel_between_log_and_reflog() {
        use AppIntent::*;
        for c in ['[', ']'] {
            assert_eq!(from(KeyScope::Commits, plain(c)), Some(SwitchCommitsTab));
            assert_eq!(from(KeyScope::Reflog, plain(c)), Some(SwitchCommitsTab));
        }
        let none = KeyModifiers::NONE;
        assert_eq!(from(KeyScope::Reflog, plain('j')), Some(ReflogNext));
        assert_eq!(
            from(KeyScope::Reflog, key(KeyCode::Down, none)),
            Some(ReflogNext)
        );
        assert_eq!(from(KeyScope::Reflog, plain('k')), Some(ReflogPrev));
        assert_eq!(
            from(KeyScope::Reflog, key(KeyCode::Up, none)),
            Some(ReflogPrev)
        );
        // The log's actions do not reach the reflog's rows.
        for c in [' ', 's', 'd', 'e'] {
            assert_eq!(from(KeyScope::Reflog, plain(c)), None, "{c:?}");
        }
        assert_eq!(KeyScope::Reflog.parent(), Some(KeyScope::Workspace));
        assert_eq!(KeyScope::Reflog.title(), "Reflog");
    }

    #[test]
    fn undo_waits_for_a_repository_an_op_and_any_operation_in_progress() {
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(undo_disabled(&state), Some("Not in a repository".into()));
        state.repo = Some("/repo".into());
        assert_eq!(undo_disabled(&state), None);
        state.branch = Some(crate::git::status::BranchStatus {
            head: Head::Branch("main".into()),
            upstream: None,
            ahead_behind: None,
            in_progress: vec![Operation::Bisect],
            bisect: None,
        });
        assert_eq!(
            undo_disabled(&state),
            Some("Finish or abort the operation in progress".into())
        );
        state.op_seq = Some(1);
        assert_eq!(undo_disabled(&state), Some(OP_RUNNING.into()));
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
    fn c_v_and_shift_c_copy_paste_and_clear_in_the_commits_panel() {
        use AppIntent::*;
        // Everything enabled: a commit to copy, one copied, a clean branch.
        let mut state = AppState::new(Context::Workspace);
        let oid = "a".repeat(40);
        state.commits.rows = vec![crate::git::log::CommitRow {
            oid: oid.clone(),
            short_oid: "aaaaaaa".into(),
            parents: vec![],
            author: "Ann".into(),
            time: 0,
            subject: "s".into(),
            subject_altered: false,
        }];
        state.copied.toggle(crate::copied::CopiedCommit {
            id: CommitId::parse(&oid).unwrap(),
            short: "aaaaaaa".into(),
            subject: "s".into(),
        });
        state.branch = Some(crate::git::status::BranchStatus {
            head: Head::Branch("main".into()),
            upstream: None,
            ahead_behind: None,
            in_progress: vec![],
            bisect: None,
        });
        let run = |k| match resolve(registry(), &Keymap::default(), KeyScope::Commits, &state, k) {
            Resolution::Run(binding) => binding.intent.clone(),
            other => panic!("{k:?}: {other:?}"),
        };
        assert_eq!(run(plain('c')), CopyCommit);
        assert_eq!(run(plain('v')), PasteCommits);
        assert_eq!(
            run(key(KeyCode::Char('C'), KeyModifiers::SHIFT)),
            ClearCopied
        );
        assert!(binding("commits.copy").bar && binding("commits.paste").bar);
        assert!(!binding("commits.clear_copied").bar);
    }

    #[test]
    fn the_copy_keys_clash_with_nothing_the_commits_panel_reaches() {
        let reached = [KeyScope::Commits, KeyScope::Workspace, KeyScope::Global];
        for id in ["commits.copy", "commits.paste", "commits.clear_copied"] {
            let new = binding(id);
            for other in registry()
                .iter()
                .filter(|b| b.id != id && reached.contains(&b.scope))
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
    fn the_log_query_and_diff_mark_keys_clash_with_nothing_their_scope_reaches() {
        use KeyScope::*;
        let new: &[(&str, &[KeyScope])] = &[
            ("commits.filter_path", &[Commits, Workspace, Global]),
            ("commits.filter_author", &[Commits, Workspace, Global]),
            ("commits.mark", &[Commits, Workspace, Global]),
            ("reflog.filter_path", &[Reflog, Workspace, Global]),
            ("reflog.filter_author", &[Reflog, Workspace, Global]),
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
        state.commits.rows = (0..2)
            .map(|i| crate::git::log::CommitRow {
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
        state.commits.tab = crate::commits::CommitsTab::Reflog;
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
    fn a_range_keeps_the_new_commits_keys_from_running() {
        let mut state = AppState::new(Context::Workspace);
        state.focus = Focus::Commits;
        state.commits.rows = vec![crate::git::log::CommitRow {
            oid: "a".repeat(40),
            short_oid: "a".into(),
            parents: vec![],
            author: "Ann".into(),
            time: 0,
            subject: "s".into(),
            subject_altered: false,
        }];
        assert_eq!(log_filter_disabled(&state), None);
        assert_eq!(commits_mark_disabled(&state), None);
        state.commits.range_anchor = Some("a".repeat(40));
        assert_eq!(
            log_filter_disabled(&state).as_deref(),
            Some(CLEAR_RANGE_FIRST)
        );
        assert_eq!(
            commits_mark_disabled(&state).as_deref(),
            Some(CLEAR_RANGE_FIRST)
        );
    }

    #[test]
    fn the_command_log_key_is_typed_into_the_input_and_needs_none_of_the_new_keys() {
        let state = AppState::new(crate::model::Context::Workspace);
        for c in ['@', 'q', '?', '/', 'V', 'n', 'N'] {
            assert!(
                matches!(
                    resolve(
                        registry(),
                        &Keymap::default(),
                        KeyScope::Input,
                        &state,
                        plain(c)
                    ),
                    Resolution::Unbound
                ),
                "{c} is typed"
            );
        }
        let log = registry()
            .iter()
            .filter(|b| b.scope == KeyScope::CommandLog);
        for b in log {
            for c in ['/', 'V', 'n', 'N'] {
                assert!(
                    !b.keys.iter().any(|k| k.matches(plain(c))),
                    "{} takes {c}",
                    b.id
                );
            }
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
    fn paste_is_locked_and_copy_and_clear_are_not() {
        assert!(REPO_LOCKED.contains(&"commits.paste"));
        assert!(build(&[("commits.paste", &["Z"])], Trust::Repo).is_err());
        for id in ["commits.copy", "commits.clear_copied"] {
            assert!(!REPO_LOCKED.contains(&id), "{id}");
            assert!(build(&[(id, &["Z"])], Trust::Repo).is_ok(), "{id}");
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
        assert_eq!(from(KeyScope::Files, plain('j')), Some(SelectNext));
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
        let reason = |state: &AppState, c| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::Files,
            state,
            plain(c),
        ) {
            Resolution::Disabled(reason) => Some(reason),
            Resolution::Run(_) => None,
            Resolution::Unbound | Resolution::Command(_) => panic!("{c} is unbound"),
        };
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(reason(&state, '+'), None);
        assert_eq!(reason(&state, '-'), None);
        state.diff_options.context = MAX_CONTEXT;
        assert_eq!(
            reason(&state, '+'),
            Some("Already at the most context".into())
        );
        assert_eq!(
            reason(&state, '='),
            Some("Already at the most context".into())
        );
        assert_eq!(reason(&state, '-'), None);
        state.diff_options.context = MIN_CONTEXT;
        assert_eq!(
            reason(&state, '-'),
            Some("Already at the least context".into())
        );
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
        let reason = |state: &AppState, scope, c| match resolve(
            registry(),
            &Keymap::default(),
            scope,
            state,
            plain(c),
        ) {
            Resolution::Disabled(reason) => Some(reason),
            Resolution::Run(_) => None,
            Resolution::Unbound | Resolution::Command(_) => panic!("{c} is unbound in {scope:?}"),
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
                Some(WHITESPACE_HIDDEN.into()),
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
                Some(WHITESPACE_HIDDEN.into()),
                "{c:?}"
            );
        }
        state.diff_options.ignore_whitespace = false;
        assert_eq!(reason(&state, KeyScope::Lines, ' '), None);
    }

    #[test]
    fn a_diff_tool_view_disables_the_patch_keys_in_the_diff_only() {
        use crate::external_diff::{ExternalRenderer, RendererKind};
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
        state.renderers = vec![ExternalRenderer {
            label: "difft".into(),
            argv: vec!["difft".into()],
            kind: RendererKind::Tool,
        }];
        state.renderer = Some(0);
        let reason = |state: &AppState, scope, c| match resolve(
            registry(),
            &Keymap::default(),
            scope,
            state,
            plain(c),
        ) {
            Resolution::Disabled(reason) => Some(reason),
            Resolution::Run(_) => None,
            Resolution::Unbound | Resolution::Command(_) => panic!("{c} is unbound in {scope:?}"),
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
                Some(DISPLAY_ONLY.into()),
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
                Some(DISPLAY_ONLY.into()),
                "{c:?}"
            );
        }
        state.renderer = None;
        assert_eq!(reason(&state, KeyScope::Lines, ' '), None);
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
            KeyScope::ResetHard => "reset_hard",
            KeyScope::LeaveCommits => "leave_commits",
            KeyScope::DropStash => "drop_stash",
            KeyScope::AbortConfirm => "abort_confirm",
            KeyScope::AutosquashConfirm => "autosquash_confirm",
            KeyScope::PatchConfirm => "patch_confirm",
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

    fn layer(entries: &[(&str, &[&str])]) -> KeysTable {
        entries
            .iter()
            .map(|(id, keys)| (id.to_string(), keys.iter().map(|k| k.to_string()).collect()))
            .collect()
    }

    fn build(entries: &[(&str, &[&str])], trust: Trust) -> Result<Keymap, String> {
        Keymap::build(&Keymap::default(), &layer(entries), trust)
    }

    fn binding(id: &str) -> &'static Binding {
        registry().iter().find(|b| b.id == id).expect("known id")
    }

    #[test]
    fn a_remap_replaces_the_keys_and_an_empty_list_disables() {
        let keymap = build(
            &[("files.stage", &["x", "ctrl-x"]), ("files.discard", &[])],
            Trust::Global,
        )
        .unwrap();
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
        .unwrap();
        let merged =
            Keymap::build(&global, &layer(&[("files.stage", &["y"])]), Trust::Repo).unwrap();
        assert_eq!(merged.keys(binding("files.stage")), [char_pattern('y')]);
        assert_eq!(merged.keys(binding("files.commit")), [char_pattern('C')]);
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
            err(&[("files.stage", &["c"])]),
            r#"key "c" is bound to both "files.stage" and "files.commit""#
        );
        assert_eq!(
            err(&[("files.stage", &["x", "x"])]),
            r#"key "x" is bound to both "files.stage" and "files.stage""#
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
        for b in registry() {
            let risky = matches!(
                b.intent,
                Quit | InterruptCommit
                    | OpenInEditor
                    | EditHunk
                    | CycleRenderer
                    | EditCommitExternally
                    | InitSubmodule
                    | UpdateSubmodule
                    | Discard
                    | DiscardCommit
                    | ConfirmYes
                    | ConfirmNo
                    | InitRepo
                    | RecentSelect
                    | DeleteBranch
                    | ConfirmDeleteBranch { .. }
                    | Fetch
                    | Pull
                    | Push
                    | ForcePush
                    | SubmitCredential
                    | CredentialAnswered(_)
                    | OpenMenu(MenuId::Merge)
                    | ContinueOperation
                    | SkipOperation
                    | AbortOperation
                    | PickConflict(_)
                    | OpenMergeTool
                    | RevertCommit
                    | PasteCommits
                    | OpenMenu(MenuId::Reset)
                    | ConfirmResetHard
                    | ConfirmLeaveCommits
                    | OpenMenu(MenuId::Stash)
                    | OpenStashFiles
                    | ApplyStash
                    | PopStash
                    | DropStash
                    | RenameStash
                    | ConfirmDropStash
                    | RemoveRemote
                    | ConfirmRemoveRemote
                    | DeleteTag
                    | ConfirmDeleteTag
                    | PushTag
                    | RewriteCommit(_)
                    | MoveCommit(_)
                    | InsertBreak
                    | CreateFixup
                    | Autosquash
                    | BisectHead(_)
                    | OpenMenu(MenuId::Bisect)
                    | OpenWorktree
                    | AddWorktree
                    | RemoveWorktree
                    | ConfirmRemoveWorktree
                    | Undo
                    | Redo
                    | ConfirmTravel
                    | ListEscape
                    | OpenMenu(MenuId::PullRequest)
                    | ConfirmCreatePullRequest
                    | OpenPatchMenu
            );
            assert_eq!(risky, REPO_LOCKED.contains(&b.id), "{}", b.id);
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

    #[test]
    fn shift_o_opens_the_recent_list_from_the_workspace_panels() {
        let o = key(KeyCode::Char('O'), KeyModifiers::SHIFT);
        for scope in [
            KeyScope::Files,
            KeyScope::Diff,
            KeyScope::Lines,
            KeyScope::Blame,
        ] {
            assert_eq!(from(scope, o), Some(AppIntent::OpenRecent), "{scope:?}");
        }
    }

    #[test]
    fn the_recent_list_has_its_own_keys() {
        use AppIntent::*;
        let recent = |k| from(KeyScope::Recent, k);
        assert_eq!(recent(plain('j')), Some(RecentNext));
        assert_eq!(
            recent(key(KeyCode::Down, KeyModifiers::NONE)),
            Some(RecentNext)
        );
        assert_eq!(recent(plain('k')), Some(RecentPrev));
        assert_eq!(
            recent(key(KeyCode::Up, KeyModifiers::NONE)),
            Some(RecentPrev)
        );
        assert_eq!(
            recent(key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(RecentSelect)
        );
        assert_eq!(
            recent(key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(PopPopup)
        );
        assert_eq!(recent(plain('q')), Some(PopPopup));
        assert_eq!(
            recent(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Quit)
        );
        // `i` only where the list offers it.
        let state = AppState::new(Context::Workspace);
        assert!(matches!(
            resolve(
                registry(),
                &Keymap::default(),
                KeyScope::Recent,
                &state,
                plain('i')
            ),
            Resolution::Disabled(ref reason) if reason == "Already in a repository"
        ));
    }

    #[test]
    fn ctrl_x_cancels_a_running_command_from_every_command_panel() {
        let ctrl_x = key(KeyCode::Char('x'), KeyModifiers::CONTROL);
        let mut state = AppState::new(Context::Workspace);
        for scope in [KeyScope::Files, KeyScope::Diff, KeyScope::Blame] {
            let at =
                |state: &AppState| resolve(registry(), &Keymap::default(), scope, state, ctrl_x);
            state.command_running = false;
            assert!(
                matches!(at(&state), Resolution::Disabled(ref reason) if reason == "No command is running"),
                "{scope:?}"
            );
            state.command_running = true;
            assert!(
                matches!(at(&state), Resolution::Run(b) if b.intent == AppIntent::CancelCommand),
                "{scope:?}"
            );
        }
    }

    fn commands(tables: &[CommandTable]) -> Result<Keymap, String> {
        Keymap::default().with_commands(tables)
    }

    fn files_command(keys: &[&str]) -> CommandTable {
        test_table(Panel::Files, keys, &["true"], &[], OutputMode::None)
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
        assert!(moved.with_commands(&[files_command(&["c"])]).is_ok());
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
    fn a_repository_layer_keeps_the_commands_and_cannot_move_a_key_onto_one() {
        let global = commands(&[files_command(&["X"])]).unwrap();
        let repo = Keymap::build(&global, &layer(&[("files.commit", &["C"])]), Trust::Repo);
        assert_eq!(repo.unwrap().commands, global.commands);
        assert_eq!(
            Keymap::build(&global, &layer(&[("files.stage", &["X"])]), Trust::Repo).unwrap_err(),
            r#"key "X" is bound to both "files.stage" and command 1"#
        );
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
        state.branch = Some(crate::git::status::BranchStatus {
            head: Head::Detached("abc".into()),
            upstream: None,
            ahead_behind: None,
            in_progress: vec![Operation::Bisect],
            bisect: Some(crate::git::bisect::BisectState::default()),
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
        assert_eq!(status('g'), Some(BisectHead(BisectMark::Good)));
        assert_eq!(status('b'), Some(BisectHead(BisectMark::Bad)));
        assert_eq!(status('s'), Some(SkipOperation));
        assert_eq!(status('a'), Some(AbortOperation));
    }

    #[test]
    fn the_bisect_keys_clash_with_nothing_they_could_hide_or_be_hidden_by() {
        // Commits and Status fall back to Workspace, then Global; neither
        // has a scope below it.
        let ids = ["commits.bisect", "status.bisect_good", "status.bisect_bad"];
        for id in ids {
            let new = registry().iter().find(|b| b.id == id).unwrap();
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
    fn the_bisect_menu_refuses_as_the_marks_do() {
        use crate::bisect::{Target, mark_disabled, reset_disabled};
        use crate::git::bisect::{BisectState, Terms};
        use crate::git::status::BranchStatus;

        let branch = |head: Head, ops: &[Operation], bisect: Option<BisectState>| {
            Some(BranchStatus {
                head,
                upstream: None,
                ahead_behind: None,
                in_progress: ops.to_vec(),
                bisect,
            })
        };
        let custom = BisectState {
            terms: Terms {
                bad: "new".into(),
                good: "old".into(),
            },
            ..BisectState::default()
        };
        let mut states = Vec::new();
        for (head, ops, bisect) in [
            (Head::Branch("main".into()), vec![], None),
            (Head::Unborn("main".into()), vec![], None),
            (Head::Branch("main".into()), vec![Operation::Merge], None),
            (
                Head::Detached("abc".into()),
                vec![Operation::Bisect],
                Some(BisectState::default()),
            ),
            (
                Head::Detached("abc".into()),
                vec![Operation::Bisect],
                Some(custom),
            ),
        ] {
            let mut state = AppState::new(Context::Workspace);
            state.commits.rows = vec![crate::git::log::CommitRow {
                oid: "abc".into(),
                short_oid: "abc".into(),
                parents: vec![],
                author: "Ann".into(),
                time: 0,
                subject: "s".into(),
                subject_altered: false,
            }];
            state.branch = branch(head, &ops, bisect);
            states.push(state.clone());
            state.op_seq = Some(1);
            states.push(state.clone());
            state.op_seq = None;
            state.files = vec![crate::git::FileEntry {
                path: "a.rs".into(),
                orig_path: None,
                index: 'M',
                worktree: '.',
                kind: EntryKind::Changed,
            }];
            states.push(state.clone());
            state.commits.rows.clear();
            states.push(state);
        }
        let items = crate::menu::items(MenuId::Bisect);
        assert_eq!(items.len(), 4);
        for state in &states {
            for item in items {
                let expected = match item.intent {
                    AppIntent::BisectSelected(mark) => mark_disabled(state, Target::Selected, mark),
                    AppIntent::Ask(_) => reset_disabled(state),
                    ref other => panic!("unexpected item {other:?}"),
                };
                assert_eq!((item.disabled)(state), expected, "{}", item.label);
            }
        }
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
        let file = |kind, index, worktree| crate::git::FileEntry {
            path: "a.log".into(),
            orig_path: None,
            index,
            worktree,
            kind,
        };
        state.files = vec![file(EntryKind::Changed, '.', 'M')];
        state.selected = Some(0);
        assert_eq!(
            resolve_i(&state),
            Err("Only untracked files can be ignored".into())
        );
        state.files = vec![file(EntryKind::Untracked, '?', '?')];
        assert_eq!(resolve_i(&state), Ok(AppIntent::OpenIgnore));
        for id in ["commits.copy_menu", "files.ignore"] {
            assert!(!REPO_LOCKED.contains(&id), "{id}");
        }
    }

    /// HEAD `fixup! A` (`d…`), then `B` (`b…`), then `A` (`a…`) on `9…`,
    /// not loaded; `a.txt` staged and the Commits panel focused on `HEAD`.
    fn fixup_state() -> AppState {
        let row = |digit: &str, parent: &str, subject: &str| crate::git::log::CommitRow {
            oid: digit.repeat(40),
            short_oid: digit.repeat(7),
            parents: vec![parent.repeat(40)],
            author: "Ann".into(),
            time: 0,
            subject: subject.into(),
            subject_altered: false,
        };
        let mut state = AppState::new(Context::Workspace);
        state.commits.rows = vec![
            row("d", "b", "fixup! A"),
            row("b", "a", "B"),
            row("a", "9", "A"),
        ];
        state.commits.tips = Some(vec![]);
        state.focus = Focus::Commits;
        state.files = vec![crate::git::FileEntry {
            path: crate::git::path::RepoPath::from("a.txt"),
            orig_path: None,
            index: 'M',
            worktree: '.',
            kind: EntryKind::Changed,
        }];
        state
    }

    fn in_rebase(state: &mut AppState) {
        state.branch = Some(crate::git::status::BranchStatus {
            head: Head::Branch("main".into()),
            upstream: None,
            ahead_behind: None,
            in_progress: vec![Operation::Rebase],
            bisect: None,
        });
    }

    fn tip(digit: &str, name: &str) -> crate::git::branch_tips::Tip {
        (
            CommitId::parse(&digit.repeat(40)).unwrap(),
            format!("refs/heads/{name}").into_bytes(),
        )
    }

    #[test]
    fn a_fixup_needs_a_staged_change_and_one_shown_log_commit() {
        let state = fixup_state();
        assert_eq!(create_fixup_disabled(&state), None);
        assert_eq!(find_base_disabled(&state), None);

        // The target is named by id, so a narrowed log is fine.
        let mut narrowed = fixup_state();
        narrowed.commits.query.author = Some("ann".into());
        assert_eq!(create_fixup_disabled(&narrowed), None);

        let mut unstaged = fixup_state();
        unstaged.files[0].index = '.';
        unstaged.files[0].worktree = 'M';
        let mut untracked = fixup_state();
        untracked.files[0].index = '?';
        untracked.files[0].kind = EntryKind::Untracked;
        let mut clean = fixup_state();
        clean.files.clear();
        for state in [&unstaged, &untracked, &clean] {
            assert_eq!(create_fixup_disabled(state).as_deref(), Some(STAGE_THE_FIX));
            assert_eq!(find_base_disabled(state).as_deref(), Some(STAGE_THE_FIX));
        }

        let mut todo = fixup_state();
        todo.commits.todo_selected = Some(0);
        let mut hidden = fixup_state();
        hidden.commits.filter = "zzz".into();
        let mut range = fixup_state();
        range.commits.range_anchor = Some("b".repeat(40));
        let mut running = fixup_state();
        running.op_seq = Some(1);
        let mut unborn = fixup_state();
        unborn.commits.rows.clear();
        unborn.branch = Some(crate::git::status::BranchStatus {
            head: Head::Unborn("main".into()),
            upstream: None,
            ahead_behind: None,
            in_progress: vec![],
            bisect: None,
        });
        let cases = [
            (&todo, "Select a commit in the log first"),
            (&hidden, FILTER_HIDES_ROW),
            (&range, CLEAR_RANGE_FIRST),
            (&running, OP_RUNNING),
            (&unborn, "No commits"),
        ];
        for (state, why) in cases {
            assert_eq!(create_fixup_disabled(state).as_deref(), Some(why));
            assert_eq!(find_base_disabled(state).as_deref(), Some(why));
        }

        // Finding the base only reads, so a rebase in progress is fine.
        let mut rebasing = fixup_state();
        in_rebase(&mut rebasing);
        assert_eq!(
            create_fixup_disabled(&rebasing).as_deref(),
            Some("Finish or abort the operation in progress")
        );
        assert_eq!(find_base_disabled(&rebasing), None);
    }

    #[test]
    fn autosquash_plans_from_the_whole_log_and_says_why_not() {
        let state = fixup_state();
        assert_eq!(autosquash_disabled(&state), None);
        let plan = autosquash_plan(&state).unwrap();
        let actions: Vec<_> = plan
            .steps
            .iter()
            .map(|s| (s.action, &s.commit.as_str()[..1]))
            .collect();
        assert_eq!(
            actions,
            [
                (rebase_plan::TodoAction::Pick, "a"),
                (rebase_plan::TodoAction::Fixup, "d"),
                (rebase_plan::TodoAction::Pick, "b")
            ]
        );
        assert_eq!(plan.base.as_str(), "9".repeat(40));

        let mut narrowed = fixup_state();
        narrowed.commits.query.author = Some("ann".into());
        let mut filtered = fixup_state();
        filtered.commits.filter = "fixup".into();
        let mut todo = fixup_state();
        todo.commits.todo_selected = Some(0);
        let mut running = fixup_state();
        running.op_seq = Some(1);
        let mut rebasing = fixup_state();
        in_rebase(&mut rebasing);
        let mut none = fixup_state();
        none.commits.rows[0].subject = "D".into();
        let cases = [
            (&narrowed, CLEAR_LOG_FILTER),
            (&filtered, FILTER_HIDES_NEIGHBOUR),
            (&todo, "Select a commit in the log first"),
            (&running, OP_RUNNING),
            (&rebasing, "Finish or abort the operation in progress"),
            (&none, rebase_plan::NO_FIXUPS),
        ];
        for (state, why) in cases {
            assert_eq!(autosquash_disabled(state).as_deref(), Some(why));
        }
    }

    #[test]
    fn autosquash_and_stacked_branches() {
        // Without `--update-refs` any other branch in the range is left behind.
        let mut old_git = fixup_state();
        old_git.commits.tips = Some(vec![tip("a", "stack")]);
        assert_eq!(
            autosquash_disabled(&old_git).as_deref(),
            Some(branch_tips::left_behind(b"refs/heads/stack").as_str())
        );
        old_git.commits.tips = None;
        assert_eq!(autosquash_disabled(&old_git).as_deref(), Some(TIPS_UNKNOWN));

        // With it, only a branch the fixup moves past, or on the fixup, is.
        let with = |tips| {
            let mut state = fixup_state();
            state.git_version = Some(version::UPDATE_REFS);
            state.branch = Some(crate::git::status::BranchStatus {
                head: Head::Branch("main".into()),
                upstream: None,
                ahead_behind: None,
                in_progress: vec![],
                bisect: None,
            });
            state.commits.tips = Some(tips);
            autosquash_disabled(&state)
        };
        assert_eq!(with(vec![tip("a", "stack"), tip("d", "main")]), None);
        for on in ["b", "d"] {
            assert_eq!(
                with(vec![tip(on, "stack")]).as_deref(),
                Some(rebase_plan::crossing(&"d".repeat(40), b"refs/heads/stack").as_str()),
                "{on}"
            );
        }
    }

    #[test]
    fn the_fixup_keys_and_which_a_repository_may_move() {
        let state = fixup_state();
        let on = |key| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::Commits,
            &state,
            key,
        ) {
            Resolution::Run(binding) => binding.intent.clone(),
            other => panic!("{key:?}: {other:?}"),
        };
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(on(key(KeyCode::Char('f'), ctrl)), AppIntent::CreateFixup);
        assert_eq!(
            on(key(KeyCode::Char('S'), KeyModifiers::SHIFT)),
            AppIntent::Autosquash
        );
        assert_eq!(on(key(KeyCode::Char('b'), ctrl)), AppIntent::FindFixupBase);
        for id in ["commits.create_fixup", "commits.autosquash"] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
            assert!(build(&[(id, &["Z"])], Trust::Repo).is_err(), "{id}");
            assert!(build(&[(id, &["Z"])], Trust::Global).is_ok(), "{id}");
        }
        assert!(!REPO_LOCKED.contains(&"commits.find_base"));

        // The question: only `y` folds; Enter answers nothing.
        let ask = |key| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::AutosquashConfirm,
            &state,
            key,
        ) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            Resolution::Unbound => None,
            other => panic!("{key:?}: {other:?}"),
        };
        let none = KeyModifiers::NONE;
        assert_eq!(ask(key(KeyCode::Enter, none)), None);
        assert_eq!(ask(plain('q')), None);
        assert_eq!(ask(plain('y')), Some(AppIntent::ConfirmYes));
        assert_eq!(ask(plain('n')), Some(AppIntent::ConfirmNo));
        assert_eq!(ask(key(KeyCode::Esc, none)), Some(AppIntent::ConfirmNo));
        assert_eq!(ask(key(KeyCode::Char('c'), ctrl)), Some(AppIntent::Quit));
        assert_eq!(KeyScope::AutosquashConfirm.parent(), None);
        for id in [
            "autosquash_confirm.yes",
            "autosquash_confirm.no",
            "autosquash_confirm.quit",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
            assert!(build(&[(id, &["enter"])], Trust::Repo).is_err(), "{id}");
        }
        assert!(build(&[("commits.find_base", &["Z"])], Trust::Repo).is_ok());
    }
}
