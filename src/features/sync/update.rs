//! The sync feature's `update` handlers: the auto-fetch result, and
//! starting, watching and finishing a fetch, pull or push.

use super::keys::{fetch_disabled, pull_push_disabled};
use crate::effect::{Effect, Tag};
use crate::git::remote::{RemoteFailure, RemoteOp};
use crate::keymap;
use crate::model::{AppState, Context};
use crate::popup::{ConfirmKind, ErrorPopup, Popup};
use crate::refresh;
use crate::remote_run::RemoteRun;

/// Auto-fetch's result, and fetch, pull and push run by hand.
impl AppState {
    /// A fetch can move remote refs and so ahead/behind. A failed one,
    /// say offline, is tried again on the next interval.
    pub(crate) fn fetch_finished(&mut self, tag: Tag, result: Result<(), String>) -> Vec<Effect> {
        self.watch.fetch_pending = false;
        if tag.generation != self.generation || result.is_err() {
            return vec![];
        }
        self.background_status()
    }

    /// Start `op` on the current branch, unless something else runs: it
    /// holds the op slot like any git mutation, so the two never race.
    pub(crate) fn start_remote(&mut self, op: RemoteOp) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        let disabled = match op {
            RemoteOp::Fetch => fetch_disabled,
            RemoteOp::Pull | RemoteOp::Push { .. } => pull_push_disabled,
            RemoteOp::PushTag { .. } => keymap::push_tag_disabled,
        };
        if let Some(reason) = disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        self.clear_op_error();
        // The guard above refuses while an op runs, so the slot is free.
        let Some(tag) = self.take_op() else {
            return vec![];
        };
        let branch = self.branches.current().map(|b| b.name.clone());
        self.remote = Some(RemoteRun::new(op.clone(), tag.seq, branch));
        vec![Effect::Remote { tag, op }]
    }

    /// The running remote operation, if `tag` is its.
    pub(crate) fn current_remote(&mut self, tag: Tag) -> Option<&mut RemoteRun> {
        if tag.generation != self.generation {
            return None;
        }
        self.remote.as_mut().filter(|run| run.seq == tag.seq)
    }

    /// Show what ssh says while the op runs, if `tag` is still its.
    pub(crate) fn remote_notice(&mut self, tag: Tag, text: String) {
        if let Some(run) = self.current_remote(tag) {
            run.notice = Some(text);
        }
    }

    /// Turn the spinner a frame, only while a remote op runs.
    pub(crate) fn spinner_tick(&mut self) {
        if let Some(run) = &mut self.remote {
            run.tick();
        }
    }

    /// Free the slot, then show what happened: nothing on success, an
    /// offer to force a rejected push, else git's words. Branches and
    /// status reload after a success, and after any failed pull, which can
    /// leave a merge half done.
    pub(crate) fn remote_finished(
        &mut self,
        tag: Tag,
        op: RemoteOp,
        result: Result<(), RemoteFailure>,
    ) -> Vec<Effect> {
        // As with other user effects: every one reports once, stale or not.
        self.release_op(tag);
        if self.remote.as_ref().is_some_and(|run| run.seq == tag.seq) {
            self.remote = None;
            // Git has gone; a question still open has nobody to answer.
            self.close_credential();
        }
        if tag.generation != self.generation {
            return vec![];
        }
        let reload = result.is_ok() || op == RemoteOp::Pull;
        if result.is_ok() {
            // The forge may know of new pull requests, or new checks.
            self.forge.want_refresh();
        }
        match result {
            Ok(()) => {}
            Err(RemoteFailure::Rejected(_)) => {
                self.popups.push(Popup::Confirm(ConfirmKind::ForcePush));
            }
            Err(RemoteFailure::Failed(message)) => {
                let next = if op == RemoteOp::Pull {
                    "The file list was reloaded. Fix the cause above, then try again."
                } else {
                    "Fix the cause above, then try again."
                };
                self.push_error(ErrorPopup::new(op.failure_title(), &message, next));
            }
        }
        if reload {
            self.apply_plan(refresh::remote_plan(&op))
        } else {
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::askpass::Secret;
    use crate::git::branches::BranchName;
    use crate::git::status::{BranchStatus, Head};
    use crate::intent::AppIntent;
    use crate::keymap::{KeyPress, KeyScope};
    use crate::layout::{self, SidePanel};
    use crate::model::Focus;
    use crate::test_support::*;
    use crate::watch::WatchEvent;
    use crossterm::event::{KeyCode, KeyModifiers};

    /// An idle workspace whose user opted in to auto-fetch.
    fn idle_with_fetch() -> AppState {
        let mut state = idle_with_files();
        state.watch.fetch_enabled = true;
        state
    }

    #[test]
    fn auto_fetch_does_nothing_by_default() {
        let mut state = idle_with_files();
        assert!(watch(&mut state, WatchEvent::Fetch).is_empty());
        assert!(!state.watch.fetch_pending);
    }

    fn fetch_tag(effects: &[Effect]) -> Tag {
        match effects {
            [Effect::Fetch { tag }] => *tag,
            other => panic!("expected one Fetch, got {other:?}"),
        }
    }

    #[test]
    fn auto_fetch_is_a_background_effect_that_runs_one_at_a_time() {
        let mut state = idle_with_fetch();
        let tag = fetch_tag(&watch(&mut state, WatchEvent::Fetch));
        assert!(!tag.user);
        assert_eq!(state.busy_count(), 0);
        assert!(watch(&mut state, WatchEvent::Fetch).is_empty());
    }

    #[test]
    fn auto_fetch_does_nothing_when_disabled() {
        let mut state = idle_with_fetch();
        state.watch.fetch_enabled = false;
        assert!(watch(&mut state, WatchEvent::Fetch).is_empty());
        assert!(!state.watch.fetch_pending);
    }

    #[test]
    fn a_successful_fetch_starts_a_background_status() {
        let mut state = idle_with_fetch();
        let tag = fetch_tag(&watch(&mut state, WatchEvent::Fetch));

        let effects = state.update(AppIntent::FetchFinished {
            tag,
            result: Ok(()),
        });

        background_status_tag(&effects);
        assert!(!state.watch.fetch_pending);
        assert_eq!(state.busy_count(), 0);
    }

    #[test]
    fn a_failed_or_stale_fetch_is_dropped_but_frees_the_slot() {
        let mut state = idle_with_fetch();
        let tag = fetch_tag(&watch(&mut state, WatchEvent::Fetch));
        let effects = state.update(AppIntent::FetchFinished {
            tag,
            result: Err("fatal: unable to access remote".into()),
        });
        assert!(effects.is_empty());
        assert!(state.popups.is_empty());

        let tag = fetch_tag(&watch(&mut state, WatchEvent::Fetch));
        state.generation += 1;
        let effects = state.update(AppIntent::FetchFinished {
            tag,
            result: Ok(()),
        });
        assert!(effects.is_empty());
        assert!(!state.watch.fetch_pending);
    }

    #[test]
    fn a_fetch_finishing_while_an_op_runs_starts_no_status() {
        let mut state = idle_with_fetch();
        let tag = fetch_tag(&watch(&mut state, WatchEvent::Fetch));
        state.hold_op(9);
        let effects = state.update(AppIntent::FetchFinished {
            tag,
            result: Ok(()),
        });
        assert!(effects.is_empty());
    }

    const FORCE_PUSH: RemoteOp = RemoteOp::Push {
        force_with_lease: true,
    };

    #[test]
    fn f_p_and_shift_p_each_start_one_remote_op_on_the_current_branch() {
        for (key, op) in [('f', RemoteOp::Fetch), ('p', RemoteOp::Pull), ('P', PUSH)] {
            let mut state = remote_ready();
            let effects = if key == 'P' {
                shift(&mut state, key)
            } else {
                press_char(&mut state, key)
            };
            let (tag, started) = remote_of(&effects);
            assert_eq!(started, op, "{key}");
            assert!(tag.user);
            assert_eq!(state.held_seq(), Some(tag.seq));
            assert_eq!(state.busy_count(), 1);
            let run = state.remote.as_ref().expect("a run is recorded");
            assert_eq!((&run.op, run.seq), (&op, tag.seq));
            assert_eq!(run.branch, Some(BranchName::from("main")));
        }
    }

    #[test]
    fn a_started_remote_op_holds_the_slot_with_its_effects_tag() {
        let mut state = remote_ready();

        let (tag, _) = remote_of(&state.start_remote(RemoteOp::Fetch));

        assert!(state.op_is_running() && state.holds_op(tag));
        assert!(state.is_busy());
        assert_eq!(state.remote.as_ref().map(|run| run.seq), Some(tag.seq));
    }

    #[test]
    fn the_keys_work_from_every_workspace_panel() {
        for panel in [SidePanel::Status, SidePanel::Files, SidePanel::Commits] {
            let mut state = remote_ready();
            state.update(AppIntent::FocusPanel(layout::Panel::Side(panel)));
            remote_of(&press_char(&mut state, 'f'));
        }
    }

    #[test]
    fn a_remote_op_is_refused_with_a_notice_while_anything_else_runs() {
        let mut state = remote_ready();
        remote_of(&press_char(&mut state, 'f'));
        for key in ['f', 'p'] {
            assert_eq!(press_char(&mut state, key), []);
            assert_eq!(state.notice, Some("Another operation is running".into()));
        }
        assert_eq!(shift(&mut state, 'P'), []);

        let mut state = remote_ready();
        state.hold_op(99);
        assert_eq!(press_char(&mut state, 'p'), []);
        assert_eq!(state.notice, Some("Another operation is running".into()));
        assert_eq!(state.remote, None);

        // An intent that skips the keymap is refused the same way.
        let mut state = remote_ready();
        state.hold_op(99);
        assert_eq!(state.update(AppIntent::ForcePush), []);
        assert_eq!(state.held_seq(), Some(99));
    }

    #[test]
    fn a_manual_fetch_waits_for_auto_fetch_and_auto_fetch_for_a_remote_op() {
        let mut state = remote_ready();
        state.watch.fetch_pending = true;
        assert_eq!(press_char(&mut state, 'f'), []);
        assert_eq!(state.notice, Some("A background fetch is running".into()));
        // Pull and push are not held up by it.
        remote_of(&press_char(&mut state, 'p'));

        let mut state = remote_ready();
        state.watch.fetch_enabled = true;
        remote_of(&shift(&mut state, 'P'));
        let effects = state.update(AppIntent::Watch(WatchEvent::Fetch));
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Fetch { .. })),
            "{effects:?}"
        );
    }

    #[test]
    fn pull_and_push_are_disabled_on_a_detached_head_and_everything_outside_a_repo() {
        let mut state = remote_ready();
        state.branch = Some(BranchStatus {
            head: Head::Detached("abc".into()),
            ..on_branch("main")
        });
        assert_eq!(press_char(&mut state, 'p'), []);
        assert_eq!(state.notice, Some("HEAD is detached".into()));
        assert_eq!(shift(&mut state, 'P'), []);
        assert_eq!(state.notice, Some("HEAD is detached".into()));
        remote_of(&press_char(&mut state, 'f'));

        let mut state = remote_ready();
        state.repo = None;
        assert_eq!(press_char(&mut state, 'f'), []);
        assert_eq!(state.notice, Some("Not in a repository".into()));
    }

    #[test]
    fn a_finished_remote_op_frees_the_slot_and_reloads_status_branches_and_commits() {
        for op in [RemoteOp::Fetch, RemoteOp::Pull, PUSH] {
            let mut state = remote_ready();
            let (tag, _) = remote_of(&state.update(match op {
                RemoteOp::Fetch => AppIntent::Fetch,
                RemoteOp::Pull => AppIntent::Pull,
                _ => AppIntent::Push,
            }));
            let effects = state.update(remote_finished(tag, op, Ok(())));
            assert_eq!(state.remote, None);
            assert_eq!(state.held_seq(), None);
            assert!(
                effects
                    .iter()
                    .any(|e| matches!(e, Effect::LoadStatus { .. })),
                "{effects:?}"
            );
            branches_tag(&effects);
            // Remote refs moved, and with a pull `HEAD` too: the log reloads.
            assert!(
                effects
                    .iter()
                    .any(|e| matches!(e, Effect::LoadCommits { .. })),
                "{effects:?}"
            );
            assert_eq!(state.popups, []);
            // Only the reloads count as busy now.
            assert_eq!(state.busy_count(), 4);
        }
    }

    #[test]
    fn keys_keep_working_while_a_remote_op_runs() {
        let mut state = remote_ready();
        remote_of(&shift(&mut state, 'P'));
        let before = selected_name(&state);
        press_char(&mut state, 'j');
        assert_ne!(selected_name(&state), before);
        press_char(&mut state, '2');
        assert_eq!(state.focus, Focus::Files);
    }

    #[test]
    fn spinner_ticks_turn_the_frame_only_while_a_remote_op_runs() {
        let mut state = remote_ready();
        state.update(AppIntent::SpinnerTick);
        assert_eq!(state.remote, None);
        remote_of(&press_char(&mut state, 'f'));
        state.update(AppIntent::SpinnerTick);
        state.update(AppIntent::SpinnerTick);
        assert_eq!(state.remote.as_ref().unwrap().frame, 2);
    }

    #[test]
    fn a_rejected_push_offers_a_lease_and_y_pushes_with_it() {
        let mut state = remote_ready();
        let (tag, _) = remote_of(&shift(&mut state, 'P'));
        let rejected = " ! [rejected]        main -> main (fetch first)".to_string();
        let effects = state.update(remote_finished(
            tag,
            PUSH,
            Err(RemoteFailure::Rejected(rejected)),
        ));
        assert_eq!(effects, [], "nothing changed, so nothing reloads");
        assert_eq!(state.popups, [Popup::Confirm(ConfirmKind::ForcePush)]);

        let (_, op) = remote_of(&press_char(&mut state, 'y'));
        assert_eq!(op, FORCE_PUSH);
        assert_eq!(state.popups, []);
    }

    #[test]
    fn n_declines_the_lease_and_nothing_runs() {
        let mut state = remote_ready();
        let (tag, _) = remote_of(&shift(&mut state, 'P'));
        state.update(remote_finished(
            tag,
            PUSH,
            Err(RemoteFailure::Rejected("r".into())),
        ));
        assert_eq!(press_char(&mut state, 'n'), []);
        assert_eq!(state.popups, []);
        assert_eq!(state.remote, None);
        assert_eq!(state.held_seq(), None);
    }

    #[test]
    fn a_failed_remote_op_shows_gits_words_with_no_offer() {
        for (op, title) in [
            (RemoteOp::Fetch, "Fetch failed"),
            (PUSH, "Push failed"),
            (FORCE_PUSH, "Push failed"),
        ] {
            let mut state = remote_ready();
            let seq = state.take_seq();
            state.hold_op(seq);
            state.set_busy(1);
            state.remote = Some(RemoteRun::new(op.clone(), seq, None));
            let tag = Tag {
                generation: state.generation,
                seq,
                user: true,
            };
            let message = " ! [rejected]        main -> main (stale info)";
            let effects = state.update(remote_finished(
                tag,
                op.clone(),
                Err(RemoteFailure::Failed(message.into())),
            ));
            assert_eq!(effects, [], "{op:?}: nothing reloads");
            match state.popups.as_slice() {
                [Popup::Error(error)] => {
                    assert_eq!(error.title, title);
                    assert_eq!(error.message, message.trim());
                }
                other => panic!("expected an error popup, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_failed_pull_still_reloads_since_it_can_leave_a_merge_half_done() {
        let mut state = remote_ready();
        let (tag, _) = remote_of(&press_char(&mut state, 'p'));
        let effects = state.update(remote_finished(
            tag,
            RemoteOp::Pull,
            Err(RemoteFailure::Failed("CONFLICT (content): a.txt".into())),
        ));
        branches_tag(&effects);
        match state.popups.as_slice() {
            [Popup::Error(error)] => {
                assert_eq!(error.title, "Pull failed");
                assert!(error.next.starts_with("The file list was reloaded"));
            }
            other => panic!("expected an error popup, got {other:?}"),
        }
    }

    #[test]
    fn a_result_for_another_repository_only_frees_the_slot() {
        let mut state = remote_ready();
        let (tag, _) = remote_of(&press_char(&mut state, 'p'));
        state.generation += 1;
        let effects = state.update(remote_finished(
            tag,
            RemoteOp::Pull,
            Err(RemoteFailure::Failed("boom".into())),
        ));
        assert_eq!(effects, []);
        assert_eq!(state.popups, []);
        assert_eq!((state.held_seq(), state.busy_count()), (None, 0));
        assert_eq!(state.remote, None);
    }

    #[test]
    fn a_typed_answer_goes_to_git_and_leaves_no_trace_in_the_state() {
        let (mut state, _) = typing_password("q?ok");
        assert!(matches!(state.popups.as_slice(), [Popup::Credential(_)]));
        // `q` and `?` are typed, not quit or help.
        assert!(!state.should_quit && !state.help_open);

        let effects = press(&mut state, KeyCode::Enter);

        assert_eq!(
            effects,
            [Effect::AnswerCredential(Some(Secret::new("q?ok")))]
        );
        assert_eq!(state.popups, []);
        assert!(!format!("{state:?}").contains("q?ok"));
        // Still running, until git says otherwise.
        assert!(state.remote.is_some());
    }

    #[test]
    fn esc_cancels_the_question() {
        let (mut state, _) = typing_password("abc");
        assert_eq!(
            press(&mut state, KeyCode::Esc),
            [Effect::AnswerCredential(None)]
        );
        assert_eq!(state.popups, []);
        assert!(!state.should_quit);
        assert!(!format!("{state:?}").contains("abc"));
    }

    #[test]
    fn ctrl_c_cancels_the_question_then_quits() {
        let (mut state, _) = typing_password("abc");
        let effects = state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(
            effects,
            [Effect::AnswerCredential(None), Effect::InterruptGit]
        );
        assert!(state.should_quit);
        assert_eq!(state.popups, []);
    }

    #[test]
    fn a_typed_password_never_shows_in_debug_output() {
        let (state, tag) = typing_password("hunter2");
        assert!(!format!("{state:?}").contains("hunter2"));
        let intent = AppIntent::CredentialAnswered(Some(Secret::new("hunter2")));
        let effect = Effect::AnswerCredential(Some(Secret::new("hunter2")));
        assert!(!format!("{intent:?}").contains("hunter2"));
        assert!(!format!("{effect:?}").contains("hunter2"));
        let _ = tag;
    }

    #[test]
    fn ssh_yes_no_question_is_answered_with_y_or_n() {
        let mut state = remote_ready();
        let (tag, _) = remote_of(&press_char(&mut state, 'f'));
        state.update(asked(tag, "Allow use of key?", true));
        assert_eq!(state.key_focus(), KeyScope::Confirm);
        assert_eq!(
            press_char(&mut state, 'y'),
            [Effect::AnswerCredential(Some(Secret::new("yes")))]
        );
        assert_eq!(state.popups, []);

        state.update(asked(tag, "Allow use of key?", true));
        assert_eq!(
            press_char(&mut state, 'n'),
            [Effect::AnswerCredential(None)]
        );
        assert_eq!(state.popups, []);
    }

    #[test]
    fn a_question_nobody_waits_for_is_answered_no_at_once() {
        let mut state = remote_ready();
        let stale = Tag {
            generation: state.generation,
            seq: 42,
            user: true,
        };
        assert_eq!(
            state.update(asked(stale, "Password:", false)),
            [Effect::AnswerCredential(None)]
        );
        assert_eq!(state.popups, []);
    }

    #[test]
    fn a_question_still_open_when_git_ends_is_closed() {
        let (mut state, tag) = typing_password("abc");
        state.update(remote_finished(
            tag,
            PUSH,
            Err(RemoteFailure::Failed("fatal: Authentication failed".into())),
        ));
        match state.popups.as_slice() {
            [Popup::Error(error)] => assert_eq!(error.title, "Push failed"),
            other => panic!("expected only the error, got {other:?}"),
        }
    }

    #[test]
    fn an_ssh_notice_shows_while_the_op_runs() {
        let mut state = remote_ready();
        let (tag, _) = remote_of(&press_char(&mut state, 'f'));
        let text = "Confirm user presence for key ED25519-SK".to_string();
        assert_eq!(
            state.update(AppIntent::RemoteNotice {
                tag,
                text: text.clone()
            }),
            []
        );
        assert_eq!(state.remote.as_ref().unwrap().notice, Some(text));
        assert_eq!(state.popups, []);
    }
}
