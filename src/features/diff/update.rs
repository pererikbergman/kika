//! The diff's side of `AppState`: taking a chunk of the shown diff from
//! the `DiffStreamer`, unless a later request superseded it, and staging
//! from the Diff pane: hunks, lines, the side and the loaded patch.

use crate::effect::{Effect, Tag};
use crate::git::diff::{DiffChunk, DiffSide};
use crate::git::patch::{self, Direction, FilePatch, PatchSelection};
use crate::hunk_view::HunkView;
use crate::keymap;
use crate::model::{AppState, Focus};
use crate::op::GitOp;
use crate::popup::ErrorPopup;

impl AppState {
    /// Take a chunk of the shown diff. One from an older generation is
    /// dropped, since a refresh has replaced what it belongs to.
    pub(crate) fn diff_loaded(&mut self, tag: Tag, result: Result<DiffChunk, String>) {
        if tag.generation == self.generation {
            self.diff.apply(tag.seq, result);
        }
    }
}

/// Hunk and line staging: moving between hunks and lines, taking a loaded
/// patch, flipping the side and staging the selection.
impl AppState {
    /// Select the next hunk, keeping it on screen.
    pub(crate) fn next_hunk(&mut self) {
        let rows = self.pane_rows();
        if let Focus::Diff(view) = &mut self.focus {
            view.next(rows);
        }
        self.follow_cursor();
    }

    /// Select the previous hunk, keeping it on screen.
    pub(crate) fn prev_hunk(&mut self) {
        let rows = self.pane_rows();
        if let Focus::Diff(view) = &mut self.focus {
            view.prev(rows);
        }
        self.follow_cursor();
    }

    /// Pick lines inside the selected hunk; a tool's rows have none.
    pub(crate) fn enter_line_mode(&mut self) {
        if self.display_only() && matches!(self.focus, Focus::Diff(_)) {
            self.notice = Some(keymap::DISPLAY_ONLY.into());
        } else {
            self.with_lines(HunkView::enter_lines);
        }
    }

    pub(crate) fn patch_loaded(&mut self, tag: Tag, result: Result<FilePatch, String>) {
        // Every user effect reports back once, stale or not.
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        let rows = self.pane_rows();
        let Focus::Diff(view) = &mut self.focus else {
            return;
        };
        if tag.generation != self.generation || view.seq != Some(tag.seq) {
            return;
        }
        view.seq = None;
        match result {
            Ok(patch) => view.loaded(patch, rows),
            Err(message) => {
                view.patch = None;
                self.push_error(ErrorPopup::new(
                    "Could not read the diff",
                    &message,
                    "Close this, fix the cause, then press r to retry.",
                ));
            }
        }
    }

    /// Change the focused view's line cursor, keeping it on screen.
    pub(crate) fn with_lines(&mut self, change: impl FnOnce(&mut HunkView)) {
        let rows = self.pane_rows();
        if let Focus::Diff(view) = &mut self.focus {
            change(view);
            view.scroll_to_cursor(rows);
        }
        self.follow_cursor();
    }

    /// Stage the selected hunk or lines, or unstage them from the staged
    /// view. When that is the file's whole change, the file is staged
    /// instead: the same change, without a patch that could fail to apply.
    pub(crate) fn toggle_selection_stage(&mut self, selection: PatchSelection) -> Vec<Effect> {
        let Focus::Diff(view) = &self.focus else {
            return vec![];
        };
        if self.display_only() {
            self.notice = Some(keymap::DISPLAY_ONLY.into());
            return vec![];
        }
        if self.diff_options.ignore_whitespace {
            self.notice = Some(keymap::WHITESPACE_HIDDEN.into());
            return vec![];
        }
        let Some(file_patch) = &view.patch else {
            return vec![];
        };
        let direction = if view.side == DiffSide::Index {
            Direction::Unstage
        } else {
            Direction::Stage
        };
        let built = match patch::build(file_patch, &selection, direction) {
            Ok(built) => built,
            Err(error) => {
                self.notice = Some(error.message().into());
                return vec![];
            }
        };
        let staged = view.side == DiffSide::Index;
        let op = if patch::selects_everything(file_patch, &selection) {
            let path = vec![view.path.clone()];
            if staged {
                GitOp::Unstage(path)
            } else {
                GitOp::Stage(path)
            }
        } else {
            GitOp::ApplyCached {
                patch: built,
                reverse: staged,
            }
        };
        self.start_op(Ok(Some(op)))
    }

