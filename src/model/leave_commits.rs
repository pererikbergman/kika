//! Checking out or resetting to a commit from a detached `HEAD`: before
//! `HEAD` moves, find the commits nothing else reaches and ask before
//! leaving them to the reflog.

use super::AppState;
use crate::effect::{Effect, Tag};
use crate::git::commit_files::CommitId;
use crate::git::ops::{GitOp, ResetMode};
use crate::git::status::Head;
use crate::popup::{ConfirmKind, ErrorPopup, LeaveCommitsConfirm, Popup, ResetHardConfirm};

impl AppState {
    /// `HEAD`'s commit when it is detached on an id git can use.
    fn detached_head(&self) -> Option<CommitId> {
        match &self.branch.as_ref()?.head {
            Head::Detached(oid) => CommitId::parse(oid),
            _ => None,
        }
    }

    /// Plan `op`, which moves `HEAD` to `target`. On a detached `HEAD`,
    /// first look for the commits it would leave behind; anywhere else, go
    /// on as without the check.
    pub(super) fn move_head(&mut self, op: GitOp, target: CommitId) -> Vec<Effect> {
        let Some(head) = self.detached_head() else {
            return self.move_head_now(op);
        };
        let seq = self.take_seq();
        let tag = self.user_tag(seq);
        vec![Effect::CheckLeftBehind {
            tag,
            op,
            head,
            target,
        }]
    }

    /// What `op` does once nothing would be left behind: a checkout and a
    /// hard reset still ask their own question, soft and mixed run.
    fn move_head_now(&mut self, op: GitOp) -> Vec<Effect> {
        match op {
            GitOp::CheckoutCommit(id) => {
                self.commits.pending = Some(id);
                self.popups
                    .push(Popup::Confirm(ConfirmKind::DetachedCheckout));
                vec![]
            }
            GitOp::Reset {
                ref to,
                mode: ResetMode::Hard,
            } => {
                let short_oid = self.commits.short_oid(to);
                self.popups
                    .push(Popup::ResetHard(ResetHardConfirm { short_oid, op }));
                vec![]
            }
            op => self.start_op(Ok(Some(op))),
        }
    }

    /// Ask before leaving commits behind, or go on if there are none. A
    /// result for a `HEAD` that has since moved, or a failed read, runs
    /// nothing.
    pub(super) fn left_behind_checked(
        &mut self,
        tag: Tag,
        op: GitOp,
        head: CommitId,
        result: Result<Vec<CommitId>, String>,
    ) -> Vec<Effect> {
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        if tag.generation != self.generation {
            return vec![];
        }
        if self.detached_head() != Some(head) {
            self.notice = Some("HEAD moved; try again".into());
            return vec![];
        }
        let left = match result {
            Ok(left) => left,
            Err(message) => {
                self.push_error(ErrorPopup::new(
                    "Could not check for commits left behind",
                    &message,
                    "Close this; HEAD did not move.",
                ));
                return vec![];
            }
        };
        if left.is_empty() {
            return self.move_head_now(op);
        }
        let target = match &op {
            GitOp::CheckoutCommit(id) | GitOp::Reset { to: id, .. } => id,
            _ => return vec![],
        };
        let short_oid = self.commits.short_oid(target);
        let left = left.iter().map(|id| self.commits.short_oid(id)).collect();
        self.popups.push(Popup::LeaveCommits(LeaveCommitsConfirm {
            op,
            short_oid,
            left,
        }));
        vec![]
    }

    /// Run the op the open question carries, and only if one is open.
    pub(super) fn confirm_leave_commits(&mut self) -> Vec<Effect> {
        if !matches!(self.popups.last(), Some(Popup::LeaveCommits(_))) {
            return vec![];
        }
        let Some(Popup::LeaveCommits(confirm)) = self.popups.pop() else {
            return vec![];
        };
        if self.confirm_refused(&confirm.op) {
            return vec![];
        }
        self.start_op(Ok(Some(confirm.op)))
    }
}
