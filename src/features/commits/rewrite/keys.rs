//! The rewrite keys of chunk 12 (squash, fixup, create fixup,
//! autosquash, find fixup base, drop and reword, then move, edit, break
//! and pick, on a log row or a todo row), the commits-left-behind,
//! autosquash and drop-range questions (chunks 59, 62 and 64), and the
//! guards only they use. `CHUNKS` in `keymap.rs` places each chunk where
//! the registry had it, so its position there is behaviour.

use crossterm::event::{KeyCode, KeyModifiers};

use super::{
    Rewrite, autosquash_disabled, create_fixup_disabled, find_base_disabled,
    range_rewrite_disabled, rewrite_disabled,
};
use crate::confirm_question::Answer;
use crate::features::commits::CommitsIntent;
use crate::features::commits::rewrite::RewriteIntent;
use crate::intent::AppIntent;
use crate::keymap::{Binding, KeyPattern, KeyScope, NONE, Reason, list_guard, never};
use crate::model::AppState;
use kika_git::rebase_plan::{Direction, Edit};

fn squash_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Edit(Edit::Squash))
}

fn fixup_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Edit(Edit::Fixup))
}

fn drop_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Edit(Edit::Drop))
}

fn reword_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Edit(Edit::Reword))
}

// Squash, fixup and drop act on a range too, in one rebase.
pub(crate) fn commits_squash_disabled(state: &AppState) -> Option<Reason> {
    commits_range_rewrite_disabled(state, Edit::Squash, squash_disabled)
}

pub(crate) fn commits_fixup_disabled(state: &AppState) -> Option<Reason> {
    commits_range_rewrite_disabled(state, Edit::Fixup, fixup_disabled)
}

pub(crate) fn commits_drop_disabled(state: &AppState) -> Option<Reason> {
    commits_range_rewrite_disabled(state, Edit::Drop, drop_disabled)
}

/// `single`'s reason without a range, [`range_rewrite_disabled`]'s with
/// one; the list's own reasons come first either way.
fn commits_range_rewrite_disabled(
    state: &AppState,
    edit: Edit,
    single: fn(&AppState) -> Option<Reason>,
) -> Option<Reason> {
    list_guard(state, true).or_else(|| {
        if state.commits.list.range_anchor.is_some() {
            range_rewrite_disabled(state, edit)
        } else {
            single(state)
        }
    })
}

pub(crate) fn commits_reword_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| reword_disabled(state))
}

pub(crate) fn stop_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Edit(Edit::Stop))
}

fn todo_pick_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Edit(Edit::Pick))
}

fn break_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Break)
}

fn move_up_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Move(Direction::Up))
}

fn move_down_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Move(Direction::Down))
}

// Chunk 12: rewrite, first half
pub(crate) static REWRITE_COMMITS: &[Binding] = &[
    Binding {
        id: "commits.squash",
        keys: &[KeyPattern::exact(KeyCode::Char('s'), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::Edit(Edit::Squash))),
        description: "Squash the selected commit into the one below, combining their messages",
        label: "squash",
        bar: true,
        disabled: commits_squash_disabled,
    },
    // `f` is fetch.
    Binding {
        id: "commits.fixup",
        keys: &[KeyPattern::any(KeyCode::Char('F'))],
        scope: KeyScope::Commits,
        intent: AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::Edit(Edit::Fixup))),
        description: "Fold the selected commit into the one below, keeping that one's message",
        label: "fixup",
        bar: true,
        disabled: commits_fixup_disabled,
    },
    Binding {
        id: "commits.create_fixup",
        keys: &[KeyPattern::exact(KeyCode::Char('f'), KeyModifiers::CONTROL)],
        scope: KeyScope::Commits,
        intent: AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::CreateFixup)),
        description: "Commit the staged change as a fixup! of the selected commit",
        label: "create fixup",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: create_fixup_disabled,
    },
    Binding {
        id: "commits.autosquash",
        keys: &[KeyPattern::any(KeyCode::Char('S'))],
        scope: KeyScope::Commits,
        intent: AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::Autosquash)),
        description: "Fold every fixup! commit into the commit it fixes, after asking",
        label: "autosquash",
        bar: false,
        disabled: autosquash_disabled,
    },
    Binding {
        id: "commits.find_base",
        keys: &[KeyPattern::exact(KeyCode::Char('b'), KeyModifiers::CONTROL)],
        scope: KeyScope::Commits,
        intent: AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::FindFixupBase)),
        description: "Select the commit that last touched the staged lines",
        label: "find fixup base",
        bar: false,
        disabled: find_base_disabled,
    },
    Binding {
        id: "commits.drop",
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::Edit(Edit::Drop))),
        description: "Drop the selected commit and its change from the history",
        label: "drop",
        bar: true,
        disabled: commits_drop_disabled,
    },
    // `r` is refresh.
    Binding {
        id: "commits.reword",
        keys: &[KeyPattern::exact(KeyCode::Char('e'), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::Edit(Edit::Reword))),
        description: "Write a new message for the selected commit",
        label: "reword",
        bar: true,
        disabled: commits_reword_disabled,
    },
];

