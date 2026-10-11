//! Every key Kika understands, declared once. The same records drive
//! dispatch, the help menu and the bottom bar, so they cannot drift apart.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::custom_command::{CommandTable, CustomCommand};
use crate::external_diff::NO_RENDERER;
use crate::git::diff::{DiffSide, MAX_CONTEXT, MIN_CONTEXT};
use crate::git::patch::{FileKind, PatchError};
use crate::git::repo_state::{Operation, resumable};
use crate::git::status::{EntryKind, Head};
use crate::git::submodule::SubmoduleState;
use crate::hunk_view::NO_CHANGES;
use crate::intent::{AppIntent, Scroll};
use crate::layout::{Panel, SidePanel};
use crate::menu::MenuId;
use crate::model::{AppState, CommitFlow, Focus};
use crate::popup::{DeleteBranchConfirm, Popup};

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
            _ => key,
        }
    }
}

/// Read a key as a config file writes it: the names [`KeyPattern::label`]
/// uses where they are ASCII, any letter case, plus `up`, `down`, `left` and
/// `right` for the arrows. A `ctrl-` prefix wants exactly Ctrl.
pub fn parse_key(text: &str) -> Result<KeyPattern, String> {
    if let Some(rest) = text.strip_prefix("ctrl-") {
        if rest.chars().any(|c| c.is_uppercase()) {
            return Err(format!(
                "bad key \"{text}\": write ctrl- keys in lower case"
            ));
        }
        let key = parse_key(rest).map_err(|_| bad_key(text))?;
        return Ok(KeyPattern::exact(key.code, KeyModifiers::CONTROL));
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
         down, left, right, pgup, pgdn, home or end, with an optional ctrl- prefix",
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
    /// The Branches panel with focus.
    Branches,
    /// The Commits panel, with a cursor on the log.
    Commits,
    /// The Commits panel showing one commit's files.
    CommitFiles,
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
    /// The submodule list on top of the popup stack.
    Submodules,
    /// The recent repositories on top of the popup stack.
    Recent,
    /// Git or ssh asking for a username, password or passphrase.
    Credential,
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
            | KeyScope::Commits
            | KeyScope::CommitFiles
            | KeyScope::Status
            | KeyScope::Diff
            | KeyScope::Blame
            | KeyScope::BlameCommit => Some(KeyScope::Workspace),
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
            | KeyScope::Submodules
            | KeyScope::Recent
            | KeyScope::Credential => None,
        }
    }

    /// The help menu heading.
    pub fn title(self) -> &'static str {
        match self {
            KeyScope::Global => "Global",
            KeyScope::Workspace => "Workspace",
            KeyScope::Files => "Files",
            KeyScope::Branches => "Branches",
            KeyScope::Commits => "Commits",
            KeyScope::CommitFiles => "Commit files",
            KeyScope::Status => "Status",
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
            KeyScope::DeleteBranch => "Delete branch",
            KeyScope::Submodules => "Submodules",
            KeyScope::Recent => "Recent repositories",
            KeyScope::Credential => "Git is asking",
        }
    }

    /// What the bottom bar says before the key hints, for scopes whose
    /// keys behave unlike everywhere else.
    pub fn bar_note(self) -> Option<&'static str> {
        match self {
            KeyScope::CommitEditor | KeyScope::Credential => Some("q and ? are typed"),
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
    pub disabled: fn(&AppState) -> Option<&'static str>,
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
/// programs (an editor, a diff renderer), reach the network, destroy work,
/// answer a question, create a repository or switch to another one. Moving
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
    Disabled(&'static str),
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
) -> Vec<(&'a Binding, Option<&'static str>)> {
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
pub(crate) fn never(_: &AppState) -> Option<&'static str> {
    None
}

pub(crate) const OP_RUNNING: &str = "An operation is running";

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

/// Enter has no commit to open before the log has rows.
fn no_commit(state: &AppState) -> Option<&'static str> {
    state.commits.rows.is_empty().then_some("No commits")
}

