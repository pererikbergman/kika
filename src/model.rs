use crate::diff_pane::DiffPane;
use crate::effect::{Effect, Tag};
use crate::git::FileEntry;
use crate::git::diff::{DiffChunk, DiffRequest};
use crate::git::ops::GitOp;
use crate::intent::AppIntent;
use crate::refresh::{self, RefreshPlan, Scope, Selection};

/// What Kika is looking at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Context {
    /// The local working tree and index.
    Workspace,
    /// A remote pull/merge request.
    Review { number: u64 },
}

/// The single source of truth for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppState {
    pub context: Context,
    pub size: (u16, u16),
    pub should_quit: bool,
    /// Bumped when the repository changes, so results for the old one are dropped.
    pub generation: u64,
    /// Next sequence number to hand out to an effect.
    pub next_seq: u64,
    /// Sequence of the latest status load; only its result is applied.
    pub status_seq: Option<u64>,
    /// User-initiated effects still in flight.
    pub busy: u32,
    pub files: Vec<FileEntry>,
    /// False until the first status result arrives, to tell "loading" from "clean".
    pub files_loaded: bool,
    pub status_error: Option<String>,
    /// Index into `files` of the file whose diff is shown.
    pub selected: Option<usize>,
    pub diff: DiffPane,
    /// Sequence of the Git operation in flight. Operations run one at a
    /// time so two quick presses never race for `index.lock`.
    pub op_seq: Option<u64>,
    /// The last operation's failure, or why one was refused.
    pub op_error: Option<String>,
    /// How the selection is kept when the pending status load lands.
    pub status_selection: Selection,
}

impl AppState {
    pub fn new(context: Context) -> Self {
        Self {
            context,
            size: (0, 0),
            should_quit: false,
            generation: 0,
            next_seq: 0,
            status_seq: None,
            busy: 0,
            files: Vec::new(),
            files_loaded: false,
            status_error: None,
            selected: None,
            diff: DiffPane::default(),
            op_seq: None,
            op_error: None,
            status_selection: Selection::KeepPath,
        }
    }

    /// Apply an intent and return the effects to run. Pure: no I/O, no rendering.
    pub fn update(&mut self, intent: AppIntent) -> Vec<Effect> {
        match intent {
            AppIntent::Quit => self.should_quit = true,
            AppIntent::Resize { width, height } => {
                self.size = (width, height);
                return self.diff.demand(self.pane_rows());
            }
            AppIntent::RefreshStatus => {
                self.status_selection = Selection::KeepPath;
                return self.refresh_status();
            }
            AppIntent::StatusLoaded { tag, result } => return self.status_loaded(tag, result),
            AppIntent::SelectNext => return self.move_selection(1),
            AppIntent::SelectPrev => return self.move_selection(-1),
            AppIntent::ScrollDiff(by) => return self.diff.scroll(by, self.pane_rows()),
            AppIntent::DiffLoaded { tag, result } => self.diff_loaded(tag, result),
            AppIntent::DiffSlow { tag } => {
                if tag.generation == self.generation {
                    self.diff.slow(tag.seq);
                }
            }
            AppIntent::ToggleStage => {
                let op = self
                    .selected
                    .and_then(|i| self.files.get(i))
                    .and_then(GitOp::toggle_for);
                return self.start_op(Ok(op));
            }
            AppIntent::ToggleStageAll => return self.start_op(GitOp::toggle_all(&self.files)),
            AppIntent::OpFinished { tag, op, result } => return self.op_finished(tag, op, result),
        }
        vec![]
    }

    /// Text rows inside the Diff pane: the screen minus the status bar and borders.
    pub fn pane_rows(&self) -> usize {
        usize::from(self.size.1).saturating_sub(3).max(1)
    }

    fn take_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    fn move_selection(&mut self, delta: isize) -> Vec<Effect> {
        let Some(current) = self.selected else {
            return vec![];
        };
        let next = current
            .saturating_add_signed(delta)
            .min(self.files.len().saturating_sub(1));
        if next == current {
            return vec![];
        }
        self.select(next)
    }

