//! Starting the check before a detached `HEAD` moves: Commits (checkout
//! and reset), Branches (checkout, remote checkout, create) and review
//! (pull request checkout) all ask for the commits nothing else reaches
//! before `HEAD` leaves them, so the check's start, its in-flight state
//! and where its op goes on live in core. The answer is handled by
//! `commits/rewrite` (`left_behind_checked`).

use super::AppState;
use crate::effect::Effect;
use crate::keymap::KeyScope;
use crate::op::GitOp;
use crate::popup::Popup;
use kika_git::commit_files::CommitId;

/// The left-behind check in flight, and where the user was when it
/// started. Its answer only asks if they are still there, so a key meant
/// for something else cannot answer the question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LeftBehindCheck {
    pub seq: u64,
    pub popups: Vec<Popup>,
    pub focus: KeyScope,
}

impl AppState {
    /// Look for the commits `op` would leave behind when it moves the
    /// detached `head` to `target`. `None` leaves the target out: a branch
    /// tip is already one of the refs the check keeps. One check runs at a
    /// time, so two quick presses never stack two questions.
    pub(crate) fn check_left_behind(
        &mut self,
        op: GitOp,
        head: CommitId,
        target: Option<CommitId>,
    ) -> Vec<Effect> {
        if self.left_behind_check.is_some() {
            self.notice = Some("Still checking for commits left behind".into());
            return vec![];
        }
        let seq = self.take_seq();
        let tag = self.user_tag(seq);
        self.left_behind_check = Some(LeftBehindCheck {
            seq,
            popups: self.popups.clone(),
            focus: self.key_focus(),
        });
        vec![Effect::CheckLeftBehind {
            tag,
            op,
            head,
            target,
        }]
    }

    /// Run `op` once the check, or the user's `y`, lets it leave `HEAD`.
    /// A pull request checkout goes through review, which leaves it for
    /// the workspace first; any other op starts here.
    pub(crate) fn run_head_move(&mut self, op: GitOp) -> Vec<Effect> {
        match op {
            GitOp::CheckoutPrBranch { .. } => self.run_pr_checkout(op),
            op => self.start_op(Ok(Some(op))),
        }
    }
}
