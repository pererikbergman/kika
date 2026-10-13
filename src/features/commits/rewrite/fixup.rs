//! Fixing an older commit from the Commits panel: finding which commit the
//! staged lines come from, committing them as a `fixup!` of the selected
//! commit, and folding every `fixup!` commit into its target.

use super::fixup_base::{FixupBase, candidates_notice};
use super::{autosquash_disabled, autosquash_plan, create_fixup_disabled, find_base_disabled};
use crate::confirm_question::{Answer, ConfirmQuestion, Then};
use crate::effect::{Effect, Tag};
use crate::git::commit_files::CommitId;
use crate::intent::AppIntent;
use crate::keymap::KeyScope;
use crate::model::{AppState, Context};
use crate::op::GitOp;
use crate::popup::{ErrorPopup, Popup};

impl AppState {
    /// Commit the staged change as a `fixup!` of the selected commit. Git
    /// writes the message from the raw subject; hooks and signing apply as
    /// for any commit, so it is probed like one.
    pub(crate) fn create_fixup(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if let Some(reason) = create_fixup_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let row = self.commits.rows.get(self.commits.selected);
        let Some(target) = row.and_then(|row| CommitId::parse(&row.oid)) else {
            self.notice = Some("Not a commit id".into());
            return vec![];
        };
        let op = GitOp::Commit {
            message: None,
            stage_all: false,
            amend: false,
            no_verify: false,
            fixup: Some(target),
        };
        self.start_op_as(Ok(Some(op)), |tag, op| Effect::ProbeCommit { tag, op })
    }

    /// Ask before folding the fixups: they may be far from the cursor.
    pub(crate) fn ask_autosquash(&mut self) {
        if self.context != Context::Workspace {
            return;
        }
        if let Some(reason) = autosquash_disabled(self) {
            self.notice = Some(reason);
            return;
        }
        self.popups.push(Popup::Question(autosquash_question()));
    }

    /// Fold the fixups, as the question asked. The plan is made again from
    /// the log as it is now, so a reload while asking is safe.
    pub(crate) fn autosquash(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        let plan = match autosquash_disabled(self) {
            Some(reason) => Err(reason),
            None => autosquash_plan(self),
        };
        match plan {
            Ok(plan) => self.run_op_where_it_belongs(GitOp::InteractiveRebase(plan)),
            Err(reason) => {
                self.notice = Some(reason);
                vec![]
            }
        }
    }

    /// Look for the commits the staged lines come from.
    pub(crate) fn find_fixup_base(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if let Some(reason) = find_base_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let seq = self.take_seq();
        self.commits.fixup_base_seq = Some(seq);
        vec![Effect::FindFixupBase {
            tag: self.user_tag(seq),
        }]
    }

    /// Move the cursor to the one commit the staged lines come from, or
    /// say which commits they come from when there are several, since
    /// picking one would be a guess.
    pub(crate) fn fixup_base_found(&mut self, tag: Tag, result: Result<FixupBase, String>) {
        self.user_done(tag);
        if tag.generation != self.generation || self.commits.fixup_base_seq != Some(tag.seq) {
            return;
        }
        self.commits.fixup_base_seq = None;
        let base = match result {
            Ok(base) => base,
            Err(message) => {
                self.push_error(ErrorPopup::new(
                    "Could not find the fixup base",
                    &message,
                    "Nothing was changed. Fix the cause above, then try again.",
                ));
                return;
            }
        };
        let notice = match base {
            FixupBase::None => "No commit touched the staged lines".to_string(),
            FixupBase::One(id) => self.select_fixup_base(&id),
            FixupBase::Many(ids) => {
                let shorts: Vec<String> = self
                    .log_order(ids)
                    .iter()
                    .map(|id| self.commits.short_oid(id))
                    .collect();
                candidates_notice(&shorts)
            }
        };
        self.notice = Some(notice.into());
    }

    /// Put the cursor on `id`'s log row when it shows, and say which
    /// commit it is.
    fn select_fixup_base(&mut self, id: &CommitId) -> String {
        let commits = &mut self.commits;
        let short = commits.short_oid(id);
        let Some(at) = commits.rows.iter().position(|row| row.oid == id.as_str()) else {
            return format!(
                "Fixup base: {short} (not loaded; move down to load more, then look again)"
            );
        };
        let subject = commits.rows[at].subject.clone();
        if !commits.visible().contains(&at) {
            return format!("Fixup base: {short} {subject} (hidden by the filter)");
        }
        commits.selected = at;
        commits.range_anchor = None;
        commits.todo_selected = None;
        format!("Fixup base: {short} {subject}")
    }

    /// `ids` newest first as the log lists them; any not loaded follow in
    /// the order they came.
    fn log_order(&self, mut ids: Vec<CommitId>) -> Vec<CommitId> {
        let rows = &self.commits.rows;
        let place = |id: &CommitId| rows.iter().position(|row| row.oid == id.as_str());
        // A stable sort keeps the unloaded ones in their order.
        ids.sort_by_key(|id| place(id).unwrap_or(usize::MAX));
        ids
    }
}

/// "Fold every fixup! commit?" It rewrites commits the user may not be
/// looking at, so only `y` answers.
pub(crate) fn autosquash_question() -> ConfirmQuestion {
    ConfirmQuestion {
        scope: KeyScope::AutosquashConfirm,
        title: "Autosquash".into(),
        text: "Fold every fixup! commit into the commit it fixes? (y/n)".into(),
        answer: Answer::YOnly,
        then: Then::Intent(Box::new(AppIntent::ConfirmAutosquash)),
        range: false,
        head: None,
    }
}
