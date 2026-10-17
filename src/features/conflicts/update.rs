//! The conflict view's `update` handlers: opening a conflicted file, its
//! load landing, moving between blocks, picking a side and writing the
//! file, staging it once resolved, and handing it to the merge tool.

use super::ConflictsIntent;
use super::keys;
use super::state::{Choice, ConflictView, PendingPick};
use crate::confirm_question::{Answer, ConfirmQuestion, Then};
use crate::effect::{Effect, Tag};
use crate::intent::AppIntent;
use crate::keymap::KeyScope;
use crate::model::{AppState, Context, Focus};
use crate::op::GitOp;
use crate::popup::{ErrorPopup, Popup};
use crate::refresh::Selection;
use kika_git::path::RepoPath;

/// The conflict view's half of `update`: core's one `AppIntent::Conflicts`
/// arm lands here. A free function, so its name cannot clash with handlers.
pub(crate) fn update(state: &mut AppState, intent: ConflictsIntent) -> Vec<Effect> {
    match intent {
        ConflictsIntent::Loaded { tag, path, result } => state.conflict_loaded(tag, path, result),
        ConflictsIntent::Next => state.with_conflict(|view, rows| view.move_by(1, rows)),
        ConflictsIntent::Prev => state.with_conflict(|view, rows| view.move_by(-1, rows)),
        ConflictsIntent::Pick(choice) => return state.pick_conflict(choice),
        ConflictsIntent::ResolvedWritten { tag, path, result } => {
            return state.resolved_written(tag, path, result);
        }
        ConflictsIntent::StageResolved => return state.stage_resolved(),
        ConflictsIntent::OpenMergeTool => return state.open_merge_tool(),
    }
    Vec::new()
}

/// The conflict view: loading a conflicted file, picking a side of each
/// block, writing the file after each pick, and staging it at the end.
impl AppState {
    /// Show the selected conflicted file's blocks in the Diff pane's
    /// place, and load it. A user effect, so the busy indicator shows.
    pub(crate) fn open_conflict(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        let Some(path) = self.files.selected_row().map(|f| f.path.clone()) else {
            return vec![];
        };
        let seq = self.take_seq();
        self.focus = Focus::Conflict(ConflictView::new(path.clone(), seq));
        let tag = self.user_tag(seq);
        vec![Effect::LoadConflict { tag, path }]
    }

    pub(crate) fn conflict_loaded(
        &mut self,
        tag: Tag,
        path: RepoPath,
        result: Result<(Vec<u8>, usize), String>,
    ) {
        // Every user effect reports back once, stale or not.
        self.user_done(tag);
        let rows = self.pane_rows();
        let Focus::Conflict(view) = &mut self.focus else {
            return;
        };
        if tag.generation != self.generation || view.seq != Some(tag.seq) || view.path != path {
            return;
        }
        view.loaded(result, rows);
    }

    /// Change the conflict view's cursor, given the pane's rows.
    pub(crate) fn with_conflict(&mut self, change: impl FnOnce(&mut ConflictView, usize)) {
        let rows = self.pane_rows();
        if let Focus::Conflict(view) = &mut self.focus {
            change(view, rows);
        }
    }