    /// Select file `index` and start loading its diff.
    fn select(&mut self, index: usize) -> Vec<Effect> {
        self.selected = Some(index);
        let request = DiffRequest::for_entry(&self.files[index]);
        let seq = self.take_seq();
        let want = self.diff.begin(seq, &request.path, self.pane_rows());
        // Diff streams report many times, so they stay out of the busy count.
        let tag = Tag {
            generation: self.generation,
            seq,
            user: false,
        };
        vec![
            Effect::LoadDiff { tag, request, want },
            Effect::DiffLoadingDelay { tag },
        ]
    }

    /// Keep the selection on the same file across a refresh when it still
    /// exists, else near the same place.
    fn reselect(&mut self, old_path: Option<String>) -> Vec<Effect> {
        if self.files.is_empty() {
            self.selected = None;
            self.diff = DiffPane::default();
            return vec![Effect::CancelDiff];
        }
        let index = old_path
            .and_then(|path| self.files.iter().position(|f| f.path == path))
            .or_else(|| self.selected.map(|i| i.min(self.files.len() - 1)))
            .unwrap_or(0);
        self.select(index)
    }

    fn diff_loaded(&mut self, tag: Tag, result: Result<DiffChunk, String>) {
        if tag.generation == self.generation {
            self.diff.apply(tag.seq, result);
        }
    }

    /// Run the operation a stage key chose. Only one runs at a time, and
    /// only on the local workspace.
    fn start_op(&mut self, op: Result<Option<GitOp>, String>) -> Vec<Effect> {
        if self.context != Context::Workspace || self.op_seq.is_some() {
            return vec![];
        }
        self.op_error = None;
        let op = match op {
            Ok(Some(op)) => op,
            Ok(None) => return vec![],
            Err(message) => {
                self.op_error = Some(message);
                return vec![];
            }
        };
        let seq = self.take_seq();
        self.op_seq = Some(seq);
        self.busy += 1;
        let tag = Tag {
            generation: self.generation,
            seq,
            user: true,
        };
        vec![Effect::RunOp { tag, op }]
    }

    fn op_finished(&mut self, tag: Tag, op: GitOp, result: Result<(), String>) -> Vec<Effect> {
        // As with status loads: every user effect reports once, stale or not.
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        if Some(tag.seq) == self.op_seq {
            self.op_seq = None;
        }
        if tag.generation != self.generation {
            return vec![];
        }
        self.op_error = result.err();
        // Refresh even after a failure: a failed `add` of several paths can
        // still have changed the index.
        self.apply_plan(refresh::plan(&op))
    }

    /// Reload what the plan says is stale. Reloading files reselects, which
    /// reloads the diff too.
    fn apply_plan(&mut self, plan: RefreshPlan) -> Vec<Effect> {
        if plan.scopes.contains(&Scope::Files) {
            self.status_selection = plan.selection;
            return self.refresh_status();
        }
        match self.selected {
            Some(index) if plan.scopes.contains(&Scope::Diff) => self.select(index),
            _ => vec![],
        }
    }

    fn refresh_status(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        self.status_seq = Some(seq);
        self.busy += 1;
        vec![Effect::LoadStatus {
            tag: Tag {
                generation: self.generation,
                seq,
                user: true,
            },
        }]
    }

