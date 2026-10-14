//! Bisecting from the Commits and Status panels: when a mark or a reset
//! may run, which commit a mark is for, and the words shown for it. Pure;
//! the model runs the ops and the view draws the state.

use crate::keymap::{OP_RUNNING, Reason, resumable_operation};
use crate::model::AppState;
use kika_git::bisect::{BisectMark, BisectState, elide};
use kika_git::commit_files::CommitId;
use kika_git::repo_state::Operation;
use kika_git::status::{EntryKind, Head};

/// Where a mark is made: on the selected Commits row, or on the commit git
/// checked out to test, from the Status panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Selected,
    Head,
}

pub(crate) const NOT_TESTING: &str =
    "Mark from the Commits panel until git checks out a commit to test";
pub(crate) const CUSTOM_TERMS: &str =
    "This bisect uses its own terms; mark it from the command line";
pub(crate) const DIRTY: &str =
    "Commit or stash your changes first: bisect checks out other commits";
pub(crate) const NO_BISECT: &str = "No bisect in progress";
/// Why the Status panel's mark keys do nothing before a bisect starts.
pub(crate) const START_FROM_COMMITS: &str =
    "No bisect in progress; mark a commit good or bad in the Commits panel to start one";
/// Why continue does nothing while only a bisect is in progress.
pub(crate) const MARK_INSTEAD: &str = "Mark the commit good or bad";

/// Why `HEAD` may not move nor a commit be made during a bisect.
pub(crate) const RESET_FIRST: &str = "Reset the bisect first: HEAD is the commit it is testing";

/// Why an action that moves `HEAD` or makes a commit cannot run: the
/// bisect is testing the commit checked out, and a commit made on it is
/// lost when the bisect is reset.
pub(crate) fn head_move_disabled(state: &AppState) -> Option<Reason> {
    bisecting(state).then(|| RESET_FIRST.into())
}

/// Whether a bisect is in progress.
pub(crate) fn bisecting(state: &AppState) -> bool {
    state
        .branch
        .as_ref()
        .is_some_and(|b| b.in_progress.contains(&Operation::Bisect))
}

/// The bisect read with the last status, if one is in progress.
pub(crate) fn bisect_state(state: &AppState) -> Option<&BisectState> {
    state
        .branch
        .as_ref()
        .filter(|b| b.in_progress.contains(&Operation::Bisect))
        .and_then(|b| b.bisect.as_ref())
}

/// A bisect is in progress and nothing that continue, skip and abort put
/// first, so the Status panel's keys act on the bisect.
pub(crate) fn bisect_only(state: &AppState) -> bool {
    bisecting(state) && resumable_operation(state).is_none()
}

/// The commit git checked out to test: a detached `HEAD`, as hex.
pub(crate) fn head_commit(state: &AppState) -> Option<CommitId> {
    match &state.branch.as_ref()?.head {
        Head::Detached(oid) => CommitId::parse(oid),
        _ => None,
    }
}

/// The selected Commits row's id, if it is one git can use.
fn selected_commit(state: &AppState) -> Option<CommitId> {
    let row = state.commits.list.rows.get(state.commits.list.selected)?;
    CommitId::parse(&row.oid)
}

/// Why `mark` on `target` cannot run now, checked in a fixed order: one
/// op at a time, a commit to mark, the default terms, and for a mark that
/// would start a bisect, a repository it can start in.
pub(crate) fn mark_disabled(state: &AppState, target: Target, mark: BisectMark) -> Option<Reason> {
    if state.op_is_running() {
        return Some(OP_RUNNING.into());
    }
    match target {
        Target::Selected if selected_commit(state).is_none() => return Some("No commit".into()),
        // Marking HEAD before git has checked one out would start a
        // bisect from the panel that only follows one.
        Target::Head if !bisecting(state) => return Some(START_FROM_COMMITS.into()),
        Target::Head if head_commit(state).is_none() => return Some(NOT_TESTING.into()),
        _ => {}
    }
    if bisect_state(state).is_some_and(|b| !b.terms.is_default()) {
        return Some(CUSTOM_TERMS.into());
    }
    if bisecting(state) {
        return None;
    }
    if mark == BisectMark::Skip {
        return Some(NO_BISECT.into());
    }
    start_disabled(state)
}

/// Why no bisect can start here now. The first mark moves nothing, so
/// there is no question; instead tracked changes are refused up front.
/// Untracked files are allowed: git refuses a checkout that would
/// overwrite one.
fn start_disabled(state: &AppState) -> Option<Reason> {
    let Some(branch) = &state.branch else {
        return Some("Loading…".into());
    };
    if resumable_operation(state).is_some() {
        Some("Finish or abort the operation in progress".into())
    } else if matches!(branch.head, Head::Unborn(_)) {
        Some("No commits yet".into())
    } else if state
        .files
        .rows
        .iter()
        .any(|f| f.kind != EntryKind::Untracked)
    {
        Some(DIRTY.into())
    } else {
        None
    }
}