    /// Keep `choice` for the selected block and write the file at once,
    /// unpicked blocks keeping their markers; then on to the next
    /// unpicked block. The write takes the operation slot, so a second
    /// pick waits for it.
    pub(crate) fn pick_conflict(&mut self, choice: Choice) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if let Some(reason) = keys::pick_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let rows = self.pane_rows();
        let Focus::Conflict(view) = &mut self.focus else {
            return vec![];
        };
        let Some(expected) = view.loaded.as_ref().map(|l| l.on_disk.clone()) else {
            return vec![];
        };
        let block = view.block;
        let Some(previous) = view.choices.get(block).copied() else {
            return vec![];
        };
        view.choices[block] = Some(choice);
        let content = view
            .loaded
            .as_ref()
            .map(|l| l.file.render(&view.choices))
            .unwrap_or_default();
        let path = view.path.clone();
        view.pending = Some(PendingPick { block, previous });
        view.next_unpicked(rows);
        self.clear_op_error();
        // The guard above refuses while an op runs, so the slot is free.
        let Some(tag) = self.take_op() else {
            return vec![];
        };
        vec![Effect::WriteResolved {
            tag,
            path,
            expected,
            content,
        }]
    }

    /// A pick was written, or not. Written, the status reloads, and once
    /// every block is picked the user is asked to stage the file. Not
    /// written, the pick is undone, as the file still is.
    pub(crate) fn resolved_written(
        &mut self,
        tag: Tag,
        path: RepoPath,
        result: Result<Vec<u8>, String>,
    ) -> Vec<Effect> {
        self.release_op(tag);
        if tag.generation != self.generation {
            return vec![];
        }
        let rows = self.pane_rows();
        // Only the view that made the pick: one reopened since has read
        // the file afresh, and its own bytes and blocks stand.
        let view = match &mut self.focus {
            Focus::Conflict(view) if view.path == path && view.pending.is_some() => Some(view),
            _ => None,
        };
        match result {
            Ok(content) => {
                // Asked again after any later pick: answering no leaves
                // the row conflicted, which Files will not stage, so a
                // pick is the way back to the question.
                let resolved = view.is_some_and(|view| {
                    view.pending = None;
                    if let Some(loaded) = &mut view.loaded {
                        loaded.on_disk = content;
                    }
                    view.resolved()
                });
                self.status_selection = Selection::KeepPath;
                let effects = self.refresh_status();
                if resolved {
                    self.popups.push(Popup::Question(stage_resolved_question()));
                }
                effects
            }
            Err(message) => {
                if let Some(view) = view
                    && let Some(PendingPick { block, previous }) = view.pending.take()
                {
                    if let Some(choice) = view.choices.get_mut(block) {
                        *choice = previous;
                    }
                    view.block = block;
                    view.scroll_to_block(rows);
                }
                self.push_error(ErrorPopup::new(
                    "Could not write the resolved file",
                    &message,
                    "The pick was undone. Close this, then try again.",
                ));
                vec![]
            }
        }
    }

    /// Stage the file the conflict view resolved, which marks it resolved
    /// for git. `stage_disabled` refuses conflicted rows, so this does not
    /// go through it.
    pub(crate) fn stage_resolved(&mut self) -> Vec<Effect> {
        let Focus::Conflict(view) = &self.focus else {
            return vec![];
        };
        let op = GitOp::Stage(vec![view.path.clone()]);
        self.start_op(Ok(Some(op)))
    }

    /// Hand the terminal to `git mergetool` for the conflicted file, from
    /// the Files panel or the conflict view, which closes: the tool
    /// rewrites the file under it.
    pub(crate) fn open_merge_tool(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if let Some(reason) = keys::mergetool_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let path = match &self.focus {
            Focus::Conflict(view) => view.path.clone(),
            _ => match self.files.selected_row() {
                Some(entry) => entry.path.clone(),
                None => return vec![],
            },
        };
        let effects = self.run_op_where_it_belongs(GitOp::MergeTool(path));
        if !effects.is_empty() && matches!(self.focus, Focus::Conflict(_)) {
            self.focus = Focus::side(self.expanded);
        }
        effects
    }
}