/// With no renderer configured there is nothing to cycle.
fn renderer_disabled(state: &AppState) -> Option<&'static str> {
    state.renderers.is_empty().then_some(NO_RENDERER)
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

fn no_command_running(state: &AppState) -> Option<&'static str> {
    (!state.command_running).then_some("No command is running")
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

fn no_submodule(state: &AppState) -> Option<&'static str> {
    state
        .selected_submodule()
        .is_none()
        .then_some("No submodule selected")
}

/// Why `i` or `u` cannot run on the selected submodule right now.
fn submodule_op_disabled(state: &AppState) -> Option<&'static str> {
    if state.op_seq.is_some() {
        return Some(OP_RUNNING);
    }
    no_submodule(state)
}

/// Why the selected submodule cannot be entered: it must be surely checked
/// out, and an operation must not be running in the repository being left.
pub(crate) fn enter_submodule_disabled(state: &AppState) -> Option<&'static str> {
    if state.op_seq.is_some() {
        return Some("Wait for the operation to finish");
    }
    match state.selected_submodule() {
        None => Some("No submodule selected"),
        Some(sub) if sub.state == SubmoduleState::Uninitialized => {
            Some("Update the submodule first")
        }
        // Git marks a conflict before it looks for a checkout, so the
        // directory may hold none, and git there would find the parent.
        Some(sub) if sub.state == SubmoduleState::Conflicted => {
            Some("Resolve the submodule conflict first")
        }
        Some(_) => None,
    }
}

/// Why there is no parent to go back to right now.
pub(crate) fn leave_submodule_disabled(state: &AppState) -> Option<&'static str> {
    if state.repo_stack.is_empty() {
        Some("Not in a submodule")
    } else if state.op_seq.is_some() {
        Some("Wait for the operation to finish")
    } else {
        None
    }
}

/// Why `i` cannot make the launch directory a repository: it is offered
/// only by the recent list shown outside one.
pub(crate) fn init_repo_disabled(state: &AppState) -> Option<&'static str> {
    let offered = matches!(state.popups.last(), Some(Popup::Recent(list)) if list.offer_init);
    if state.repo.is_some() || !offered {
        Some("Already in a repository")
    } else if state.busy > 0 {
        Some("Loading…")
    } else {
        None
    }
}

/// Why no branch action can run right now: one needs a selected branch,
/// and git one operation at a time.
pub(crate) fn branch_action_disabled(state: &AppState) -> Option<&'static str> {
    if state.op_seq.is_some() {
        Some(OP_RUNNING)
    } else if state.branches.selected_branch().is_none() {
        Some("No branch selected")
    } else {
        None
    }
}

pub(crate) fn checkout_branch_disabled(state: &AppState) -> Option<&'static str> {
    branch_action_disabled(state).or_else(|| {
        let selected = state.branches.selected_branch();
        selected
            .is_some_and(|b| b.head)
            .then_some("Already on this branch")
    })
}

pub(crate) fn delete_branch_disabled(state: &AppState) -> Option<&'static str> {
    branch_action_disabled(state).or_else(|| {
        let selected = state.branches.selected_branch();
        selected
            .is_some_and(|b| b.head)
            .then_some("Cannot delete the current branch")
    })
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
pub(crate) fn merge_disabled(state: &AppState) -> Option<&'static str> {
    branch_action_disabled(state).or_else(|| {
        if state.branches.selected_branch().is_some_and(|b| b.head) {
            Some("Already on this branch")
        } else if resumable_operation(state).is_some() {
            Some("Finish or abort the operation in progress")
        } else {
            None
        }
    })
}

/// Why continue or abort cannot run: one at a time, and only with a
/// merge, rebase or cherry-pick to act on.
pub(crate) fn resume_disabled(state: &AppState) -> Option<&'static str> {
    if state.op_seq.is_some() {
        Some(OP_RUNNING)
    } else if resumable_operation(state).is_none() {
        Some("Nothing in progress")
    } else {
        None
    }
}