/// Why the bisect cannot be reset now.
pub(crate) fn reset_disabled(state: &AppState) -> Option<Reason> {
    if state.op_is_running() {
        Some(OP_RUNNING.into())
    } else if !bisecting(state) {
        Some(NO_BISECT.into())
    } else {
        None
    }
}

// One fn per key and menu item, as `Binding` and `MenuItem` take.

pub(crate) fn selected_bad_disabled(state: &AppState) -> Option<Reason> {
    mark_disabled(state, Target::Selected, BisectMark::Bad)
}

pub(crate) fn selected_good_disabled(state: &AppState) -> Option<Reason> {
    mark_disabled(state, Target::Selected, BisectMark::Good)
}

pub(crate) fn selected_skip_disabled(state: &AppState) -> Option<Reason> {
    mark_disabled(state, Target::Selected, BisectMark::Skip)
}

pub(crate) fn head_good_disabled(state: &AppState) -> Option<Reason> {
    mark_disabled(state, Target::Head, BisectMark::Good)
}

pub(crate) fn head_bad_disabled(state: &AppState) -> Option<Reason> {
    mark_disabled(state, Target::Head, BisectMark::Bad)
}

/// The `disabled` of the Status panel's mark keys and their intents.
pub(crate) fn head_disabled(mark: BisectMark) -> fn(&AppState) -> Option<Reason> {
    match mark {
        BisectMark::Good => head_good_disabled,
        BisectMark::Bad => head_bad_disabled,
        BisectMark::Skip => head_skip_disabled,
    }
}

/// The `disabled` of the Commits panel's menu items and their intents.
pub(crate) fn selected_disabled(mark: BisectMark) -> fn(&AppState) -> Option<Reason> {
    match mark {
        BisectMark::Good => selected_good_disabled,
        BisectMark::Bad => selected_bad_disabled,
        BisectMark::Skip => selected_skip_disabled,
    }
}

fn head_skip_disabled(state: &AppState) -> Option<Reason> {
    mark_disabled(state, Target::Head, BisectMark::Skip)
}

/// Continue: never for a bisect, which goes on by marking.
pub(crate) fn continue_disabled(state: &AppState) -> Option<Reason> {
    if !state.op_is_running() && bisect_only(state) {
        return Some(MARK_INSTEAD.into());
    }
    crate::keymap::resume_disabled(state)
}

/// Skip: the commit the operation stopped on, or with only a bisect in
/// progress, the commit being tested.
pub(crate) fn skip_disabled(state: &AppState) -> Option<Reason> {
    if bisect_only(state) {
        return head_skip_disabled(state);
    }
    crate::keymap::skip_disabled(state)
}

/// Abort: the operation in progress, or with only a bisect, a reset.
pub(crate) fn abort_disabled(state: &AppState) -> Option<Reason> {
    if bisect_only(state) {
        return reset_disabled(state);
    }
    crate::keymap::resume_disabled(state)
}

/// The reset question, naming where `HEAD` goes back to.
pub(crate) fn reset_question(bisect: Option<&BisectState>) -> String {
    let start = bisect
        .map(|b| b.start.as_str())
        .filter(|start| !start.is_empty())
        .map_or_else(
            || "where the bisect started".to_string(),
            |start| elide(start, 40),
        );
    format!(
        "Reset the bisect? Its marks are dropped and HEAD goes back to {start}; \
         files change to match. Git refuses if a local change would be overwritten. (y/n)"
    )
}

/// What the bar says when git names the first bad commit. The subject
/// goes last, so a narrow bar cuts it before the way to end the bisect.
pub(crate) fn found_notice(short: &str, subject: &str) -> String {
    format!("Bisect found {short} (a resets): {subject}")
}

