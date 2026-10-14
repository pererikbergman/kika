//! The op slot: one git mutation at a time, so two quick presses never
//! race for `index.lock`. A running op holds the slot with the sequence
//! of its effect until its result comes back.
//!
//! The slot also keeps the busy count, the user effects still in flight
//! that the status bar shows as loading, and the op error, why the last op
//! was refused. A fetch, pull, push or review fetch holds the slot while
//! git or ssh may ask for a credential, so the credential question lives
//! here too, and quitting answers it and stops git.
//!
//! [`OpSlot`]'s fields are private to this file, so features and tests go
//! through the methods here and cannot hand-copy a take or a release.

use crate::effect::{Effect, Tag};
use crate::features::commits::reflog;
use crate::git::askpass::Secret;
use crate::git::repo_state::Operation;
use crate::layout::SidePanel;
use crate::model::{AppState, Context, Focus};
use crate::op::{GitOp, MergeStrategy};
use crate::popup::{CredentialPrompt, ErrorPopup, Popup};
use crate::refresh;

/// The state behind the op slot, held by `AppState`. Its fields are
/// private so only this file can change them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct OpSlot {
    /// Sequence of the git op in flight. Ops run one at a time so two
    /// quick presses never race for `index.lock`.
    seq: Option<u64>,
    /// User-started effects still in flight, for the loading indicator.
    busy: u32,
    /// The last op's failure, or why one was refused.
    error: Option<String>,
}

/// What the slot holds: queries for guards and views.
impl AppState {
    /// Whether a git op holds the slot, so no other may start.
    pub(crate) fn op_is_running(&self) -> bool {
        self.op_slot.seq.is_some()
    }

    /// Whether `tag`'s effect is the op holding the slot.
    pub(crate) fn holds_op(&self, tag: Tag) -> bool {
        self.op_slot.seq == Some(tag.seq)
    }

    /// Whether a user effect is still in flight, for the loading indicator.
    pub(crate) fn is_busy(&self) -> bool {
        self.op_slot.busy > 0
    }

    /// Why the last op was refused, for the status bar.
    pub(crate) fn op_error(&self) -> Option<&str> {
        self.op_slot.error.as_deref()
    }
}

/// Taking and giving back the slot and the busy count.
impl AppState {
    /// Take the slot if it is free. It does not check the context: the
    /// review fetch takes the slot in review mode.
    pub(crate) fn take_op(&mut self) -> Option<Tag> {
        if self.op_is_running() {
            return None;
        }
        let seq = self.take_seq();
        self.op_slot.seq = Some(seq);
        Some(self.user_tag(seq))
    }

    /// Give back what `tag` took. Every user effect reports once, stale or
    /// not, so the busy count drops even when the slot has moved on.
    pub(crate) fn release_op(&mut self, tag: Tag) {
        self.user_done(tag);
        if self.holds_op(tag) {
            self.op_slot.seq = None;
        }
    }

    /// A tag counted as busy until its answer lands.
    pub(crate) fn user_tag(&mut self, seq: u64) -> Tag {
        self.raise_busy();
        Tag {
            generation: self.generation,
            seq,
            user: true,
        }
    }

    /// A tag for a load that a user may or may not have asked for: only a
    /// user's counts as busy.
    pub(crate) fn effect_tag(&mut self, seq: u64, user: bool) -> Tag {
        if user {
            return self.user_tag(seq);
        }
        Tag {
            generation: self.generation,
            seq,
            user: false,
        }
    }

    /// A user effect's answer landed, stale or not.
    pub(crate) fn user_done(&mut self, tag: Tag) {
        if tag.user {
            self.lower_busy();
        }
    }

    /// Count a user effect that carries no tag, a custom command with
    /// captured output, as busy until [`Self::lower_busy`].
    pub(crate) fn raise_busy(&mut self) {
        self.op_slot.busy += 1;
    }

    /// The untagged user effect [`Self::raise_busy`] counted is done.
    pub(crate) fn lower_busy(&mut self) {
        self.op_slot.busy = self.op_slot.busy.saturating_sub(1);
    }