pub(crate) fn skip_disabled(state: &AppState) -> Option<&'static str> {
    resume_disabled(state).or_else(|| {
        resumable_operation(state)
            .is_some_and(|op| !op.can_skip())
            .then_some("A merge cannot be skipped")
    })
}

/// The delete question on top, if one is.
fn delete_question(state: &AppState) -> Option<&DeleteBranchConfirm> {
    match state.popups.last() {
        Some(Popup::DeleteBranch(confirm)) => Some(confirm),
        _ => None,
    }
}

/// `y` deletes only a merged branch; an unmerged one needs `D`.
fn plain_delete_disabled(state: &AppState) -> Option<&'static str> {
    delete_question(state)
        .is_some_and(DeleteBranchConfirm::force)
        .then_some("Not fully merged: press D to force delete")
}

/// `D` answers only the stronger question, about an unmerged branch.
fn force_delete_disabled(state: &AppState) -> Option<&'static str> {
    delete_question(state)
        .is_some_and(|c| !c.force())
        .then_some("Fully merged: press y to delete")
}

/// Why no fetch, pull or push may start: one needs a repository, and git
/// one operation at a time.
pub(crate) fn remote_disabled(state: &AppState) -> Option<&'static str> {
    if state.repo.is_none() {
        Some("Not in a repository")
    } else if state.op_seq.is_some() || state.remote.is_some() {
        Some(REMOTE_BUSY)
    } else {
        None
    }
}

pub(crate) const REMOTE_BUSY: &str = "Another operation is running";

/// A fetch also waits for auto-fetch, which would race it for the same refs.
pub(crate) fn fetch_disabled(state: &AppState) -> Option<&'static str> {
    remote_disabled(state).or_else(|| {
        state
            .watch
            .fetch_pending
            .then_some("A background fetch is running")
    })
}

