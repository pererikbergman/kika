//! Popups that sit on one stack over the workspace: menus, confirmations,
//! one-line prompts and errors. Pure types; the model owns the stack and
//! each popup hands its result back as an intent.

use std::borrow::Cow;
use std::fmt;

use crossterm::event::{KeyCode, KeyModifiers};

use crate::command_log::CommandLogView;
use crate::commit_editor::{byte_at, char_len};
use crate::git::askpass::Secret;
use crate::git::diff::terminal_safe;
use crate::git::ops::GitOp;
use crate::git::path::RepoPath;
use crate::git::repo_state::Operation;
use crate::intent::AppIntent;
use crate::keymap::KeyPress;
use crate::menu::MenuId;
use crate::recent_list::RecentList;
use crate::submodule_list::SubmoduleList;
use crate::worktree_list::WorktreeList;

/// One entry on the popup stack. The top one takes the keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Popup {
    Menu {
        id: MenuId,
        cursor: usize,
    },
    Confirm(ConfirmKind),
    Prompt {
        kind: PromptKind,
        input: LineInput,
    },
    Error(ErrorPopup),
    Discard(DiscardConfirm),
    DeleteBranch(DeleteBranchConfirm),
    ResetHard(ResetHardConfirm),
    DropStash(DropStashConfirm),
    RemoveRemote(RemoveRemoteConfirm),
    DeleteTag(DeleteTagConfirm),
    Submodules(SubmoduleList),
    Worktrees(WorktreeList),
    RemoveWorktree(RemoveWorktreeConfirm),
    Recent(RecentList),
    /// Git or ssh asks something for a running fetch, pull or push.
    Credential(CredentialPrompt),
    /// "Undo this?" or "Redo this?", with what would happen.
    Travel(TravelConfirm),
    /// The git commands Kika ran; the entries live in `AppState`.
    CommandLog(CommandLogView),
}

/// A question from git or ssh, and what the user has typed so far. The
/// text leaves only in the answer; it is never kept anywhere else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialPrompt {
    /// Git's or ssh's words, terminal-safe.
    pub prompt: String,
    /// Typed text is drawn as dots, unless the answer is a name or a yes/no.
    pub masked: bool,
    /// Ssh's yes/no kind, answered with y or n instead of text.
    pub confirm: bool,
    pub input: SecretInput,
}

impl CredentialPrompt {
    /// Only git's username question and ssh's new-host-key question are
    /// shown as typed, matched on their exact words, so a prompt a server
    /// chose cannot ask for a secret with the text in sight.
    pub fn new(prompt: String, confirm: bool) -> Self {
        let in_clear = prompt.starts_with("Username for '")
            || prompt.contains("want to continue connecting (yes/no");
        let masked = !in_clear;
        Self {
            prompt,
            masked,
            confirm,
            input: SecretInput::default(),
        }
    }
}

/// A [`LineInput`] that may hold a password, so its `Debug` hides the text.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretInput(LineInput);

impl SecretInput {
    pub fn edit(&mut self, key: KeyPress) {
        self.0.edit(key);
    }

    /// The typed text and cursor, only for drawing it.
    pub fn line(&self) -> &LineInput {
        &self.0
    }

    /// The typed text, as the answer to send.
    pub fn secret(&self) -> Secret {
        Secret::new(self.0.text.clone())
    }
}

impl fmt::Debug for SecretInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretInput(..)")
    }
}

/// "Throw this work away?" Unlike [`ConfirmKind`], it carries the op chosen
/// when it was asked, so a refresh before the answer cannot change what goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscardConfirm {
    pub what: DiscardWhat,
    /// The file's display path, already terminal-safe.
    pub path: String,
    pub op: GitOp,
}

/// What a discard throws away, so the question can say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscardWhat {
    File,
    Untracked,
    Hunk,
    Lines,
    /// A range of files: `restore` lose their unstaged changes and
    /// `delete` untracked ones are deleted.
    Range {
        restore: usize,
        delete: usize,
    },
}

