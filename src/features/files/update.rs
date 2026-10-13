//! The Files panel's list navigation: moving the selection among the rows
//! the filter shows. Also what Space and `a` stage, and the question `d`
//! asks for a range.

use super::git::{toggle_all, toggle_for, toggle_many};
use crate::effect::Effect;
use crate::git::FileEntry;
use crate::list_filter;
use crate::model::{AppState, Focus};
use crate::op::GitOp;
use crate::popup::{DiscardConfirm, DiscardWhat, Popup};

impl AppState {
    /// Move by `delta` rows among those the filter shows.
    pub(crate) fn move_selection(&mut self, delta: isize) -> Vec<Effect> {
        let Some(current) = self.selected else {
            return vec![];
        };
        let Some(next) = list_filter::step(&self.files_visible(), current, delta) else {
            return vec![];
        };
        if next == current {
            return vec![];
        }
        self.select(next)
    }

    /// Space: stage or unstage the range where it shows, else the selected
    /// file.
    pub(crate) fn toggle_stage(&mut self) -> Vec<Effect> {
        // Only where the range shows; elsewhere Stage is the one file's.
        if self.focus == Focus::Files && self.files_anchor.is_some() {
            return self.stage_range();
        }
        let op = self
            .selected
            .and_then(|i| self.files.get(i))
            .and_then(toggle_for);
        self.start_op(Ok(op))
    }

    /// `a`: stage everything, or unstage everything when all is staged.
    pub(crate) fn toggle_stage_all(&mut self) -> Vec<Effect> {
        self.start_op(toggle_all(&self.files))
    }

    /// Stage every file in the range, or unstage them all when none has
    /// anything left to stage. One op, so one git call.
    fn stage_range(&mut self) -> Vec<Effect> {
        let entries: Vec<&FileEntry> = self.files_range().iter().map(|&i| &self.files[i]).collect();
        let op = toggle_many(&entries);
        if op.is_some() {
            self.files_anchor = None;
        }
        self.start_op(Ok(op))
    }

    /// Ask before discarding the range: the files it can restore and the
    /// untracked ones it would delete, counted in the question.
    pub(crate) fn ask_discard_range(&mut self) {
        let entries: Vec<&FileEntry> = self.files_range().iter().map(|&i| &self.files[i]).collect();
        let Some(op) = GitOp::discard_many(&entries) else {
            self.notice = Some("No unstaged changes to discard".into());
            return;
        };
        let (restore, delete) = match &op {
            GitOp::DiscardWorktree(paths) => (paths.len(), 0),
            GitOp::DeleteUntracked(paths) => (0, paths.len()),
            GitOp::DiscardFiles { restore, delete } => (restore.len(), delete.len()),
            _ => (0, 0),
        };
        self.popups.push(Popup::Discard(DiscardConfirm {
            what: DiscardWhat::Range { restore, delete },
            path: String::new(),
            op,
        }));
    }
}

#[cfg(test)]
mod tests {
    use crate::effect::Effect;
    use crate::features::files::git::toggle_for;
    use crate::git::FileEntry;
    use crate::git::diff::{DiffOptions, DiffRequest};
    use crate::git::status::EntryKind;
    use crate::intent::AppIntent;
    use crate::layout::SidePanel;
    use crate::model::{AppState, Context};
    use crate::op::GitOp;
    use crate::popup::Popup;
    use crate::test_support::*;
    use crossterm::event::KeyCode;

    #[test]
    fn select_next_loads_the_next_file_and_its_chunk_replaces_the_lines() {
        let (mut state, effects) = with_files(three_files(), 24);
        let first = diff_tag(&effects);
        state.update(chunk(first, 3, true));
        assert_eq!(state.diff.lines.len(), 3);

        let effects = state.update(AppIntent::SelectNext);
        let second = diff_tag(&effects);
        assert!(second.seq > first.seq);
        assert_eq!(state.selected, Some(1));
        match &effects[0] {
            Effect::LoadDiff { request, .. } => {
                assert_eq!(
                    request,
                    &DiffRequest::for_entry(&state.files[1], DiffOptions::default())
                )
            }
            other => panic!("{other:?}"),
        }
        // The old lines stay until the new ones arrive.
        assert_eq!(state.diff.lines.len(), 3);

        state.update(chunk(second, 2, true));
        let seq = second.seq;
        assert_eq!(
            state.diff.lines,
            [line(&format!("{seq}:0")), line(&format!("{seq}:1"))]
        );
        assert!(!state.diff.pending);
        assert!(state.diff.done);
    }

