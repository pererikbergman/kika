//! The range question shared by the Branches panel's Local, Remotes and
//! Tags tabs and by the Stash panel: asking before deleting what a range
//! covers, and running the op once `y` answers. Each list builds its own
//! question; this only picks the guard and the builder by kind.

use crate::confirm_question::{Answer, Then};
use crate::effect::Effect;
use crate::features::branches::BranchesTab;
use crate::features::branches::keys::delete_branch_disabled;
use crate::features::branches::remotes::keys::change_remote_disabled;
use crate::features::branches::tags::keys::delete_tag_disabled;
use crate::features::stash::keys::drop_stash_disabled;
use crate::keymap::{KeyScope, Reason};
use crate::model::{AppState, Context};
use crate::op::GitOp;
use crate::popup::{Popup, RangeKind};

impl AppState {
    /// Ask before deleting what the range of `kind`'s tab covers, naming
    /// what goes and counting what stays; with nothing to delete, say so
    /// and ask nothing.
    pub(crate) fn ask_delete_range(&mut self, kind: RangeKind) {
        if self.context != Context::Workspace {
            return;
        }
        let disabled: fn(&AppState) -> Option<Reason> = match kind {
            RangeKind::Branches => delete_branch_disabled,
            RangeKind::Tags => delete_tag_disabled,
            RangeKind::Remotes => change_remote_disabled,
            RangeKind::Stashes => drop_stash_disabled,
        };
        if let Some(reason) = disabled(self) {
            self.notice = Some(reason);
            return;
        }
        let confirm = match kind {
            RangeKind::Branches => self.branches_range_confirm(),
            RangeKind::Tags => self.tags_range_confirm(),
            RangeKind::Remotes => self.remotes_range_confirm(),
            RangeKind::Stashes => self.stashes_range_confirm(),
        };
        if confirm.names.is_empty() {
            self.notice = Some(
                match kind {
                    RangeKind::Branches => "No branch in the selection can be deleted with y",
                    RangeKind::Tags => "No tag in the selection",
                    RangeKind::Remotes => "No remote in the selection can be removed",
                    RangeKind::Stashes => "No stash in the selection",
                }
                .into(),
            );
            return;
        }
        self.popups.push(Popup::Question(confirm.into_question()));
    }

    /// Run the open range question's op, and only if one is open. The
    /// range it was asked over, the one of the list its op names, is done
    /// with.
    pub(crate) fn confirm_delete_range(&mut self) -> Vec<Effect> {
        let Some(question) = self.take_question(KeyScope::DeleteRange, Answer::YOnly) else {
            return vec![];
        };
        let Then::Op(op) = question.then else {
            return vec![];
        };
        match op {
            GitOp::DeleteBranches(_) => self.branches.clear_range(BranchesTab::Local),
            GitOp::DeleteTags(_) => self.branches.clear_range(BranchesTab::Tags),
            GitOp::RemoveRemotes(_) => self.branches.clear_range(BranchesTab::Remotes),
            GitOp::StashDrops(_) => self.stash.list.range_anchor = None,
            _ => {}
        }
        self.start_op(Ok(Some(op)))
    }
}

#[cfg(test)]
mod tests {
    use crate::git::branches::BranchName;
    use crate::git::commit_files::CommitId;
    use crate::git::tags::TagName;
    use crate::model::AppState;
    use crate::op::GitOp;
    use crate::popup::{DeleteRangeConfirm, Popup, RangeKind};
    use crate::test_support::{on_commit, op_of};

    /// A state with the Local, Tags and Stash ranges all started.
    fn with_ranges() -> AppState {
        let mut state = on_commit();
        state.branches.list.range_anchor = Some(BranchName::from("a"));
        state.branches.tags.range_anchor = Some(TagName(b"v1".to_vec()));
        state.stash.list.range_anchor = CommitId::parse("abc");
        state
    }

    /// Which of the Local, Tags and Stash ranges are still there.
    fn ranges(state: &AppState) -> [bool; 3] {
        [
            state.branches.list.range_anchor.is_some(),
            state.branches.tags.range_anchor.is_some(),
            state.stash.list.range_anchor.is_some(),
        ]
    }

    #[test]
    fn a_yes_forgets_only_the_range_of_the_list_it_deletes_from() {
        let cases = [
            (
                RangeKind::Branches,
                GitOp::DeleteBranches(vec!["a".into()]),
                [false, true, true],
            ),
            (
                RangeKind::Tags,
                GitOp::DeleteTags(vec![]),
                [true, false, true],
            ),
            (
                RangeKind::Remotes,
                GitOp::RemoveRemotes(vec![]),
                [true, true, true],
            ),
            (
                RangeKind::Stashes,
                GitOp::StashDrops(vec![]),
                [true, true, false],
            ),
        ];
        for (kind, op, left) in cases {
            let mut state = with_ranges();
            let question = DeleteRangeConfirm {
                kind,
                names: vec!["a".into()],
                skipped: vec![],
                op: op.clone(),
            }
            .into_question();
            state.popups = vec![Popup::Question(question)];
            let (_, ran) = op_of(&state.confirm_delete_range());
            assert_eq!(ran, op, "{kind:?}");
            assert_eq!(ranges(&state), left, "{kind:?}");
            assert_eq!(state.popups, [], "{kind:?}");
        }
    }
}