    /// Flip the focused view between unstaged and staged changes.
    pub(crate) fn toggle_diff_side(&mut self) -> Vec<Effect> {
        let Focus::Diff(view) = &mut self.focus else {
            return vec![];
        };
        if view.commit.is_some() {
            self.notice = Some(keymap::NOT_IN_COMMIT.into());
            return vec![];
        }
        let side = match view.side {
            DiffSide::Worktree => DiffSide::Index,
            DiffSide::Index => DiffSide::Worktree,
            DiffSide::Untracked => return vec![],
        };
        *view = HunkView::new(view.path.clone(), side);
        let mut effects = self.load_patch();
        self.remap();
        effects.extend(self.render_shown());
        effects
    }
}

#[cfg(test)]
mod tests {
    use crate::effect::{Effect, Tag};
    use crate::git::diff::{DiffRequest, DiffSide};
    use crate::git::status::EntryKind;
    use crate::intent::{AppIntent, Scroll};
    use crate::keymap::{KeyPress, KeyScope};
    use crate::model::Focus;
    use crate::op::GitOp;
    use crate::popup::Popup;
    use crate::test_support::{
        BINARY, HEAD, ONE_HUNK, TWO_HUNKS, changed, chunk, diff_tag, finished, focused, loaded,
        op_of, patch_load, patch_of, press, refresh_tag, repo, three_files, untracked, view,
        with_files,
    };
    use crossterm::event::{KeyCode, KeyModifiers};

    #[test]
    fn later_chunks_append() {
        let (mut state, effects) = with_files(three_files(), 24);
        let tag = diff_tag(&effects);
        state.update(chunk(tag, 5, false));
        state.update(chunk(tag, 4, true));
        assert_eq!(state.diff.lines.len(), 9);
    }

    #[test]
    fn superseded_diff_results_never_appear() {
        let (mut state, effects) = with_files(three_files(), 24);
        let first = diff_tag(&effects);
        let second = diff_tag(&state.update(AppIntent::SelectNext));
        let before = state.diff.clone();

        state.update(chunk(first, 3, true));
        state.update(AppIntent::DiffSlow { tag: first });
        assert_eq!(state.diff, before);

        let current_generation = Tag {
            generation: second.generation + 1,
            ..second
        };
        state.generation += 1;
        let after_bump = state.diff.clone();
        state.update(chunk(second, 3, true));
        state.update(AppIntent::DiffSlow { tag: second });
        assert_eq!(state.diff, after_bump);
        // Sanity: the tag for the new generation would have applied.
        state.update(chunk(current_generation, 1, true));
        assert_eq!(state.diff.lines.len(), 1);
    }

    #[test]
    fn a_diff_error_is_stored_and_ends_the_stream() {
        let (mut state, effects) = with_files(three_files(), 10);
        let tag = diff_tag(&effects);
        state.update(AppIntent::DiffLoaded {
            tag,
            result: Err("fatal: oops".into()),
        });
        assert_eq!(state.diff.error.as_deref(), Some("fatal: oops"));
        assert!(state.diff.done);
        assert!(!state.diff.pending);
        assert_eq!(
            state.update(AppIntent::ScrollDiff(Scroll::PageDown)),
            vec![]
        );
    }

