//! The commit popup's side of `AppState`: opening it to commit or amend,
//! reading `HEAD`'s message for an amend, submitting it through the
//! terminal probe, the message back from the external editor, and leaving
//! the popup when git is done.

use super::CommitEditor;
use super::CommitIntent;
use crate::effect::{Effect, Tag};
use crate::handoff::HandOff;
use crate::intent::AppIntent;
use crate::model::{AppState, CommitFlow, Context};
use crate::op::GitOp;
use crate::popup::ErrorPopup;
use crate::prompts::{PromptFacts, needs_terminal};

/// The commit popup's half of `update`: core's one `AppIntent::Commit` arm
/// lands here. A free function, so its name cannot clash with handlers.
pub(crate) fn update(state: &mut AppState, intent: CommitIntent) -> Vec<Effect> {
    match intent {
        CommitIntent::Open => state.open_commit(),
        CommitIntent::ConfirmStageAll => {
            if state.commit == CommitFlow::ConfirmStageAll {
                state.commit = CommitFlow::Editing(CommitEditor::new(true));
            }
        }
        CommitIntent::Cancel => state.cancel_commit(),
        CommitIntent::Discard => state.discard_commit(),
        CommitIntent::Resume => state.resume_commit(),
        CommitIntent::Submit => return state.submit_commit(),
        CommitIntent::Interrupt => return state.interrupt_commit(),
        CommitIntent::ToggleField => {
            if let CommitFlow::Editing(editor) = &mut state.commit {
                editor.toggle_field();
            }
        }
        CommitIntent::OpenAmend => return state.open_amend(),
        CommitIntent::HeadMessageLoaded { tag, result } => state.head_message_loaded(tag, result),
        CommitIntent::ToggleNoVerify => {
            if let CommitFlow::Editing(editor) = &mut state.commit {
                editor.no_verify = !editor.no_verify;
            }
        }
        CommitIntent::EditExternally => {
            if let CommitFlow::Editing(editor) = &state.commit {
                let initial = editor.message();
                return vec![Effect::HandOff(HandOff::EditCommitMessage { initial })];
            }
        }
        CommitIntent::MessageEdited { result } => {
            return state.commit_message_edited(result);
        }
        CommitIntent::DismissError => state.dismiss_commit_error(),
        CommitIntent::Probed { tag, op, facts } => {
            return state.commit_probed(tag, op, facts);
        }
    }
    Vec::new()
}

impl AppState {
    /// Open the commit popup: straight to the editor when something is
    /// staged, else ask whether to stage everything first.
    pub(crate) fn open_commit(&mut self) {
        if self.context != Context::Workspace || self.commit != CommitFlow::Closed {
            return;
        }
        if let Some(reason) = super::keys::commit_disabled(self) {
            self.notice = Some(reason);
            return;
        }
        let staged = self
            .files
            .rows
            .iter()
            .any(|f| !matches!(f.index, '.' | '?'));
        self.commit = if staged {
            CommitFlow::Editing(CommitEditor::new(false))
        } else {
            CommitFlow::ConfirmStageAll
        };
    }

    /// Open the popup to amend `HEAD`, once its message is read. Nothing
    /// is asked about staging: with nothing staged, only the message changes.
    pub(crate) fn open_amend(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace || self.commit != CommitFlow::Closed {
            return vec![];
        }
        if let Some(reason) = super::keys::amend_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let seq = self.take_seq();
        self.commit = CommitFlow::LoadingAmend { seq };
        vec![Effect::LoadHeadMessage {
            tag: self.user_tag(seq),
        }]
    }

    pub(crate) fn head_message_loaded(&mut self, tag: Tag, result: Result<String, String>) {
        self.user_done(tag);
        let current = CommitFlow::LoadingAmend { seq: tag.seq };
        if tag.generation != self.generation || self.commit != current {
            return;
        }
        match result {
            Ok(message) => self.commit = CommitFlow::Editing(CommitEditor::amend(&message)),
            Err(message) => {
                self.commit = CommitFlow::Closed;
                self.push_error(ErrorPopup::new(
                    "Could not read the last commit",
                    &message,
                    "Nothing was changed. Fix the cause above, then try again.",
                ));
            }
        }
    }

    pub(crate) fn submit_commit(&mut self) -> Vec<Effect> {
        let CommitFlow::Editing(editor) = &self.commit else {
            return vec![];
        };
        if editor.subject.trim().is_empty() {
            self.notice = Some("Write a subject first".into());
            return vec![];
        }
        // Git keeps `HEAD`'s own message, byte for byte, when it is untouched.
        let message = (!editor.message_unchanged()).then(|| editor.message());
        self.start_commit(message)
    }

