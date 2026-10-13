//! The reflog's `update` handlers: reading the reflog for the tab and
//! for an undo or redo, planning the step from a fresh read, and running
//! it once the question is answered.

use super::git::Reflog;
use super::keys::undo_disabled;
use super::{ReflogIntent, undo};
use crate::confirm_question::{Answer, ConfirmQuestion, Then};
use crate::effect::{Effect, Tag};
use crate::git::status::EntryKind;
use crate::keymap::KeyScope;
use crate::model::{AppState, Context};
use crate::op::GitOp;
use crate::popup::Popup;

/// The reflog's half of `update`, which `commits::update` passes
/// `CommitsIntent::Reflog` to. A free function, so its name cannot clash
/// with handlers.
pub(crate) fn update(state: &mut AppState, intent: ReflogIntent) -> Vec<Effect> {
    match intent {
        ReflogIntent::Next => {
            state.commits.reflog.list.move_by(1);
        }
        ReflogIntent::Prev => {
            state.commits.reflog.list.move_by(-1);
        }
        ReflogIntent::Loaded { tag, result } => return state.reflog_loaded(tag, result),
        ReflogIntent::Undo => return state.travel(undo::Direction::Undo),
        ReflogIntent::Redo => return state.travel(undo::Direction::Redo),
        ReflogIntent::ConfirmTravel => return state.confirm_travel(),
    }
    Vec::new()
}

/// The Reflog tab, undo and redo.
impl AppState {
    /// `HEAD` is on a branch with no commits, so it has no reflog yet.
    fn head_unborn(&self) -> bool {
        self.branch
            .as_ref()
            .is_some_and(|b| matches!(b.head, crate::git::status::Head::Unborn(_)))
    }

    /// Start reading the reflog; `user` as for the branch list. With no
    /// commits there is nothing to read: the tab shows an empty list.
    pub(crate) fn load_reflog(&mut self, user: bool) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if self.head_unborn() {
            self.commits.reflog.unborn();
            return vec![];
        }
        let seq = self.take_seq();
        self.commits.reflog.list.begin(seq);
        vec![Effect::LoadReflog {
            tag: self.effect_tag(seq, user),
        }]
    }

    /// Show the reflog read, and plan the undo or redo waiting for it. A
    /// failed read plans nothing: the list shown may be older than the
    /// key press.
    pub(crate) fn reflog_loaded(
        &mut self,
        tag: Tag,
        result: Result<Reflog, String>,
    ) -> Vec<Effect> {
        self.user_done(tag);
        let failed = result.is_err();
        if !self.commits.reflog.read(tag, self.generation, result) {
            return vec![];
        }
        let Some(direction) = self.commits.reflog.planning.take() else {
            return vec![];
        };
        if failed {
            match self.commits.reflog.list.error.clone() {
                Some(message) => self.set_op_error(message),
                None => self.clear_op_error(),
            }
            return vec![];
        }
        self.plan_travel(direction);
        vec![]
    }

    /// Undo or redo: read the reflog afresh, then plan from it. Nothing
    /// moves until the question that follows is answered.
    pub(crate) fn travel(&mut self, direction: undo::Direction) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if let Some(reason) = undo_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        if self.head_unborn() {
            self.notice = Some(match direction {
                undo::Direction::Undo => "Nothing to undo".into(),
                undo::Direction::Redo => "Nothing to redo".into(),
            });
            return vec![];
        }
        self.clear_op_error();
        self.commits.reflog.planning = Some(direction);
        self.load_reflog(true)
    }

    /// Ask about the plan for `direction` from the reflog just read, or
    /// say why there is none. The state may have changed while it was
    /// read, so the refusals are checked again.
    fn plan_travel(&mut self, direction: undo::Direction) {
        if let Some(reason) = undo_disabled(self) {
            self.notice = Some(reason);
            return;
        }
        let Some(reflog) = self.commits.reflog.reflog() else {
            return;
        };
        // The branch as read with the reflog, not as the last status saw it.
        let branch = reflog
            .head_ref
            .as_deref()
            .and_then(|r| r.strip_prefix(b"refs/heads/"))
            .map(String::from_utf8_lossy);
        let ctx = undo::Context {
            branch: branch.as_deref(),
            branches: &self.branches.list.rows,
            dirty: self.files.iter().any(|f| f.kind != EntryKind::Untracked),
        };
        match undo::plan(direction, &reflog, &ctx) {
            Ok(plan) => self.popups.push(Popup::Question(travel_question(
                plan.direction.title(),
                &plan.preview,
                GitOp::Travel {
                    direction: plan.direction,
                    step: plan.step,
                    expect: plan.expect,
                },
            ))),
            Err(message) => self.set_op_error(message),
        }
    }

    /// Undo or redo as the open question said, and only if one is open.
    pub(crate) fn confirm_travel(&mut self) -> Vec<Effect> {
        self.answer_question(KeyScope::Travel, Answer::YOnly)
    }
}