impl DiscardWhat {
    pub fn title(self) -> Cow<'static, str> {
        match self {
            DiscardWhat::File => "Discard file".into(),
            DiscardWhat::Untracked => "Delete file".into(),
            DiscardWhat::Hunk => "Discard hunk".into(),
            DiscardWhat::Lines => "Discard lines".into(),
            DiscardWhat::Range { restore, delete } => {
                format!("Discard {}", files(restore + delete)).into()
            }
        }
    }

    pub fn question(self) -> Cow<'static, str> {
        match self {
            DiscardWhat::File => "Discard all unstaged changes to this file? (y/n)".into(),
            DiscardWhat::Untracked => "Delete this untracked file from disk? (y/n)".into(),
            DiscardWhat::Hunk => "Discard this hunk from the working tree? (y/n)".into(),
            DiscardWhat::Lines => "Discard these lines from the working tree? (y/n)".into(),
            DiscardWhat::Range { restore, delete: 0 } => {
                format!("Discard unstaged changes to {}? (y/n)", files(restore)).into()
            }
            DiscardWhat::Range { restore: 0, delete } => {
                format!("Delete {} from disk? (y/n)", untracked(delete)).into()
            }
            DiscardWhat::Range { restore, delete } => format!(
                "Discard unstaged changes to {} and delete {}? (y/n)",
                files(restore),
                untracked(delete)
            )
            .into(),
        }
    }
}

/// `1 file` or `n files`.
fn files(n: usize) -> String {
    if n == 1 {
        "1 file".to_string()
    } else {
        format!("{n} files")
    }
}

/// `1 untracked file` or `n untracked files`.
fn untracked(n: usize) -> String {
    format!("{n} untracked {}", if n == 1 { "file" } else { "files" })
}

/// "Delete this branch?" Like [`DiscardConfirm`], it carries the op chosen
/// when it was asked. A branch whose commits are not merged is asked about
/// in stronger words, and only a force delete answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteBranchConfirm {
    /// The branch's display name, already terminal-safe.
    pub name: String,
    pub op: GitOp,
}

impl DeleteBranchConfirm {
    /// Whether answering deletes with `-D`, so only `D` may answer.
    pub fn force(&self) -> bool {
        matches!(self.op, GitOp::DeleteBranch { force: true, .. })
    }

    pub fn title(&self) -> &'static str {
        if self.force() {
            "Force delete branch"
        } else {
            "Delete branch"
        }
    }

    pub fn question(&self) -> String {
        let name = &self.name;
        if self.force() {
            format!(
                "{name} is not fully merged; its commits may be lost.\n\
                 Press D to force delete, n or Esc to cancel"
            )
        } else {
            format!("Delete branch {name}? (y/n)")
        }
    }
}

/// "Reset hard and lose every uncommitted change?" Like
/// [`DeleteBranchConfirm`], it carries the op chosen when it was asked, so
/// a reload before the answer cannot change the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetHardConfirm {
    /// The target's short id, as the log shows it.
    pub short_oid: String,
    pub op: GitOp,
}

impl ResetHardConfirm {
    pub fn title(&self) -> &'static str {
        "Reset hard"
    }

    pub fn question(&self) -> String {
        format!(
            "Reset to {} and throw away every uncommitted change, staged and \
             unstaged? Commits after it leave the branch. (y/n)",
            self.short_oid
        )
    }
}

/// "Undo this?" or "Redo this?" Like [`ResetHardConfirm`], it carries the
/// op planned when it was asked, so a reload before the answer cannot
/// change what moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TravelConfirm {
    /// `Undo` or `Redo`.
    pub title: &'static str,
    /// What will happen, line by line, already terminal-safe.
    pub preview: Vec<String>,
    pub op: GitOp,
}

impl TravelConfirm {
    pub fn question(&self) -> String {
        format!("{}\n(y/n)", self.preview.join("\n"))
    }
}