    /// Commit the editor's choices with `message`, and wait for git with
    /// the text kept.
    fn start_commit(&mut self, message: Option<String>) -> Vec<Effect> {
        let CommitFlow::Editing(editor) = &self.commit else {
            return vec![];
        };
        let editor = editor.clone();
        // A bisect may have begun while the editor was open; the text stays.
        if self.bisect_refuses() {
            return vec![];
        }
        let op = GitOp::Commit {
            message,
            stage_all: editor.stage_all,
            amend: editor.amend.is_some(),
            no_verify: editor.no_verify,
            fixup: None,
        };
        // Whether it needs the terminal is read first; see `commit_probed`.
        let effects = self.start_op_as(Ok(Some(op)), |tag, op| Effect::ProbeCommit { tag, op });
        if !effects.is_empty() {
            self.commit = CommitFlow::Running(editor);
        }
        effects
    }

    /// Run the probed commit on the terminal when it may ask something,
    /// else detached. The op slot and busy count stay as they are until
    /// `OpFinished`; a probe for another repository or op ends the commit
    /// here, giving the message back.
    pub(crate) fn commit_probed(
        &mut self,
        tag: Tag,
        op: GitOp,
        facts: Option<PromptFacts>,
    ) -> Vec<Effect> {
        if tag.generation != self.generation || !self.holds_op(tag) {
            self.release_op(tag);
            self.end_commit(None);
            return vec![];
        }
        if needs_terminal(&op, facts.as_ref(), self.commit_hooks_detached) {
            self.commit_on_terminal = true;
            vec![Effect::HandOff(HandOff::Commit { tag, op })]
        } else {
            vec![Effect::RunOp { tag, op }]
        }
    }

    /// Back from writing the message in the editor: commit it as written,
    /// or keep the popup as it was when there is nothing to commit.
    pub(crate) fn commit_message_edited(
        &mut self,
        result: Result<Option<String>, String>,
    ) -> Vec<Effect> {
        let CommitFlow::Editing(editor) = &self.commit else {
            return vec![];
        };
        match result {
            Ok(Some(message)) => {
                self.commit = CommitFlow::Editing(CommitEditor {
                    stage_all: editor.stage_all,
                    no_verify: editor.no_verify,
                    amend: editor.amend.clone(),
                    ..CommitEditor::from_message(&message)
                });
                self.start_commit(Some(message))
            }
            Ok(None) => {
                self.notice = Some("Empty message; commit aborted".into());
                vec![]
            }
            Err(message) => {
                self.push_error(ErrorPopup::new(
                    "Could not open the editor",
                    &message,
                    "Your message is kept. Check core.editor, $VISUAL or $EDITOR, then try again.",
                ));
                vec![]
            }
        }
    }

    /// Close the popup, or ask first when a typed message would be lost.
    pub(crate) fn cancel_commit(&mut self) {
        let commit = std::mem::replace(&mut self.commit, CommitFlow::Closed);
        self.commit = match commit {
            // An untouched amend has nothing typed to lose.
            CommitFlow::Editing(editor) if !editor.is_blank() && !editor.message_unchanged() => {
                CommitFlow::ConfirmDiscard(editor)
            }
            CommitFlow::ConfirmStageAll
            | CommitFlow::LoadingAmend { .. }
            | CommitFlow::Editing(_) => CommitFlow::Closed,
            other => other,
        };
    }

    /// Discard the kept message when asked to.
    pub(crate) fn discard_commit(&mut self) {
        if matches!(self.commit, CommitFlow::ConfirmDiscard(_)) {
            self.commit = CommitFlow::Closed;
        }
    }

    /// Back to the kept message from the discard question.
    pub(crate) fn resume_commit(&mut self) {
        let commit = std::mem::replace(&mut self.commit, CommitFlow::Closed);
        self.commit = match commit {
            CommitFlow::ConfirmDiscard(editor) => CommitFlow::Editing(editor),
            other => other,
        };
    }

    /// Ctrl+C in the popup: quit while git runs, else cancel.
    pub(crate) fn interrupt_commit(&mut self) -> Vec<Effect> {
        let intent = match self.commit {
            CommitFlow::Running(_) => AppIntent::Quit,
            _ => AppIntent::Commit(CommitIntent::Cancel),
        };
        self.update(intent)
    }