/// What the bar says when only skipped commits are left.
pub(crate) const ONLY_SKIPPED: &str = "Only skipped commits are left; see the Status panel";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Context;
    use kika_git::bisect::Terms;
    use kika_git::log::CommitRow;
    use kika_git::status::{BranchStatus, FileEntry};

    const HEX: &str = "abc1234";

    fn row(oid: &str) -> CommitRow {
        CommitRow {
            oid: oid.into(),
            short_oid: oid.chars().take(7).collect(),
            parents: vec![],
            author: "a".into(),
            time: 0,
            subject: "s".into(),
            subject_altered: false,
        }
    }

    fn state(head: Head, ops: &[Operation], bisect: Option<BisectState>) -> AppState {
        let mut state = AppState::new(Context::Workspace);
        state.commits.list.rows = vec![row(HEX)];
        state.branch = Some(BranchStatus {
            head,
            upstream: None,
            ahead_behind: None,
            in_progress: ops.to_vec(),
            bisect,
        });
        state
    }

    fn on_main() -> AppState {
        state(Head::Branch("main".into()), &[], None)
    }

    fn testing() -> AppState {
        state(
            Head::Detached(HEX.into()),
            &[Operation::Bisect],
            Some(BisectState::default()),
        )
    }

    fn file(kind: EntryKind, index: char, worktree: char) -> FileEntry {
        FileEntry {
            path: "a".into(),
            orig_path: None,
            index,
            worktree,
            kind,
        }
    }

    #[test]
    fn a_clean_branch_can_start_with_good_or_bad_but_not_skip() {
        let state = on_main();
        assert_eq!(selected_bad_disabled(&state), None);
        assert_eq!(selected_good_disabled(&state), None);
        assert_eq!(selected_skip_disabled(&state), Some(NO_BISECT.into()));
        assert_eq!(reset_disabled(&state), Some(NO_BISECT.into()));
    }

    #[test]
    fn a_running_op_comes_first() {
        let mut state = testing();
        state.hold_op(1);
        state.commits.list.rows.clear();
        for disabled in [
            selected_bad_disabled,
            head_good_disabled,
            head_bad_disabled,
            reset_disabled,
        ] {
            assert_eq!(disabled(&state), Some(OP_RUNNING.into()));
        }
    }

    #[test]
    fn the_status_panel_marks_only_a_commit_git_checked_out_to_test() {
        assert_eq!(head_good_disabled(&testing()), None);
        let started = state(
            Head::Branch("main".into()),
            &[Operation::Bisect],
            Some(BisectState::default()),
        );
        assert_eq!(head_bad_disabled(&started), Some(NOT_TESTING.into()));
        let detached_without_bisect = state(Head::Detached(HEX.into()), &[], None);
        assert_eq!(
            head_good_disabled(&detached_without_bisect),
            Some(START_FROM_COMMITS.into())
        );
        let mut no_rows = on_main();
        no_rows.commits.list.rows.clear();
        assert_eq!(selected_bad_disabled(&no_rows), Some("No commit".into()));
    }

    #[test]
    fn custom_terms_refuse_every_mark_but_not_the_reset() {
        let custom = BisectState {
            terms: Terms {
                bad: "new".into(),
                good: "old".into(),
            },
            ..BisectState::default()
        };
        let state = state(
            Head::Detached(HEX.into()),
            &[Operation::Bisect],
            Some(custom),
        );
        for disabled in [
            selected_bad_disabled,
            selected_good_disabled,
            selected_skip_disabled,
            head_good_disabled,
            head_bad_disabled,
        ] {
            assert_eq!(disabled(&state), Some(CUSTOM_TERMS.into()));
        }
        assert_eq!(reset_disabled(&state), None);
    }

    #[test]
    fn a_start_is_refused_with_tracked_changes_an_operation_or_no_commits() {
        for (index, worktree, kind) in [
            ('M', '.', EntryKind::Changed),
            ('.', 'M', EntryKind::Changed),
            ('U', 'U', EntryKind::Conflicted),
        ] {
            let mut state = on_main();
            state.files.rows = vec![file(kind, index, worktree)];
            assert_eq!(
                selected_bad_disabled(&state),
                Some(DIRTY.into()),
                "{kind:?}"
            );
        }
        let mut untracked = on_main();
        untracked.files.rows = vec![file(EntryKind::Untracked, '?', '?')];
        assert_eq!(selected_good_disabled(&untracked), None);

        let merging = state(Head::Branch("main".into()), &[Operation::Merge], None);
        assert_eq!(
            selected_bad_disabled(&merging),
            Some("Finish or abort the operation in progress".into())
        );
        let unborn = state(Head::Unborn("main".into()), &[], None);
        assert_eq!(
            selected_bad_disabled(&unborn),
            Some("No commits yet".into())
        );
    }

    #[test]
    fn during_a_bisect_changes_do_not_stop_a_mark() {
        let mut state = testing();
        state.files.rows = vec![file(EntryKind::Changed, '.', 'M')];
        assert_eq!(selected_skip_disabled(&state), None);
        assert_eq!(head_bad_disabled(&state), None);
    }

    #[test]
    fn the_status_keys_follow_the_bisect_only_when_nothing_else_is_in_progress() {
        let state = testing();
        assert_eq!(continue_disabled(&state), Some(MARK_INSTEAD.into()));
        assert_eq!(skip_disabled(&state), None);
        assert_eq!(abort_disabled(&state), None);

        let both = state_with(&[Operation::Rebase, Operation::Bisect]);
        assert_eq!(continue_disabled(&both), None);
        assert_eq!(skip_disabled(&both), None);
        assert_eq!(abort_disabled(&both), None);

        let none = on_main();
        assert_eq!(continue_disabled(&none), Some("Nothing in progress".into()));
        assert_eq!(abort_disabled(&none), Some("Nothing in progress".into()));
    }

    fn state_with(ops: &[Operation]) -> AppState {
        state(
            Head::Detached(HEX.into()),
            ops,
            Some(BisectState::default()),
        )
    }

    #[test]
    fn the_reset_question_names_the_start_or_says_where_it_started() {
        let bisect = BisectState {
            start: "main".into(),
            ..BisectState::default()
        };
        assert!(
            reset_question(Some(&bisect)).contains("HEAD goes back to main; files change"),
            "{}",
            reset_question(Some(&bisect))
        );
        assert!(reset_question(None).contains("back to where the bisect started;"));
        let long = BisectState {
            start: "b".repeat(60),
            ..BisectState::default()
        };
        assert!(reset_question(Some(&long)).contains(&format!("{}…;", "b".repeat(39))));
    }
}