// Chunk 12: rewrite, second half
pub(crate) static REWRITE_TODO_COMMITS: &[Binding] = &[
    // `J` and `K` scroll the diff from the Workspace.
    Binding {
        id: "commits.move_up",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), KeyModifiers::ALT),
            KeyPattern::exact(KeyCode::Up, KeyModifiers::ALT),
        ],
        scope: KeyScope::Commits,
        intent: AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::Move(Direction::Up))),
        description: "Move the selected commit or todo row up, above the newer one",
        label: "move up",
        bar: true,
        disabled: move_up_disabled,
    },
    Binding {
        id: "commits.move_down",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), KeyModifiers::ALT),
            KeyPattern::exact(KeyCode::Down, KeyModifiers::ALT),
        ],
        scope: KeyScope::Commits,
        intent: AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::Move(Direction::Down))),
        description: "Move the selected commit or todo row down, below the older one",
        label: "move down",
        bar: true,
        disabled: move_down_disabled,
    },
    Binding {
        id: "commits.edit",
        keys: &[KeyPattern::any(KeyCode::Char('E'))],
        scope: KeyScope::Commits,
        intent: AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::Edit(Edit::Stop))),
        description: "Stop the rebase at the selected commit, to amend it, then continue from Status",
        label: "edit",
        bar: true,
        disabled: stop_disabled,
    },
    Binding {
        id: "commits.break",
        keys: &[KeyPattern::exact(KeyCode::Char('b'), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::InsertBreak)),
        description: "Stop the rebase right after the selected commit, or toggle the stop after a todo row",
        label: "break",
        bar: true,
        disabled: break_disabled,
    },
    // `p` is pull; a pick is git's "use commit".
    Binding {
        id: "commits.pick",
        keys: &[KeyPattern::exact(KeyCode::Char('u'), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::Edit(Edit::Pick))),
        description: "Pick the selected todo row as it is, undoing its other action",
        label: "pick",
        bar: true,
        disabled: todo_pick_disabled,
    },
];

// Chunk 59: rewrite
pub(crate) static REWRITE_LEAVE_COMMITS_CONFIRM: &[Binding] = &[
    // As with a hard reset, Enter is left unbound: commits left behind
    // are only in the reflog.
    Binding {
        id: "leave_commits.yes",
        keys: Answer::YOnly.keys(),
        scope: KeyScope::LeaveCommits,
        intent: AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::ConfirmLeaveCommits)),
        description: "Yes, move HEAD and leave the commits behind",
        label: "leave",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "leave_commits.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::LeaveCommits,
        intent: AppIntent::ConfirmNo,
        description: "No, stay where HEAD is",
        label: "stay",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "leave_commits.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::LeaveCommits,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];