    #[test]
    fn enter_focuses_the_diff_on_the_entry_side_and_loads_the_patch() {
        let (mut state, _) = with_files(three_files(), 24);
        let busy = state.busy;
        let (tag, request) = patch_load(&press(&mut state, KeyCode::Enter));
        assert!(tag.user);
        assert_eq!(state.busy, busy + 1);
        assert_eq!(
            request,
            DiffRequest {
                path: "a.rs".into(),
                orig_path: None,
                side: DiffSide::Worktree,
                options: Default::default(),
                commit: None,
            }
        );
        assert_eq!(view(&state).side, DiffSide::Worktree);
        assert_eq!(view(&state).seq, Some(tag.seq));
        assert_eq!(state.panel_focus(), KeyScope::Diff);

        // A staged-only file opens on the staged side.
        let (mut state, _) = with_files(vec![changed("b.rs", 'M', '.')], 24);
        let (_, request) = patch_load(&press(&mut state, KeyCode::Enter));
        assert_eq!(request.side, DiffSide::Index);
    }

    #[test]
    fn enter_without_a_file_says_so() {
        let (mut state, _) = with_files(vec![], 24);
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice, Some("No file selected".into()));
        assert_eq!(state.focus, Focus::Files);
    }

    #[test]
    fn a_loaded_patch_lowers_busy_and_shows() {
        let state = focused(three_files(), TWO_HUNKS);
        assert_eq!(state.busy, 0);
        assert_eq!(view(&state).seq, None);
        assert_eq!(view(&state).hunk_count(), 2);
    }

    #[test]
    fn j_and_k_move_between_hunks_and_stop_at_the_ends() {
        let mut state = focused(three_files(), TWO_HUNKS);
        press(&mut state, KeyCode::Char('k'));
        assert_eq!(view(&state).hunk, 0);
        press(&mut state, KeyCode::Char('j'));
        press(&mut state, KeyCode::Down);
        assert_eq!(view(&state).hunk, 1);
        press(&mut state, KeyCode::Up);
        assert_eq!(view(&state).hunk, 0);
        // The file selection never moved.
        assert_eq!(state.selected, Some(0));
    }

    #[test]
    fn space_on_one_of_several_hunks_applies_a_patch() {
        let mut state = focused(three_files(), TWO_HUNKS);
        press(&mut state, KeyCode::Char('j'));
        let (tag, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        assert!(tag.user);
        assert_eq!(
            op,
            GitOp::ApplyCached {
                patch: b"diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -9 +9 @@\n-e\n+f\n"
                    .to_vec(),
                reverse: false,
            }
        );
    }

    #[test]
    fn space_in_the_staged_view_applies_the_patch_in_reverse() {
        let mut state = focused(vec![changed("a.rs", 'M', '.')], TWO_HUNKS);
        assert_eq!(view(&state).side, DiffSide::Index);
        let (_, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        assert!(
            matches!(op, GitOp::ApplyCached { reverse: true, .. }),
            "{op:?}"
        );
    }

    #[test]
    fn space_on_the_only_hunk_stages_or_unstages_the_file() {
        let mut state = focused(three_files(), ONE_HUNK);
        let (_, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        assert_eq!(op, GitOp::Stage(vec!["a.rs".into()]));

        let mut state = focused(vec![changed("a.rs", 'M', '.')], ONE_HUNK);
        let (_, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        assert_eq!(op, GitOp::Unstage(vec!["a.rs".into()]));
    }

    #[test]
    fn space_on_a_binary_file_says_why_and_runs_nothing() {
        let mut state = focused(three_files(), BINARY);
        assert_eq!(press(&mut state, KeyCode::Char(' ')), vec![]);
        assert_eq!(
            state.notice,
            Some("Binary file: stage it from the Files panel".into())
        );
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn space_is_refused_in_order_of_what_matters_most() {
        let mut state = focused(three_files(), TWO_HUNKS);
        state.op_seq = Some(1);
        press(&mut state, KeyCode::Char(' '));
        assert_eq!(state.notice, Some("An operation is running".into()));
        state.op_seq = None;

        let mut loading = state.clone();
        press(&mut loading, KeyCode::Char('t'));
        press(&mut loading, KeyCode::Char(' '));
        assert_eq!(loading.notice, Some("Loading…".into()));

        for (kind, reason) in [
            (EntryKind::Conflicted, "Resolve the conflict first"),
            (
                EntryKind::Renamed,
                "Renamed file: stage it from the Files panel",
            ),
            (
                EntryKind::Copied,
                "Renamed file: stage it from the Files panel",
            ),
        ] {
            let mut odd = state.clone();
            odd.files[0].kind = kind;
            assert_eq!(press(&mut odd, KeyCode::Char(' ')), vec![]);
            assert_eq!(odd.notice, Some(reason.into()), "{kind:?}");
        }

        let mut empty = focused(three_files(), b"");
        press(&mut empty, KeyCode::Char(' '));
        assert_eq!(empty.notice, Some("No changes in this view".into()));
    }

    #[test]
    fn a_submodule_is_refused_by_the_patch_engine() {
        let submodule = b"diff --git a/s b/s\nindex 1..2 160000\n--- a/s\n+++ b/s\n\
            @@ -1 +1 @@\n-Subproject commit 1\n+Subproject commit 2\n";
        let mut state = focused(three_files(), submodule);
        assert_eq!(press(&mut state, KeyCode::Char(' ')), vec![]);
        assert_eq!(
            state.notice,
            Some("This change can only be staged from the Files panel".into())
        );
    }

    #[test]
    fn t_flips_the_side_and_reloads() {
        let mut state = focused(three_files(), TWO_HUNKS);
        press(&mut state, KeyCode::Char('j'));
        let (_, request) = patch_load(&press(&mut state, KeyCode::Char('t')));
        assert_eq!(request.side, DiffSide::Index);
        assert_eq!(view(&state).side, DiffSide::Index);
        assert_eq!(view(&state).hunk, 0);
        assert_eq!(view(&state).patch, None);
        let (_, request) = patch_load(&press(&mut state, KeyCode::Char('t')));
        assert_eq!(request.side, DiffSide::Worktree);
    }

    #[test]
    fn t_on_an_untracked_file_says_nothing_is_staged() {
        let mut state = focused(vec![untracked("n.txt")], ONE_HUNK);
        assert_eq!(view(&state).side, DiffSide::Untracked);
        assert_eq!(press(&mut state, KeyCode::Char('t')), vec![]);
        assert_eq!(
            state.notice,
            Some("Untracked file: nothing is staged".into())
        );
    }

    #[test]
    fn a_stale_patch_load_is_dropped_but_busy_still_falls() {
        let (mut state, _) = with_files(three_files(), 24);
        let (old, _) = patch_load(&press(&mut state, KeyCode::Enter));
        let (new, _) = patch_load(&press(&mut state, KeyCode::Char('t')));
        assert_eq!(state.busy, 2);

        state.update(AppIntent::PatchLoaded {
            tag: old,
            result: Ok(patch_of(TWO_HUNKS)),
        });
        assert_eq!(state.busy, 1);
        assert_eq!(view(&state).patch, None);
        assert_eq!(view(&state).seq, Some(new.seq));

        // Another repository's result is dropped too.
        state.generation += 1;
        state.update(AppIntent::PatchLoaded {
            tag: new,
            result: Ok(patch_of(TWO_HUNKS)),
        });
        assert_eq!(state.busy, 0);
        assert_eq!(view(&state).patch, None);
    }

    #[test]
    fn a_failed_patch_load_shows_an_error_popup() {
        let (mut state, _) = with_files(three_files(), 24);
        let (tag, _) = patch_load(&press(&mut state, KeyCode::Enter));
        state.update(AppIntent::PatchLoaded {
            tag,
            result: Err("fatal: bad".into()),
        });
        match state.popups.as_slice() {
            [Popup::Error(error)] => {
                assert_eq!(error.title, "Could not read the diff");
                assert_eq!(error.message, "fatal: bad");
            }
            other => panic!("expected one error popup, got {other:?}"),
        }
        assert_eq!(view(&state).seq, None);
        assert_eq!(state.busy, 0);
    }

    #[test]
    fn a_failed_hunk_stage_names_what_failed() {
        let mut state = focused(three_files(), TWO_HUNKS);
        let (tag, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        state.update(finished(tag, op, Err("error: patch failed".into())));
        assert!(
            matches!(state.popups.as_slice(), [Popup::Error(e)] if e.title == "Could not stage the change"),
            "{:?}",
            state.popups
        );

        let mut state = focused(vec![changed("a.rs", 'M', '.')], TWO_HUNKS);
        let (tag, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        state.update(finished(tag, op, Err("no".into())));
        assert!(
            matches!(state.popups.as_slice(), [Popup::Error(e)] if e.title == "Could not unstage the change"),
            "{:?}",
            state.popups
        );
    }

    #[test]
    fn after_a_hunk_is_staged_the_patch_reloads_and_the_cursor_is_clamped() {
        let mut state = focused(three_files(), TWO_HUNKS);
        press(&mut state, KeyCode::Char('j'));
        let (tag, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        let status = match state.update(finished(tag, op, Ok(()))).as_slice() {
            [Effect::LoadStatus { tag }] => *tag,
            other => panic!("expected one LoadStatus, got {other:?}"),
        };
        let effects = state.update(AppIntent::StatusLoaded {
            tag: status,
            result: Ok(repo(vec![changed("a.rs", 'M', 'M')])),
        });
        let (tag, request) = patch_load(&effects);
        // Same file, so the same side.
        assert_eq!(request.side, DiffSide::Worktree);
        assert_eq!(view(&state).hunk, 1);
        state.update(AppIntent::PatchLoaded {
            tag,
            result: Ok(patch_of(ONE_HUNK)),
        });
        assert_eq!(view(&state).hunk, 0);
        assert_eq!(state.busy, 0);
    }

    #[test]
    fn a_file_that_stops_being_untracked_takes_its_new_side() {
        let mut state = focused(vec![untracked("n.txt")], ONE_HUNK);
        let (tag, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        assert_eq!(op, GitOp::Stage(vec!["n.txt".into()]));
        let status = match state.update(finished(tag, op, Ok(()))).as_slice() {
            [Effect::LoadStatus { tag }] => *tag,
            other => panic!("{other:?}"),
        };
        let effects = state.update(AppIntent::StatusLoaded {
            tag: status,
            result: Ok(repo(vec![changed("n.txt", 'A', '.')])),
        });
        assert_eq!(patch_load(&effects).1.side, DiffSide::Index);
    }

    #[test]
    fn a_clean_tree_returns_focus_to_the_files() {
        let mut state = focused(three_files(), ONE_HUNK);
        let tag = refresh_tag(&mut state);
        state.update(loaded(tag, &[]));
        assert_eq!(state.focus, Focus::Files);
    }

    #[test]
    fn esc_h_or_left_in_the_diff_go_back_to_the_files_without_quitting() {
        for key in [KeyCode::Esc, KeyCode::Char('h'), KeyCode::Left] {
            let mut state = focused(three_files(), TWO_HUNKS);
            assert_eq!(press(&mut state, key), vec![]);
            assert_eq!(state.focus, Focus::Files, "{key:?}");
            assert!(!state.should_quit);
            assert_eq!(state.key_focus(), KeyScope::Files);
        }
    }

    #[test]
    fn scrolling_in_the_diff_focus_moves_the_hunk_view() {
        let mut state = focused(three_files(), TWO_HUNKS);
        let before = state.diff.clone();
        assert_eq!(
            state.update(AppIntent::ScrollDiff(Scroll::LineDown)),
            vec![]
        );
        assert_eq!(view(&state).scroll, 1);
        assert_eq!(state.diff, before);
    }

    #[test]
    fn v_enters_line_mode_and_esc_or_v_leaves_it_without_leaving_the_diff() {
        for leave in [KeyCode::Esc, KeyCode::Char('v')] {
            let mut state = focused(three_files(), TWO_HUNKS);
            press(&mut state, KeyCode::Char('v'));
            assert!(view(&state).lines.is_some());
            assert_eq!(state.panel_focus(), KeyScope::Lines);
            assert_eq!(state.key_focus(), KeyScope::Lines);
            assert_eq!(press(&mut state, leave), vec![]);
            assert_eq!(view(&state).lines, None, "{leave:?}");
            assert_eq!(state.panel_focus(), KeyScope::Diff);
        }
    }

    #[test]
    fn in_line_mode_j_and_k_move_between_lines_not_hunks() {
        let mut state = focused(three_files(), TWO_HUNKS);
        press(&mut state, KeyCode::Char('v'));
        press(&mut state, KeyCode::Char('j'));
        press(&mut state, KeyCode::Char('j'));
        press(&mut state, KeyCode::Char('j'));
        assert_eq!(view(&state).hunk, 0);
        assert_eq!(view(&state).selected_range(), [2]);
        press(&mut state, KeyCode::Char('k'));
        state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('K'),
            KeyModifiers::SHIFT,
        )));
        assert_eq!(view(&state).selected_range(), [0, 1]);
    }

    #[test]
    fn space_in_line_mode_applies_the_selected_lines_per_side() {
        let mut state = focused(three_files(), TWO_HUNKS);
        press(&mut state, KeyCode::Char('v'));
        press(&mut state, KeyCode::Char('j'));
        let (tag, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        assert!(tag.user);
        assert_eq!(
            op,
            GitOp::ApplyCached {
                patch: format!("{HEAD}@@ -1,2 +1,3 @@\n a\n+b\n d\n").into_bytes(),
                reverse: false,
            }
        );

        let mut state = focused(vec![changed("a.rs", 'M', '.')], TWO_HUNKS);
        press(&mut state, KeyCode::Char('v'));
        press(&mut state, KeyCode::Char('j'));
        let (_, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        assert_eq!(
            op,
            GitOp::ApplyCached {
                patch: format!("{HEAD}@@ -1,2 +1,3 @@\n+b\n c\n d\n").into_bytes(),
                reverse: true,
            }
        );
    }

    #[test]
    fn selecting_every_line_of_the_file_stages_or_unstages_the_file() {
        for (index, worktree, expected) in [
            ('.', 'M', GitOp::Stage(vec!["a.rs".into()])),
            ('M', '.', GitOp::Unstage(vec!["a.rs".into()])),
        ] {
            let mut state = focused(vec![changed("a.rs", index, worktree)], ONE_HUNK);
            press(&mut state, KeyCode::Char('v'));
            press(&mut state, KeyCode::Char('J'));
            let (_, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
            assert_eq!(op, expected);
        }
    }

    #[test]
    fn a_refused_line_selection_says_why_and_runs_nothing() {
        let new_file = b"diff --git a/n.txt b/n.txt\nnew file mode 100644\n\
            --- /dev/null\n+++ b/n.txt\n@@ -0,0 +1,2 @@\n+x\n+y\n";
        let mut state = focused(vec![untracked("n.txt")], new_file);
        press(&mut state, KeyCode::Char('v'));
        assert_eq!(press(&mut state, KeyCode::Char(' ')), vec![]);
        assert_eq!(
            state.notice,
            Some("New file: press Esc, then Space for the whole file".into())
        );
        assert_eq!(state.op_seq, None);
        assert_eq!(state.busy, 0);
    }

    #[test]
    fn t_leaves_line_mode_and_v_is_refused_without_hunks() {
        let mut state = focused(three_files(), TWO_HUNKS);
        press(&mut state, KeyCode::Char('v'));
        press(&mut state, KeyCode::Char('t'));
        assert_eq!(view(&state).lines, None);
        assert_eq!(state.panel_focus(), KeyScope::Diff);

        let mut empty = focused(three_files(), b"");
        assert_eq!(press(&mut empty, KeyCode::Char('v')), vec![]);
        assert_eq!(empty.notice, Some("No changes in this view".into()));
        assert_eq!(view(&empty).lines, None);
    }

    #[test]
    fn a_failed_lines_stage_names_the_change() {
        let mut state = focused(three_files(), TWO_HUNKS);
        press(&mut state, KeyCode::Char('v'));
        let (tag, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        state.update(finished(tag, op, Err("error: patch failed".into())));
        assert!(
            matches!(state.popups.as_slice(), [Popup::Error(e)] if e.title == "Could not stage the change"),
            "{:?}",
            state.popups
        );
    }
}