/// "Remove this remote?" Like [`DeleteBranchConfirm`], it carries the op
/// chosen when it was asked, so a reload before the answer cannot change
/// the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveRemoteConfirm {
    /// The remote's display name, already terminal-safe.
    pub name: String,
    pub op: GitOp,
}

impl RemoveRemoteConfirm {
    pub fn title(&self) -> &'static str {
        "Remove remote"
    }

    pub fn question(&self) -> String {
        format!(
            "Remove remote {}? Its remote-tracking branches are deleted, and \
             branches tracking it lose their upstream. (y/n)",
            self.name
        )
    }
}

/// "Delete this tag?" Like [`DeleteBranchConfirm`], it carries the op
/// chosen when it was asked, so a reload before the answer cannot change
/// the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteTagConfirm {
    /// The tag's display name, already terminal-safe.
    pub name: String,
    pub op: GitOp,
}

impl DeleteTagConfirm {
    pub fn title(&self) -> &'static str {
        "Delete tag"
    }

    pub fn question(&self) -> String {
        format!(
            "Delete tag {}? It is not deleted on any remote. (y/n)",
            self.name
        )
    }
}

/// "Remove this worktree?" It carries the path taken when it was asked,
/// so a reload before the answer cannot change what is removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveWorktreeConfirm {
    /// The path's display form, already terminal-safe.
    pub display: String,
    pub path: RepoPath,
}

impl RemoveWorktreeConfirm {
    pub fn title(&self) -> &'static str {
        "Remove worktree"
    }

    pub fn question(&self) -> String {
        format!(
            "Remove worktree {}? Git refuses if it has uncommitted or untracked \
             changes. (y/n)",
            self.display
        )
    }
}

/// "Drop this stash?" Like [`ResetHardConfirm`], it carries the op chosen
/// when it was asked, so a reload before the answer cannot change which
/// entry goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropStashConfirm {
    /// `stash@{N}: <subject>`, already terminal-safe.
    pub label: String,
    pub op: GitOp,
}

impl DropStashConfirm {
    pub fn title(&self) -> &'static str {
        "Drop stash"
    }

    pub fn question(&self) -> String {
        format!(
            "Drop {}? It cannot be recovered from the stash list. (y/n)",
            self.label
        )
    }
}

/// A yes/no question. Its answer is an intent, so the asker needs no callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKind {
    Quit,
    /// A plain push was rejected because the remote moved on.
    ForcePush,
    /// Abort this operation in progress. It names the operation found when
    /// asked, so the answer aborts what the question said.
    Abort(Operation),
    /// Every block of the conflict view's file is picked: stage it?
    StageResolved,
    /// Check out the commit kept in the Commits panel's `pending` as a
    /// detached `HEAD`. Git refuses to overwrite local changes, so nothing
    /// is lost and Enter may answer.
    DetachedCheckout,
}