    /// Forget a refusal; it named something no longer on screen.
    pub(crate) fn clear_op_error(&mut self) {
        self.op_slot.error = None;
    }

    /// Say why an op a feature checked itself cannot go ahead.
    pub(crate) fn set_op_error(&mut self, message: String) {
        self.op_slot.error = Some(message);
    }

    /// What quitting tells git: no to an open credential question first,
    /// so nothing waits on the user, then stop, since a hook or signing
    /// step may wait forever.
    pub(crate) fn quit_interrupts(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        if self.close_credential() {
            effects.push(Effect::AnswerCredential(None));
        }
        if self.op_is_running() {
            effects.push(Effect::InterruptGit);
        }
        effects
    }

    /// Run the operation a stage key chose. Only one runs at a time, and
    /// only on the local workspace.
    pub(crate) fn start_op(&mut self, op: Result<Option<GitOp>, String>) -> Vec<Effect> {
        self.start_op_as(op, |tag, op| Effect::RunOp { tag, op })
    }

    /// [`Self::start_op`], with `effect` saying what work starts it: a
    /// commit is probed first, everything else runs at once.
    pub(crate) fn start_op_as(
        &mut self,
        op: Result<Option<GitOp>, String>,
        effect: fn(Tag, GitOp) -> Effect,
    ) -> Vec<Effect> {
        if self.context != Context::Workspace || self.op_is_running() {
            return vec![];
        }
        self.clear_op_error();
        let op = match op {
            Ok(Some(op)) => op,
            Ok(None) => return vec![],
            Err(message) => {
                self.set_op_error(message);
                return vec![];
            }
        };
        let Some(tag) = self.take_op() else {
            return vec![];
        };
        vec![effect(tag, op)]
    }

    pub(crate) fn op_finished(
        &mut self,
        tag: Tag,
        op: GitOp,
        result: Result<(), String>,
    ) -> Vec<Effect> {
        // As with status loads: every user effect reports once, stale or not.
        self.release_op(tag);
        // A fixup is made from the Commits panel, not the commit popup, so
        // its failure shows as any other op's.
        let commit = matches!(op, GitOp::Commit { fixup: None, .. });
        if matches!(op, GitOp::Commit { fixup: Some(_), .. }) {
            self.commit_on_terminal = false;
        }
        if tag.generation != self.generation {
            // The result is for another repository; give the message back.
            if commit {
                self.end_commit(None);
            }
            return vec![];
        }
        // The patch is in history now; a failure keeps it to try again.
        let rewritten = matches!(op, GitOp::CustomPatch { .. }) && result.is_ok();
        // The resolved file is staged: nothing is left for the view.
        if let (Focus::Conflict(view), GitOp::Stage(paths), Ok(())) = (&self.focus, &op, &result)
            && paths == std::slice::from_ref(&view.path)
        {
            self.focus = Focus::Files;
            self.expanded = SidePanel::Files;
        }
        if commit {
            // A commit's failure shows in its own popup, not the status bar.
            self.end_commit(Some(result));
        } else {
            // A failed git command gets a popup; the op error is for refusals.
            self.clear_op_error();
            match result {
                // Git stops before committing a squash, so say what is left.
                Ok(())
                    if matches!(
                        op,
                        GitOp::Merge {
                            strategy: MergeStrategy::Squash,
                            ..
                        }
                    ) =>
                {
                    self.notice = Some("Squashed changes are staged; commit them".into());
                }
                Ok(()) => {}
                Err(message) => {
                    let error = if matches!(op, GitOp::ApplyEdited { .. }) {
                        self.edited_hunk_error(&message)
                    } else {
                        ErrorPopup::new(failure_title(&op), &message, failure_next(&op))
                    };
                    self.push_error(error);
                }
            }
        }
        let mut effects = if rewritten {
            self.patch_rewritten()
        } else {
            vec![]
        };
        // Refresh even after a failure: a failed `add` of several paths can
        // still have changed the index.
        effects.extend(self.apply_plan(refresh::plan(&op)));
        effects
    }
}

