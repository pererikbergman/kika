//! Popups that sit on one stack over the workspace: menus, confirmations,
//! one-line prompts and errors. Pure types; the model owns the stack and
//! each popup hands its result back as an intent.

use std::borrow::Cow;

use crossterm::event::{KeyCode, KeyModifiers};

use crate::commit_editor::{byte_at, char_len};
use crate::git::diff::terminal_safe;
use crate::git::ops::GitOp;
use crate::git::repo_state::Operation;
use crate::intent::AppIntent;
use crate::keymap::KeyPress;
use crate::menu::MenuId;
use crate::recent_list::RecentList;
use crate::submodule_list::SubmoduleList;

/// One entry on the popup stack. The top one takes the keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Popup {
    Menu { id: MenuId, cursor: usize },
    Confirm(ConfirmKind),
    Prompt { kind: PromptKind, input: LineInput },
    Error(ErrorPopup),
    Discard(DiscardConfirm),
    DeleteBranch(DeleteBranchConfirm),
    ResetHard(ResetHardConfirm),
    DropStash(DropStashConfirm),
    Submodules(SubmoduleList),
    Recent(RecentList),
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
}

impl DiscardWhat {
    pub fn title(self) -> &'static str {
        match self {
            DiscardWhat::File => "Discard file",
            DiscardWhat::Untracked => "Delete file",
            DiscardWhat::Hunk => "Discard hunk",
            DiscardWhat::Lines => "Discard lines",
        }
    }

    pub fn question(self) -> &'static str {
        match self {
            DiscardWhat::File => "Discard all unstaged changes to this file? (y/n)",
            DiscardWhat::Untracked => "Delete this untracked file from disk? (y/n)",
            DiscardWhat::Hunk => "Discard this hunk from the working tree? (y/n)",
            DiscardWhat::Lines => "Discard these lines from the working tree? (y/n)",
        }
    }
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
            ConfirmKind::Abort(Operation::Merge) => "Abort merge",
            ConfirmKind::Abort(Operation::Rebase) => "Abort rebase",
            ConfirmKind::Abort(Operation::CherryPick) => "Abort cherry-pick",
            ConfirmKind::Abort(Operation::Revert) => "Abort revert",
            ConfirmKind::Abort(Operation::Bisect) => "Abort bisect",
            ConfirmKind::StageResolved => "Stage resolved file",
            ConfirmKind::DetachedCheckout => "Detached checkout",
        }
    }

    pub fn question(self) -> &'static str {
        match self {
            ConfirmKind::Quit => "Quit Kika? (y/n)",
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
            // Never asked: a bisect is not resumable.
            ConfirmKind::Abort(Operation::Bisect) => "Abort the bisect? (y/n)",
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
            ConfirmKind::Abort(op) => AppIntent::ConfirmAbort(op),
            ConfirmKind::StageResolved => AppIntent::StageResolved,
            ConfirmKind::DetachedCheckout => AppIntent::ConfirmCheckoutCommit,
        }
    }

    pub fn no(self) -> Option<AppIntent> {
        match self {
            ConfirmKind::Quit
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