    /// Back to the kept message from a failed commit's output.
    pub(crate) fn dismiss_commit_error(&mut self) {
        let commit = std::mem::replace(&mut self.commit, CommitFlow::Closed);
        self.commit = match commit {
            CommitFlow::Failed { editor, .. } => CommitFlow::Editing(editor),
            other => other,
        };
    }

    /// Leave `Running`: closed on success, `Failed` with the output on
    /// failure, back to `Editing` when there is no result to show.
    pub(crate) fn end_commit(&mut self, result: Option<Result<(), String>>) {
        self.commit_on_terminal = false;
        let commit = std::mem::replace(&mut self.commit, CommitFlow::Closed);
        let CommitFlow::Running(editor) = commit else {
            self.commit = commit;
            return;
        };
        self.commit = match result {
            Some(Ok(())) => CommitFlow::Closed,
            Some(Err(output)) => CommitFlow::Failed { editor, output },
            None => CommitFlow::Editing(editor),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::commit::editor::Field;
    use crate::git::FileEntry;
    use crate::git::status::{BranchStatus, EntryKind, Head};
    use crate::keymap::KeyScope;
    use crate::popup::Popup;
    use crate::test_support::*;
    use crossterm::event::KeyCode;

    #[test]
    fn typing_a_message_and_ctrl_s_runs_the_commit() {
        let mut state = editing();
        let busy = state.busy_count();
        type_text(&mut state, "subj\nline 1\nline 2");

        let (tag, op) = probe_of(&ctrl(&mut state, 's'));

        assert_eq!(
            op,
            GitOp::Commit {
                message: Some("subj\n\nline 1\nline 2\n".into()),
                stage_all: false,
                amend: false,
                no_verify: false,
                fixup: None,
            }
        );
        assert!(tag.user);
        assert_eq!(state.held_seq(), Some(tag.seq));
        assert_eq!(state.busy_count(), busy + 1);
        assert!(matches!(&state.commit, CommitFlow::Running(e) if e.subject == "subj"));
    }

    #[test]
    fn an_empty_subject_is_refused_with_a_notice() {
        let mut state = editing();
        type_text(&mut state, "  ");
        assert_eq!(ctrl(&mut state, 's'), vec![]);
        assert_eq!(state.notice, Some("Write a subject first".into()));
        assert_eq!(state.held_seq(), None);
        assert_eq!(editor(&state).subject, "  ");
    }

    #[test]
    fn with_nothing_staged_c_asks_to_stage_all_and_y_opens_the_editor() {
        let files = vec![changed("a.rs", '.', 'M'), untracked("n.txt")];
        let (mut state, _) = with_files(files, 24);
        press(&mut state, KeyCode::Char('c'));
        assert_eq!(state.commit, CommitFlow::ConfirmStageAll);
        assert_eq!(state.key_focus(), KeyScope::CommitConfirm);

        press(&mut state, KeyCode::Char('y'));
        assert!(editor(&state).stage_all);
        type_text(&mut state, "all");
        let (_, op) = probe_of(&ctrl(&mut state, 's'));
        assert_eq!(
            op,
            GitOp::Commit {
                message: Some("all\n".into()),
                stage_all: true,
                amend: false,
                no_verify: false,
                fixup: None,
            }
        );
    }

    #[test]
    fn enter_also_confirms_staging_everything() {
        let (mut state, _) = with_files(vec![changed("a.rs", '.', 'M')], 24);
        press(&mut state, KeyCode::Char('c'));
        press(&mut state, KeyCode::Enter);
        assert!(editor(&state).stage_all);
    }

    #[test]
    fn n_or_esc_declines_staging_everything_and_nothing_runs() {
        for key in [KeyCode::Char('n'), KeyCode::Esc] {
            let (mut state, _) = with_files(vec![changed("a.rs", '.', 'M')], 24);
            press(&mut state, KeyCode::Char('c'));
            assert_eq!(press(&mut state, key), vec![]);
            assert_eq!(state.commit, CommitFlow::Closed);
            assert!(!state.should_quit);
        }
    }

    #[test]
    fn a_staged_file_opens_the_editor_directly() {
        let state = editing();
        assert!(!editor(&state).stage_all);
        assert_eq!(state.key_focus(), KeyScope::CommitEditor);
    }

    #[test]
    fn c_with_a_conflict_or_no_files_is_disabled_with_a_reason() {
        let conflict = FileEntry {
            kind: EntryKind::Conflicted,
            ..changed("both.rs", 'U', 'U')
        };
        let (mut state, _) = with_files(vec![changed("a.rs", 'M', '.'), conflict], 24);
        press(&mut state, KeyCode::Char('c'));
        assert_eq!(
            state.notice,
            Some("Resolve conflicts before committing".into())
        );
        assert_eq!(state.commit, CommitFlow::Closed);

        let (mut state, _) = with_files(vec![], 24);
        press(&mut state, KeyCode::Char('c'));
        assert_eq!(state.notice, Some("Nothing to commit".into()));

        let (mut state, _) = with_files(three_files(), 24);
        state.hold_op(5);
        press(&mut state, KeyCode::Char('c'));
        assert_eq!(state.notice, Some("An operation is running".into()));
        assert_eq!(state.commit, CommitFlow::Closed);
    }

    #[test]
    fn q_and_question_mark_are_typed_and_esc_cancels_while_editing() {
        let mut state = editing();
        type_text(&mut state, "q?");
        assert!(!state.should_quit);
        assert!(!state.help_open);
        assert_eq!(editor(&state).subject, "q?");

        // The message is not empty, so Esc asks before throwing it away.
        assert_eq!(press(&mut state, KeyCode::Esc), vec![]);
        assert!(matches!(state.commit, CommitFlow::ConfirmDiscard(_)));
        assert_eq!(state.key_focus(), KeyScope::CommitDiscard);
        assert!(!state.should_quit);
    }

    #[test]
    fn esc_on_an_empty_message_closes_at_once() {
        let mut state = editing();
        type_text(&mut state, " \n ");
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.commit, CommitFlow::Closed);
    }

    #[test]
    fn y_discards_the_message_and_n_or_esc_goes_back_to_it() {
        for key in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut state = editing();
            type_text(&mut state, "keep me");
            press(&mut state, KeyCode::Esc);
            assert_eq!(press(&mut state, key), vec![]);
            assert_eq!(editor(&state).subject, "keep me");
        }
        let mut state = editing();
        type_text(&mut state, "drop me");
        press(&mut state, KeyCode::Esc);
        // Other keys, Enter included, neither discard nor quit.
        press(&mut state, KeyCode::Enter);
        press(&mut state, KeyCode::Char('q'));
        assert!(matches!(state.commit, CommitFlow::ConfirmDiscard(_)));
        assert!(!state.should_quit);
        press(&mut state, KeyCode::Char('y'));
        assert_eq!(state.commit, CommitFlow::Closed);
    }