/// The questions git and ssh ask while a fetch, pull, push or review
/// fetch runs: the credential popup that sync and review share.
impl AppState {
    /// Show the question, or say no at once to one nobody is waiting for.
    pub(crate) fn credential_asked(
        &mut self,
        tag: Tag,
        prompt: String,
        confirm: bool,
    ) -> Vec<Effect> {
        if self.current_remote(tag).is_none() && !self.review_fetch_running(tag) {
            return vec![Effect::AnswerCredential(None)];
        }
        self.popups
            .push(Popup::Credential(CredentialPrompt::new(prompt, confirm)));
        vec![]
    }

    /// Close the question on top and send its answer.
    pub(crate) fn credential_answered(&mut self, answer: Option<Secret>) -> Vec<Effect> {
        if !matches!(self.popups.last(), Some(Popup::Credential(_))) {
            return vec![];
        }
        self.popups.pop();
        vec![Effect::AnswerCredential(answer)]
    }

    /// Take every credential question off the stack; whether there was one.
    pub(crate) fn close_credential(&mut self) -> bool {
        let before = self.popups.len();
        self.popups
            .retain(|popup| !matches!(popup, Popup::Credential(_)));
        self.popups.len() != before
    }
}

/// Test helpers that set up or read the slot without running an op.
#[cfg(test)]
impl AppState {
    /// Pretend the op with sequence `seq` holds the slot.
    pub(crate) fn hold_op(&mut self, seq: u64) {
        self.op_slot.seq = Some(seq);
    }

    /// Pretend no op holds the slot.
    pub(crate) fn free_op(&mut self) {
        self.op_slot.seq = None;
    }

    /// Pretend `n` user effects are in flight.
    pub(crate) fn set_busy(&mut self, n: u32) {
        self.op_slot.busy = n;
    }

    /// How many user effects are in flight.
    pub(crate) fn busy_count(&self) -> u32 {
        self.op_slot.busy
    }

    /// The sequence of the op holding the slot.
    pub(crate) fn held_seq(&self) -> Option<u64> {
        self.op_slot.seq
    }
}

/// What the error popup says to do after a failed operation: what was
/// reloaded, then try again.
pub(crate) fn failure_next(op: &GitOp) -> &'static str {
    match op {
        // The run aborts itself, so there is nothing to resolve.
        GitOp::CustomPatch { .. } => {
            "Nothing was changed and the patch is kept. Fix the cause above, then try again."
        }
        // A pick that the branch already has stops as empty, not on a
        // conflict; skip drops it.
        GitOp::CherryPick(_) => {
            "If git stopped on conflicts or an empty pick, resolve what it says, then continue, skip or abort from the Status panel."
        }
        GitOp::Merge { .. }
        | GitOp::Rebase { .. }
        | GitOp::InteractiveRebase(_)
        | GitOp::Revert(_)
        | GitOp::Continue(_)
        | GitOp::Skip(_) => {
            "If git stopped on conflicts, resolve them, then continue or abort from the Status panel."
        }
        GitOp::RenameBranch { .. }
        | GitOp::DeleteBranch { .. }
        | GitOp::DeleteBranches(_)
        | GitOp::SetUpstream { .. }
        | GitOp::UnsetUpstream(_)
        | GitOp::AddRemote { .. }
        | GitOp::RenameRemote { .. }
        | GitOp::RemoveRemote(_)
        | GitOp::RemoveRemotes(_)
        | GitOp::CreateTag { .. }
        | GitOp::DeleteTag { .. }
        | GitOp::DeleteTags(_) => {
            "The branch list was reloaded. Fix the cause above, then try again."
        }
        GitOp::MergeTool(_) => {
            "The file list was reloaded. Check merge.tool, or resolve the file in your editor."
        }
        // A conflicted apply or pop leaves the entry in the list.
        GitOp::StashApply(_) | GitOp::StashPop(_) => {
            "The file list was reloaded. If git stopped on conflicts, resolve them in the Files panel; the stash is kept."
        }
        GitOp::StashDrop(_) | GitOp::StashDrops(_) | GitOp::StashRename { .. } => {
            "The stash list was reloaded. Fix the cause above, then try again."
        }
        _ => "The file list was reloaded. Fix the cause above, then try again.",
    }
}