/// Pull and push work on the current branch, so they need one.
pub(crate) fn pull_push_disabled(state: &AppState) -> Option<&'static str> {
    remote_disabled(state).or_else(|| {
        let detached = state
            .branch
            .as_ref()
            .is_some_and(|b| matches!(b.head, Head::Detached(_)));
        detached.then_some("HEAD is detached")
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
        disabled: blame_open,
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
        description: "Focus the diff, to work on the selected file's hunks",
        label: "diff",
        bar: false,
        disabled: no_file_selected,
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
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
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
            KeyPattern::any(KeyCode::Up),
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
        description: "Show the files the selected commit changed",
        label: "files",
        bar: true,
        disabled: no_commit,
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
        id: "files.stage",
        keys: &[KeyPattern::exact(KeyCode::Char(' '), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::ToggleStage,
        description: "Stage the selected file, or unstage it if fully staged",
        label: "stage",
        bar: true,
        disabled: stage_disabled,
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
        disabled: discard_disabled,
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
        disabled: open_disabled,
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
        disabled: blame_disabled,
    },
    Binding {
        id: "files.hunks",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Files,
        intent: AppIntent::FocusDiff,
        description: "Work on the hunks of the selected file",
        label: "hunks",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: no_file_selected,
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
        description: "Stage the selected hunk, or unstage it in the staged view",
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
        keys: &[
            KeyPattern::any(KeyCode::Esc),
            KeyPattern::exact(KeyCode::Char('h'), NONE),
            KeyPattern::any(KeyCode::Left),
        ],
        scope: KeyScope::Diff,
        intent: AppIntent::FocusLeft,
        description: "Back to the side panel last focused",
        label: "back",
        bar: true,
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
        description: "Stage the selected lines, or unstage them in the staged view",
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
        id: "status.continue",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), NONE)],
        scope: KeyScope::Status,
        intent: AppIntent::ContinueOperation,
        description: "Continue the merge, rebase or cherry-pick once conflicts are resolved",
        label: "continue",
        bar: true,
        disabled: resume_disabled,
    },
    Binding {
        id: "status.skip",
        keys: &[KeyPattern::exact(KeyCode::Char('s'), NONE)],
        scope: KeyScope::Status,
        intent: AppIntent::SkipOperation,
        description: "Skip the commit the rebase or cherry-pick stopped on",
        label: "skip",
        bar: true,
        disabled: skip_disabled,
    },
    Binding {
        id: "status.abort",
        keys: &[KeyPattern::exact(KeyCode::Char('a'), NONE)],
        scope: KeyScope::Status,
        intent: AppIntent::AbortOperation,
        description: "Abort the operation in progress, asking first",
        label: "abort",
        bar: true,
        disabled: resume_disabled,
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
            available(&table, &Keymap::default(), KeyScope::Files, &state)
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
        // selected and a commit in the log.
        let mut state = AppState::new(Context::Workspace);
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
        }];
        match resolve(registry(), &Keymap::default(), focus, &state, key) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            Resolution::Disabled(reason) => panic!("{key:?} disabled: {reason}"),
            Resolution::Unbound | Resolution::Command(_) => None,
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
            assert_eq!(diff(k), Some(FocusLeft), "{k:?}");
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
        assert_eq!(lines(plain('h')), Some(FocusLeft));
        assert_eq!(lines(plain('r')), Some(RefreshStatus));
        assert_eq!(lines(plain('q')), Some(Quit));

        // Space and `v` share the hunk key's refusals; with no hunks here,
        // that is the same reason.
        let state = AppState::new(Context::Workspace);
        for (scope, k) in [(KeyScope::Lines, plain(' ')), (KeyScope::Diff, plain('v'))] {
            assert!(matches!(
                resolve(registry(), &Keymap::default(), scope, &state, k),
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
                resolve(
                    registry(),
                    &Keymap::default(),
                    KeyScope::CommitEditor,
                    &state,
                    ctrl(c)
                ),
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

    #[test]
    fn backspace_leaves_a_submodule_from_every_workspace_panel_once_inside_one() {
        let backspace = key(KeyCode::Backspace, KeyModifiers::NONE);
        let mut state = AppState::new(Context::Workspace);
        for scope in [KeyScope::Files, KeyScope::Diff, KeyScope::Lines] {
            assert!(
                matches!(
                    resolve(registry(), &Keymap::default(), scope, &state, backspace),
                    Resolution::Disabled("Not in a submodule")
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
            Resolution::Disabled("Wait for the operation to finish")
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
                resolve(
                    registry(),
                    &Keymap::default(),
                    KeyScope::CommitEditor,
                    &state,
                    k
                ),
                Resolution::Disabled("An operation is running")
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
                Resolution::Disabled("No file selected")
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

    /// The `<scope>` part of a binding id.
    fn scope_name(scope: KeyScope) -> &'static str {
        match scope {
            KeyScope::Global => "global",
            KeyScope::Workspace => "workspace",
            KeyScope::Files => "files",
            KeyScope::Branches => "branches",
            KeyScope::Commits => "commits",
            KeyScope::CommitFiles => "commit_files",
            KeyScope::Status => "status",
            KeyScope::Diff => "diff",
            KeyScope::Lines => "lines",
            KeyScope::Blame => "blame",
            KeyScope::BlameCommit => "blame_commit",
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
            KeyScope::Submodules => "submodules",
            KeyScope::Recent => "recent",
            KeyScope::Credential => "credential",
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
            let moved = build(&[(id, &["F"])], Trust::Repo).unwrap_err();
            assert_eq!(
                moved,
                format!("\"{id}\" can only be changed in the global config file")
            );
            assert!(build(&[(id, &[])], Trust::Repo).is_err(), "{id}");
            if *id != "global.quit" {
                assert!(build(&[(id, &["F"])], Trust::Global).is_ok(), "{id}");
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
        assert!(build(&[("diff.back", &["esc", "h", "left"])], Trust::Repo).is_ok());
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
            Resolution::Disabled("Already in a repository")
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
                matches!(at(&state), Resolution::Disabled("No command is running")),
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
}