impl ConfirmKind {
    pub fn title(self) -> &'static str {
        match self {
            ConfirmKind::Quit => "Quit",
            ConfirmKind::ForcePush => "Push rejected",
            ConfirmKind::Abort(Operation::Merge) => "Abort merge",
            ConfirmKind::Abort(Operation::Rebase) => "Abort rebase",
            ConfirmKind::Abort(Operation::CherryPick) => "Abort cherry-pick",
            ConfirmKind::Abort(Operation::Revert) => "Abort revert",
            ConfirmKind::Abort(Operation::Bisect) => "Reset bisect",
            ConfirmKind::StageResolved => "Stage resolved file",
            ConfirmKind::DetachedCheckout => "Detached checkout",
        }
    }

    pub fn question(self) -> &'static str {
        match self {
            ConfirmKind::Quit => "Quit Kika? (y/n)",
            ConfirmKind::ForcePush => {
                "The remote has commits this branch does not. Push with \
                 --force-with-lease, replacing them only if nobody pushed since \
                 your last fetch? (y/n)"
            }
            ConfirmKind::Abort(Operation::Merge) => {
                "Abort the merge and return to where it started? \
                 Resolved conflicts are lost. (y/n)"
            }
            ConfirmKind::Abort(Operation::Rebase) => {
                "Abort the rebase and return to where it started? \
                 Resolved conflicts are lost. (y/n)"
            }
            ConfirmKind::Abort(Operation::CherryPick) => {
                "Abort the cherry-pick and return to where it started? \
                 Resolved conflicts are lost. (y/n)"
            }
            ConfirmKind::Abort(Operation::Revert) => {
                "Abort the revert and return to where it started? \
                 Resolved conflicts are lost. (y/n)"
            }
            // `AppState::confirm_question` names where the bisect started;
            // this is the fallback when it cannot.
            ConfirmKind::Abort(Operation::Bisect) => {
                "Reset the bisect? Its marks are dropped and HEAD goes back to where \
                 the bisect started; files change to match. Git refuses if a local \
                 change would be overwritten. (y/n)"
            }
            ConfirmKind::StageResolved => {
                "Every conflict in this file is resolved. Stage it? (y/n)"
            }
            // `AppState::confirm_question` names the commit; this is the
            // fallback when it cannot.
            ConfirmKind::DetachedCheckout => {
                "Check out this commit as a detached HEAD? You will be on no \
                 branch; new commits there belong to no branch until you \
                 create one. (y/n)"
            }
        }
    }

    pub fn yes(self) -> AppIntent {
        match self {
            ConfirmKind::Quit => AppIntent::Quit,
            ConfirmKind::ForcePush => AppIntent::ForcePush,
            ConfirmKind::Abort(Operation::Bisect) => AppIntent::ConfirmBisectReset,
            ConfirmKind::Abort(op) => AppIntent::ConfirmAbort(op),
            ConfirmKind::StageResolved => AppIntent::StageResolved,
            ConfirmKind::DetachedCheckout => AppIntent::ConfirmCheckoutCommit,
        }
    }

    pub fn no(self) -> Option<AppIntent> {
        match self {
            ConfirmKind::Quit
            | ConfirmKind::ForcePush
            | ConfirmKind::Abort(_)
            | ConfirmKind::StageResolved
            | ConfirmKind::DetachedCheckout => None,
        }
    }
}

/// A one-line text question. The typed text comes back as an intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    GoToFile,
    /// One of a custom command's prompts. Its title comes from the
    /// command, so [`AppState::prompt_title`](crate::model::AppState::prompt_title)
    /// gives it; this one is only a fallback.
    Command,
    /// The name of a branch to make at the selected one.
    NewBranch,
    /// A new name for the selected branch.
    RenameBranch,
    /// The message of a stash being made; empty for git's own.
    StashMessage,
    /// A new message for the selected stash.
    RenameStash,
    /// The name of a local branch to track the selected remote branch.
    TrackBranch,
    /// A new remote's name.
    RemoteName,
    /// A new remote's URL, asked after its name.
    RemoteUrl,
    /// A new name for the selected remote.
    RenameRemote,
    /// The remote branch the selected local branch should track.
    SetUpstream,
    /// Where to add a worktree for the branch `a` was pressed on.
    WorktreePath,
    /// A new tag's name.
    TagName,
    /// An annotated tag's message, asked after its name.
    TagMessage,
    /// The pattern to add to `.gitignore`, filled with the selected file's.
    IgnorePattern,
}