/// What the error popup calls a failed operation.
pub(crate) fn failure_title(op: &GitOp) -> &'static str {
    match op {
        GitOp::Stage(_) => "Could not stage",
        GitOp::Unstage(_) => "Could not unstage",
        GitOp::StageAll => "Could not stage everything",
        GitOp::UnstageAll => "Could not unstage everything",
        GitOp::Commit { fixup: Some(_), .. } => "Could not create the fixup",
        GitOp::Commit { amend: true, .. } => "Could not amend",
        GitOp::Commit { .. } => "Could not commit",
        GitOp::ApplyCached { reverse: false, .. } => "Could not stage the change",
        GitOp::ApplyCached { reverse: true, .. } => "Could not unstage the change",
        GitOp::DiscardWorktree(_) => "Could not discard the changes",
        GitOp::DeleteUntracked(_) => "Could not delete the file",
        GitOp::DiscardFiles { .. } => "Could not discard the files",
        GitOp::ApplyWorktree { .. } => "Could not discard the change",
        GitOp::ApplyEdited { .. } => "Could not stage the edited hunk",
        GitOp::SubmoduleInit(_) => "Could not initialize the submodule",
        GitOp::SubmoduleUpdate(_) => "Could not update the submodule",
        GitOp::Checkout(_) => "Could not check out the branch",
        GitOp::CreateBranch { .. } | GitOp::CreateBranchAt { .. } => "Could not create the branch",
        GitOp::RenameBranch { .. } => "Could not rename the branch",
        GitOp::DeleteBranch { .. } => "Could not delete the branch",
        GitOp::DeleteBranches(_) => "Could not delete every branch",
        GitOp::Merge { .. } => "Could not merge",
        GitOp::Rebase { .. } | GitOp::InteractiveRebase(_) => "Could not rebase",
        GitOp::CustomPatch { .. } => "Could not apply the custom patch",
        GitOp::CheckoutCommit(_) => "Could not check out the commit",
        GitOp::Revert(_) => "Could not revert",
        GitOp::CherryPick(_) => "Could not cherry-pick",
        GitOp::Reset { .. } => "Could not reset",
        GitOp::Continue(Operation::Merge) => "Could not continue the merge",
        GitOp::Continue(Operation::Rebase) => "Could not continue the rebase",
        GitOp::Continue(Operation::CherryPick) => "Could not continue the cherry-pick",
        GitOp::Continue(Operation::Revert) => "Could not continue the revert",
        GitOp::Continue(Operation::Bisect) => "Could not continue the bisect",
        GitOp::Skip(_) => "Could not skip",
        GitOp::Abort(Operation::Merge) => "Could not abort the merge",
        GitOp::Abort(Operation::Rebase) => "Could not abort the rebase",
        GitOp::Abort(Operation::CherryPick) => "Could not abort the cherry-pick",
        GitOp::Abort(Operation::Revert) => "Could not abort the revert",
        GitOp::Abort(Operation::Bisect) => "Could not abort the bisect",
        GitOp::MergeTool(_) => "The merge tool did not resolve the file",
        GitOp::StashPush { .. } => "Could not stash",
        GitOp::StashApply(_) => "Could not apply the stash",
        GitOp::StashPop(_) => "Could not pop the stash",
        GitOp::StashDrop(_) => "Could not drop the stash",
        GitOp::StashDrops(_) => "Could not drop the stashes",
        GitOp::StashRename { .. } => "Could not rename the stash",
        GitOp::CheckoutTracking { .. } => "Could not check out the remote branch",
        GitOp::SetUpstream { .. } => "Could not set the upstream",
        GitOp::UnsetUpstream(_) => "Could not unset the upstream",
        GitOp::AddRemote { .. } => "Could not add the remote",
        GitOp::RenameRemote { .. } => "Could not rename the remote",
        GitOp::RemoveRemote(_) => "Could not remove the remote",
        GitOp::RemoveRemotes(_) => "Could not remove every remote",
        GitOp::CreateTag { .. } => "Could not create the tag",
        GitOp::DeleteTag { .. } => "Could not delete the tag",
        GitOp::DeleteTags(_) => "Could not delete every tag",
        GitOp::BisectMark { .. } => "Could not mark the commit",
        GitOp::BisectReset => "Could not reset the bisect",
        GitOp::Travel {
            direction: reflog::Direction::Undo,
            ..
        } => "Could not undo",
        GitOp::Travel {
            direction: reflog::Direction::Redo,
            ..
        } => "Could not redo",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::files::FilesIntent;
    use crate::intent::AppIntent;
    use crate::test_support::*;

    fn workspace() -> AppState {
        AppState::new(Context::Workspace)
    }

    fn stage_a() -> GitOp {
        GitOp::Stage(paths(&["a.rs"]))
    }

    fn user(generation: u64, seq: u64) -> Tag {
        Tag {
            generation,
            seq,
            user: true,
        }
    }

    fn ask(state: &mut AppState, tag: Tag) -> Vec<Effect> {
        state.update(AppIntent::CredentialAsked {
            tag,
            prompt: "Password:".into(),
            confirm: false,
        })
    }

    #[test]
    fn a_free_slot_is_taken_once() {
        let mut state = workspace();
        state.generation = 3;

        let tag = state.take_op().expect("the slot is free");

        assert_eq!((tag.generation, tag.user), (3, true));
        assert_eq!(state.held_seq(), Some(tag.seq));
        assert!(state.holds_op(tag) && state.op_is_running());
        assert_eq!(state.busy_count(), 1);

        let before = (state.held_seq(), state.busy_count(), state.next_seq);
        assert_eq!(state.take_op(), None);
        assert_eq!(
            (state.held_seq(), state.busy_count(), state.next_seq),
            before
        );
    }

    #[test]
    fn take_op_ignores_the_context_but_start_op_needs_the_workspace() {
        let mut review = AppState::new(Context::Review { number: None });
        assert_eq!(review.start_op(Ok(Some(stage_a()))), vec![]);
        assert!(!review.op_is_running());
        assert!(!review.is_busy());

        let tag = review.take_op().expect("the review fetch takes the slot");
        assert!(review.holds_op(tag));
    }

    #[test]
    fn a_refused_op_says_why_and_takes_nothing() {
        let mut state = workspace();

        assert_eq!(state.start_op(Err("Nothing to stage".into())), vec![]);
        assert_eq!(state.op_error(), Some("Nothing to stage"));
        assert!(!state.op_is_running() && !state.is_busy());

        assert_eq!(state.start_op(Ok(None)), vec![]);
        assert_eq!(state.op_error(), None);
        assert!(!state.op_is_running() && !state.is_busy());
    }

    #[test]
    fn a_started_op_runs_with_the_tag_that_holds_the_slot() {
        let mut state = workspace();

        match state.start_op(Ok(Some(stage_a()))).as_slice() {
            [Effect::RunOp { tag, op }] => {
                assert!(state.holds_op(*tag));
                assert_eq!(op, &stage_a());
            }
            other => panic!("expected one RunOp, got {other:?}"),
        }
        assert_eq!(state.busy_count(), 1);
        assert_eq!(state.start_op(Ok(Some(stage_a()))), vec![]);
    }

    #[test]
    fn release_frees_only_the_slot_it_holds_and_never_underflows() {
        let mut state = workspace();
        let tag = state.take_op().unwrap();

        state.release_op(user(tag.generation, tag.seq + 1));
        assert_eq!(
            state.held_seq(),
            Some(tag.seq),
            "another seq leaves the slot"
        );
        assert_eq!(state.busy_count(), 0, "a user tag still lowers busy");

        state.release_op(tag);
        assert_eq!(state.held_seq(), None);
        assert_eq!(state.busy_count(), 0, "busy never underflows");

        state.set_busy(2);
        let background = Tag {
            user: false,
            ..user(0, 99)
        };
        state.release_op(background);
        assert_eq!(state.busy_count(), 2, "a background tag leaves busy alone");
    }

    #[test]
    fn user_tags_count_until_they_are_done() {
        let mut state = workspace();
        let tag = state.user_tag(7);
        assert_eq!(tag, user(state.generation, 7));
        assert!(state.is_busy());

        state.user_done(tag);
        state.user_done(tag);
        assert_eq!(state.busy_count(), 0);
        assert!(!state.op_is_running(), "a user tag is not the slot");
    }

    #[test]
    fn a_user_effect_tag_counts_as_busy_and_a_background_one_does_not() {
        let mut state = workspace();
        state.generation = 5;

        assert_eq!(state.effect_tag(3, true), user(5, 3));
        assert_eq!(state.busy_count(), 1);

        let background = state.effect_tag(4, false);
        assert_eq!(
            background,
            Tag {
                user: false,
                ..user(5, 4)
            }
        );
        assert_eq!(state.busy_count(), 1);
        assert!(!state.op_is_running(), "neither tag is the slot");
    }

    #[test]
    fn a_feature_can_say_why_an_op_was_refused() {
        let mut state = workspace();

        state.set_op_error("Nothing to undo".into());
        assert_eq!(state.op_error(), Some("Nothing to undo"));

        state.clear_op_error();
        assert_eq!(state.op_error(), None);
    }

    #[test]
    fn untagged_busy_counts_up_and_down_without_underflow() {
        let mut state = workspace();

        state.raise_busy();
        assert!(state.is_busy());
        state.lower_busy();
        state.lower_busy();
        assert_eq!(state.busy_count(), 0);
    }

    #[test]
    fn a_failed_op_shows_its_title_and_next_step_and_frees_the_slot() {
        let (mut state, _) = with_files(three_files(), 24);
        let (tag, op) = op_of(&state.update(AppIntent::Files(FilesIntent::ToggleStage)));

        state.update(finished(tag, op.clone(), Err("fatal: boom".into())));

        match state.popups.as_slice() {
            [Popup::Error(error)] => {
                assert_eq!(error.title, failure_title(&op));
                assert_eq!(error.next, failure_next(&op));
            }
            other => panic!("expected one error popup, got {other:?}"),
        }
        assert!(!state.op_is_running());
    }

    #[test]
    fn an_older_generations_result_frees_the_slot_and_shows_nothing() {
        let (mut state, _) = with_files(three_files(), 24);
        let (tag, op) = op_of(&state.update(AppIntent::Files(FilesIntent::ToggleStage)));
        let busy = state.busy_count();
        state.generation += 1;

        assert_eq!(state.update(finished(tag, op, Err("boom".into()))), vec![]);
        assert!(!state.op_is_running());
        assert_eq!(state.busy_count(), busy - 1);
        assert_eq!(state.popups, vec![]);
    }

    #[test]
    fn quitting_answers_the_question_before_stopping_git() {
        let mut state = workspace();
        state.take_op().unwrap();
        state.popups.push(Popup::Credential(CredentialPrompt::new(
            "Password:".into(),
            false,
        )));

        assert_eq!(
            state.update(AppIntent::Quit),
            vec![Effect::AnswerCredential(None), Effect::InterruptGit]
        );
        assert!(state.should_quit);
        assert!(
            !state
                .popups
                .iter()
                .any(|popup| matches!(popup, Popup::Credential(_)))
        );
    }

    #[test]
    fn quitting_with_nothing_running_tells_git_nothing() {
        let mut state = workspace();
        assert_eq!(state.update(AppIntent::Quit), vec![]);
        assert!(state.should_quit);
    }

    #[test]
    fn a_question_nobody_waits_for_is_answered_no_at_once() {
        let mut state = workspace();
        let held = state.take_op().unwrap();
        let other = user(held.generation, held.seq + 1);

        assert_eq!(ask(&mut state, other), vec![Effect::AnswerCredential(None)]);
        assert_eq!(state.popups, vec![]);
    }

    #[test]
    fn an_answer_without_a_question_on_top_sends_nothing() {
        let mut state = workspace();
        assert_eq!(state.update(AppIntent::CredentialAnswered(None)), vec![]);
    }
}