    #[test]
    fn selection_stops_at_the_ends_of_the_list() {
        let (mut state, _) = with_files(three_files(), 24);
        assert_eq!(state.update(AppIntent::SelectPrev), vec![]);
        assert_eq!(state.selected, Some(0));

        state.update(AppIntent::SelectNext);
        state.update(AppIntent::SelectNext);
        assert_eq!(state.update(AppIntent::SelectNext), vec![]);
        assert_eq!(state.selected, Some(2));

        diff_tag(&state.update(AppIntent::SelectPrev));
        assert_eq!(state.selected, Some(1));
    }

    #[test]
    fn selection_does_nothing_without_files() {
        let (mut state, effects) = with_files(vec![], 24);
        assert_eq!(effects, vec![Effect::CancelDiff]);
        assert_eq!(state.selected, None);
        assert_eq!(state.update(AppIntent::SelectNext), vec![]);
        assert_eq!(state.update(AppIntent::SelectPrev), vec![]);
    }

    #[test]
    fn space_while_an_op_runs_shows_why_and_does_nothing() {
        let (mut state, _) = with_files(three_files(), 24);
        state.op_seq = Some(99);
        let busy = state.busy;
        assert_eq!(press(&mut state, KeyCode::Char(' ')), vec![]);
        assert_eq!(state.notice, Some("An operation is running".into()));
        assert_eq!(state.busy, busy);
    }

    #[test]
    fn space_without_a_selection_says_so() {
        let (mut state, _) = with_files(vec![], 24);
        assert_eq!(press(&mut state, KeyCode::Char(' ')), vec![]);
        assert_eq!(state.notice, Some("No file selected".into()));
    }

