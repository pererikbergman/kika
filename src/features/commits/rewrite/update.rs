//! The rewrite side of `AppState`: a rewrite of the selected log commit
//! handed to the terminal, or an edit of the stopped rebase's todo list
//! written back, and the todo list's loads.

use super::todo::{self, RebaseTodo, Rewrite, TODO_CHANGED, TodoFile};
use super::{log_plan, rewrite_disabled, todo_edit};
use crate::effect::{Effect, Tag};
use crate::git::rebase_plan;
use crate::model::{AppState, Context};
use crate::op::GitOp;
use crate::worktree_file::CHANGED_ON_DISK;

impl AppState {
    /// Rewrite the selected commit on the terminal: squash and reword open
    /// git's editor, and hooks, signing and conflict stops work as on the
    /// command line. On a todo row, edit the stopped rebase's todo list
    /// instead.
    pub(crate) fn rewrite(&mut self, rewrite: Rewrite) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if let Some(reason) = rewrite_disabled(self, rewrite) {
            self.notice = Some(reason);
            return vec![];
        }
        if self.commits.todo_selected.is_some() {
            return self.edit_todo(rewrite);
        }
        match log_plan(self, rewrite) {
            Ok(plan) => {
                // The cursor follows the moved commit.
                match rewrite {
                    Rewrite::Move(rebase_plan::Direction::Up) => self.commits.selected -= 1,
                    Rewrite::Move(rebase_plan::Direction::Down) => self.commits.selected += 1,
                    _ => {}
                }
                self.run_op_where_it_belongs(GitOp::InteractiveRebase(plan))
            }
            Err(reason) => {
                self.notice = Some(reason);
                vec![]
            }
        }
    }

    /// Write the todo list with `rewrite` made on the selected row. The
    /// list shows the edit at once; the reload after the write shows what
    /// git has.
    fn edit_todo(&mut self, rewrite: Rewrite) -> Vec<Effect> {
        let (entries, cursor) = match todo_edit(self, rewrite) {
            Ok(edited) => edited,
            Err(reason) => {
                self.notice = Some(reason);
                return vec![];
            }
        };
        let Some(todo) = self.commits.todo.as_mut() else {
            return vec![];
        };
        let content = todo::serialize(&entries);
        let (path, expected) = (todo.path.clone(), todo.original.clone());
        let comment = todo.comment.clone();
        todo.entries = entries;
        self.commits.todo_selected = Some(cursor);
        let seq = self.take_seq();
        self.commits.todo_write_seq = Some(seq);
        self.busy += 1;
        let tag = Tag {
            generation: self.generation,
            seq,
            user: true,
        };
        vec![Effect::WriteRebaseTodo {
            tag,
            path,
            expected,
            content,
            comment,
        }]
    }

    /// An edited todo list was written, or not; either way, reload it so
    /// the panel shows what git will run.
    pub(crate) fn rebase_todo_written(
        &mut self,
        tag: Tag,
        result: Result<(), String>,
    ) -> Vec<Effect> {
        self.busy = self.busy.saturating_sub(1);
        if tag.generation != self.generation || self.commits.todo_write_seq != Some(tag.seq) {
            return vec![];
        }
        self.commits.todo_write_seq = None;
        match result {
            Ok(()) => {}
            Err(message) if message == CHANGED_ON_DISK => self.notice = Some(TODO_CHANGED.into()),
            Err(message) => self.op_error = Some(message),
        }
        self.reload_rebase_todo()
    }

    /// Read the stopped rebase's todo list.
    pub(crate) fn reload_rebase_todo(&mut self) -> Vec<Effect> {
        let tag = self.background_tag();
        self.commits.todo_seq = Some(tag.seq);
        // An older git ignores `core.commentString`, so its value does not
        // count; an unknown git counts as older.
        let string_too = self.git_has(crate::git::version::COMMENT_STRING);
        vec![Effect::LoadRebaseTodo { tag, string_too }]
    }

    /// Show the todo list read, if it is the load in flight.
    pub(crate) fn rebase_todo_loaded(
        &mut self,
        tag: Tag,
        result: Result<Option<TodoFile>, String>,
    ) {
        if tag.generation != self.generation || self.commits.todo_seq != Some(tag.seq) {
            return;
        }
        self.commits.todo_seq = None;
        // Unreadable counts as none: nothing is edited that was not read.
        let todo = result.ok().flatten().map(RebaseTodo::load);
        self.commits.set_todo(todo);
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::features::commits::keys::TIPS_UNKNOWN;
    use crate::features::commits::rewrite;
    use crate::git::repo_state::Operation;
    use crate::intent::AppIntent;
    use crate::keymap::{self, KeyPress, KeyScope};
    use crate::model::{CommitFlow, Focus};
    use crate::popup::{ConfirmKind, Popup};
    use crate::test_support::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn alt(state: &mut AppState, code: KeyCode) -> Vec<Effect> {
        state.update(AppIntent::Key(KeyPress::new(code, KeyModifiers::ALT)))
    }

    fn picks(digits: &[char]) -> Vec<rebase_plan::Step> {
        digits
            .iter()
            .map(|&d| rebase_step(rebase_plan::TodoAction::Pick, d))
            .collect()
    }

    #[test]
    fn alt_k_and_alt_j_move_the_commit_and_the_cursor_follows_it() {
        let mut state = on_linear_log();
        let (_, op) = attached_of(&alt(&mut state, KeyCode::Char('k')));
        let GitOp::InteractiveRebase(plan) = op else {
            panic!("{op:?}")
        };
        assert_eq!(plan.base, cid(&"c".repeat(40)));
        assert_eq!(plan.steps, picks(&['a', 'b']));
        assert!(!plan.update_refs);
        assert_eq!(state.commits.selected, 0);

        let mut state = on_linear_log();
        state.git_version = version(38);
        let (_, op) = attached_of(&alt(&mut state, KeyCode::Down));
        let GitOp::InteractiveRebase(plan) = op else {
            panic!("{op:?}")
        };
        assert_eq!(plan.base, cid(&"d".repeat(40)));
        assert_eq!(plan.steps, picks(&['b', 'c', 'a']));
        assert!(plan.update_refs);
        assert_eq!(state.commits.selected, 2);
    }

    #[test]
    fn capital_e_stops_at_the_commit_and_b_after_it() {
        use rebase_plan::TodoAction as A;
        let mut state = on_linear_log();
        let (_, op) = attached_of(&press_char(&mut state, 'E'));
        let GitOp::InteractiveRebase(plan) = op else {
            panic!("{op:?}")
        };
        assert_eq!(
            plan.steps,
            [rebase_step(A::Edit, 'b'), rebase_step(A::Pick, 'a')]
        );

        let mut state = on_linear_log();
        assert_eq!(press_char(&mut state, 'b'), vec![]);
        assert_eq!(state.notice, Some(rewrite::NEEDS_BREAK.into()));
        state.git_version = version(20);
        let (_, op) = attached_of(&press_char(&mut state, 'b'));
        let GitOp::InteractiveRebase(plan) = op else {
            panic!("{op:?}")
        };
        let mut stop = rebase_step(A::Pick, 'b');
        stop.break_after = true;
        assert_eq!(plan.steps, [stop, rebase_step(A::Pick, 'a')]);
    }

    /// `on_linear_log` with branch `stack` on `bbb…` and `main` checked out
    /// at `aaa…`.
    fn with_stacked_branch(minor: u32) -> AppState {
        let mut state = on_linear_log();
        state.git_version = version(minor);
        state.branch = Some(on_branch("main"));
        state.commits.tips = Some(vec![
            (cid(&"a".repeat(40)), b"refs/heads/main".to_vec()),
            (cid(&"b".repeat(40)), b"refs/heads/stack".to_vec()),
        ]);
        state
    }

    #[test]
    fn an_older_git_refuses_every_rewrite_that_would_leave_a_branch_behind() {
        let reason = "Branch stack sits on these commits; Git 2.38 or newer moves it along";
        let mut state = with_stacked_branch(37);
        for key in ['s', 'F', 'd', 'e', 'E', 'b'] {
            let mut state = state.clone();
            assert_eq!(press_char(&mut state, key), vec![], "{key}");
            assert_eq!(state.notice, Some(reason.into()), "{key}");
            assert_eq!(state.op_seq, None, "{key}");
        }
        for code in [KeyCode::Char('k'), KeyCode::Char('j')] {
            let mut state = state.clone();
            assert_eq!(alt(&mut state, code), vec![], "{code:?}");
            assert_eq!(state.notice, Some(reason.into()), "{code:?}");
        }
        // Rewriting only the head commit leaves `stack` where it is.
        state.commits.selected = 0;
        assert!(matches!(
            press_char(&mut state, 'e').as_slice(),
            [Effect::HandOff(_)]
        ));
        // Unknown tips count as a branch that may be there.
        let mut unknown = on_linear_log();
        unknown.commits.tips = None;
        assert_eq!(press_char(&mut unknown, 'd'), vec![]);
        assert_eq!(unknown.notice, Some(TIPS_UNKNOWN.into()));
        // A failed read says so, and how to try again.
        unknown.commits.tips_error = Some("fatal: bad ref\nmore".into());
        assert_eq!(press_char(&mut unknown, 'd'), vec![]);
        assert_eq!(
            unknown.notice.as_deref(),
            Some("Could not read the branches (fatal: bad ref); press r to retry")
        );
    }

    #[test]
    fn with_update_refs_only_a_move_that_changes_a_stacked_branch_is_refused() {
        let state = with_stacked_branch(38);
        for key in ['s', 'd', 'e', 'E', 'b'] {
            let mut state = state.clone();
            let (_, op) = attached_of(&press_char(&mut state, key));
            assert!(
                matches!(op, GitOp::InteractiveRebase(ref plan) if plan.update_refs),
                "{key}"
            );
        }
        // `bbb…` moving above `aaa…` would leave `stack` holding `aaa…`.
        let mut up = state.clone();
        assert_eq!(alt(&mut up, KeyCode::Char('k')), vec![]);
        assert_eq!(
            up.notice,
            Some("Moving bbbbbbb past branch stack would change what it holds".into())
        );
        let mut from_head = state.clone();
        from_head.commits.selected = 0;
        assert_eq!(alt(&mut from_head, KeyCode::Char('j')), vec![]);
        assert_eq!(
            from_head.notice,
            Some("Moving aaaaaaa past branch stack would change what it holds".into())
        );
        // Moving `bbb…` below `ccc…` is fine: `stack` holds both either way.
        let mut down = state.clone();
        assert!(matches!(
            alt(&mut down, KeyCode::Char('j')).as_slice(),
            [Effect::HandOff(_)]
        ));
    }

    /// A rebase stopped with `todo` left, read from `/r/.git/rebase-merge`,
    /// its row `selected` under the cursor.
    fn stopped(todo: &[u8], selected: usize) -> AppState {
        let mut state = on_linear_log();
        state.git_version = version(38);
        let mut branch = on_branch("main");
        branch.in_progress = vec![Operation::Rebase];
        state.branch = Some(branch);
        state.commits.set_todo(Some(RebaseTodo::load(TodoFile {
            path: PathBuf::from("/r/.git/rebase-merge/git-rebase-todo"),
            todo: todo.to_vec(),
            done: Some(b"pick 0000 x\n".to_vec()),
            comment: Ok(b"#".to_vec()),
        })));
        state.commits.todo_selected = Some(selected);
        state
    }

    const TODO: &[u8] = b"pick 1111 one\nupdate-ref refs/heads/x\npick 2222 two\npick 3333 three\n";

    /// The one todo write in `effects`: what it expects on disk and writes.
    fn todo_write(effects: &[Effect]) -> (Vec<u8>, String) {
        match effects {
            [
                Effect::WriteRebaseTodo {
                    path,
                    expected,
                    content,
                    ..
                },
            ] => {
                assert_eq!(path, Path::new("/r/.git/rebase-merge/git-rebase-todo"));
                (
                    expected.clone(),
                    String::from_utf8(content.clone()).unwrap(),
                )
            }
            other => panic!("expected one todo write, got {other:?}"),
        }
    }

    #[test]
    fn keys_on_a_todo_row_write_the_edited_list() {
        let head = "pick 1111 one\nupdate-ref refs/heads/x\n";
        let with_alt = |code| KeyPress::new(code, KeyModifiers::ALT);
        let plain = |c| KeyPress::new(KeyCode::Char(c), KeyModifiers::NONE);
        let cases: [(&str, KeyPress, String); 9] = [
            (
                "alt-k",
                with_alt(KeyCode::Char('k')),
                format!("{head}pick 3333 three\npick 2222 two\n"),
            ),
            (
                "alt-j",
                with_alt(KeyCode::Down),
                format!("{head}pick 3333 three\npick 2222 two\n"),
            ),
            (
                "E",
                plain('E'),
                format!("{head}edit 2222 two\npick 3333 three\n"),
            ),
            (
                "b",
                plain('b'),
                format!("{head}pick 2222 two\nbreak\npick 3333 three\n"),
            ),
            (
                "s",
                plain('s'),
                format!("{head}squash 2222 two\npick 3333 three\n"),
            ),
            (
                "F",
                plain('F'),
                format!("{head}fixup 2222 two\npick 3333 three\n"),
            ),
            (
                "d",
                plain('d'),
                format!("{head}drop 2222 two\npick 3333 three\n"),
            ),
            (
                "e",
                plain('e'),
                format!("{head}reword 2222 two\npick 3333 three\n"),
            ),
            ("u", plain('u'), String::new()),
        ];
        for (name, press, written) in cases {
            // `alt-j` from the row above, every other key on `two`.
            let mut state = stopped(TODO, if name == "alt-j" { 3 } else { 2 });
            let effects = state.update(AppIntent::Key(press));
            if written.is_empty() {
                assert_eq!(effects, vec![], "{name}");
                assert_eq!(state.notice, Some("Already a pick".into()), "{name}");
                continue;
            }
            let (expected, content) = todo_write(&effects);
            assert_eq!(expected, TODO, "{name}");
            assert_eq!(content, written, "{name}");
            assert!(state.commits.todo_write_seq.is_some(), "{name}");
            assert_eq!(state.op_seq, None, "{name}");
        }
        // Pick undoes another action.
        let mut state = stopped(b"drop 1111 one\n", 0);
        let (_, content) = todo_write(&press_char(&mut state, 'u'));
        assert_eq!(content, "pick 1111 one\n");
    }

    #[test]
    fn a_todo_move_follows_the_row_and_one_write_runs_at_a_time() {
        let mut state = stopped(TODO, 2);
        let tag = match alt(&mut state, KeyCode::Char('k')).as_slice() {
            [Effect::WriteRebaseTodo { tag, .. }] => *tag,
            other => panic!("{other:?}"),
        };
        assert_eq!(state.commits.todo_selected, Some(3));
        assert_eq!(alt(&mut state, KeyCode::Char('j')), vec![]);
        assert_eq!(state.notice, Some(rewrite::TODO_SAVING.into()));
        // Until the written list is read back, too.
        state.update(AppIntent::RebaseTodoWritten {
            tag,
            result: Ok(()),
        });
        assert_eq!(press_char(&mut state, 'd'), vec![]);
        assert_eq!(state.notice, Some(rewrite::TODO_SAVING.into()));
    }

    #[test]
    fn a_todo_move_across_a_branch_update_is_refused_with_a_notice() {
        let mut state = stopped(TODO, 2);
        assert_eq!(alt(&mut state, KeyCode::Char('j')), vec![]);
        assert_eq!(
            state.notice,
            Some("Moving 2222 past branch x would change what it holds".into())
        );
        assert_eq!(state.commits.todo_write_seq, None);
    }

    #[test]
    fn a_todo_kika_does_not_edit_writes_nothing() {
        let todo = b"pick 1111 one\nexec make\n";
        for key in ['s', 'F', 'd', 'e', 'E', 'b', 'u'] {
            let mut state = stopped(todo, 0);
            assert_eq!(press_char(&mut state, key), vec![], "{key}");
            assert_eq!(
                state.notice.as_deref(),
                Some(
                    "Read-only, fix with git rebase --edit-todo: line 2, unknown command \
                     \"exec make\""
                ),
                "{key}"
            );
        }
        let mut state = stopped(todo, 0);
        assert_eq!(alt(&mut state, KeyCode::Char('k')), vec![]);
    }

    #[test]
    fn b_on_a_todo_row_needs_git_2_20() {
        let mut state = stopped(TODO, 2);
        state.git_version = version(19);
        assert_eq!(press_char(&mut state, 'b'), vec![]);
        assert_eq!(state.notice, Some(rewrite::NEEDS_BREAK.into()));
    }

    #[test]
    fn j_and_k_move_through_the_todo_rows_and_on_into_the_log() {
        let mut state = stopped(TODO, 0);
        state.commits.todo_selected = None;
        state.commits.selected = 1;
        press_char(&mut state, 'k');
        assert_eq!(state.commits.selected, 0);
        press_char(&mut state, 'k');
        assert_eq!(state.commits.todo_selected, Some(0));
        for _ in 0..5 {
            press_char(&mut state, 'k');
        }
        assert_eq!(state.commits.todo_selected, Some(3));
        for _ in 0..4 {
            press_char(&mut state, 'j');
        }
        assert_eq!(state.commits.todo_selected, None);
        assert_eq!(state.commits.selected, 0);
        // Log keys do not act on a todo row.
        state.commits.todo_selected = Some(0);
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(
            state.notice,
            Some("Select a commit in the log first".into())
        );
    }

    #[test]
    fn a_status_showing_a_rebase_loads_the_todo_and_one_without_clears_it() {
        let mut state = on_linear_log();
        let tag = refresh_tag(&mut state);
        let mut status = repo(vec![]);
        status.branch.in_progress = vec![Operation::Rebase];
        let effects = state.update(AppIntent::StatusLoaded {
            tag,
            result: Ok(status),
        });
        let load = match effects.as_slice() {
            [.., Effect::LoadRebaseTodo { tag, .. }] => *tag,
            other => panic!("expected LoadRebaseTodo, got {other:?}"),
        };
        state.update(AppIntent::RebaseTodoLoaded {
            tag: load,
            result: Ok(Some(TodoFile {
                path: PathBuf::from("/r/.git/rebase-merge/git-rebase-todo"),
                todo: TODO.to_vec(),
                done: None,
                comment: Ok(b"#".to_vec()),
            })),
        });
        assert_eq!(
            state.commits.todo.as_ref().map(|t| t.entries.len()),
            Some(4)
        );
        assert_eq!(state.commits.todo_seq, None);

        state.commits.todo_selected = Some(2);
        let tag = refresh_tag(&mut state);
        let effects = state.update(AppIntent::StatusLoaded {
            tag,
            result: Ok(repo(vec![])),
        });
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::LoadRebaseTodo { .. })),
            "{effects:?}"
        );
        assert_eq!(state.commits.todo, None);
        assert_eq!(state.commits.todo_selected, None);
    }

    #[test]
    fn the_todo_load_reads_core_comment_string_only_from_git_2_45() {
        for (git, string_too) in [(version(45), true), (version(44), false), (None, false)] {
            let mut state = on_linear_log();
            state.git_version = git;
            let tag = refresh_tag(&mut state);
            let mut status = repo(vec![]);
            status.branch.in_progress = vec![Operation::Rebase];
            let effects = state.update(AppIntent::StatusLoaded {
                tag,
                result: Ok(status),
            });
            assert!(
                matches!(
                    effects.as_slice(),
                    [.., Effect::LoadRebaseTodo { string_too: s, .. }] if *s == string_too
                ),
                "{git:?}: {effects:?}"
            );
        }
    }

    #[test]
    fn a_todo_write_carries_the_comment_prefix_the_list_was_loaded_with() {
        let mut state = stopped(TODO, 0);
        state.commits.set_todo(Some(RebaseTodo::load(TodoFile {
            path: PathBuf::from("/r/.git/rebase-merge/git-rebase-todo"),
            todo: b"; help\npick 1111 one\npick 2222 two\n".to_vec(),
            done: Some(b"pick 0000 x\n".to_vec()),
            comment: Ok(b";".to_vec()),
        })));
        state.commits.todo_selected = Some(0);
        match press_char(&mut state, 'd').as_slice() {
            [Effect::WriteRebaseTodo { comment, .. }] => assert_eq!(comment, b";"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_todo_read_with_a_letter_prefix_writes_nothing() {
        for prefix in [&b"b"[..], b"p"] {
            let mut state = stopped(TODO, 0);
            state.commits.set_todo(Some(RebaseTodo::load(TodoFile {
                path: PathBuf::from("/r/.git/rebase-merge/git-rebase-todo"),
                todo: b"pick 1111 one\npick 2222 two\n".to_vec(),
                done: Some(b"pick 0000 x\n".to_vec()),
                comment: Ok(prefix.to_vec()),
            })));
            state.commits.todo_selected = Some(0);
            assert_eq!(press_char(&mut state, 'd'), vec![], "{prefix:?}");
            assert!(
                state
                    .notice
                    .as_deref()
                    .is_some_and(|n| n.contains("hides commands")),
                "{:?}",
                state.notice
            );
        }
    }

    #[test]
    fn a_written_todo_is_reloaded_and_a_changed_one_says_so() {
        for (result, notice, error) in [
            (Ok(()), None, None),
            (Err(CHANGED_ON_DISK.to_string()), Some(TODO_CHANGED), None),
            (
                Err("disk full".to_string()),
                None,
                Some("disk full".to_string()),
            ),
        ] {
            let mut state = stopped(TODO, 2);
            let busy = state.busy;
            let tag = match press_char(&mut state, 'd').as_slice() {
                [Effect::WriteRebaseTodo { tag, .. }] => *tag,
                other => panic!("{other:?}"),
            };
            assert_eq!(state.busy, busy + 1);
            let effects = state.update(AppIntent::RebaseTodoWritten { tag, result });
            assert!(
                matches!(effects.as_slice(), [Effect::LoadRebaseTodo { .. }]),
                "{effects:?}"
            );
            assert_eq!(state.busy, busy);
            assert_eq!(state.commits.todo_write_seq, None);
            assert_eq!(state.notice.as_deref(), notice);
            assert_eq!(state.op_error, error);
        }
    }

    #[test]
    fn a_log_rewrite_still_waits_for_the_rebase_in_progress() {
        let mut state = stopped(TODO, 0);
        state.commits.todo_selected = None;
        for key in ['d', 'E', 'b'] {
            let mut state = state.clone();
            assert_eq!(press_char(&mut state, key), vec![], "{key}");
            assert_eq!(
                state.notice,
                Some("Finish or abort the operation in progress".into()),
                "{key}"
            );
        }
    }

    #[test]
    fn reorder_and_edit_keys_act_on_one_row_only() {
        let range = Some(keymap::CLEAR_RANGE_FIRST);
        // Over a range of log rows.
        let mut state = on_linear_log();
        state.commits.range_anchor = Some("a".repeat(40));
        for key in ['E', 'b', 'u'] {
            let mut state = state.clone();
            assert_eq!(press_char(&mut state, key), vec![], "{key}");
            assert_eq!(state.notice.as_deref(), range, "{key}");
        }
        assert_eq!(alt(&mut state, KeyCode::Char('k')), vec![]);
        assert_eq!(state.notice.as_deref(), range);
        // On a todo row while a range is kept.
        let mut todo = stopped(TODO, 2);
        todo.commits.range_anchor = Some("a".repeat(40));
        assert_eq!(press_char(&mut todo, 'e'), vec![]);
        assert_eq!(todo.notice.as_deref(), range);
        // Drop takes a range, but only one of log rows.
        assert_eq!(press_char(&mut todo, 'd'), vec![]);
        assert_eq!(
            todo.notice.as_deref(),
            Some("Select a commit in the log first")
        );
        // A range starts on log rows only.
        let mut todo = stopped(TODO, 2);
        press_char(&mut todo, 'V');
        assert_eq!(todo.commits.range_anchor, None);
        assert_eq!(
            todo.notice.as_deref(),
            Some("Select a commit in the log first")
        );
    }

    #[test]
    fn a_filter_stops_moves_and_any_rewrite_of_a_hidden_row() {
        let mut state = on_linear_log();
        state.git_version = version(38);
        // `commit 1` is the selected row, the only one it shows.
        state.commits.filter = "commit 1".into();
        assert_eq!(alt(&mut state, KeyCode::Char('k')), vec![]);
        assert_eq!(
            state.notice.as_deref(),
            Some(rewrite::FILTER_HIDES_NEIGHBOUR)
        );
        assert!(matches!(
            press_char(&mut state.clone(), 'E').as_slice(),
            [Effect::HandOff(_)]
        ));
        state.commits.filter = "commit 2".into();
        for key in ['E', 'b'] {
            let mut state = state.clone();
            assert_eq!(press_char(&mut state, key), vec![], "{key}");
            assert_eq!(
                state.notice.as_deref(),
                Some(keymap::FILTER_HIDES_ROW),
                "{key}"
            );
        }
        // The filter hides log rows only: the todo above stays editable.
        let mut todo = stopped(TODO, 2);
        todo.commits.filter = "nothing matches".into();
        todo_write(&press_char(&mut todo, 'd'));
    }

    #[test]
    fn with_a_filter_k_enters_the_todo_from_the_top_row_it_shows() {
        let mut state = stopped(TODO, 0);
        state.commits.todo_selected = None;
        state.commits.filter = "commit 1".into();
        state.commits.selected = 1;
        press_char(&mut state, 'k');
        assert_eq!(state.commits.todo_selected, Some(0));
        press_char(&mut state, 'j');
        assert_eq!(
            (state.commits.todo_selected, state.commits.selected),
            (None, 1)
        );
    }

    #[test]
    fn s_f_d_and_e_rewrite_the_selected_commit_on_the_terminal() {
        use rebase_plan::{RebasePlan, TodoAction as A};
        let fold = |action| RebasePlan {
            base: cid(&"d".repeat(40)),
            steps: vec![
                rebase_step(A::Pick, 'c'),
                rebase_step(action, 'b'),
                rebase_step(A::Pick, 'a'),
            ],
            update_refs: false,
        };
        let alone = |action| RebasePlan {
            base: cid(&"c".repeat(40)),
            steps: vec![rebase_step(action, 'b'), rebase_step(A::Pick, 'a')],
            update_refs: false,
        };
        let cases = [
            ('s', fold(A::Squash)),
            ('F', fold(A::Fixup)),
            ('d', alone(A::Drop)),
            ('e', alone(A::Reword)),
        ];
        for (key, plan) in cases {
            let mut state = on_linear_log();
            let (tag, op) = attached_of(&press_char(&mut state, key));
            assert_eq!(op, GitOp::InteractiveRebase(plan), "{key}");
            assert_eq!(state.op_seq, Some(tag.seq), "{key}");
            assert_eq!(state.notice, None, "{key}");
        }
    }

    /// Press each rewrite key on `state` and expect nothing to start, with
    /// `notice` said.
    fn no_rewrite(state: &AppState, notice: &'static str) {
        for key in ['s', 'F', 'd', 'e'] {
            let mut state = state.clone();
            assert_eq!(press_char(&mut state, key), vec![], "{key}");
            assert_eq!(state.notice, Some(notice.into()), "{key}");
            assert_eq!(state.op_seq, None, "{key}");
            assert_eq!(state.popups, [], "{key}");
        }
    }

    #[test]
    fn no_rewrite_starts_across_a_merge() {
        let mut state = on_linear_log();
        state.commits.rows[0].parents.push("e".repeat(40));
        no_rewrite(&state, "Cannot rewrite across a merge");
    }

    #[test]
    fn the_root_commit_is_not_rewritten() {
        let mut state = on_linear_log();
        state.commits.rows[1].parents.clear();
        no_rewrite(&state, "Cannot rewrite the first commit");
    }

    #[test]
    fn no_rewrite_starts_while_an_op_runs_or_an_operation_is_in_progress() {
        let mut running = on_linear_log();
        running.op_seq = Some(9);
        for key in ['s', 'F', 'd', 'e'] {
            let mut state = running.clone();
            assert_eq!(press_char(&mut state, key), vec![], "{key}");
            assert_eq!(state.notice, Some(keymap::OP_RUNNING.into()), "{key}");
            assert_eq!(state.op_seq, Some(9), "{key}");
        }

        let mut rebasing = on_linear_log();
        let mut branch = on_branch("main");
        branch.in_progress = vec![Operation::Rebase];
        rebasing.branch = Some(branch);
        no_rewrite(&rebasing, "Finish or abort the operation in progress");
    }

    #[test]
    fn the_intent_outside_the_workspace_starts_nothing() {
        let mut state = on_linear_log();
        state.context = Context::Review { number: Some(1) };
        assert_eq!(
            state.update(AppIntent::RewriteCommit(rebase_plan::Edit::Drop)),
            vec![]
        );
        assert_eq!(state.op_seq, None);
    }

    // Create fixup, autosquash and the fixup base (`fixup.rs`).

    #[test]
    fn ctrl_f_commits_the_staged_change_as_a_fixup_of_the_selected_commit() {
        let mut state = with_a_fixup();
        let effects = state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        )));
        let (tag, op) = probe_of(&effects);
        assert_eq!(
            op,
            GitOp::Commit {
                message: None,
                stage_all: false,
                amend: false,
                no_verify: false,
                fixup: Some(cid(&"b".repeat(40))),
            }
        );
        assert_eq!(state.op_seq, Some(tag.seq));
        assert_eq!(state.commit, CommitFlow::Closed);

        // Its failure shows as any op's, not in the commit popup.
        state.update(AppIntent::OpFinished {
            tag,
            op,
            result: Err("hook said no".into()),
        });
        match state.popups.as_slice() {
            [Popup::Error(error)] => {
                assert_eq!(error.title, "Could not create the fixup");
                assert!(error.message.contains("hook said no"));
            }
            other => panic!("expected the error, got {other:?}"),
        }
        assert_eq!(state.op_seq, None);
        assert!(!state.commit_on_terminal);
    }

    #[test]
    fn a_fixup_needs_something_staged() {
        let mut state = with_a_fixup();
        state.files.clear();
        assert_eq!(state.update(AppIntent::CreateFixup), vec![]);
        assert_eq!(state.notice, Some(rewrite::STAGE_THE_FIX.into()));
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn shift_s_asks_and_only_y_folds_every_fixup() {
        use rebase_plan::{RebasePlan, TodoAction as A};
        let mut state = with_a_fixup();
        let ask = |state: &mut AppState| {
            assert_eq!(press_char(state, 'S'), vec![]);
            assert_eq!(state.popups, [Popup::Confirm(ConfirmKind::Autosquash)]);
            assert_eq!(ConfirmKind::Autosquash.title(), "Autosquash");
            assert!(
                state
                    .confirm_question(&ConfirmKind::Autosquash)
                    .starts_with("Fold every fixup! commit into the commit it fixes?")
            );
        };

        ask(&mut state);
        assert_eq!(state.key_focus(), KeyScope::AutosquashConfirm);
        // Enter is too easy to press by habit: it answers nothing.
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.popups, [Popup::Confirm(ConfirmKind::Autosquash)]);
        assert_eq!(press_char(&mut state, 'q'), vec![]);
        assert_eq!(state.op_seq, None);
        assert_eq!(press_char(&mut state, 'n'), vec![]);
        assert_eq!(state.popups, []);
        assert_eq!(state.op_seq, None);

        ask(&mut state);
        let (tag, op) = attached_of(&press_char(&mut state, 'y'));
        assert_eq!(
            op,
            GitOp::InteractiveRebase(RebasePlan {
                base: cid(&"d".repeat(40)),
                steps: vec![
                    rebase_step(A::Pick, 'c'),
                    rebase_step(A::Fixup, 'a'),
                    rebase_step(A::Pick, 'b'),
                ],
                update_refs: false,
            })
        );
        assert_eq!(state.op_seq, Some(tag.seq));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn y_plans_again_from_the_log_as_it_is_now() {
        let mut state = with_a_fixup();
        press_char(&mut state, 'S');
        // A reload while asking found the fixup folded already.
        state.commits.rows[0].subject = "A".into();
        assert_eq!(press_char(&mut state, 'y'), vec![]);
        assert_eq!(state.notice, Some(rebase_plan::NO_FIXUPS.into()));
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn autosquash_says_why_not_and_asks_nothing() {
        let mut state = with_a_fixup();
        state.commits.rows[0].subject = "A".into();
        assert_eq!(state.update(AppIntent::Autosquash), vec![]);
        assert_eq!(state.popups, []);
        assert_eq!(state.notice, Some(rebase_plan::NO_FIXUPS.into()));
    }

    /// Ask for the fixup base, as `ctrl-b` does, and return its tag.
    fn find_base(state: &mut AppState) -> Tag {
        let effects = state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        )));
        match effects.as_slice() {
            [Effect::FindFixupBase { tag }] => *tag,
            other => panic!("expected FindFixupBase, got {other:?}"),
        }
    }

    fn base_found(tag: Tag, base: rewrite::FixupBase) -> AppIntent {
        AppIntent::FixupBaseFound {
            tag,
            result: Ok(base),
        }
    }

    #[test]
    fn one_fixup_base_moves_the_cursor_to_it() {
        use rewrite::FixupBase;
        let mut state = with_a_fixup();
        state.commits.range_anchor = Some("b".repeat(40));
        // From the Files panel a range in the log does not stop it.
        state.focus = Focus::Files;
        let before = state.busy;
        let tag = state.update(AppIntent::FindFixupBase);
        let tag = match tag.as_slice() {
            [Effect::FindFixupBase { tag }] => *tag,
            other => panic!("{other:?}"),
        };
        assert_eq!(state.busy, before + 1);
        state.update(base_found(tag, FixupBase::One(cid(&"c".repeat(40)))));
        assert_eq!(state.commits.selected, 2);
        assert_eq!(state.commits.range_anchor, None);
        assert_eq!(state.notice, Some("Fixup base: c1 C".into()));
        assert_eq!(state.busy, before);
        state.focus = Focus::Commits;

        let tag = find_base(&mut state);
        state.update(base_found(tag, FixupBase::One(cid(&"e".repeat(40)))));
        assert_eq!(state.commits.selected, 2);
        assert_eq!(
            state.notice,
            Some(
                "Fixup base: eeeeeee (not loaded; move down to load more, then look again)".into()
            )
        );
    }

    #[test]
    fn several_fixup_bases_are_listed_newest_first_and_the_cursor_stays() {
        use rewrite::FixupBase;
        let mut state = with_a_fixup();
        let tag = find_base(&mut state);
        let ids = ["e", "c", "a"].map(|d| cid(&d.repeat(40)));
        state.update(base_found(tag, FixupBase::Many(ids.to_vec())));
        assert_eq!(state.commits.selected, 1);
        assert_eq!(
            state.notice,
            Some("Staged lines come from 3 commits: a1, c1, eeeeeee".into())
        );

        let tag = find_base(&mut state);
        state.update(base_found(tag, FixupBase::None));
        assert_eq!(
            state.notice,
            Some("No commit touched the staged lines".into())
        );

        let tag = find_base(&mut state);
        state.update(AppIntent::FixupBaseFound {
            tag,
            result: Err("fatal: no".into()),
        });
        assert!(matches!(
            state.popups.as_slice(),
            [Popup::Error(error)] if error.title == "Could not find the fixup base"
        ));
    }

    #[test]
    fn a_stale_fixup_base_changes_nothing() {
        use rewrite::FixupBase;
        let mut state = with_a_fixup();
        let old = find_base(&mut state);
        let new = find_base(&mut state);
        let busy = state.busy;
        state.notice = None;
        state.update(base_found(old, FixupBase::One(cid(&"c".repeat(40)))));
        assert_eq!(state.commits.selected, 1);
        assert_eq!(state.notice, None);
        assert_eq!(state.busy, busy - 1, "the newer look is still on its way");
        assert_eq!(state.commits.fixup_base_seq, Some(new.seq));

        let other = Tag {
            generation: new.generation + 1,
            ..new
        };
        state.update(base_found(other, FixupBase::One(cid(&"c".repeat(40)))));
        assert_eq!(state.commits.selected, 1);
        assert_eq!(state.notice, None);
    }
}
