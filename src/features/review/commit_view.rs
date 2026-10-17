//! Reviewing a pull request commit by commit: `c` on the files screen
//! reads the commits between the merge base and the head once, from local
//! git, then picks "Whole pull request" or one commit, shown against its
//! first parent. The file list and diff follow the pick, and the selected
//! file stays selected, matched by path, when the new view lists it; the
//! viewed marks stay the pull request's, put on again by path. Nothing
//! reaches the network.

use super::files::{FilesStage, ReviewView, files_screen};
use super::git::{MAX_PR_COMMITS, PrCommit};
use super::pending::not_files;
use crate::effect::{Effect, Tag};
use crate::keymap::{OP_RUNNING, Reason};
use crate::model::AppState;
use crate::pick_list::{PickList, PickPurpose};
use crate::popup::Popup;
use crate::view::short_hash;
use kika_forge::forge::Words;

const FILES_LOADING: &str = "The files are still loading";

fn no_range(words: &Words) -> String {
    format!(
        "The {}'s files did not load; press esc, then f",
        words.request
    )
}

fn commits_loading(words: &Words) -> String {
    format!("Reading the {}'s commits…", words.request)
}

/// The pick list's first row: the view of every commit together.
fn whole(words: &Words) -> String {
    format!("Whole {}", words.request)
}

/// The pick list's last row when git listed more commits than are offered.
fn cut_note() -> String {
    format!("(only the latest {MAX_PR_COMMITS} commits)")
}

/// Why `c` cannot pick a view now: the files screen must be shown with
/// its files listed (or failed once the whole range is known, so a commit
/// that failed to list is no dead end), no commit list on its way, and no
/// git op running.
pub(crate) fn review_commit_disabled(state: &AppState) -> Option<Reason> {
    let Some(files) = files_screen(state).filter(|_| state.review_files_shown()) else {
        return Some(not_files(state.review_words()).into());
    };
    match files.stage {
        FilesStage::Ready => {}
        FilesStage::Failed(_) if files.whole.is_some() => {}
        FilesStage::Failed(_) => return Some(no_range(state.review_words()).into()),
        _ => return Some(FILES_LOADING.into()),
    }
    if files.seq.is_some() {
        return Some(commits_loading(state.review_words()).into());
    }
    state.op_is_running().then_some(OP_RUNNING.into())
}

/// `"<7-hex> <subject>"`; the subject was made safe when git's answer was read.
fn commit_row(commit: &PrCommit) -> String {
    format!(
        "{} {}",
        short_hash(commit.range.oid.as_str()),
        commit.subject
    )
}

impl AppState {
    /// `c`: pick the whole pull request or one of its commits, reading
    /// the commits first the first time.
    pub(crate) fn open_review_commits(&mut self) -> Vec<Effect> {
        if let Some(reason) = review_commit_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let Some(files) = &self.review.files else {
            return vec![];
        };
        if files.commits.is_some() {
            self.show_commit_pick();
            return vec![];
        }
        let Some(range) = files.whole.clone() else {
            return vec![];
        };
        // A background read, like the files': it needs no op slot.
        let tag = self.files_tag();
        if let Some(files) = &mut self.review.files {
            files.seq = Some(tag.seq);
        }
        vec![Effect::LoadReviewCommits { tag, range }]
    }

    /// The commits came: keep them for the session of this screen and
    /// offer the pick, unless another popup has the keys by now.
    pub(crate) fn review_commits_loaded(
        &mut self,
        tag: Tag,
        result: Result<(Vec<PrCommit>, bool), String>,
    ) -> Vec<Effect> {
        let Some(files) = self.files_for(tag) else {
            return vec![];
        };
        files.seq = None;
        match result {
            Ok(list) => files.commits = Some(list),
            Err(message) => {
                // Git's words were made safe when they were read.
                self.notice = Some(message.into());
                return vec![];
            }
        }
        if self.popups.is_empty() {
            self.show_commit_pick();
        }
        vec![]
    }