/// "Stage it?", asked once every block of the conflict view's file is
/// picked. `y` or Enter answers: staging loses nothing.
pub(crate) fn stage_resolved_question() -> ConfirmQuestion {
    ConfirmQuestion {
        scope: KeyScope::Confirm,
        title: "Stage resolved file".into(),
        text: "Every conflict in this file is resolved. Stage it? (y/n)".into(),
        answer: Answer::YOrEnter,
        then: Then::Intent(Box::new(AppIntent::Conflicts(
            ConflictsIntent::StageResolved,
        ))),
        range: false,
        head: None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::AppIntent;
    use crate::keymap::{self, KeyScope};
    use crate::layout;
    use crate::test_support::*;
    use crossterm::event::KeyCode;

    // --- The conflict view ---

    const CONFLICT: &[u8] = b"a\n<<<<<<< HEAD\nours 1\n=======\ntheirs 1\n>>>>>>> other\nb\n\
<<<<<<< HEAD\nours 2\n=======\ntheirs 2\n>>>>>>> other\n";

    /// `CONFLICT` with only its second block, as it stays after the
    /// first is picked.
    const SECOND_BLOCK: &[u8] = b"<<<<<<< HEAD\nours 2\n=======\ntheirs 2\n>>>>>>> other\n";

    /// Files listing conflicted `c.txt` first, after Enter on it: the
    /// conflict view and its load's tag.
    fn conflict_opened() -> (AppState, Tag) {
        let (mut state, _) = with_files(vec![conflicted("c.txt"), changed("a.rs", '.', 'M')], 24);
        let busy = state.busy_count();
        match press(&mut state, KeyCode::Enter).as_slice() {
            [Effect::LoadConflict { tag, path }] => {
                assert_eq!(path, &RepoPath::from("c.txt"));
                assert_eq!(state.busy_count(), busy + 1, "a user load");
                (state, *tag)
            }
            other => panic!("expected one LoadConflict, got {other:?}"),
        }
    }

    /// `conflict_opened`, with `CONFLICT` read.
    fn conflict_shown() -> AppState {
        let (mut state, tag) = conflict_opened();
        state.update(AppIntent::Conflicts(ConflictsIntent::Loaded {
            tag,
            path: "c.txt".into(),
            result: Ok((CONFLICT.to_vec(), 7)),
        }));
        state
    }

    fn conflict(state: &AppState) -> &ConflictView {
        match &state.focus {
            Focus::Conflict(view) => view,
            other => panic!("expected the conflict view, got {other:?}"),
        }
    }

    /// The one write in `effects`: its tag, what it expects on disk and
    /// what it writes.
    fn write_of(effects: &[Effect]) -> (Tag, Vec<u8>, Vec<u8>) {
        match effects {
            [
                Effect::WriteResolved {
                    tag,
                    path,
                    expected,
                    content,
                },
            ] => {
                assert_eq!(path, &RepoPath::from("c.txt"));
                (*tag, expected.clone(), content.clone())
            }
            other => panic!("expected one WriteResolved, got {other:?}"),
        }
    }

    fn written(tag: Tag, result: Result<Vec<u8>, String>) -> AppIntent {
        AppIntent::Conflicts(ConflictsIntent::ResolvedWritten {
            tag,
            path: "c.txt".into(),
            result,
        })
    }

    #[test]
    fn enter_on_a_conflicted_row_opens_the_conflict_view_in_the_diffs_place() {
        let (state, _) = conflict_opened();
        assert_eq!(state.panel_focus(), KeyScope::Conflict);
        assert_eq!(state.focus.panel(), layout::Panel::Diff);
        assert_eq!(conflict(&state).loaded, None);

        let state = conflict_shown();
        let view = conflict(&state);
        assert_eq!(view.block_count(), 2);
        assert_eq!(view.choices, [None, None]);
        assert_eq!(view.seq, None);
        assert_eq!(state.busy_count(), 0);
    }

    #[test]
    fn a_stale_load_is_dropped() {
        let (mut state, tag) = conflict_opened();
        let stale = Tag {
            seq: tag.seq + 100,
            ..tag
        };
        state.update(AppIntent::Conflicts(ConflictsIntent::Loaded {
            tag: stale,
            path: "c.txt".into(),
            result: Ok((CONFLICT.to_vec(), 7)),
        }));
        assert_eq!(conflict(&state).loaded, None);
        assert_eq!(conflict(&state).seq, Some(tag.seq));
    }

    #[test]
    fn o_writes_our_side_at_once_and_moves_to_the_next_block() {
        let mut state = conflict_shown();
        let (tag, expected, content) = write_of(&press_char(&mut state, 'o'));
        assert_eq!(expected, CONFLICT);
        assert_eq!(content, [&b"a\nours 1\nb\n"[..], SECOND_BLOCK].concat());
        assert_eq!(state.held_seq(), Some(tag.seq));
        assert!(tag.user);
        let view = conflict(&state);
        assert_eq!(view.choices, [Some(Choice::Ours), None]);
        assert_eq!(view.block, 1);
    }

    #[test]
    fn a_second_pick_while_the_write_runs_is_refused() {
        let mut state = conflict_shown();
        press_char(&mut state, 'o');
        assert_eq!(press_char(&mut state, 't'), vec![]);
        assert_eq!(state.notice, Some(keymap::OP_RUNNING.into()));
        assert_eq!(
            state.update(AppIntent::Conflicts(ConflictsIntent::Pick(Choice::Theirs))),
            vec![]
        );
        assert_eq!(conflict(&state).choices, [Some(Choice::Ours), None]);
    }

    #[test]
    fn once_every_block_is_written_y_stages_the_file_and_returns_to_files() {
        let mut state = conflict_shown();
        let (tag, _, first) = write_of(&press_char(&mut state, 'o'));
        let effects = state.update(written(tag, Ok(first.clone())));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::LoadStatus { .. })),
            "{effects:?}"
        );
        assert_eq!(state.popups, [], "one block is left");
        assert_eq!(state.held_seq(), None);

        let (tag, expected, content) = write_of(&press_char(&mut state, 't'));
        assert_eq!(expected, first, "the bytes the first write left");
        assert_eq!(content, b"a\nours 1\nb\ntheirs 2\n");
        state.update(written(tag, Ok(content.clone())));
        assert_eq!(state.popups, [Popup::Question(stage_resolved_question())]);
        assert_eq!(conflict(&state).loaded.as_ref().unwrap().on_disk, content);

        // After no, a later pick asks again: Files will not stage the row.
        press_char(&mut state, 'n');
        assert_eq!(state.popups, []);
        let (again, _, both) = write_of(&press_char(&mut state, 'b'));
        state.update(written(again, Ok(both)));
        assert_eq!(state.popups, [Popup::Question(stage_resolved_question())]);

        let (tag, op) = op_of(&press_char(&mut state, 'y'));
        assert_eq!(op, GitOp::Stage(vec!["c.txt".into()]));
        state.update(finished(tag, op, Ok(())));
        assert_eq!(state.focus, Focus::Files);
    }

    #[test]
    fn a_write_landing_after_the_file_was_reopened_leaves_the_new_view_alone() {
        let mut state = conflict_shown();
        let (tag, ..) = write_of(&press_char(&mut state, 'o'));
        press(&mut state, KeyCode::Esc);
        let load = match press(&mut state, KeyCode::Enter).as_slice() {
            [Effect::LoadConflict { tag, .. }] => *tag,
            other => panic!("expected one LoadConflict, got {other:?}"),
        };
        state.update(AppIntent::Conflicts(ConflictsIntent::Loaded {
            tag: load,
            path: "c.txt".into(),
            result: Ok((CONFLICT.to_vec(), 7)),
        }));
        state.update(written(tag, Ok(b"a\nours 1\nb\n".to_vec())));
        let view = conflict(&state);
        assert_eq!(view.loaded.as_ref().unwrap().on_disk, CONFLICT);
        assert_eq!(view.choices, [None, None]);
        assert_eq!(state.held_seq(), None);
    }

    #[test]
    fn a_failed_write_undoes_the_pick_and_says_so() {
        let mut state = conflict_shown();
        let (tag, ..) = write_of(&press_char(&mut state, 'o'));
        let effects = state.update(written(
            tag,
            Err(crate::worktree_file::CHANGED_ON_DISK.to_string()),
        ));
        assert_eq!(effects, vec![]);
        assert_eq!(state.held_seq(), None);
        let view = conflict(&state);
        assert_eq!(view.choices, [None, None]);
        assert_eq!(view.block, 0);
        match state.popups.as_slice() {
            [Popup::Error(error)] => {
                assert_eq!(error.title, "Could not write the resolved file");
                assert!(error.message.contains("changed on disk"), "{error:?}");
            }
            other => panic!("expected an error popup, got {other:?}"),
        }
    }

    #[test]
    fn an_unparsable_file_shows_why_and_nothing_can_be_picked() {
        let (mut state, tag) = conflict_opened();
        state.update(AppIntent::Conflicts(ConflictsIntent::Loaded {
            tag,
            path: "c.txt".into(),
            result: Ok((b"a\n=======\n".to_vec(), 7)),
        }));
        assert!(
            conflict(&state)
                .error
                .as_deref()
                .is_some_and(|e| e.contains("Line 2")),
            "{:?}",
            conflict(&state).error
        );
        for c in ['o', 't', 'b'] {
            assert_eq!(press_char(&mut state, c), vec![], "{c}");
            assert_eq!(state.notice, Some("Nothing to pick".into()), "{c}");
        }
    }

    #[test]
    fn the_diff_settings_wait_while_the_conflict_view_is_open_and_esc_leaves_it() {
        let mut state = conflict_shown();
        assert_eq!(press_char(&mut state, '+'), vec![]);
        assert_eq!(state.notice, Some("Close the conflict view first".into()));
        press_char(&mut state, 'j');
        assert_eq!(conflict(&state).block, 1);
        press_char(&mut state, 'k');
        assert_eq!(conflict(&state).block, 0);
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.focus, Focus::Files);
        assert!(!state.should_quit);
    }

    #[test]
    fn m_hands_a_conflicted_file_to_the_merge_tool_and_is_disabled_on_others() {
        let (mut state, _) = with_files(vec![conflicted("c.txt"), changed("a.rs", '.', 'M')], 24);
        let (tag, op) = attached_of(&press_char(&mut state, 'M'));
        assert_eq!(op, GitOp::MergeTool("c.txt".into()));
        assert_eq!(state.held_seq(), Some(tag.seq));
        let effects = state.update(finished(tag, op, Ok(())));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::LoadStatus { .. })),
            "{effects:?}"
        );
        land_status(
            &mut state,
            &effects,
            vec![conflicted("c.txt"), changed("a.rs", '.', 'M')],
        );

        press_char(&mut state, 'j');
        assert_eq!(press_char(&mut state, 'M'), vec![]);
        assert_eq!(state.notice, Some("Not a conflicted file".into()));
    }

    #[test]
    fn m_in_the_conflict_view_closes_it_and_runs_the_merge_tool() {
        let mut state = conflict_shown();
        let (_, op) = attached_of(&press_char(&mut state, 'M'));
        assert_eq!(op, GitOp::MergeTool("c.txt".into()));
        assert_eq!(state.focus, Focus::Files);
    }
}