    fn status_loaded(&mut self, tag: Tag, result: Result<Vec<FileEntry>, String>) -> Vec<Effect> {
        // Every user effect reports back exactly once, stale or not, so
        // decrement before the staleness check or the counter would leak.
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        if tag.generation != self.generation || Some(tag.seq) != self.status_seq {
            return vec![];
        }
        self.files_loaded = true;
        match result {
            Ok(files) => {
                let old_path = self
                    .selected
                    .and_then(|i| self.files.get(i))
                    .map(|f| f.path.clone());
                self.files = files;
                self.status_error = None;
                match self.status_selection {
                    Selection::KeepPath => self.reselect(old_path),
                }
            }
            Err(message) => {
                self.status_error = Some(message);
                vec![]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_pane::DIFF_PREFETCH;
    use crate::git::diff::{DiffLine, DiffSide, LineKind};
    use crate::git::status::EntryKind;
    use crate::intent::Scroll;

    fn untracked(path: &str) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            orig_path: None,
            index: '?',
            worktree: '?',
            kind: EntryKind::Untracked,
        }
    }

    fn loaded(tag: Tag, paths: &[&str]) -> AppIntent {
        AppIntent::StatusLoaded {
            tag,
            result: Ok(paths.iter().map(|p| untracked(p)).collect()),
        }
    }

    fn refresh_tag(state: &mut AppState) -> Tag {
        match state.update(AppIntent::RefreshStatus).as_slice() {
            [Effect::LoadStatus { tag }] => *tag,
            other => panic!("expected one LoadStatus, got {other:?}"),
        }
    }

    #[test]
    fn quit_sets_should_quit() {
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(state.update(AppIntent::Quit), vec![]);
        assert!(state.should_quit);
    }

    #[test]
    fn resize_records_size() {
        let mut state = AppState::new(Context::Workspace);
        state.update(AppIntent::Resize {
            width: 120,
            height: 40,
        });
        assert_eq!(state.size, (120, 40));
    }

    #[test]
    fn refresh_in_workspace_requests_a_status_load_with_increasing_seq() {
        let mut state = AppState::new(Context::Workspace);
        state.generation = 7;

        let first = refresh_tag(&mut state);
        let second = refresh_tag(&mut state);

        assert_eq!(
            first,
            Tag {
                generation: 7,
                seq: 0,
                user: true
            }
        );
        assert_eq!(
            second,
            Tag {
                generation: 7,
                seq: 1,
                user: true
            }
        );
    }

    #[test]
    fn refresh_in_review_does_nothing() {
        let mut state = AppState::new(Context::Review { number: 3 });
        assert_eq!(state.update(AppIntent::RefreshStatus), vec![]);
        assert_eq!(state.busy, 0);
    }

    #[test]
    fn latest_status_result_is_applied() {
        let mut state = AppState::new(Context::Workspace);
        let tag = refresh_tag(&mut state);

        state.update(loaded(tag, &["a.txt"]));

        assert!(state.files_loaded);
        assert_eq!(state.files, [untracked("a.txt")]);
    }

    #[test]
    fn result_for_superseded_seq_is_dropped() {
        let mut state = AppState::new(Context::Workspace);
        let old = refresh_tag(&mut state);
        let new = refresh_tag(&mut state);

        state.update(loaded(old, &["stale.txt"]));
        assert!(!state.files_loaded);
        assert_eq!(state.files, []);

        state.update(loaded(new, &["fresh.txt"]));
        assert_eq!(state.files, [untracked("fresh.txt")]);

        state.update(loaded(old, &["stale.txt"]));
        assert_eq!(state.files, [untracked("fresh.txt")]);
    }

    #[test]
    fn result_from_older_generation_is_dropped() {
        let mut state = AppState::new(Context::Workspace);
        let tag = refresh_tag(&mut state);
        state.generation += 1;

        state.update(loaded(tag, &["other-repo.txt"]));

        assert!(!state.files_loaded);
        assert_eq!(state.files, []);
    }

    #[test]
    fn error_result_is_recorded_and_cleared_by_the_next_success() {
        let mut state = AppState::new(Context::Workspace);
        let tag = refresh_tag(&mut state);
        state.update(AppIntent::StatusLoaded {
            tag,
            result: Err("fatal: not a git repository".into()),
        });
        assert!(state.files_loaded);
        assert_eq!(
            state.status_error.as_deref(),
            Some("fatal: not a git repository")
        );

        let tag = refresh_tag(&mut state);
        state.update(loaded(tag, &["a.txt"]));
        assert_eq!(state.status_error, None);
        assert_eq!(state.files, [untracked("a.txt")]);
    }

    #[test]
    fn busy_rises_per_refresh_and_falls_per_result_even_when_stale() {
        let mut state = AppState::new(Context::Workspace);
        let old = refresh_tag(&mut state);
        assert_eq!(state.busy, 1);
        let new = refresh_tag(&mut state);
        assert_eq!(state.busy, 2);

        state.update(loaded(old, &[]));
        assert_eq!(state.busy, 1);
        state.update(loaded(new, &[]));
        assert_eq!(state.busy, 0);
    }

    #[test]
    fn busy_never_underflows() {
        let mut state = AppState::new(Context::Workspace);
        let tag = Tag {
            generation: 0,
            seq: 0,
            user: true,
        };
        state.update(loaded(tag, &[]));
        assert_eq!(state.busy, 0);
    }

    #[test]
    fn non_user_results_leave_busy_alone() {
        let mut state = AppState::new(Context::Workspace);
        let mut tag = refresh_tag(&mut state);
        tag.user = false;
        state.update(loaded(tag, &[]));
        assert_eq!(state.busy, 1);
    }

    fn changed(path: &str, index: char, worktree: char) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            orig_path: None,
            index,
            worktree,
            kind: EntryKind::Changed,
        }
    }

    /// A workspace of `height` rows whose status load listed `files`, with
    /// the first file's diff already requested. Returns that request's effects.
    fn with_files(files: Vec<FileEntry>, height: u16) -> (AppState, Vec<Effect>) {
        let mut state = AppState::new(Context::Workspace);
        state.update(AppIntent::Resize { width: 80, height });
        let tag = refresh_tag(&mut state);
        let effects = state.update(AppIntent::StatusLoaded {
            tag,
            result: Ok(files),
        });
        (state, effects)
    }

    fn three_files() -> Vec<FileEntry> {
        vec![
            changed("a.rs", '.', 'M'),
            changed("b.rs", 'M', '.'),
            untracked("c.txt"),
        ]
    }

    fn diff_tag(effects: &[Effect]) -> Tag {
        match effects {
            [
                Effect::LoadDiff { tag, .. },
                Effect::DiffLoadingDelay { tag: delay },
            ] => {
                assert_eq!(tag, delay);
                *tag
            }
            other => panic!("expected LoadDiff + DiffLoadingDelay, got {other:?}"),
        }
    }

    fn line(text: &str) -> DiffLine {
        DiffLine {
            kind: LineKind::Context,
            text: text.into(),
        }
    }

    fn chunk(tag: Tag, n: usize, done: bool) -> AppIntent {
        AppIntent::DiffLoaded {
            tag,
            result: Ok(DiffChunk {
                lines: (0..n).map(|i| line(&format!("{}:{i}", tag.seq))).collect(),
                done,
            }),
        }
    }

    #[test]
    fn a_status_load_selects_the_first_file_and_requests_its_diff() {
        // Height 24: 21 diff rows.
        let (state, effects) = with_files(three_files(), 24);
        assert_eq!(state.selected, Some(0));
        let tag = diff_tag(&effects);
        assert_eq!(
            effects[0],
            Effect::LoadDiff {
                tag,
                request: DiffRequest {
                    path: "a.rs".into(),
                    orig_path: None,
                    side: DiffSide::Worktree,
                },
                want: 21 + DIFF_PREFETCH,
            }
        );
        assert!(!tag.user, "diff streams stay out of the busy count");
        assert!(state.diff.pending);
    }

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
                assert_eq!(request, &DiffRequest::for_entry(&state.files[1]))
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
    fn later_chunks_append() {
        let (mut state, effects) = with_files(three_files(), 24);
        let tag = diff_tag(&effects);
        state.update(chunk(tag, 5, false));
        state.update(chunk(tag, 4, true));
        assert_eq!(state.diff.lines.len(), 9);
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
    fn a_refresh_keeps_the_selected_path_and_its_scroll() {
        let (mut state, effects) = with_files(three_files(), 10);
        state.update(chunk(diff_tag(&effects), 50, true));
        state.update(AppIntent::SelectNext);
        let tag = diff_tag(&state.update(AppIntent::SelectNext));
        state.update(chunk(tag, 50, true));
        state.update(AppIntent::ScrollDiff(Scroll::LineDown));
        assert_eq!(state.selected, Some(2));

        // c.txt moved to the front; the selection follows it.
        let status = refresh_tag(&mut state);
        let effects = state.update(AppIntent::StatusLoaded {
            tag: status,
            result: Ok(vec![untracked("c.txt"), changed("a.rs", '.', 'M')]),
        });
        diff_tag(&effects);
        assert_eq!(state.selected, Some(0));
        assert_eq!(state.diff.scroll, 1);
    }

    #[test]
    fn a_refresh_that_drops_the_selected_file_clamps_and_resets_scroll() {
        let (mut state, _) = with_files(three_files(), 10);
        state.update(AppIntent::SelectNext);
        let tag = diff_tag(&state.update(AppIntent::SelectNext));
        state.update(chunk(tag, 50, true));
        state.update(AppIntent::ScrollDiff(Scroll::LineDown));

        let status = refresh_tag(&mut state);
        let effects = state.update(AppIntent::StatusLoaded {
            tag: status,
            result: Ok(vec![changed("a.rs", '.', 'M'), changed("z.rs", '.', 'M')]),
        });
        diff_tag(&effects);
        assert_eq!(state.selected, Some(1));
        assert_eq!(state.diff.path.as_deref(), Some("z.rs"));
        assert_eq!(state.diff.scroll, 0);
    }

    #[test]
    fn a_refresh_to_a_clean_tree_clears_the_diff_and_cancels() {
        let (mut state, effects) = with_files(three_files(), 10);
        state.update(chunk(diff_tag(&effects), 5, true));

        let status = refresh_tag(&mut state);
        let effects = state.update(loaded(status, &[]));

        assert_eq!(effects, vec![Effect::CancelDiff]);
        assert_eq!(state.selected, None);
        assert_eq!(state.diff, DiffPane::default());
    }

    #[test]
    fn a_failed_refresh_keeps_the_selection_and_diff() {
        let (mut state, effects) = with_files(three_files(), 10);
        state.update(chunk(diff_tag(&effects), 5, true));
        let status = refresh_tag(&mut state);
        let effects = state.update(AppIntent::StatusLoaded {
            tag: status,
            result: Err("boom".into()),
        });
        assert_eq!(effects, vec![]);
        assert_eq!(state.selected, Some(0));
        assert_eq!(state.diff.lines.len(), 5);
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
    fn scrolling_past_the_prefetch_window_demands_more() {
        // Height 13: 10 rows, so the first request wants 210 lines.
        let (mut state, effects) = with_files(three_files(), 13);
        let tag = diff_tag(&effects);
        state.update(chunk(tag, 210, false));
        assert_eq!(state.diff.requested, 210);

        // Still inside the window: no effect.
        assert_eq!(state.update(AppIntent::ScrollDiff(Scroll::LineUp)), vec![]);
        assert_eq!(state.diff.scroll, 0);

        let effects = state.update(AppIntent::ScrollDiff(Scroll::PageDown));
        assert_eq!(state.diff.scroll, 10);
        assert_eq!(
            effects,
            vec![Effect::DiffDemand {
                seq: tag.seq,
                want: 220
            }]
        );
        assert_eq!(state.diff.requested, 220);

        // The same position again asks for nothing new.
        state.update(AppIntent::ScrollDiff(Scroll::LineUp));
        assert_eq!(
            state.update(AppIntent::ScrollDiff(Scroll::LineDown)),
            vec![]
        );
    }

    #[test]
    fn a_finished_diff_demands_nothing_and_scroll_is_clamped() {
        let (mut state, effects) = with_files(three_files(), 13);
        state.update(chunk(diff_tag(&effects), 15, true));

        for _ in 0..5 {
            assert_eq!(
                state.update(AppIntent::ScrollDiff(Scroll::PageDown)),
                vec![]
            );
        }
        assert_eq!(state.diff.scroll, 14);
        state.update(AppIntent::ScrollDiff(Scroll::PageUp));
        state.update(AppIntent::ScrollDiff(Scroll::PageUp));
        assert_eq!(state.diff.scroll, 0);
    }

    #[test]
    fn a_taller_terminal_demands_more_lines() {
        let (mut state, effects) = with_files(three_files(), 13);
        let tag = diff_tag(&effects);
        state.update(chunk(tag, 210, false));
        let effects = state.update(AppIntent::Resize {
            width: 80,
            height: 53,
        });
        assert_eq!(
            effects,
            vec![Effect::DiffDemand {
                seq: tag.seq,
                want: 250
            }]
        );
        assert_eq!(
            state.update(AppIntent::Resize {
                width: 80,
                height: 13
            }),
            vec![]
        );
    }

    #[test]
    fn slow_before_any_chunk_shows_loading() {
        let (mut state, effects) = with_files(three_files(), 13);
        let tag = diff_tag(&effects);
        state.update(AppIntent::DiffSlow { tag });
        assert!(state.diff.show_loading);

        state.update(chunk(tag, 1, true));
        assert!(!state.diff.show_loading);
    }

    #[test]
    fn slow_after_the_first_chunk_does_nothing() {
        let (mut state, effects) = with_files(three_files(), 13);
        let tag = diff_tag(&effects);
        state.update(chunk(tag, 1, false));
        state.update(AppIntent::DiffSlow { tag });
        assert!(!state.diff.show_loading);
    }

    #[test]
    fn pane_rows_never_drops_below_one() {
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(state.pane_rows(), 1);
        state.update(AppIntent::Resize {
            width: 1,
            height: 2,
        });
        assert_eq!(state.pane_rows(), 1);
    }

    fn op_of(effects: &[Effect]) -> (Tag, GitOp) {
        match effects {
            [Effect::RunOp { tag, op }] => (*tag, op.clone()),
            other => panic!("expected one RunOp, got {other:?}"),
        }
    }

    fn select_path(state: &mut AppState, path: &str) {
        while state.files[state.selected.unwrap()].path != path {
            state.update(AppIntent::SelectNext);
        }
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
        let mut review = AppState::new(Context::Review { number: 1 });
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

    fn finished(tag: Tag, op: GitOp, result: Result<(), String>) -> AppIntent {
        AppIntent::OpFinished { tag, op, result }
    }

    #[test]
    fn a_finished_op_refreshes_status_and_keeps_the_selected_path() {
        let (mut state, _) = with_files(three_files(), 24);
        select_path(&mut state, "b.rs");
        let (tag, op) = op_of(&state.update(AppIntent::ToggleStage));
        let busy = state.busy;

        let effects = state.update(finished(tag, op, Ok(())));
        // The op's slot is released and the status load takes one.
        assert_eq!(state.busy, busy);
        assert_eq!(state.op_seq, None);
        let status = match effects.as_slice() {
            [Effect::LoadStatus { tag }] => *tag,
            other => panic!("expected one LoadStatus, got {other:?}"),
        };

        // b.rs is now unstaged and moved to the front.
        let effects = state.update(AppIntent::StatusLoaded {
            tag: status,
            result: Ok(vec![changed("b.rs", '.', 'M'), changed("a.rs", '.', 'M')]),
        });
        diff_tag(&effects);
        assert_eq!(state.selected, Some(0));
        match &effects[0] {
            Effect::LoadDiff { request, .. } => assert_eq!(
                request,
                &DiffRequest {
                    path: "b.rs".into(),
                    orig_path: None,
                    side: DiffSide::Worktree,
                }
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_failed_op_records_the_error_and_still_refreshes() {
        let (mut state, _) = with_files(three_files(), 24);
        let (tag, op) = op_of(&state.update(AppIntent::ToggleStage));

        let effects = state.update(finished(tag, op, Err("fatal: index.lock".into())));

        assert_eq!(state.op_error.as_deref(), Some("fatal: index.lock"));
        assert_eq!(state.op_seq, None);
        assert!(
            matches!(effects.as_slice(), [Effect::LoadStatus { .. }]),
            "{effects:?}"
        );
    }

    #[test]
    fn a_successful_op_clears_an_earlier_error() {
        let (mut state, _) = with_files(three_files(), 24);
        let (tag, op) = op_of(&state.update(AppIntent::ToggleStage));
        state.op_error = Some("left over".into());
        state.update(finished(tag, op, Ok(())));
        assert_eq!(state.op_error, None);
    }

    #[test]
    fn a_stale_generation_op_lowers_busy_and_does_nothing_else() {
        let (mut state, _) = with_files(three_files(), 24);
        let (tag, op) = op_of(&state.update(AppIntent::ToggleStage));
        let busy = state.busy;
        state.generation += 1;

        let effects = state.update(finished(tag, op, Err("boom".into())));

        assert_eq!(effects, vec![]);
        assert_eq!(state.busy, busy - 1);
        assert_eq!(state.op_error, None);
        assert_eq!(state.op_seq, None, "the slot is freed even when stale");
    }

    #[test]
    fn a_diff_only_plan_reloads_the_selected_diff() {
        let (mut state, _) = with_files(three_files(), 24);
        select_path(&mut state, "b.rs");
        let effects = state.apply_plan(RefreshPlan {
            scopes: [Scope::Diff].into(),
            selection: Selection::KeepPath,
        });
        diff_tag(&effects);
        assert_eq!(state.selected, Some(1));
        assert_eq!(state.diff.path.as_deref(), Some("b.rs"));
    }
}