impl PromptKind {
    pub fn title(self) -> &'static str {
        match self {
            PromptKind::GoToFile => "Go to file",
            PromptKind::Command => "Command",
            PromptKind::NewBranch => "New branch",
            PromptKind::RenameBranch => "Rename branch",
            PromptKind::StashMessage => "Stash message (optional)",
            PromptKind::RenameStash => "Rename stash",
            PromptKind::TrackBranch => "Check out as",
            PromptKind::RemoteName => "New remote name",
            PromptKind::RemoteUrl => "New remote URL",
            PromptKind::RenameRemote => "Rename remote",
            PromptKind::SetUpstream => "Set upstream",
            PromptKind::WorktreePath => "New worktree",
            PromptKind::TagName => "New tag",
            PromptKind::TagMessage => "Tag message",
            PromptKind::IgnorePattern => "Add to .gitignore",
        }
    }

    pub fn submit(self, text: String) -> AppIntent {
        match self {
            PromptKind::GoToFile => AppIntent::GoToFile(text),
            PromptKind::Command => AppIntent::CommandPromptAnswered(text),
            PromptKind::NewBranch => AppIntent::CreateBranchNamed(text),
            PromptKind::RenameBranch => AppIntent::RenameBranchTo(text),
            PromptKind::StashMessage => AppIntent::StashMessageAnswered(text),
            PromptKind::RenameStash => AppIntent::RenameStashTo(text),
            PromptKind::TrackBranch => AppIntent::TrackRemoteBranchNamed(text),
            PromptKind::RemoteName => AppIntent::AddRemoteNamed(text),
            PromptKind::RemoteUrl => AppIntent::AddRemoteUrl(text),
            PromptKind::RenameRemote => AppIntent::RenameRemoteTo(text),
            PromptKind::SetUpstream => AppIntent::SetUpstreamTo(text),
            PromptKind::WorktreePath => AppIntent::AddWorktreeAt(text),
            PromptKind::TagName => AppIntent::TagNamed(text),
            PromptKind::TagMessage => AppIntent::TagMessaged(text),
            PromptKind::IgnorePattern => AppIntent::IgnorePatternAnswered(text),
        }
    }
}

/// One line of text being typed. `col` counts chars, not bytes, and always
/// points inside the text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineInput {
    pub text: String,
    pub col: usize,
}

impl LineInput {
    /// `text` already typed, with the cursor after it.
    pub fn with_text(text: &str) -> Self {
        Self {
            text: text.to_string(),
            col: char_len(text),
        }
    }

    /// Apply one key. Keys that mean nothing on one line are ignored.
    pub fn edit(&mut self, key: KeyPress) {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return;
        }
        match key.code {
            KeyCode::Char(c) if !c.is_control() => {
                self.text.insert(byte_at(&self.text, self.col), c);
                self.col += 1;
            }
            KeyCode::Backspace if self.col > 0 => {
                self.col -= 1;
                self.text.remove(byte_at(&self.text, self.col));
            }
            KeyCode::Delete if self.col < char_len(&self.text) => {
                self.text.remove(byte_at(&self.text, self.col));
            }
            KeyCode::Left => self.col = self.col.saturating_sub(1),
            KeyCode::Right => self.col = (self.col + 1).min(char_len(&self.text)),
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = char_len(&self.text),
            _ => {}
        }
    }
}

/// Something failed: what, git's own words, and what to do now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorPopup {
    pub title: String,
    pub message: String,
    /// Usually fixed text; owned when it names a key from the user's keymap.
    pub next: Cow<'static, str>,
}