    /// The "Review commit" list, its cursor on the view shown.
    fn show_commit_pick(&mut self) {
        let words = self.review_words();
        let Some(files) = &self.review.files else {
            return;
        };
        let Some((commits, cut)) = &files.commits else {
            return;
        };
        let mut rows = vec![whole(words)];
        rows.extend(commits.iter().map(commit_row));
        if *cut {
            rows.push(cut_note());
        }
        let cursor = match files.view {
            ReviewView::Whole => 0,
            ReviewView::Commit(i) => i + 1,
        };
        self.popups.push(Popup::PickList(PickList::new(
            PickPurpose::ReviewCommit,
            rows,
            cursor,
        )));
    }

    /// The row picked from the "Review commit" list: show that range's
    /// files once they come, on the file selected now when the range has
    /// it (else the first), its diff at its top. The view already shown,
    /// or the note on a cut list, changes nothing.
    pub(crate) fn pick_review_commit(&mut self, row: usize) -> Vec<Effect> {
        if let Some(reason) = review_commit_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let Some(files) = &self.review.files else {
            return vec![];
        };
        let picked = match row.checked_sub(1) {
            None => files.whole.clone().map(|range| (ReviewView::Whole, range)),
            Some(i) => files
                .commits
                .as_ref()
                .and_then(|(commits, _)| commits.get(i))
                .map(|commit| (ReviewView::Commit(i), commit.range.clone())),
        };
        let Some((view, range)) = picked else {
            return vec![];
        };
        if view == files.view && files.stage == FilesStage::Ready {
            return vec![];
        }
        let tag = self.files_tag();
        if let Some(files) = &mut self.review.files {
            files.view = view;
            files.range = Some(range.clone());
            // A view with no row selected leaves the file remembered before.
            files.keep = files
                .selected()
                .map(|r| r.file.path.clone())
                .or(files.keep.take());
            files.rows.clear();
            files.cursor = 0;
            files.stage = FilesStage::Listing;
            files.seq = Some(tag.seq);
            files.thread = None;
            files.jump = None;
            files.reader = None;
        }
        let mut effects = self.close_diff();
        effects.push(Effect::LoadReviewRange { tag, range });
        effects
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use crossterm::event::KeyCode;

    use super::*;
    use crate::features::review::files::PrFiles;
    use crate::features::review::git::FileCounts;
    use crate::features::review::state::tests::{in_detail, press, type_char};
    use crate::features::review::{CommitsHere, PrFileRow, ReviewIntent};
    use crate::intent::AppIntent;
    use crate::keymap::KeyScope;
    use kika_forge::review::ViewedState;
    use kika_git::commit_files::{CommitFile, CommitId, CommitRange};
    use kika_git::diff::DiffRequest;

    /// The texts as GitHub words them.
    const NO_RANGE: &str = "The pull request's files did not load; press esc, then f";
    const COMMITS_LOADING: &str = "Reading the pull request's commits…";

    fn id(c: char) -> CommitId {
        CommitId::parse(&c.to_string().repeat(40)).unwrap()
    }

    /// Merge base `3…` to head `1…`.
    fn whole() -> CommitRange {
        CommitRange {
            oid: id('1'),
            parent: Some(id('3')),
        }
    }

    /// Three commits, oldest first: `a…` on the merge base, `b…` on `a…`,
    /// and the head on `b…`.
    fn commits() -> Vec<PrCommit> {
        let commit = |oid, parent, subject: &str| PrCommit {
            range: CommitRange {
                oid: id(oid),
                parent: Some(id(parent)),
            },
            subject: subject.into(),
        };
        vec![
            commit('a', '3', "Add the cache"),
            commit('b', 'a', "Use it"),
            commit('1', 'b', "Tidy up"),
        ]
    }

    fn row(path: &str) -> PrFileRow {
        PrFileRow {
            file: CommitFile {
                status: 'M',
                path: path.into(),
                orig_path: None,
            },
            counts: FileCounts::Lines {
                added: 1,
                removed: 0,
            },
        }
    }

    fn files(state: &AppState) -> &PrFiles {
        state.review.files.as_ref().unwrap()
    }

    /// Pull request 12's files screen, listed for the whole pull request.
    fn ready() -> AppState {
        let mut state = in_detail(Vec::new());
        // The fixture's detail is for the screen's pull request.
        state.review.detail.as_mut().unwrap().number = 12;
        let effects = type_char(&mut state, 'f');
        let [
            Effect::CheckReviewCommits { tag, .. },
            Effect::LoadPendingReview { .. },
        ] = effects.as_slice()
        else {
            panic!("{effects:?}");
        };
        let tag = *tag;
        state.update(AppIntent::Review(ReviewIntent::ObjectsChecked {
            tag,
            result: Ok(CommitsHere::All),
        }));
        state.update(AppIntent::Review(ReviewIntent::FilesLoaded {
            tag,
            result: Ok((
                whole(),
                vec![row("src/cache.rs"), row("src/lib.rs"), row("docs/café.md")],
            )),
        }));
        assert_eq!(files(&state).stage, FilesStage::Ready);
        assert_eq!(files(&state).whole, Some(whole()));
        // The first load after `f` opens on the first file.
        assert_eq!(files(&state).cursor, 0);
        assert_eq!(files(&state).keep, None);
        state
    }

    /// Press `c` with no commits read yet and return the read it asks for.
    fn ask_commits(state: &mut AppState) -> Tag {
        let effects = type_char(state, 'c');
        match effects.as_slice() {
            [Effect::LoadReviewCommits { tag, range }] => {
                assert_eq!(*range, whole());
                assert!(!tag.user);
                *tag
            }
            other => panic!("expected one commit read, got {other:?}"),
        }
    }

    /// `ready()` with the commits read and the pick list open.
    fn picking() -> AppState {
        let mut state = ready();
        let tag = ask_commits(&mut state);
        state.update(AppIntent::Review(ReviewIntent::CommitsLoaded {
            tag,
            result: Ok((commits(), false)),
        }));
        state
    }

    fn pick_list(state: &AppState) -> &PickList {
        match state.popups.last() {
            Some(Popup::PickList(list)) => list,
            other => panic!("expected a pick list, got {other:?}"),
        }
    }

    /// Move the open list's cursor to `row` and pick it.
    fn pick(state: &mut AppState, row: usize) -> Vec<Effect> {
        let at = pick_list(state).cursor;
        let (key, steps) = if row >= at {
            (KeyCode::Down, row - at)
        } else {
            (KeyCode::Up, at - row)
        };
        for _ in 0..steps {
            press(state, key);
        }
        press(state, KeyCode::Enter)
    }

    fn range_load(effects: &[Effect]) -> (Tag, CommitRange) {
        effects
            .iter()
            .find_map(|e| match e {
                Effect::LoadReviewRange { tag, range } => Some((*tag, range.clone())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no range load: {effects:?}"))
    }

    fn streamed(effects: &[Effect]) -> &DiffRequest {
        effects
            .iter()
            .find_map(|e| match e {
                Effect::LoadDiff { request, .. } => Some(request),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no diff streamed: {effects:?}"))
    }

    /// Nothing in this batch may reach the forge.
    pub(crate) fn assert_local(effects: &[Effect]) {
        for effect in effects {
            assert!(
                !matches!(
                    effect,
                    Effect::SetFileViewed { .. }
                        | Effect::WriteDiscussion { .. }
                        | Effect::MergePullRequest { .. }
                        | Effect::LoadReviewQueues { .. }
                        | Effect::LoadPrDetail { .. }
                        | Effect::LoadPrDiscussion { .. }
                        | Effect::FetchReviewCommits { .. }
                        | Effect::LoadPullRequests { .. }
                        | Effect::OpenUrl { .. }
                ),
                "{effect:?}"
            );
        }
    }

    #[test]
    fn c_reads_the_commits_once_then_offers_the_whole_pull_request_and_each_commit() {
        let mut state = ready();
        let tag = ask_commits(&mut state);
        // A second `c` while git reads asks nothing more.
        assert_eq!(type_char(&mut state, 'c'), []);
        assert_eq!(state.notice.as_deref(), Some(COMMITS_LOADING));
        let effects = state.update(AppIntent::Review(ReviewIntent::CommitsLoaded {
            tag,
            result: Ok((commits(), false)),
        }));
        assert_eq!(effects, []);
        let list = pick_list(&state);
        assert_eq!(list.purpose, PickPurpose::ReviewCommit);
        assert_eq!(list.purpose.title(), "Review commit");
        assert_eq!(
            list.rows,
            [
                "Whole pull request",
                "aaaaaaa Add the cache",
                "bbbbbbb Use it",
                "1111111 Tidy up"
            ]
        );
        assert_eq!(list.cursor, 0);
        assert_eq!(state.key_focus(), KeyScope::PickList);
        // Closing it changes nothing; the next `c` reads nothing again.
        assert_eq!(press(&mut state, KeyCode::Esc), []);
        assert!(state.popups.is_empty());
        assert_eq!(type_char(&mut state, 'c'), []);
        assert_eq!(pick_list(&state).rows.len(), 4);
    }

    #[test]
    fn a_cut_list_ends_with_a_note_that_picks_nothing() {
        let mut state = ready();
        let tag = ask_commits(&mut state);
        state.update(AppIntent::Review(ReviewIntent::CommitsLoaded {
            tag,
            result: Ok((commits(), true)),
        }));
        let rows = pick_list(&state).rows.clone();
        assert_eq!(rows.last().unwrap(), "(only the latest 250 commits)");
        let effects = pick(&mut state, rows.len() - 1);
        assert_eq!(effects, []);
        assert_eq!(files(&state).view, ReviewView::Whole);
        assert_eq!(files(&state).stage, FilesStage::Ready);
    }

    #[test]
    fn picking_a_commit_lists_its_files_against_its_first_parent_and_whole_goes_back() {
        let mut state = picking();
        let effects = pick(&mut state, 2);
        assert_local(&effects);
        assert!(effects.contains(&Effect::CancelDiff), "{effects:?}");
        let (tag, range) = range_load(&effects);
        assert_eq!(range, commits()[1].range);
        let shown = files(&state);
        assert_eq!(shown.view, ReviewView::Commit(1));
        assert_eq!(shown.range, Some(commits()[1].range.clone()));
        assert_eq!(shown.stage, FilesStage::Listing);
        assert_eq!(shown.seq, Some(tag.seq));
        assert!(shown.rows.is_empty());
        assert!(state.popups.is_empty());

        // The commit's files come with the pull request's marks on them.
        let effects = state.update(AppIntent::Review(ReviewIntent::FilesLoaded {
            tag,
            result: Ok((
                range.clone(),
                vec![row("docs/café.md"), row("src/cache.rs")],
            )),
        }));
        assert_local(&effects);
        let shown = files(&state);
        assert_eq!(shown.stage, FilesStage::Ready);
        // `src/cache.rs` was selected, and the commit lists it second.
        assert_eq!(shown.cursor, 1);
        let marks: Vec<_> = shown.rows.iter().map(|r| r.viewed).collect();
        assert_eq!(
            marks,
            [Some(ViewedState::Unviewed), Some(ViewedState::Viewed)]
        );
        // The whole range is kept for going back.
        assert_eq!(shown.whole, Some(whole()));
        let request = streamed(&effects);
        assert_eq!(request.commit, Some(range));
        assert_eq!(request.path, "src/cache.rs".into());
        assert_eq!(state.diff.scroll, 0);

        // The list opens on the commit shown; picking it again does nothing.
        type_char(&mut state, 'c');
        assert_eq!(pick_list(&state).cursor, 2);
        assert_eq!(press(&mut state, KeyCode::Enter), []);

        // Whole pull request: back to the merge base range, with no merge-base asked again.
        type_char(&mut state, 'c');
        let effects = pick(&mut state, 0);
        assert_local(&effects);
        let (tag, range) = range_load(&effects);
        assert_eq!(range, whole());
        assert_eq!(files(&state).view, ReviewView::Whole);
        assert_eq!(files(&state).range, Some(whole()));
        state.update(AppIntent::Review(ReviewIntent::FilesLoaded {
            tag,
            result: Ok((whole(), vec![row("src/lib.rs")])),
        }));
        assert_eq!(files(&state).rows[0].viewed, Some(ViewedState::Dismissed));
    }

    /// Open the list with `c` and pick `row`.
    fn open_and_pick(state: &mut AppState, row: usize) -> Vec<Effect> {
        type_char(state, 'c');
        let effects = pick(state, row);
        assert_local(&effects);
        effects
    }

    fn whole_rows() -> Vec<PrFileRow> {
        vec![row("src/cache.rs"), row("src/lib.rs"), row("docs/café.md")]
    }

    /// Answer the pick's range load with `rows`, checking nothing reached
    /// the forge, and return what the answer asked for.
    fn list(state: &mut AppState, picked: &[Effect], rows: Vec<PrFileRow>) -> Vec<Effect> {
        assert_local(picked);
        let (tag, range) = range_load(picked);
        let effects = state.update(AppIntent::Review(ReviewIntent::FilesLoaded {
            tag,
            result: Ok((range, rows)),
        }));
        assert_local(&effects);
        effects
    }

    /// `picking()` with the list closed and the cursor moved to `row`.
    fn picking_at(row: usize) -> AppState {
        let mut state = picking();
        press(&mut state, KeyCode::Esc);
        for _ in 0..row {
            assert_local(&press(&mut state, KeyCode::Down));
        }
        assert_eq!(files(&state).cursor, row);
        state
    }

    #[test]
    fn switching_between_the_whole_pull_request_and_a_commit_keeps_the_selected_file() {
        let mut state = picking_at(1);
        let picked = open_and_pick(&mut state, 2);
        let effects = list(
            &mut state,
            &picked,
            vec![row("docs/café.md"), row("src/lib.rs")],
        );
        assert_eq!(files(&state).cursor, 1);
        let request = streamed(&effects);
        assert_eq!(request.path, "src/lib.rs".into());
        assert_eq!(request.commit, Some(commits()[1].range.clone()));

        // And back to the whole pull request, on the same file.
        let picked = open_and_pick(&mut state, 0);
        let effects = list(&mut state, &picked, whole_rows());
        assert_eq!(files(&state).cursor, 1);
        let request = streamed(&effects);
        assert_eq!(request.path, "src/lib.rs".into());
        assert_eq!(request.commit, Some(whole()));
        assert_eq!(files(&state).keep, None);
    }

    #[test]
    fn a_file_the_new_view_does_not_list_falls_back_to_the_first() {
        let mut state = picking_at(1);
        let picked = open_and_pick(&mut state, 1);
        let effects = list(
            &mut state,
            &picked,
            vec![row("src/cache.rs"), row("docs/café.md")],
        );
        assert_eq!(files(&state).cursor, 0);
        assert_eq!(streamed(&effects).path, "src/cache.rs".into());
        assert_eq!(files(&state).keep, None);
    }

    #[test]
    fn the_file_is_remembered_through_a_view_that_fails_or_lists_nothing() {
        for failed in [true, false] {
            let mut state = picking_at(1);
            let picked = open_and_pick(&mut state, 1);
            assert_local(&picked);
            let (tag, range) = range_load(&picked);
            let result = if failed {
                Err("fatal: bad tree".to_string())
            } else {
                Ok((range, Vec::new()))
            };
            let effects =
                state.update(AppIntent::Review(ReviewIntent::FilesLoaded { tag, result }));
            assert_local(&effects);
            assert_eq!(files(&state).selected(), None);

            let picked = open_and_pick(&mut state, 0);
            let effects = list(&mut state, &picked, whole_rows());
            assert_eq!(files(&state).cursor, 1, "failed: {failed}");
            assert_eq!(streamed(&effects).path, "src/lib.rs".into());
        }
    }

    #[test]
    fn commit_to_commit_keeps_the_file_when_both_list_it() {
        let mut state = picking();
        let picked = pick(&mut state, 1);
        list(
            &mut state,
            &picked,
            vec![row("src/cache.rs"), row("docs/café.md")],
        );
        assert_eq!(files(&state).cursor, 0);

        let picked = open_and_pick(&mut state, 3);
        let effects = list(
            &mut state,
            &picked,
            vec![row("docs/café.md"), row("src/cache.rs")],
        );
        assert_eq!(files(&state).cursor, 1);
        let request = streamed(&effects);
        assert_eq!(request.path, "src/cache.rs".into());
        assert_eq!(request.commit, Some(commits()[2].range.clone()));
    }

    #[test]
    fn a_stale_answer_leaves_the_kept_file_for_the_answer_that_counts() {
        let mut state = picking_at(1);
        let picked = open_and_pick(&mut state, 2);
        let (tag, range) = range_load(&picked);
        for stale in [
            Tag {
                seq: tag.seq + 1,
                ..tag
            },
            Tag {
                generation: tag.generation + 1,
                ..tag
            },
        ] {
            let effects = state.update(AppIntent::Review(ReviewIntent::FilesLoaded {
                tag: stale,
                result: Ok((range.clone(), vec![row("src/lib.rs")])),
            }));
            assert_eq!(effects, []);
            assert_eq!(files(&state).keep, Some("src/lib.rs".into()));
        }
        let effects = list(
            &mut state,
            &picked,
            vec![row("docs/café.md"), row("src/lib.rs")],
        );
        assert_eq!(files(&state).cursor, 1);
        assert_eq!(streamed(&effects).path, "src/lib.rs".into());
    }

    #[test]
    fn a_kept_file_below_the_first_screen_is_drawn_selected_after_switching_back() {
        use crate::test_support::reversed_at;
        use ratatui::{Terminal, backend::TestBackend};

        let names: Vec<String> = (0..20).map(|i| format!("src/f{i:02}.rs")).collect();
        let rows = || names.iter().map(|n| row(n)).collect::<Vec<_>>();
        let mut state = picking();
        press(&mut state, KeyCode::Esc);
        // The whole pull request lists twenty files; select the eighteenth.
        let picked = open_and_pick(&mut state, 1);
        list(&mut state, &picked, rows());
        let picked = open_and_pick(&mut state, 0);
        list(&mut state, &picked, rows());
        for _ in 0..17 {
            press(&mut state, KeyCode::Down);
        }
        assert_eq!(files(&state).cursor, 17);

        let picked = open_and_pick(&mut state, 2);
        list(&mut state, &picked, vec![row("src/f17.rs")]);
        let picked = open_and_pick(&mut state, 0);
        list(&mut state, &picked, rows());
        assert_eq!(files(&state).cursor, 17);

        state.size = (100, 12);
        let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
        terminal
            .draw(|frame| crate::view::render(&state, frame))
            .unwrap();
        // The cursor bar is drawn reversed, on the kept file's row.
        let buffer = terminal.backend().buffer();
        let drawn = crate::view::buffer_rows(buffer);
        assert!(reversed_at(buffer, "? src/f17.rs"), "{drawn:#?}");
        assert!(!reversed_at(buffer, "? src/f16.rs"), "{drawn:#?}");
        assert!(
            !drawn.iter().any(|r| r.contains("src/f00.rs")),
            "{drawn:#?}"
        );
    }

    #[test]
    fn a_mark_taken_while_another_view_loads_shows_in_the_view_that_comes() {
        let mut state = picking();
        let (tag, range) = range_load(&pick(&mut state, 1));
        let mark = state.take_seq();
        let host = kika_forge::ForgeHost::parse("github.com").unwrap();
        state.review.files.as_mut().unwrap().marking = Some(super::super::files::ViewedMark {
            seq: mark,
            path: "src/lib.rs".into(),
            viewed: true,
        });
        state.update(AppIntent::Review(ReviewIntent::ViewedSet {
            tag: Tag {
                generation: state.generation,
                seq: mark,
                user: true,
            },
            host,
            number: 12,
            path: "src/lib.rs".into(),
            viewed: true,
            result: Ok(()),
        }));
        state.update(AppIntent::Review(ReviewIntent::FilesLoaded {
            tag,
            result: Ok((range, vec![row("src/lib.rs")])),
        }));
        assert_eq!(files(&state).rows[0].viewed, Some(ViewedState::Viewed));
    }

    #[test]
    fn stale_answers_are_dropped() {
        let mut state = ready();
        let tag = ask_commits(&mut state);
        for stale in [
            Tag {
                seq: tag.seq + 1,
                ..tag
            },
            Tag {
                generation: tag.generation + 1,
                ..tag
            },
        ] {
            state.update(AppIntent::Review(ReviewIntent::CommitsLoaded {
                tag: stale,
                result: Ok((commits(), false)),
            }));
            assert_eq!(files(&state).commits, None);
            assert!(state.popups.is_empty());
        }
        state.update(AppIntent::Review(ReviewIntent::CommitsLoaded {
            tag,
            result: Ok((commits(), false)),
        }));
        // The first pick's files are stale once a second pick is made.
        let (first, _) = range_load(&pick(&mut state, 1));
        type_char(&mut state, 'c');
        // Not yet listed: `c` waits for the files.
        assert!(state.popups.is_empty());
        assert_eq!(state.notice.as_deref(), Some(FILES_LOADING));
        state.update(AppIntent::Review(ReviewIntent::FilesLoaded {
            tag: first,
            result: Ok((commits()[0].range.clone(), vec![row("a")])),
        }));
        type_char(&mut state, 'c');
        let (second, _) = range_load(&pick(&mut state, 3));
        let effects = state.update(AppIntent::Review(ReviewIntent::FilesLoaded {
            tag: first,
            result: Ok((commits()[0].range.clone(), vec![row("stale")])),
        }));
        assert_eq!(effects, []);
        assert_eq!(files(&state).stage, FilesStage::Listing);
        assert_eq!(files(&state).view, ReviewView::Commit(2));
        // Leaving the screen drops the answer it waited for.
        press(&mut state, KeyCode::Esc);
        let effects = state.update(AppIntent::Review(ReviewIntent::FilesLoaded {
            tag: second,
            result: Ok((commits()[2].range.clone(), vec![row("late")])),
        }));
        assert_eq!(effects, []);
        assert_eq!(state.review.files, None);
    }

    #[test]
    fn a_failed_read_says_why_and_c_may_try_again() {
        let mut state = ready();
        let tag = ask_commits(&mut state);
        state.update(AppIntent::Review(ReviewIntent::CommitsLoaded {
            tag,
            result: Err("fatal: bad object".into()),
        }));
        assert_eq!(state.notice.as_deref(), Some("fatal: bad object"));
        assert!(state.popups.is_empty());
        ask_commits(&mut state);
    }

    #[test]
    fn a_commit_whose_files_fail_can_still_go_back_to_the_whole_pull_request() {
        let mut state = picking();
        let (tag, _) = range_load(&pick(&mut state, 1));
        state.update(AppIntent::Review(ReviewIntent::FilesLoaded {
            tag,
            result: Err("fatal: bad tree".into()),
        }));
        assert_eq!(
            files(&state).stage,
            FilesStage::Failed("fatal: bad tree".into())
        );
        type_char(&mut state, 'c');
        // The same commit again tries again.
        let (_, range) = range_load(&press(&mut state, KeyCode::Enter));
        assert_eq!(range, commits()[0].range);
    }

    #[test]
    fn c_says_why_when_it_cannot_pick() {
        // On the detail, not the files.
        let mut state = in_detail(Vec::new());
        assert_eq!(type_char(&mut state, 'c'), []);
        assert!(state.popups.is_empty());

        // The whole pull request's files not listed yet.
        let mut state = ready();
        state.review.files.as_mut().unwrap().stage = FilesStage::Listing;
        assert_eq!(type_char(&mut state, 'c'), []);
        assert_eq!(state.notice.as_deref(), Some(FILES_LOADING));

        // They failed before the merge base was known.
        let mut state = ready();
        let files = state.review.files.as_mut().unwrap();
        files.stage = FilesStage::Failed("no".into());
        files.whole = None;
        assert_eq!(type_char(&mut state, 'c'), []);
        assert_eq!(state.notice.as_deref(), Some(NO_RANGE));

        // Git is busy with an op.
        let mut state = ready();
        state.hold_op(77);
        assert_eq!(type_char(&mut state, 'c'), []);
        assert_eq!(state.notice.as_deref(), Some(OP_RUNNING));

        // From the focused diff, `c` works as on the list.
        let mut state = ready();
        press(&mut state, KeyCode::Tab);
        assert_eq!(state.panel_focus(), KeyScope::ReviewDiff);
        ask_commits(&mut state);
    }

    #[test]
    fn the_commits_arriving_under_another_popup_wait_for_the_next_c() {
        let mut state = ready();
        let tag = ask_commits(&mut state);
        state.popups.push(Popup::PickList(PickList::new(
            PickPurpose::CheckDetails,
            vec!["x".into()],
            0,
        )));
        state.update(AppIntent::Review(ReviewIntent::CommitsLoaded {
            tag,
            result: Ok((commits(), false)),
        }));
        assert_eq!(state.popups.len(), 1);
        assert!(files(&state).commits.is_some());
    }
    #[test]
    fn a_hostile_subject_is_drawn_escaped_in_the_pick_list() {
        use crate::features::review::git::{pr_commits, pr_commits_command};
        use kika_git::fake::{FakeRunner, ok};
        use ratatui::{Terminal, backend::TestBackend};

        let argv: Vec<String> = pr_commits_command(&whole())
            .args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let fake = FakeRunner::new().expect(
            &argv,
            ok("bbbb\0aaaa\0Fix \x1b]52;c;eA==\x07\x1b[2Jit\u{202e}gnp.exe\n".as_bytes()),
        );
        let mut state = ready();
        state.size = (100, 24);
        let tag = ask_commits(&mut state);
        state.update(AppIntent::Review(ReviewIntent::CommitsLoaded {
            tag,
            result: pr_commits(&fake, &whole()),
        }));
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|frame| crate::view::render(&state, frame))
            .unwrap();
        let rows = crate::view::buffer_rows(terminal.backend().buffer());
        assert!(
            rows.iter().any(|r| r.contains("Review commit")),
            "{rows:#?}"
        );
        assert!(
            rows.iter().any(|r| r.contains("Whole pull request")),
            "{rows:#?}"
        );
        let row = rows
            .iter()
            .find(|r| r.contains("bbbb Fix"))
            .unwrap_or_else(|| panic!("{rows:#?}"));
        assert!(!row.chars().any(char::is_control), "{row:?}");
        assert!(row.contains("<U+202E>"), "{row:?}");
        assert!(!row.contains('\u{202e}'), "{row:?}");
    }
}
