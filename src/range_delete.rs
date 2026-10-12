//! The range question shared by the Branches panel's Local, Remotes and
//! Tags tabs and by the Stash panel: asking before deleting what a range
//! covers, and running the op once `y` answers. Each list builds its own
//! question; this only picks the guard and the builder by kind.

use crate::effect::Effect;
use crate::features::branches::BranchesTab;
use crate::features::branches::remotes::keys::change_remote_disabled;
use crate::features::branches::tags::keys::delete_tag_disabled;
use crate::features::stash::keys::drop_stash_disabled;
use crate::keymap::{self, Reason};
use crate::model::{AppState, Context};
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
            RangeKind::Branches => keymap::delete_branch_disabled,
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
        self.popups.push(Popup::DeleteRange(confirm));
    }

    /// Run the open range question's op, and only if one is open. The
    /// range it was asked over is done with.
    pub(crate) fn confirm_delete_range(&mut self) -> Vec<Effect> {
        if !matches!(self.popups.last(), Some(Popup::DeleteRange(_))) {
            return vec![];
        }
        let Some(Popup::DeleteRange(confirm)) = self.popups.pop() else {
            return vec![];
        };
        match confirm.kind {
            RangeKind::Branches => self.branches.clear_range(BranchesTab::Local),
            RangeKind::Tags => self.branches.clear_range(BranchesTab::Tags),
            RangeKind::Remotes => self.branches.clear_range(BranchesTab::Remotes),
            RangeKind::Stashes => self.stash.range_anchor = None,
        }
        self.start_op(Ok(Some(confirm.op)))
    }
}