    #[test]
    fn ctrl_c_in_the_editor_asks_before_discarding_a_typed_message() {
        let mut state = editing();
        type_text(&mut state, "abc");
        assert_eq!(ctrl(&mut state, 'c'), vec![]);
        assert!(!state.should_quit);
        assert!(matches!(state.commit, CommitFlow::ConfirmDiscard(_)));

        // Nothing typed: nothing to lose, so it just closes.
        let mut state = editing();
        ctrl(&mut state, 'c');
        assert_eq!(state.commit, CommitFlow::Closed);
        assert!(!state.should_quit);
    }

    #[test]
    fn interrupt_commit_cancels_while_editing_and_quits_while_running() {
        let mut state = editing();
        type_text(&mut state, "abc");
        assert_eq!(
            state.update(AppIntent::Commit(CommitIntent::Interrupt)),
            vec![]
        );
        assert!(matches!(state.commit, CommitFlow::ConfirmDiscard(_)));
        assert!(!state.should_quit);

        let (mut state, _, _) = submitted();
        assert_eq!(
            state.update(AppIntent::Commit(CommitIntent::Interrupt)),
            vec![Effect::InterruptGit]
        );
        assert!(state.should_quit);
    }

    #[test]
    fn ctrl_c_quits_while_the_commit_runs_and_interrupts_git() {
        let (mut state, _, _) = submitted();
        assert_eq!(ctrl(&mut state, 'c'), vec![Effect::InterruptGit]);
        assert!(state.should_quit);
    }

    #[test]
    fn tab_moves_between_the_fields() {
        let mut state = editing();
        type_text(&mut state, "s");
        press(&mut state, KeyCode::Tab);
        assert_eq!(editor(&state).field, Field::Body);
        state.update(AppIntent::Commit(CommitIntent::ToggleField));
        assert_eq!(editor(&state).field, Field::Subject);
    }

    /// Submit "subj" with a body and return the op's tag and op.
    fn submitted() -> (AppState, Tag, GitOp) {
        let mut state = editing();
        type_text(&mut state, "subj\nbody");
        let (tag, op) = probe_of(&ctrl(&mut state, 's'));
        (state, tag, op)
    }