// Chunk 62: rewrite
pub(crate) static REWRITE_AUTOSQUASH_CONFIRM: &[Binding] = &[
    // Enter is left unbound here too: autosquash rewrites commits the user
    // may not be looking at.
    Binding {
        id: "autosquash_confirm.yes",
        keys: Answer::YOnly.keys(),
        scope: KeyScope::AutosquashConfirm,
        intent: AppIntent::ConfirmYes,
        description: "Yes, fold every fixup! commit into its target",
        label: "fold",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "autosquash_confirm.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::AutosquashConfirm,
        intent: AppIntent::ConfirmNo,
        description: "No, leave the history as it is",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "autosquash_confirm.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::AutosquashConfirm,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];

// Chunk 64: rewrite
pub(crate) static REWRITE_DROP_RANGE_CONFIRM: &[Binding] = &[
    // Enter is left unbound here too: dropping a range throws away the
    // changes of several commits at once, so only a deliberate y answers.
    Binding {
        id: "drop_range_confirm.yes",
        keys: Answer::YOnly.keys(),
        scope: KeyScope::DropRangeConfirm,
        intent: AppIntent::ConfirmYes,
        description: "Yes, drop the selected commits and their changes",
        label: "drop",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "drop_range_confirm.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::DropRangeConfirm,
        intent: AppIntent::ConfirmNo,
        description: "No, leave the history as it is",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "drop_range_confirm.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::DropRangeConfirm,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::commits::git as branch_tips;
    use crate::features::commits::keys::TIPS_UNKNOWN;
    use crate::features::commits::rewrite::{
        FILTER_HIDES_NEIGHBOUR, STAGE_THE_FIX, autosquash_plan, range_log_plan,
    };
    use crate::keymap::{
        CLEAR_LOG_FILTER, CLEAR_RANGE_FIRST, FILTER_HIDES_ROW, Keymap, OP_RUNNING, REPO_LOCKED,
        Resolution, Trust, registry, resolve,
    };
    use crate::model::{Context, Focus};
    use crate::test_support::{binding, build, key, plain};
    use kika_git::commit_files::CommitId;
    use kika_git::rebase_plan;
    use kika_git::repo_state::Operation;
    use kika_git::status::{EntryKind, Head};
    use kika_git::version;

    #[test]
    fn rewrites_say_why_they_cannot_run() {
        let ids = ['a', 'b', 'c'].map(|d| d.to_string().repeat(40));
        let row = |i: usize, parents: Vec<String>| kika_git::log::CommitRow {
            oid: ids[i].clone(),
            short_oid: "x".into(),
            parents,
            author: "Ann".into(),
            time: 0,
            subject: "s".into(),
            subject_altered: false,
        };
        let all = [
            squash_disabled,
            fixup_disabled,
            drop_disabled,
            reword_disabled,
            stop_disabled,
        ];
        let mut state = AppState::new(Context::Workspace);
        state.commits.tips = Some(vec![]);
        for disabled in all {
            assert_eq!(disabled(&state), Some("No commits".into()));
        }
        // `a` on `b`, the root.
        state.commits.list.rows = vec![row(0, vec![ids[1].clone()]), row(1, vec![])];
        assert_eq!(drop_disabled(&state), None);
        assert_eq!(reword_disabled(&state), None);
        let below_is_root = Some("The commit below is the first commit");
        assert_eq!(squash_disabled(&state).as_deref(), below_is_root);
        assert_eq!(fixup_disabled(&state).as_deref(), below_is_root);
        state.commits.list.selected = 1;
        for disabled in all {
            assert_eq!(
                disabled(&state),
                Some("Cannot rewrite the first commit".into())
            );
        }

        state.commits.list.selected = 0;
        state.commits.list.rows[1].parents = vec![ids[2].clone()];
        for disabled in all {
            assert_eq!(disabled(&state), None);
        }
        state.branch = Some(kika_git::status::BranchStatus {
            head: Head::Branch("main".into()),
            upstream: None,
            ahead_behind: None,
            in_progress: vec![Operation::Rebase],
            bisect: None,
        });
        for disabled in all {
            assert_eq!(
                disabled(&state),
                Some("Finish or abort the operation in progress".into())
            );
        }
        state.hold_op(1);
        for disabled in all {
            assert_eq!(disabled(&state), Some(OP_RUNNING.into()));
        }
    }

    /// A linear log `a`..`d` on `e`, the Commits panel focused, with the
    /// range from `b` (row 1) to `c` (row 2) selected.
    fn range_state() -> AppState {
        let ids = ['a', 'b', 'c', 'd', 'e'].map(|d| d.to_string().repeat(40));
        let mut state = AppState::new(Context::Workspace);
        state.focus = Focus::Commits;
        state.commits.list.rows = (0..4)
            .map(|i| kika_git::log::CommitRow {
                oid: ids[i].clone(),
                short_oid: ids[i][..7].into(),
                parents: vec![ids[i + 1].clone()],
                author: "Ann".into(),
                time: 0,
                subject: "keep".into(),
                subject_altered: false,
            })
            .collect();
        state.commits.tips = Some(vec![]);
        state.commits.list.range_anchor = Some(ids[1].clone());
        state.commits.list.selected = 2;
        state
    }

    const RANGE_KEYS: [&str; 3] = ["commits.squash", "commits.fixup", "commits.drop"];

    fn range_reasons(state: &AppState) -> Vec<Option<Reason>> {
        RANGE_KEYS
            .iter()
            .map(|id| (binding(id).disabled)(state))
            .collect()
    }

    #[test]
    fn squash_fixup_and_drop_take_a_range_and_say_why_not() {
        let state = range_state();
        assert_eq!(range_reasons(&state), [None, None, None]);
        assert_eq!(
            (binding("commits.reword").disabled)(&state).as_deref(),
            Some(CLEAR_RANGE_FIRST)
        );
        let plan = range_log_plan(&state, Edit::Squash).unwrap();
        assert_eq!(plan.base.as_str(), "e".repeat(40));

        // A filter that hides `b`, between `a` and `c`.
        let mut gap = range_state();
        gap.commits.list.rows[1].subject = "hide".into();
        gap.commits.list.filter = "keep".into();
        gap.commits.list.range_anchor = Some("a".repeat(40));
        assert_eq!(gap.commits.list.range(), [0, 2]);
        let mut todo = range_state();
        todo.commits.todo_selected = Some(0);
        let mut narrowed = range_state();
        narrowed.commits.query.author = Some("ann".into());
        let mut running = range_state();
        running.hold_op(1);
        let mut rebasing = range_state();
        in_rebase(&mut rebasing);
        let mut stacked = range_state();
        stacked.commits.tips = Some(vec![tip("c", "stack")]);
        let left_behind = branch_tips::left_behind(b"refs/heads/stack");
        let cases = [
            (&gap, rebase_plan::RANGE_GAP),
            (&todo, "Select a commit in the log first"),
            (&narrowed, CLEAR_LOG_FILTER),
            (&running, OP_RUNNING),
            (&rebasing, "Finish or abort the operation in progress"),
            (&stacked, left_behind.as_str()),
        ];
        for (state, why) in cases {
            for (id, reason) in RANGE_KEYS.iter().zip(range_reasons(state)) {
                assert_eq!(reason.as_deref(), Some(why), "{id}");
            }
        }

        // With `--update-refs` the stacked branch moves along.
        stacked.git_version = Some(version::UPDATE_REFS);
        assert_eq!(range_reasons(&stacked), [None, None, None]);
    }

    #[test]
    fn only_y_answers_the_drop_range_question_and_a_repository_cannot_move_it() {
        let state = range_state();
        let ask = |key| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::DropRangeConfirm,
            &state,
            key,
        ) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            Resolution::Unbound => None,
            other => panic!("{key:?}: {other:?}"),
        };
        let none = KeyModifiers::NONE;
        assert_eq!(ask(key(KeyCode::Enter, none)), None);
        assert_eq!(ask(plain('q')), None);
        assert_eq!(ask(plain('y')), Some(AppIntent::ConfirmYes));
        assert_eq!(ask(plain('n')), Some(AppIntent::ConfirmNo));
        assert_eq!(ask(key(KeyCode::Esc, none)), Some(AppIntent::ConfirmNo));
        assert_eq!(
            ask(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(AppIntent::Quit)
        );
        assert_eq!(KeyScope::DropRangeConfirm.parent(), None);
        assert_eq!(KeyScope::DropRangeConfirm.title(), "Drop commits");
        for id in [
            "drop_range_confirm.yes",
            "drop_range_confirm.no",
            "drop_range_confirm.quit",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
            assert!(build(&[(id, &["enter"])], Trust::Repo).is_err(), "{id}");
            assert!(build(&[(id, &["Z"])], Trust::Global).is_ok(), "{id}");
        }
    }

    /// HEAD `fixup! A` (`d…`), then `B` (`b…`), then `A` (`a…`) on `9…`,
    /// not loaded; `a.txt` staged and the Commits panel focused on `HEAD`.
    fn fixup_state() -> AppState {
        let row = |digit: &str, parent: &str, subject: &str| kika_git::log::CommitRow {
            oid: digit.repeat(40),
            short_oid: digit.repeat(7),
            parents: vec![parent.repeat(40)],
            author: "Ann".into(),
            time: 0,
            subject: subject.into(),
            subject_altered: false,
        };
        let mut state = AppState::new(Context::Workspace);
        state.commits.list.rows = vec![
            row("d", "b", "fixup! A"),
            row("b", "a", "B"),
            row("a", "9", "A"),
        ];
        state.commits.tips = Some(vec![]);
        state.focus = Focus::Commits;
        state.files.rows = vec![kika_git::FileEntry {
            path: kika_git::path::RepoPath::from("a.txt"),
            orig_path: None,
            index: 'M',
            worktree: '.',
            kind: EntryKind::Changed,
        }];
        state
    }

    fn in_rebase(state: &mut AppState) {
        state.branch = Some(kika_git::status::BranchStatus {
            head: Head::Branch("main".into()),
            upstream: None,
            ahead_behind: None,
            in_progress: vec![Operation::Rebase],
            bisect: None,
        });
    }

    fn tip(digit: &str, name: &str) -> crate::features::commits::git::Tip {
        (
            CommitId::parse(&digit.repeat(40)).unwrap(),
            format!("refs/heads/{name}").into_bytes(),
        )
    }

    #[test]
    fn a_fixup_needs_a_staged_change_and_one_shown_log_commit() {
        let state = fixup_state();
        assert_eq!(create_fixup_disabled(&state), None);
        assert_eq!(find_base_disabled(&state), None);

        // The target is named by id, so a narrowed log is fine.
        let mut narrowed = fixup_state();
        narrowed.commits.query.author = Some("ann".into());
        assert_eq!(create_fixup_disabled(&narrowed), None);

        let mut unstaged = fixup_state();
        unstaged.files.rows[0].index = '.';
        unstaged.files.rows[0].worktree = 'M';
        let mut untracked = fixup_state();
        untracked.files.rows[0].index = '?';
        untracked.files.rows[0].kind = EntryKind::Untracked;
        let mut clean = fixup_state();
        clean.files.rows.clear();
        for state in [&unstaged, &untracked, &clean] {
            assert_eq!(create_fixup_disabled(state).as_deref(), Some(STAGE_THE_FIX));
            assert_eq!(find_base_disabled(state).as_deref(), Some(STAGE_THE_FIX));
        }

        let mut todo = fixup_state();
        todo.commits.todo_selected = Some(0);
        let mut hidden = fixup_state();
        hidden.commits.list.filter = "zzz".into();
        let mut range = fixup_state();
        range.commits.list.range_anchor = Some("b".repeat(40));
        let mut running = fixup_state();
        running.hold_op(1);
        let mut unborn = fixup_state();
        unborn.commits.list.rows.clear();
        unborn.branch = Some(kika_git::status::BranchStatus {
            head: Head::Unborn("main".into()),
            upstream: None,
            ahead_behind: None,
            in_progress: vec![],
            bisect: None,
        });
        let cases = [
            (&todo, "Select a commit in the log first"),
            (&hidden, FILTER_HIDES_ROW),
            (&range, CLEAR_RANGE_FIRST),
            (&running, OP_RUNNING),
            (&unborn, "No commits"),
        ];
        for (state, why) in cases {
            assert_eq!(create_fixup_disabled(state).as_deref(), Some(why));
            assert_eq!(find_base_disabled(state).as_deref(), Some(why));
        }

        // Finding the base only reads, so a rebase in progress is fine.
        let mut rebasing = fixup_state();
        in_rebase(&mut rebasing);
        assert_eq!(
            create_fixup_disabled(&rebasing).as_deref(),
            Some("Finish or abort the operation in progress")
        );
        assert_eq!(find_base_disabled(&rebasing), None);
    }

    #[test]
    fn autosquash_plans_from_the_whole_log_and_says_why_not() {
        let state = fixup_state();
        assert_eq!(autosquash_disabled(&state), None);
        let plan = autosquash_plan(&state).unwrap();
        let actions: Vec<_> = plan
            .steps
            .iter()
            .map(|s| (s.action, &s.commit.as_str()[..1]))
            .collect();
        assert_eq!(
            actions,
            [
                (rebase_plan::TodoAction::Pick, "a"),
                (rebase_plan::TodoAction::Fixup, "d"),
                (rebase_plan::TodoAction::Pick, "b")
            ]
        );
        assert_eq!(plan.base.as_str(), "9".repeat(40));

        let mut narrowed = fixup_state();
        narrowed.commits.query.author = Some("ann".into());
        let mut filtered = fixup_state();
        filtered.commits.list.filter = "fixup".into();
        let mut todo = fixup_state();
        todo.commits.todo_selected = Some(0);
        let mut running = fixup_state();
        running.hold_op(1);
        let mut rebasing = fixup_state();
        in_rebase(&mut rebasing);
        let mut none = fixup_state();
        none.commits.list.rows[0].subject = "D".into();
        let cases = [
            (&narrowed, CLEAR_LOG_FILTER),
            (&filtered, FILTER_HIDES_NEIGHBOUR),
            (&todo, "Select a commit in the log first"),
            (&running, OP_RUNNING),
            (&rebasing, "Finish or abort the operation in progress"),
            (&none, rebase_plan::NO_FIXUPS),
        ];
        for (state, why) in cases {
            assert_eq!(autosquash_disabled(state).as_deref(), Some(why));
        }
    }

    #[test]
    fn autosquash_and_stacked_branches() {
        // Without `--update-refs` any other branch in the range is left behind.
        let mut old_git = fixup_state();
        old_git.commits.tips = Some(vec![tip("a", "stack")]);
        assert_eq!(
            autosquash_disabled(&old_git).as_deref(),
            Some(branch_tips::left_behind(b"refs/heads/stack").as_str())
        );
        old_git.commits.tips = None;
        assert_eq!(autosquash_disabled(&old_git).as_deref(), Some(TIPS_UNKNOWN));

        // With it, only a branch the fixup moves past, or on the fixup, is.
        let with = |tips| {
            let mut state = fixup_state();
            state.git_version = Some(version::UPDATE_REFS);
            state.branch = Some(kika_git::status::BranchStatus {
                head: Head::Branch("main".into()),
                upstream: None,
                ahead_behind: None,
                in_progress: vec![],
                bisect: None,
            });
            state.commits.tips = Some(tips);
            autosquash_disabled(&state)
        };
        assert_eq!(with(vec![tip("a", "stack"), tip("d", "main")]), None);
        for on in ["b", "d"] {
            assert_eq!(
                with(vec![tip(on, "stack")]).as_deref(),
                Some(rebase_plan::crossing(&"d".repeat(40), b"refs/heads/stack").as_str()),
                "{on}"
            );
        }
    }

    #[test]
    fn the_fixup_keys_and_which_a_repository_may_move() {
        let state = fixup_state();
        let on = |key| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::Commits,
            &state,
            key,
        ) {
            Resolution::Run(binding) => binding.intent.clone(),
            other => panic!("{key:?}: {other:?}"),
        };
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(
            on(key(KeyCode::Char('f'), ctrl)),
            AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::CreateFixup))
        );
        assert_eq!(
            on(key(KeyCode::Char('S'), KeyModifiers::SHIFT)),
            AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::Autosquash))
        );
        assert_eq!(
            on(key(KeyCode::Char('b'), ctrl)),
            AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::FindFixupBase))
        );
        for id in ["commits.create_fixup", "commits.autosquash"] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
            assert!(build(&[(id, &["Z"])], Trust::Repo).is_err(), "{id}");
            assert!(build(&[(id, &["Z"])], Trust::Global).is_ok(), "{id}");
        }
        assert!(!REPO_LOCKED.contains(&"commits.find_base"));

        // The question: only `y` folds; Enter answers nothing.
        let ask = |key| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::AutosquashConfirm,
            &state,
            key,
        ) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            Resolution::Unbound => None,
            other => panic!("{key:?}: {other:?}"),
        };
        let none = KeyModifiers::NONE;
        assert_eq!(ask(key(KeyCode::Enter, none)), None);
        assert_eq!(ask(plain('q')), None);
        assert_eq!(ask(plain('y')), Some(AppIntent::ConfirmYes));
        assert_eq!(ask(plain('n')), Some(AppIntent::ConfirmNo));
        assert_eq!(ask(key(KeyCode::Esc, none)), Some(AppIntent::ConfirmNo));
        assert_eq!(ask(key(KeyCode::Char('c'), ctrl)), Some(AppIntent::Quit));
        assert_eq!(KeyScope::AutosquashConfirm.parent(), None);
        for id in [
            "autosquash_confirm.yes",
            "autosquash_confirm.no",
            "autosquash_confirm.quit",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
            assert!(build(&[(id, &["enter"])], Trust::Repo).is_err(), "{id}");
        }
        assert!(build(&[("commits.find_base", &["Z"])], Trust::Repo).is_ok());
    }

    #[test]
    fn the_leave_commits_question_has_no_enter_and_n_or_esc_stays() {
        let state = AppState::new(Context::Workspace);
        let resolve_in = |key| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::LeaveCommits,
            &state,
            key,
        ) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            Resolution::Unbound => None,
            other => panic!("{key:?}: {other:?}"),
        };
        let none = KeyModifiers::NONE;
        assert_eq!(resolve_in(key(KeyCode::Enter, none)), None);
        assert_eq!(resolve_in(plain('q')), None);
        assert_eq!(
            resolve_in(plain('y')),
            Some(AppIntent::Commits(CommitsIntent::Rewrite(
                RewriteIntent::ConfirmLeaveCommits
            )))
        );
        assert_eq!(resolve_in(plain('n')), Some(AppIntent::ConfirmNo));
        assert_eq!(
            resolve_in(key(KeyCode::Esc, none)),
            Some(AppIntent::ConfirmNo)
        );
        assert_eq!(
            resolve_in(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(AppIntent::Quit)
        );
        assert_eq!(KeyScope::LeaveCommits.parent(), None);
        assert_eq!(KeyScope::LeaveCommits.title(), "Leave commits behind");

        let enter = key(KeyCode::Enter, none);
        for b in registry()
            .iter()
            .filter(|b| b.id.starts_with("leave_commits."))
        {
            assert!(!b.keys.iter().any(|k| k.matches(enter)), "{}", b.id);
        }
    }

    #[test]
    fn a_repository_layer_cannot_rebind_the_leave_commits_answers() {
        for id in [
            "leave_commits.yes",
            "leave_commits.no",
            "leave_commits.quit",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
            assert_eq!(
                build(&[(id, &["enter"])], Trust::Repo).unwrap_err(),
                format!("\"{id}\" can only be changed in the global config file")
            );
            assert!(build(&[(id, &["F"])], Trust::Repo).is_err(), "{id}");
        }
    }
}