impl ErrorPopup {
    /// The message is made safe to draw here, whatever its source, since it
    /// can quote repo-controlled paths.
    pub fn new(
        title: impl Into<String>,
        message: &str,
        next: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            title: title.into(),
            message: terminal_safe(message.as_bytes()),
            next: next.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(input: &mut LineInput, code: KeyCode) {
        input.edit(KeyPress::new(code, KeyModifiers::NONE));
    }

    fn typed(text: &str) -> LineInput {
        let mut input = LineInput::default();
        for c in text.chars() {
            press(&mut input, KeyCode::Char(c));
        }
        input
    }

    #[test]
    fn the_abort_question_names_the_operation_and_yes_aborts_that_one() {
        let ask = ConfirmKind::Abort(Operation::CherryPick);
        assert_eq!(ask.title(), "Abort cherry-pick");
        assert_eq!(
            ask.question(),
            "Abort the cherry-pick and return to where it started? \
             Resolved conflicts are lost. (y/n)"
        );
        assert_eq!(ask.yes(), AppIntent::ConfirmAbort(Operation::CherryPick));
        assert_eq!(ask.no(), None);
        assert_eq!(ConfirmKind::Abort(Operation::Merge).title(), "Abort merge");
        assert_eq!(
            ConfirmKind::Abort(Operation::Rebase).title(),
            "Abort rebase"
        );
        let revert = ConfirmKind::Abort(Operation::Revert);
        assert_eq!(revert.title(), "Abort revert");
        assert!(revert.question().starts_with("Abort the revert "));
        assert_eq!(revert.yes(), AppIntent::ConfirmAbort(Operation::Revert));
    }

    #[test]
    fn the_drop_stash_question_names_the_entry_and_says_it_is_gone_for_good() {
        use crate::git::commit_files::CommitId;
        use crate::git::stash::StashRef;
        let ask = DropStashConfirm {
            label: "stash@{2}: On main: wip".into(),
            op: GitOp::StashDrop(StashRef {
                index: 2,
                oid: CommitId::parse("abc").unwrap(),
            }),
        };
        assert_eq!(ask.title(), "Drop stash");
        assert_eq!(
            ask.question(),
            "Drop stash@{2}: On main: wip? It cannot be recovered from the stash list. (y/n)"
        );
    }

    #[test]
    fn the_stash_prompts_answer_with_their_text() {
        assert_eq!(PromptKind::StashMessage.title(), "Stash message (optional)");
        assert_eq!(
            PromptKind::StashMessage.submit("wip".into()),
            AppIntent::StashMessageAnswered("wip".into())
        );
        assert_eq!(PromptKind::RenameStash.title(), "Rename stash");
        assert_eq!(
            PromptKind::RenameStash.submit("new".into()),
            AppIntent::RenameStashTo("new".into())
        );
    }

    #[test]
    fn the_detached_checkout_question_warns_and_yes_checks_out() {
        let ask = ConfirmKind::DetachedCheckout;
        assert_eq!(ask.title(), "Detached checkout");
        assert!(ask.question().contains("detached HEAD"));
        assert_eq!(ask.yes(), AppIntent::ConfirmCheckoutCommit);
        assert_eq!(ask.no(), None);
    }

    #[test]
    fn the_hard_reset_question_names_the_target_and_what_is_lost() {
        use crate::git::commit_files::CommitId;
        use crate::git::ops::ResetMode;
        let ask = ResetHardConfirm {
            short_oid: "abc1234".into(),
            op: GitOp::Reset {
                to: CommitId::parse("abc1234").unwrap(),
                mode: ResetMode::Hard,
            },
        };
        assert_eq!(ask.title(), "Reset hard");
        assert_eq!(
            ask.question(),
            "Reset to abc1234 and throw away every uncommitted change, staged and \
             unstaged? Commits after it leave the branch. (y/n)"
        );
    }

    #[test]
    fn the_travel_question_is_the_preview_then_y_or_n() {
        use crate::git::commit_files::CommitId;
        use crate::git::undo::{Direction, Expected, Step};
        let ask = TravelConfirm {
            title: "Undo",
            preview: vec!["Undo: commit: x".into(), "Branch main moves.".into()],
            op: GitOp::Travel {
                direction: Direction::Undo,
                step: Step::MoveTo(CommitId::parse("abc").unwrap()),
                expect: Expected {
                    head_ref: None,
                    oid: CommitId::parse("def").unwrap(),
                },
            },
        };
        assert_eq!(ask.question(), "Undo: commit: x\nBranch main moves.\n(y/n)");
    }

    #[test]
    fn typing_inserts_at_the_cursor_by_char() {
        let mut input = typed("héllo 日本");
        assert_eq!(input.col, 8);
        press(&mut input, KeyCode::Home);
        press(&mut input, KeyCode::Right);
        press(&mut input, KeyCode::Char('x'));
        assert_eq!(input.text, "hxéllo 日本");
        assert_eq!(input.col, 2);
    }

    #[test]
    fn backspace_and_delete_remove_whole_chars_and_stop_at_the_ends() {
        let mut input = typed("a日b");
        press(&mut input, KeyCode::Left);
        press(&mut input, KeyCode::Backspace);
        assert_eq!((input.text.as_str(), input.col), ("ab", 1));
        press(&mut input, KeyCode::Delete);
        assert_eq!((input.text.as_str(), input.col), ("a", 1));
        press(&mut input, KeyCode::Delete);
        assert_eq!(input.text, "a");
        press(&mut input, KeyCode::Home);
        press(&mut input, KeyCode::Backspace);
        assert_eq!((input.text.as_str(), input.col), ("a", 0));
    }

    #[test]
    fn arrows_home_and_end_stay_inside_the_text() {
        let mut input = typed("abc");
        press(&mut input, KeyCode::Right);
        assert_eq!(input.col, 3);
        press(&mut input, KeyCode::Home);
        press(&mut input, KeyCode::Left);
        assert_eq!(input.col, 0);
        press(&mut input, KeyCode::End);
        assert_eq!(input.col, 3);
    }

    #[test]
    fn control_alt_chords_and_control_chars_are_not_typed() {
        let mut input = LineInput::default();
        input.edit(KeyPress::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
        input.edit(KeyPress::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        input.edit(KeyPress::new(KeyCode::Char('x'), KeyModifiers::ALT));
        press(&mut input, KeyCode::Char('\x1b'));
        press(&mut input, KeyCode::Char('\u{9b}'));
        press(&mut input, KeyCode::Enter);
        press(&mut input, KeyCode::Tab);
        assert_eq!(input.text, "A");
        assert_eq!(input.col, 1);
    }

    #[test]
    fn prefilled_text_puts_the_cursor_after_it() {
        let input = LineInput::with_text("fé");
        assert_eq!((input.text.as_str(), input.col), ("fé", 2));
    }

    #[test]
    fn a_merged_branch_is_asked_plainly_and_an_unmerged_one_with_a_warning() {
        let ask = |force| DeleteBranchConfirm {
            name: "topic".into(),
            op: GitOp::DeleteBranch {
                name: "topic".into(),
                force,
            },
        };
        assert!(!ask(false).force());
        assert_eq!(ask(false).question(), "Delete branch topic? (y/n)");
        assert!(ask(true).force());
        assert_eq!(
            ask(true).question(),
            "topic is not fully merged; its commits may be lost.\n\
             Press D to force delete, n or Esc to cancel"
        );
    }

    #[test]
    fn branch_prompts_answer_with_their_intents() {
        assert_eq!(
            PromptKind::NewBranch.submit("n".into()),
            AppIntent::CreateBranchNamed("n".into())
        );
        assert_eq!(
            PromptKind::RenameBranch.submit("r".into()),
            AppIntent::RenameBranchTo("r".into())
        );
    }

    #[test]
    fn remote_and_upstream_prompts_answer_with_their_intents() {
        let t = || "t".to_string();
        assert_eq!(
            PromptKind::TrackBranch.submit(t()),
            AppIntent::TrackRemoteBranchNamed(t())
        );
        assert_eq!(
            PromptKind::RemoteName.submit(t()),
            AppIntent::AddRemoteNamed(t())
        );
        assert_eq!(
            PromptKind::RemoteUrl.submit(t()),
            AppIntent::AddRemoteUrl(t())
        );
        assert_eq!(
            PromptKind::RenameRemote.submit(t()),
            AppIntent::RenameRemoteTo(t())
        );
        assert_eq!(
            PromptKind::SetUpstream.submit(t()),
            AppIntent::SetUpstreamTo(t())
        );
    }

    #[test]
    fn tag_prompts_answer_with_their_intents_and_delete_asks_plainly() {
        let t = || "t".to_string();
        assert_eq!(PromptKind::TagName.submit(t()), AppIntent::TagNamed(t()));
        assert_eq!(
            PromptKind::TagMessage.submit(t()),
            AppIntent::TagMessaged(t())
        );
        let ask = DeleteTagConfirm {
            name: "v1".into(),
            op: GitOp::DeleteTag {
                name: "v1".into(),
                oid: "1".repeat(40),
            },
        };
        assert_eq!(
            ask.question(),
            "Delete tag v1? It is not deleted on any remote. (y/n)"
        );
        assert_eq!(ask.title(), "Delete tag");
    }

    #[test]
    fn the_remove_remote_question_names_it_and_its_branches() {
        let ask = RemoveRemoteConfirm {
            name: "fork".into(),
            op: GitOp::RemoveRemote("fork".into()),
        };
        let question = ask.question();
        assert!(question.starts_with("Remove remote fork?"), "{question}");
        assert!(
            question.contains("remote-tracking branches are deleted"),
            "{question}"
        );
    }

    #[test]
    fn an_error_message_holds_no_control_chars() {
        let error = ErrorPopup::new("Could not stage", "a\x1b[31mb\u{9b}c", "Try again.");
        assert!(!error.message.chars().any(char::is_control), "{error:?}");
        assert!(error.message.starts_with('a') && error.message.ends_with('c'));
        // Making it safe twice changes nothing, so any path may do it.
        let again = ErrorPopup::new("Could not stage", &error.message, "Try again.");
        assert_eq!(again, error);
    }

    #[test]
    fn an_error_keeps_the_lines_of_git_message() {
        let error = ErrorPopup::new("t", "fatal: one\nhint: two\n", "n");
        assert_eq!(error.message, "fatal: one\nhint: two");
    }

    #[test]
    fn a_credential_prompt_masks_all_but_names_and_yes_no_questions() {
        let masked = |prompt: &str| CredentialPrompt::new(prompt.into(), false).masked;
        assert!(masked("Password for 'https://u@h':"));
        assert!(masked(
            "Enter passphrase for key '/home/u/.ssh/id_ed25519':"
        ));
        assert!(!masked("Username for 'https://h':"));
        assert!(!masked(
            "The authenticity of host 'h' can't be established.\n\
             Are you sure you want to continue connecting (yes/no/[fingerprint])?"
        ));
        // Older ssh asks without the fingerprint choice.
        assert!(!masked(
            "Are you sure you want to continue connecting (yes/no)?"
        ));
        // A server's own prompt that only looks like one stays masked.
        assert!(masked("Username and token:"));
        assert!(masked("Token (yes/no is fine):"));
    }

    #[test]
    fn typed_credential_text_never_prints() {
        let mut prompt = CredentialPrompt::new("Password:".into(), false);
        for c in "hunter2".chars() {
            prompt
                .input
                .edit(KeyPress::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(prompt.input.line().text, "hunter2");
        assert_eq!(prompt.input.secret().expose(), "hunter2");
        let printed = format!("{:?}", Popup::Credential(prompt));
        assert!(!printed.contains("hunter2"), "{printed}");
    }

    #[test]
    fn a_rejected_push_asks_before_forcing_with_a_lease() {
        assert_eq!(ConfirmKind::ForcePush.yes(), AppIntent::ForcePush);
        assert_eq!(ConfirmKind::ForcePush.no(), None);
        assert!(ConfirmKind::ForcePush.question().starts_with(
            "The remote has commits this branch does not. Push with --force-with-lease"
        ));
    }

    #[test]
    fn confirm_and_prompt_answer_with_intents() {
        assert_eq!(ConfirmKind::Quit.yes(), AppIntent::Quit);
        assert_eq!(ConfirmKind::Quit.no(), None);
        assert_eq!(ConfirmKind::Quit.question(), "Quit Kika? (y/n)");
        assert_eq!(
            PromptKind::GoToFile.submit("b".into()),
            AppIntent::GoToFile("b".into())
        );
        assert_eq!(
            PromptKind::Command.submit("c".into()),
            AppIntent::CommandPromptAnswered("c".into())
        );
    }
}