    #[test]
    fn keys_do_nothing_while_the_commit_runs() {
        let (mut state, _, _) = submitted();
        let before = state.commit.clone();
        type_text(&mut state, "xq");
        press(&mut state, KeyCode::Esc);
        assert_eq!(ctrl(&mut state, 's'), vec![]);
        assert_eq!(state.commit, before);
        assert_eq!(state.notice, Some("An operation is running".into()));
        assert!(!state.should_quit);
    }

    #[test]
    fn a_successful_commit_closes_the_popup_and_refreshes() {
        let (mut state, tag, op) = submitted();
        let effects = state.update(finished(tag, op, Ok(())));
        assert_eq!(state.commit, CommitFlow::Closed);
        assert_eq!(state.held_seq(), None);
        assert!(
            matches!(
                effects.as_slice(),
                [
                    Effect::LoadStatus { .. },
                    Effect::LoadBranches { .. },
                    Effect::LoadRemotes { .. },
                    Effect::LoadTags { .. },
                    Effect::LoadCommits { skip: 0, .. },
                    Effect::LoadMarkers { .. },
                    Effect::LoadBranchTips { .. },
                ]
            ),
            "{effects:?}"
        );
    }

    #[test]
    fn a_failed_commit_shows_its_output_and_esc_returns_to_the_kept_message() {
        let (mut state, tag, op) = submitted();
        let effects = state.update(finished(tag, op, Err("lint failed".into())));

        assert_eq!(state.op_error(), None);
        assert!(
            matches!(
                effects.as_slice(),
                [
                    Effect::LoadStatus { .. },
                    Effect::LoadBranches { .. },
                    Effect::LoadRemotes { .. },
                    Effect::LoadTags { .. },
                    Effect::LoadCommits { skip: 0, .. },
                    Effect::LoadMarkers { .. },
                    Effect::LoadBranchTips { .. },
                ]
            ),
            "{effects:?}"
        );
        match &state.commit {
            CommitFlow::Failed { editor, output } => {
                assert_eq!(output, "lint failed");
                assert_eq!(editor.subject, "subj");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(state.key_focus(), KeyScope::CommitError);
        // Other keys are dropped while the error shows.
        press(&mut state, KeyCode::Char('q'));
        assert!(!state.should_quit);

        press(&mut state, KeyCode::Esc);
        let editor = editor(&state);
        assert_eq!(editor.subject, "subj");
        assert_eq!(editor.body, ["body"]);
    }

    #[test]
    fn enter_also_dismisses_the_commit_error() {
        let (mut state, tag, op) = submitted();
        state.update(finished(tag, op, Err("no".into())));
        press(&mut state, KeyCode::Enter);
        editor(&state);
    }

    #[test]
    fn a_stale_commit_result_returns_to_editing_without_a_refresh() {
        let (mut state, tag, op) = submitted();
        let busy = state.busy_count();
        state.generation += 1;
        let effects = state.update(finished(tag, op, Err("boom".into())));
        assert_eq!(effects, vec![]);
        assert_eq!(state.busy_count(), busy - 1);
        assert_eq!(state.held_seq(), None);
        assert_eq!(editor(&state).subject, "subj");
    }

    fn probed(tag: Tag, op: &GitOp, facts: Option<PromptFacts>) -> AppIntent {
        AppIntent::Commit(CommitIntent::Probed {
            tag,
            op: op.clone(),
            facts,
        })
    }

    fn signing() -> Option<PromptFacts> {
        Some(PromptFacts {
            signs: true,
            hooks: vec![],
        })
    }

    /// Commits as each way of submitting makes them: typed, amended with
    /// HEAD's message, with hooks skipped, and from the editor.
    fn submissions() -> Vec<(AppState, Tag, GitOp)> {
        let mut amend = amending();
        let (tag, op) = probe_of(&ctrl(&mut amend, 's'));
        let amended = (amend, tag, op);

        let mut skip = editing();
        ctrl(&mut skip, 'n');
        type_text(&mut skip, "subj");
        let (tag, op) = probe_of(&ctrl(&mut skip, 's'));
        let skipped = (skip, tag, op);

        let mut edited = editing();
        let effects = edited.update(AppIntent::Commit(CommitIntent::MessageEdited {
            result: Ok(Some("x\n".into())),
        }));
        let (tag, op) = probe_of(&effects);
        let from_editor = (edited, tag, op);

        vec![submitted(), amended, skipped, from_editor]
    }

    #[test]
    fn a_submitted_commit_is_probed_once_and_counts_as_running() {
        for (state, tag, op) in submissions() {
            assert!(matches!(op, GitOp::Commit { .. }), "{op:?}");
            assert!(tag.user);
            assert_eq!(state.held_seq(), Some(tag.seq));
            assert!(matches!(state.commit, CommitFlow::Running(_)));
        }
    }

    #[test]
    fn a_commit_that_signs_is_handed_the_terminal() {
        for (mut state, tag, op) in submissions() {
            let busy = state.busy_count();
            let effects = state.update(probed(tag, &op, signing()));
            assert_eq!(
                effects,
                vec![Effect::HandOff(HandOff::Commit {
                    tag,
                    op: op.clone()
                })]
            );
            assert_eq!(state.busy_count(), busy, "OpFinished still settles busy");
            assert_eq!(state.held_seq(), Some(tag.seq));
            assert!(matches!(state.commit, CommitFlow::Running(_)));
        }
    }

    fn hooks_only() -> Option<PromptFacts> {
        Some(PromptFacts {
            signs: false,
            hooks: vec!["pre-commit", "commit-msg", "post-commit"],
        })
    }

    #[test]
    fn with_hooks_detached_a_commit_that_only_runs_hooks_stays_detached() {
        for (mut state, tag, op) in submissions() {
            state.commit_hooks_detached = true;
            let effects = state.update(probed(tag, &op, hooks_only()));
            assert_eq!(effects, vec![Effect::RunOp { tag, op }]);
            assert!(!state.commit_on_terminal);
        }
    }

    #[test]
    fn without_hooks_detached_a_commit_with_hooks_is_handed_the_terminal() {
        let (mut state, tag, op) = submitted();
        assert!(!state.commit_hooks_detached, "off by default");
        assert_eq!(
            state.update(probed(tag, &op, hooks_only())),
            vec![Effect::HandOff(HandOff::Commit { tag, op })]
        );
        assert!(state.commit_on_terminal);
    }

    #[test]
    fn with_hooks_detached_a_commit_that_signs_is_still_handed_the_terminal() {
        for (mut state, tag, op) in submissions() {
            state.commit_hooks_detached = true;
            let effects = state.update(probed(tag, &op, signing()));
            assert_eq!(effects, vec![Effect::HandOff(HandOff::Commit { tag, op })]);
            assert!(state.commit_on_terminal);
        }
    }

    #[test]
    fn only_a_handed_over_commit_is_marked_as_on_the_terminal_until_it_ends() {
        let (mut state, tag, op) = submitted();
        assert!(!state.commit_on_terminal);
        state.update(probed(tag, &op, signing()));
        assert!(state.commit_on_terminal);
        state.update(finished(tag, op, Err("no".into())));
        assert!(!state.commit_on_terminal);

        let (mut state, tag, op) = submitted();
        state.update(probed(tag, &op, Some(PromptFacts::default())));
        assert!(!state.commit_on_terminal);
    }

    #[test]
    fn a_commit_with_no_signing_and_no_hooks_runs_detached() {
        for (mut state, tag, op) in submissions() {
            let busy = state.busy_count();
            let effects = state.update(probed(tag, &op, Some(PromptFacts::default())));
            assert_eq!(effects, vec![Effect::RunOp { tag, op }]);
            assert_eq!(state.busy_count(), busy);
            assert_eq!(state.held_seq(), Some(tag.seq));
        }
    }

    #[test]
    fn a_failed_probe_hands_the_terminal_over() {
        let (mut state, tag, op) = submitted();
        assert_eq!(
            state.update(probed(tag, &op, None)),
            vec![Effect::HandOff(HandOff::Commit { tag, op })]
        );
    }

    #[test]
    fn a_handed_over_commit_ends_like_a_detached_one() {
        let (mut state, tag, op) = submitted();
        state.update(probed(tag, &op, signing()));
        let effects = state.update(finished(tag, op, Ok(())));
        assert_eq!(state.commit, CommitFlow::Closed);
        assert_eq!(state.held_seq(), None);
        assert!(!effects.is_empty(), "the status is reloaded");
    }

    #[test]
    fn a_stale_probe_gives_the_message_back_and_frees_the_slot() {
        for (mut state, tag, op) in submissions() {
            state.generation += 1;
            let effects = state.update(probed(tag, &op, signing()));
            assert_eq!(effects, vec![]);
            assert_eq!(state.busy_count(), 0);
            assert_eq!(state.held_seq(), None);
            editor(&state);
        }
    }

    /// `A` on a clean tree with commits: the popup is loading HEAD's message.
    fn loading_amend() -> (AppState, Tag) {
        let (mut state, _) = with_files(vec![], 24);
        let busy = state.busy_count();
        let tag = head_load(&press(&mut state, KeyCode::Char('A')));
        assert!(tag.user);
        assert_eq!(state.busy_count(), busy + 1);
        assert_eq!(state.commit, CommitFlow::LoadingAmend { seq: tag.seq });
        assert_eq!(state.key_focus(), KeyScope::CommitLoading);
        (state, tag)
    }

    fn head_loaded(tag: Tag, result: Result<&str, &str>) -> AppIntent {
        AppIntent::Commit(CommitIntent::HeadMessageLoaded {
            tag,
            result: result.map(Into::into).map_err(Into::into),
        })
    }

    /// Amending HEAD's "subj\n\nbody", the popup open on it.
    fn amending() -> AppState {
        let (mut state, tag) = loading_amend();
        let busy = state.busy_count();
        assert_eq!(state.update(head_loaded(tag, Ok("subj\n\nbody"))), vec![]);
        assert_eq!(state.busy_count(), busy - 1);
        state
    }

    #[test]
    fn a_shift_a_loads_heads_message_into_the_popup() {
        let state = amending();
        let editor = editor(&state);
        assert_eq!(editor.subject, "subj");
        assert_eq!(editor.body, ["body"]);
        assert!(editor.amend.is_some());
        assert!(!editor.stage_all, "an amend never stages everything");
        assert_eq!(state.key_focus(), KeyScope::CommitEditor);
    }

    #[test]
    fn an_untouched_amend_keeps_heads_message() {
        let mut state = amending();
        let (_, op) = probe_of(&ctrl(&mut state, 's'));
        assert_eq!(
            op,
            GitOp::Commit {
                message: None,
                stage_all: false,
                amend: true,
                no_verify: false,
                fixup: None,
            }
        );
        assert!(matches!(state.commit, CommitFlow::Running(_)));
    }

    #[test]
    fn an_edited_amend_commits_the_new_message() {
        let mut state = amending();
        type_text(&mut state, "!");
        let (_, op) = probe_of(&ctrl(&mut state, 's'));
        assert_eq!(
            op,
            GitOp::Commit {
                message: Some("subj!\n\nbody\n".into()),
                stage_all: false,
                amend: true,
                no_verify: false,
                fixup: None,
            }
        );
    }

    #[test]
    fn a_failed_head_load_closes_the_popup_and_says_why() {
        let (mut state, tag) = loading_amend();
        state.update(head_loaded(tag, Err("fatal: bad HEAD")));
        assert_eq!(state.commit, CommitFlow::Closed);
        match state.popups.last() {
            Some(Popup::Error(error)) => {
                assert_eq!(error.title, "Could not read the last commit")
            }
            other => panic!("expected an error popup, got {other:?}"),
        }
    }

    #[test]
    fn esc_while_loading_closes_and_the_late_result_is_ignored() {
        let (mut state, tag) = loading_amend();
        let busy = state.busy_count();
        assert_eq!(press(&mut state, KeyCode::Esc), vec![]);
        assert_eq!(state.commit, CommitFlow::Closed);
        assert!(!state.should_quit);

        state.update(head_loaded(tag, Ok("late")));
        assert_eq!(state.commit, CommitFlow::Closed);
        assert_eq!(state.busy_count(), busy - 1);
    }

    #[test]
    fn a_stale_head_load_is_ignored_but_still_lowers_busy() {
        let (mut state, tag) = loading_amend();
        // Another user load in flight, so two decrements stay above zero.
        state.set_busy(state.busy_count() + 1);
        let busy = state.busy_count();
        state.update(head_loaded(
            Tag {
                seq: tag.seq + 99,
                ..tag
            },
            Ok("other"),
        ));
        assert_eq!(state.commit, CommitFlow::LoadingAmend { seq: tag.seq });
        state.update(head_loaded(
            Tag {
                generation: tag.generation + 1,
                ..tag
            },
            Ok("other"),
        ));
        assert_eq!(state.commit, CommitFlow::LoadingAmend { seq: tag.seq });
        assert_eq!(state.busy_count(), busy - 2);
    }

    #[test]
    fn esc_on_an_untouched_amend_closes_at_once() {
        let mut state = amending();
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.commit, CommitFlow::Closed);
    }

    #[test]
    fn a_shift_a_is_refused_on_an_unborn_branch_or_while_an_op_runs() {
        let (mut state, _) = with_files(vec![], 24);
        state.branch = Some(BranchStatus {
            head: Head::Unborn("main".into()),
            ..on_branch("main")
        });
        assert_eq!(press(&mut state, KeyCode::Char('A')), vec![]);
        assert_eq!(state.notice, Some("No commit to amend".into()));
        assert_eq!(state.commit, CommitFlow::Closed);

        let (mut state, _) = with_files(three_files(), 24);
        state.hold_op(7);
        assert_eq!(press(&mut state, KeyCode::Char('A')), vec![]);
        assert_eq!(state.notice, Some("An operation is running".into()));
        assert_eq!(state.commit, CommitFlow::Closed);
    }

    #[test]
    fn open_amend_does_nothing_with_a_popup_already_open() {
        let mut state = editing();
        assert_eq!(
            state.update(AppIntent::Commit(CommitIntent::OpenAmend)),
            vec![]
        );
        assert!(editor(&state).amend.is_none());
    }

    #[test]
    fn ctrl_n_toggles_skipping_hooks_and_the_commit_carries_it() {
        let mut state = editing();
        assert!(!editor(&state).no_verify);
        ctrl(&mut state, 'n');
        assert!(editor(&state).no_verify);
        ctrl(&mut state, 'n');
        assert!(!editor(&state).no_verify);
        ctrl(&mut state, 'n');
        type_text(&mut state, "subj");
        let (_, op) = probe_of(&ctrl(&mut state, 's'));
        assert!(
            matches!(
                op,
                GitOp::Commit {
                    no_verify: true,
                    ..
                }
            ),
            "{op:?}"
        );

        let (_, _, op) = submitted();
        assert!(
            matches!(
                op,
                GitOp::Commit {
                    no_verify: false,
                    ..
                }
            ),
            "{op:?}"
        );
    }

    #[test]
    fn ctrl_e_hands_the_current_message_to_the_editor() {
        let mut state = editing();
        type_text(&mut state, "subj\nbody");
        let before = state.commit.clone();
        assert_eq!(
            ctrl(&mut state, 'e'),
            vec![Effect::HandOff(HandOff::EditCommitMessage {
                initial: "subj\n\nbody\n".into()
            })]
        );
        assert_eq!(state.commit, before);
    }

    #[test]
    fn a_message_from_the_editor_commits_right_away_keeping_the_choices() {
        let mut state = amending();
        ctrl(&mut state, 'n');
        let busy = state.busy_count();
        let effects = state.update(AppIntent::Commit(CommitIntent::MessageEdited {
            result: Ok(Some("x\n\ny\n".into())),
        }));
        let (tag, op) = probe_of(&effects);
        assert_eq!(
            op,
            GitOp::Commit {
                message: Some("x\n\ny\n".into()),
                stage_all: false,
                amend: true,
                no_verify: true,
                fixup: None,
            }
        );
        assert_eq!(state.held_seq(), Some(tag.seq));
        assert_eq!(state.busy_count(), busy + 1);
        match &state.commit {
            CommitFlow::Running(editor) => {
                assert_eq!(editor.subject, "x");
                assert_eq!(editor.body, ["y"]);
                assert!(editor.no_verify && editor.amend.is_some());
            }
            other => panic!("expected Running, got {other:?}"),
        }
    }

    #[test]
    fn an_emptied_message_aborts_the_commit_and_keeps_the_text() {
        let mut state = editing();
        type_text(&mut state, "keep me");
        let before = state.commit.clone();
        let effects = state.update(AppIntent::Commit(CommitIntent::MessageEdited {
            result: Ok(None),
        }));
        assert_eq!(effects, vec![]);
        assert_eq!(state.commit, before);
        assert_eq!(state.notice, Some("Empty message; commit aborted".into()));
        assert_eq!(state.held_seq(), None);
    }

    #[test]
    fn an_editor_failure_shows_an_error_over_the_kept_text() {
        let mut state = editing();
        type_text(&mut state, "keep me");
        let effects = state.update(AppIntent::Commit(CommitIntent::MessageEdited {
            result: Err("The editor exited with 3".into()),
        }));
        assert_eq!(effects, vec![]);
        assert!(matches!(state.popups.last(), Some(Popup::Error(_))));
        press(&mut state, KeyCode::Esc);
        assert_eq!(editor(&state).subject, "keep me");
    }

    #[test]
    fn an_edited_message_with_no_popup_open_is_dropped() {
        let (mut state, _) = with_files(three_files(), 24);
        let effects = state.update(AppIntent::Commit(CommitIntent::MessageEdited {
            result: Ok(Some("x\n".into())),
        }));
        assert_eq!(effects, vec![]);
        assert_eq!(state.commit, CommitFlow::Closed);
        assert_eq!(state.held_seq(), None);
    }
}