/// "Undo this?" or "Redo this?": what will happen, line by line (already
/// terminal-safe), then how to answer. It carries the op planned when it
/// was asked, so a reload before the answer cannot change what moves.
pub(crate) fn travel_question(
    title: &'static str,
    preview: &[String],
    op: GitOp,
) -> ConfirmQuestion {
    ConfirmQuestion {
        scope: KeyScope::Travel,
        title: title.into(),
        text: format!("{}\n(y/n)", preview.join("\n")),
        answer: Answer::YOnly,
        then: Then::Op(op),
        range: false,
        head: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confirm_question::quit_question;
    use crate::features::commits::CommitsIntent;
    use crate::features::commits::CommitsTab;
    use crate::git::repo_state::Operation;
    use crate::git::status::{BranchStatus, Head};
    use crate::intent::AppIntent;
    use crate::keymap::{self, KeyScope};
    use crate::list_filter::{self, InputTarget};
    use crate::test_support::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn undo_commit_op() -> GitOp {
        GitOp::Travel {
            direction: undo::Direction::Undo,
            step: undo::Step::MoveTo(cid(&"b".repeat(40))),
            expect: undo::Expected {
                head_ref: Some(b"refs/heads/main".to_vec()),
                oid: cid(&"c".repeat(40)),
            },
        }
    }

    #[test]
    fn undo_reads_the_reflog_afresh_then_asks_with_the_plan_and_emits_nothing_else() {
        let mut state = undoable();
        let busy = state.busy_count();
        let effects = press_with(&mut state, 'z', KeyModifiers::CONTROL);
        let tag = reflog_tag(&effects);
        assert!(tag.user);
        assert_eq!(state.busy_count(), busy + 1);
        assert_eq!(state.commits.reflog.planning, Some(undo::Direction::Undo));
        assert_eq!(state.popups, []);

        let effects = state.update(AppIntent::Commits(CommitsIntent::Reflog(
            ReflogIntent::Loaded {
                tag,
                result: Ok(commit_reflog('c')),
            },
        )));

        assert_eq!(effects, vec![]);
        assert_eq!(state.busy_count(), busy);
        assert_eq!(state.commits.reflog.planning, None);
        assert_eq!(
            state.popups,
            [Popup::Question(travel_question(
                "Undo",
                &[
                    "Undo: commit: second".into(),
                    "Branch main moves from ccccccc to bbbbbbb.".into(),
                    "Files change to match bbbbbbb; ignored files in the way are replaced.".into(),
                    "Commits after bbbbbbb leave main; redo brings them back.".into(),
                ],
                undo_commit_op(),
            ))]
        );
        assert_eq!(state.key_focus(), KeyScope::Travel);
        assert_eq!(state.held_seq(), None);
    }

    #[test]
    fn a_stale_or_other_repository_read_is_dropped_but_still_lowers_busy() {
        let mut state = undoable();
        let tag = reflog_tag(&press_with(&mut state, 'z', KeyModifiers::CONTROL));
        let busy = state.busy_count();
        for stale in [
            Tag {
                seq: tag.seq + 100,
                ..tag
            },
            Tag {
                generation: tag.generation + 1,
                ..tag
            },
        ] {
            state.update(AppIntent::Commits(CommitsIntent::Reflog(
                ReflogIntent::Loaded {
                    tag: stale,
                    result: Ok(commit_reflog('c')),
                },
            )));
        }
        assert_eq!(state.busy_count(), busy - 2);
        assert!(!state.commits.reflog.list.loaded);
        assert_eq!(
            state.commits.reflog.list.seq,
            Some(tag.seq),
            "still in flight"
        );
        assert_eq!(state.commits.reflog.planning, Some(undo::Direction::Undo));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn redo_plans_from_its_own_fresh_read() {
        let mut state = undoable();
        let tag = reflog_tag(&press_with(&mut state, 'r', KeyModifiers::CONTROL));
        assert_eq!(state.commits.reflog.planning, Some(undo::Direction::Redo));
        state.update(AppIntent::Commits(CommitsIntent::Reflog(
            ReflogIntent::Loaded {
                tag,
                result: Ok(commit_reflog('c')),
            },
        )));
        assert_eq!(state.popups, []);
        assert_eq!(state.op_error(), Some("Nothing to redo"));
    }

    #[test]
    fn only_y_runs_the_planned_step_and_n_or_esc_close_the_question() {
        let mut state = undoable();
        undo_with(&mut state, commit_reflog('c'));
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.popups.len(), 1, "Enter does not answer");
        let (_, op) = op_of(&press_char(&mut state, 'y'));
        assert_eq!(op, undo_commit_op());
        assert_eq!(state.popups, []);

        for code in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut state = undoable();
            undo_with(&mut state, commit_reflog('c'));
            assert_eq!(press(&mut state, code), vec![], "{code:?}");
            assert_eq!(state.popups, [], "{code:?}");
            assert_eq!(state.held_seq(), None, "{code:?}");
        }
    }

    #[test]
    fn confirm_travel_does_nothing_without_the_question() {
        let mut state = undoable();
        assert_eq!(
            state.update(AppIntent::Commits(CommitsIntent::Reflog(
                ReflogIntent::ConfirmTravel
            ))),
            vec![]
        );
        state.popups.push(Popup::Question(quit_question()));
        assert_eq!(
            state.update(AppIntent::Commits(CommitsIntent::Reflog(
                ReflogIntent::ConfirmTravel
            ))),
            vec![]
        );
        assert_eq!(state.popups, [Popup::Question(quit_question())]);
        assert_eq!(state.held_seq(), None);
    }

    #[test]
    fn undo_and_redo_wait_for_a_running_op_or_an_operation_in_progress() {
        for intent in [
            AppIntent::Commits(CommitsIntent::Reflog(ReflogIntent::Undo)),
            AppIntent::Commits(CommitsIntent::Reflog(ReflogIntent::Redo)),
        ] {
            let mut state = undoable();
            state.hold_op(7);
            assert_eq!(state.update(intent.clone()), vec![]);
            assert_eq!(state.notice, Some(keymap::OP_RUNNING.into()));
            assert_eq!(state.commits.reflog.planning, None);

            for operation in [Operation::Rebase, Operation::Bisect] {
                let mut state = undoable();
                let mut branch = on_branch("main");
                branch.in_progress = vec![operation];
                state.branch = Some(branch);
                assert_eq!(state.update(intent.clone()), vec![]);
                assert_eq!(
                    state.notice,
                    Some("Finish or abort the operation in progress".into())
                );
            }

            let mut state = AppState::new(Context::Workspace);
            assert_eq!(state.update(intent.clone()), vec![]);
            assert_eq!(state.notice, Some("Not in a repository".into()));
        }
    }

    #[test]
    fn a_refused_plan_says_why_and_asks_nothing() {
        let mut state = undoable();
        let effects = undo_with(&mut state, commit_reflog('d'));
        assert_eq!(effects, vec![]);
        assert_eq!(state.popups, []);
        assert_eq!(
            state.op_error(),
            Some("HEAD has moved since commit; nothing is undone")
        );

        let mut state = undoable();
        state.files = vec![changed("a.rs", '.', 'M')];
        undo_with(&mut state, commit_reflog('c'));
        assert_eq!(state.op_error(), Some("Commit or stash your changes first"));

        // Untracked files are left to `reset --keep`.
        let mut state = undoable();
        state.files = vec![untracked("new.rs")];
        undo_with(&mut state, commit_reflog('c'));
        assert_eq!(state.popups.len(), 1);
    }

    #[test]
    fn a_stale_or_failed_read_plans_nothing() {
        let mut state = undoable();
        let tag = reflog_tag(&state.update(AppIntent::Commits(CommitsIntent::Reflog(
            ReflogIntent::Undo,
        ))));
        let stale = Tag {
            seq: tag.seq + 100,
            ..tag
        };
        state.update(AppIntent::Commits(CommitsIntent::Reflog(
            ReflogIntent::Loaded {
                tag: stale,
                result: Ok(commit_reflog('c')),
            },
        )));
        assert_eq!(state.popups, []);
        assert_eq!(state.commits.reflog.planning, Some(undo::Direction::Undo));

        state.update(AppIntent::Commits(CommitsIntent::Reflog(
            ReflogIntent::Loaded {
                tag,
                result: Err("fatal: broken".into()),
            },
        )));
        assert_eq!(state.popups, []);
        assert_eq!(state.commits.reflog.planning, None);
        assert_eq!(
            state.op_error(),
            Some("Could not read the reflog: fatal: broken")
        );
    }

    #[test]
    fn brackets_switch_to_the_reflog_tab_reading_it_once() {
        let mut state = undoable();
        let tag = reflog_tag(&press_char(&mut state, ']'));
        assert_eq!(state.commits.tab, CommitsTab::Reflog);
        assert_eq!(state.key_focus(), KeyScope::Reflog);
        state.update(AppIntent::Commits(CommitsIntent::Reflog(
            ReflogIntent::Loaded {
                tag,
                result: Ok(commit_reflog('c')),
            },
        )));
        press_char(&mut state, 'j');
        press(&mut state, KeyCode::Down);
        press(&mut state, KeyCode::Down);
        assert_eq!(state.commits.reflog.list.selected, 2);
        press_char(&mut state, 'k');
        assert_eq!(state.commits.reflog.list.selected, 1);
        assert_eq!(state.popups, [], "the tab acts on no row");

        assert_eq!(press_char(&mut state, '['), vec![]);
        assert_eq!(state.commits.tab, CommitsTab::Log);
        assert_eq!(state.key_focus(), KeyScope::Commits);
        assert_eq!(press_char(&mut state, ']'), vec![], "read once");
    }

    /// The Reflog tab shown, its read landed: second, first, root.
    fn on_reflog_tab() -> AppState {
        let mut state = undoable();
        let tag = reflog_tag(&press_char(&mut state, ']'));
        state.update(AppIntent::Commits(CommitsIntent::Reflog(
            ReflogIntent::Loaded {
                tag,
                result: Ok(commit_reflog('c')),
            },
        )));
        state
    }

    #[test]
    fn slash_in_the_reflog_tab_narrows_it_as_typed_and_enter_keeps_it() {
        let mut state = on_reflog_tab();
        press_char(&mut state, '/');
        let input = state.input.as_ref().expect("the input opens");
        assert_eq!(input.target, InputTarget::ReflogFilter);
        assert_eq!(input.target.prompt(), "/");
        assert_eq!(state.key_focus(), KeyScope::Input);
        type_text(&mut state, "bbb");
        assert_eq!(state.commits.reflog.list.visible(), [1], "by short id");
        assert_eq!(state.commits.reflog.list.selected, 1, "the cursor follows");
        for _ in 0..3 {
            press(&mut state, KeyCode::Backspace);
        }
        type_text(&mut state, "root");
        assert_eq!(state.commits.reflog.list.visible(), [2], "by message");
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.input, None);
        assert_eq!(state.commits.reflog.list.filter, "root");
        assert_eq!(state.key_focus(), KeyScope::Reflog);
        // `/` again starts from the kept text.
        press_char(&mut state, '/');
        assert_eq!(state.input.as_ref().unwrap().line.text, "root");
    }

    #[test]
    fn esc_in_the_reflog_tab_clears_the_filter_then_quits_and_v_sets_no_range() {
        let mut state = on_reflog_tab();
        assert_eq!(press_char(&mut state, 'V'), vec![]);
        assert_eq!(state.commits.list.range_anchor, None);
        assert_eq!(state.popups, []);
        press_char(&mut state, '/');
        type_text(&mut state, "first\n");
        assert_eq!(press(&mut state, KeyCode::Esc), vec![]);
        assert_eq!(state.commits.reflog.list.filter, "");
        assert!(!state.should_quit);
        press(&mut state, KeyCode::Esc);
        assert!(state.should_quit);
    }

    #[test]
    fn the_reflog_filter_stops_at_its_limit() {
        let mut state = on_reflog_tab();
        press_char(&mut state, '/');
        for _ in 0..300 {
            press_char(&mut state, 'z');
        }
        assert_eq!(
            state.commits.reflog.list.filter.chars().count(),
            list_filter::MAX_INPUT
        );
        assert_eq!(state.notice, Some(list_filter::input_full_notice().into()));
    }

    #[test]
    fn the_log_and_the_reflog_keep_their_own_filters() {
        let mut state = on_reflog_tab();
        press_char(&mut state, '/');
        type_text(&mut state, "root\n");
        press_char(&mut state, '[');
        assert_eq!(state.key_focus(), KeyScope::Commits);
        press_char(&mut state, '/');
        assert_eq!(
            state.input.as_ref().unwrap().target,
            InputTarget::CommitsFilter
        );
        assert_eq!(state.input.as_ref().unwrap().line.text, "");
        type_text(&mut state, "c1\n");
        assert_eq!(state.commits.list.filter, "c1");
        assert_eq!(state.commits.reflog.list.filter, "root");
        press_char(&mut state, ']');
        assert_eq!(state.commits.reflog.list.filter, "root");
        assert_eq!(state.commits.list.filter, "c1");
        // Esc in the Reflog tab clears only its own.
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.commits.reflog.list.filter, "");
        assert_eq!(state.commits.list.filter, "c1");
    }

    #[test]
    fn the_reflog_filter_never_reaches_git_nor_changes_the_undo_plan() {
        let hostile = "--force -D x\x1b[31m";
        let leaks = |effects: &[Effect]| {
            let debug = format!("{effects:?}");
            debug.contains("--force") || debug.contains("-D x") || debug.contains("[31m")
        };
        let mut state = on_reflog_tab();
        let mut effects = press_char(&mut state, '/');
        for c in hostile.chars() {
            effects.extend(press_char(&mut state, c));
        }
        effects.extend(press(&mut state, KeyCode::Enter));
        assert!(state.commits.reflog.list.filter.starts_with("--force -D x"));
        assert!(state.commits.reflog.list.visible().is_empty());
        assert!(!leaks(&effects), "{effects:?}");

        // A refresh asks git exactly what it asks with no filter set.
        let refresh = press_char(&mut state, 'r');
        assert!(!leaks(&refresh), "{refresh:?}");
        assert_eq!(refresh, press_char(&mut on_reflog_tab(), 'r'));
        let tag = refresh
            .iter()
            .find_map(|e| match e {
                Effect::LoadReflog { tag } => Some(*tag),
                _ => None,
            })
            .expect("a refresh rereads the reflog");
        state.update(AppIntent::Commits(CommitsIntent::Reflog(
            ReflogIntent::Loaded {
                tag,
                result: Ok(commit_reflog('c')),
            },
        )));

        // Undo plans from every entry, though the filter hides them all.
        let undo = ctrl(&mut state, 'z');
        assert!(!leaks(&undo), "{undo:?}");
        let tag = reflog_tag(&undo);
        let loaded = state.update(AppIntent::Commits(CommitsIntent::Reflog(
            ReflogIntent::Loaded {
                tag,
                result: Ok(commit_reflog('c')),
            },
        )));
        assert!(!leaks(&loaded), "{loaded:?}");
        match state.popups.as_slice() {
            [Popup::Question(q)] => assert_eq!(q.then, Then::Op(undo_commit_op())),
            other => panic!("expected the undo question, got {other:?}"),
        }
        let ran = press_char(&mut state, 'y');
        assert!(!leaks(&ran), "{ran:?}");
        assert_eq!(op_of(&ran).1, undo_commit_op());

        state.free_op();
        let redo = ctrl(&mut state, 'r');
        assert!(!leaks(&redo), "{redo:?}");
        let tag = reflog_tag(&redo);
        let loaded = state.update(AppIntent::Commits(CommitsIntent::Reflog(
            ReflogIntent::Loaded {
                tag,
                result: Ok(commit_reflog('c')),
            },
        )));
        assert!(!leaks(&loaded), "{loaded:?}");
        assert!(state.commits.reflog.list.filter.starts_with("--force -D x"));
    }

    #[test]
    fn a_log_reload_rereads_the_reflog_only_once_it_was_read() {
        let mut state = undoable();
        let effects = state.update(AppIntent::RefreshStatus);
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::LoadReflog { .. })),
            "{effects:?}"
        );
        let tag = reflog_tag(&state.update(AppIntent::Commits(CommitsIntent::SwitchTab)));
        state.update(AppIntent::Commits(CommitsIntent::Reflog(
            ReflogIntent::Loaded {
                tag,
                result: Ok(commit_reflog('c')),
            },
        )));
        let effects = state.update(AppIntent::RefreshStatus);
        let reloads: Vec<&Tag> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::LoadReflog { tag } => Some(tag),
                _ => None,
            })
            .collect();
        assert_eq!(reloads.len(), 1, "{effects:?}");
        assert!(!reloads[0].user);
    }

    #[test]
    fn with_no_commits_the_reflog_is_empty_and_there_is_nothing_to_undo() {
        let mut state = undoable();
        state.branch = Some(BranchStatus {
            head: Head::Unborn("main".into()),
            ..on_branch("main")
        });
        assert_eq!(
            state.update(AppIntent::Commits(CommitsIntent::SwitchTab)),
            vec![]
        );
        assert_eq!(state.commits.reflog.reflog(), Some(Reflog::default()));
        assert_eq!(
            state.update(AppIntent::Commits(CommitsIntent::Reflog(
                ReflogIntent::Undo
            ))),
            vec![]
        );
        assert_eq!(state.notice, Some("Nothing to undo".into()));
    }
}