    #[test]
    fn a_with_a_conflict_shows_the_reason_as_a_notice_not_an_error() {
        let conflict = FileEntry {
            kind: EntryKind::Conflicted,
            ..changed("both.rs", 'U', 'U')
        };
        let (mut state, _) = with_files(vec![changed("a.rs", '.', 'M'), conflict], 24);
        assert_eq!(press(&mut state, KeyCode::Char('a')), vec![]);
        assert_eq!(
            state.notice,
            Some("Resolve conflicts before staging everything".into())
        );
        assert_eq!(state.op_error, None);
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn space_on_a_stageable_file_runs_the_op() {
        let (mut state, _) = with_files(three_files(), 24);
        let (_, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        assert_eq!(op, GitOp::Stage(vec!["a.rs".into()]));
        assert_eq!(state.notice, None);
    }

    #[test]
    fn toggle_stage_on_an_unstaged_file_runs_one_user_op_and_raises_busy() {
        let (mut state, _) = with_files(three_files(), 24);
        let busy = state.busy;

        let (tag, op) = op_of(&state.update(AppIntent::ToggleStage));

        assert_eq!(op, GitOp::Stage(vec!["a.rs".into()]));
        assert!(tag.user);
        assert_eq!(tag.generation, state.generation);
        assert_eq!(state.op_seq, Some(tag.seq));
        assert_eq!(state.busy, busy + 1);
    }

    #[test]
    fn toggle_stage_on_a_staged_file_unstages_it() {
        let (mut state, _) = with_files(three_files(), 24);
        select_path(&mut state, "b.rs");
        let (_, op) = op_of(&state.update(AppIntent::ToggleStage));
        assert_eq!(op, GitOp::Unstage(vec!["b.rs".into()]));
    }

    #[test]
    fn toggle_stage_all_stages_or_unstages_everything() {
        let (mut state, _) = with_files(three_files(), 24);
        let (_, op) = op_of(&state.update(AppIntent::ToggleStageAll));
        assert_eq!(op, GitOp::StageAll);

        let (mut state, _) = with_files(vec![changed("b.rs", 'M', '.')], 24);
        let (_, op) = op_of(&state.update(AppIntent::ToggleStageAll));
        assert_eq!(op, GitOp::UnstageAll);
    }

    #[test]
    fn toggle_stage_all_with_conflicts_refuses_with_an_error() {
        let conflict = FileEntry {
            kind: EntryKind::Conflicted,
            ..changed("both.rs", 'U', 'U')
        };
        let (mut state, _) = with_files(vec![changed("a.rs", '.', 'M'), conflict], 24);
        let busy = state.busy;

        assert_eq!(state.update(AppIntent::ToggleStageAll), vec![]);

        assert_eq!(
            state.op_error.as_deref(),
            Some("Resolve conflicts before staging everything")
        );
        assert_eq!(state.op_seq, None);
        assert_eq!(state.busy, busy);
    }

    #[test]
    fn toggle_stage_on_a_conflicted_file_does_nothing() {
        let conflict = FileEntry {
            kind: EntryKind::Conflicted,
            ..changed("both.rs", 'U', 'U')
        };
        let (mut state, _) = with_files(vec![conflict], 24);
        assert_eq!(state.update(AppIntent::ToggleStage), vec![]);
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn a_second_toggle_while_one_is_in_flight_does_nothing() {
        let (mut state, _) = with_files(three_files(), 24);
        op_of(&state.update(AppIntent::ToggleStage));
        let busy = state.busy;

        assert_eq!(state.update(AppIntent::ToggleStage), vec![]);
        assert_eq!(state.update(AppIntent::ToggleStageAll), vec![]);
        assert_eq!(state.busy, busy);
    }

    #[test]
    fn toggles_do_nothing_in_review_or_without_files() {
        let mut review = AppState::new(Context::Review { number: Some(1) });
        review.files = three_files();
        review.selected = Some(0);
        let (mut clean, _) = with_files(vec![], 24);
        for state in [&mut review, &mut clean] {
            assert_eq!(state.update(AppIntent::ToggleStage), vec![]);
            assert_eq!(state.update(AppIntent::ToggleStageAll), vec![]);
            assert_eq!(state.op_seq, None);
            assert_eq!(state.op_error, None);
        }
    }

    #[test]
    fn the_next_stage_key_clears_an_old_error() {
        let (mut state, _) = with_files(three_files(), 24);
        state.op_error = Some("old".into());
        op_of(&state.update(AppIntent::ToggleStage));
        assert_eq!(state.op_error, None);
    }

    #[test]
    fn space_stages_a_range_in_one_op_skipping_conflicts() {
        let files = vec![
            changed("a.rs", '.', 'M'),
            conflicted("c.rs"),
            untracked("d.rs"),
        ];
        let (mut state, _) = with_files(files, 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        press_char(&mut state, 'j');
        assert_eq!(state.files_range(), [0, 1, 2]);
        let (_, op) = op_of(&press_char(&mut state, ' '));
        assert_eq!(op, GitOp::Stage(vec!["a.rs".into(), "d.rs".into()]));
        assert_eq!(state.files_anchor, None, "the range is done with");
    }

    #[test]
    fn space_unstages_a_staged_range_with_each_renames_old_path() {
        let renamed = FileEntry {
            orig_path: Some("old.rs".into()),
            kind: EntryKind::Renamed,
            ..changed("new.rs", 'R', '.')
        };
        let files = vec![renamed, changed("b.rs", 'M', '.'), conflicted("c.rs")];
        let (mut state, _) = with_files(files, 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        press_char(&mut state, 'j');
        let (_, op) = op_of(&press_char(&mut state, ' '));
        assert_eq!(
            op,
            GitOp::Unstage(vec!["old.rs".into(), "new.rs".into(), "b.rs".into()])
        );
    }

    #[test]
    fn discarding_a_mixed_range_asks_with_the_counts_and_only_y_answers() {
        let files = vec![
            changed("a.rs", '.', 'M'),
            changed("b.rs", '.', 'M'),
            untracked("n.txt"),
        ];
        let (mut state, _) = with_files(files, 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        press_char(&mut state, 'j');
        assert_eq!(press_char(&mut state, 'd'), vec![]);
        let Some(Popup::Discard(confirm)) = state.popups.last() else {
            panic!("expected the discard question, got {:?}", state.popups);
        };
        assert_eq!(
            confirm.what.question(),
            "Discard unstaged changes to 2 files and delete 1 untracked file? (y/n)"
        );
        assert_eq!(confirm.what.title(), "Discard 3 files");
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.popups.len(), 1, "Enter does not answer");
        assert_eq!(press_char(&mut state, 'n'), vec![]);
        assert!(state.popups.is_empty());
        assert!(state.files_anchor.is_some(), "no keeps the range");

        press_char(&mut state, 'd');
        let (_, op) = op_of(&press_char(&mut state, 'y'));
        assert_eq!(
            op,
            GitOp::DiscardFiles {
                restore: vec!["a.rs".into(), "b.rs".into()],
                delete: vec!["n.txt".into()],
            }
        );
        assert_eq!(state.files_anchor, None);
    }

    #[test]
    fn a_range_with_nothing_to_discard_says_so() {
        let files = vec![changed("a.rs", 'M', '.'), changed("b.rs", 'M', '.')];
        let (mut state, _) = with_files(files, 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        assert_eq!(press_char(&mut state, 'd'), vec![]);
        assert!(state.popups.is_empty());
        assert_eq!(state.notice, Some("No unstaged changes to discard".into()));
    }

    #[test]
    fn stage_from_another_panel_stages_the_one_file_not_the_range() {
        let (mut state, _) = with_files(three_files(), 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        state.update(AppIntent::FocusPanel(crate::layout::Panel::Side(
            SidePanel::Branches,
        )));
        let (_, op) = op_of(&state.update(AppIntent::ToggleStage));
        assert_eq!(op, toggle_for(&state.files[1]).unwrap());
    }
}
