//! The Remotes feature's `update` handlers: loading and moving through the
//! Remotes tab, checking out a remote branch as a tracking branch, and
//! adding, renaming and removing a remote.

use super::RemotePending;
use super::keys;
use super::lines;
use crate::confirm_question::{Answer, ConfirmQuestion, Then};
use crate::effect::{Effect, Tag};
use crate::features::branches::BranchesTab;
use crate::git::remotes::{Remote, validate_remote_name, validate_remote_url};
use crate::keymap::{self, KeyScope, Reason};
use crate::model::{AppState, Context};
use crate::op::GitOp;
use crate::popup::{LineInput, Popup, PromptKind};

/// Loading the Remotes tab and moving its selection.
impl AppState {
    /// Show the remotes, and on their tab load the selected branch's log
    /// afresh, as for the local list.
    pub(crate) fn remotes_loaded(
        &mut self,
        tag: Tag,
        result: Result<Vec<Remote>, String>,
    ) -> Vec<Effect> {
        self.user_done(tag);
        let list = &mut self.branches.remotes.list;
        if !list.loaded(tag, self.generation, result.map(lines)) {
            return vec![];
        }
        if self.branches.tab == BranchesTab::Remotes {
            self.load_branch_log(tag.user)
        } else {
            vec![]
        }
    }

    pub(crate) fn move_remote(&mut self, delta: isize) -> Vec<Effect> {
        if self.branches.remotes.list.move_by(delta) {
            self.load_branch_log(true)
        } else {
            vec![]
        }
    }
}

/// The Remotes tab's actions. Each prompt keeps its target from when it
/// opened, so a reload while the user types cannot change it.
impl AppState {
    /// Whether a Remotes action may run now; if not, the bar says why.
    fn remotes_action(&mut self, disabled: fn(&AppState) -> Option<Reason>) -> bool {
        if self.context != Context::Workspace {
            return false;
        }
        if let Some(reason) = disabled(self) {
            self.notice = Some(reason);
            return false;
        }
        true
    }

    /// Ask what to call the local branch, starting from the remote
    /// branch's own short name.
    pub(crate) fn ask_track_name(&mut self) {
        if !self.remotes_action(keys::track_disabled) {
            return;
        }
        let Some(from) = self.branches.remotes.selected_branch().cloned() else {
            return;
        };
        let input = LineInput::with_text(&from.short_display());
        self.branches.remotes.pending = Some(RemotePending::Track(from));
        self.popups.push(Popup::Prompt {
            kind: PromptKind::TrackBranch,
            input,
        });
    }

    pub(crate) fn track_named(&mut self, text: &str) -> Vec<Effect> {
        let Some(RemotePending::Track(from)) = self.branches.remotes.pending.take() else {
            return vec![];
        };
        // A bisect may have begun while the prompt was open.
        if self.bisect_refuses() {
            return vec![];
        }
        let name = text.trim();
        if name.is_empty() {
            self.notice = Some("A branch needs a name".into());
            return vec![];
        }
        if self.op_is_running() {
            self.notice = Some(keymap::OP_RUNNING.into());
            return vec![];
        }
        let name = name.to_string();
        self.start_op(Ok(Some(GitOp::CheckoutTracking { name, from })))
    }

    pub(crate) fn ask_remote_name(&mut self) {
        if !self.remotes_action(keys::add_remote_disabled) {
            return;
        }
        self.branches.remotes.pending = None;
        self.popups.push(Popup::Prompt {
            kind: PromptKind::RemoteName,
            input: LineInput::default(),
        });
    }

    /// Check the new remote's name, then ask for its URL.
    pub(crate) fn remote_named(&mut self, text: &str) {
        let name = text.trim();
        if let Err(reason) = validate_remote_name(name) {
            self.notice = Some(reason.into());
            return;
        }
        self.branches.remotes.pending = Some(RemotePending::AddUrl {
            name: name.to_string(),
        });
        self.popups.push(Popup::Prompt {
            kind: PromptKind::RemoteUrl,
            input: LineInput::default(),
        });
    }

    /// Add the remote named before, once its URL passes the checks. No
    /// fetch: its branches appear after the user's own.
    pub(crate) fn remote_url(&mut self, text: &str) -> Vec<Effect> {
        let Some(RemotePending::AddUrl { name }) = self.branches.remotes.pending.take() else {
            return vec![];
        };
        let url = text.trim();
        if let Err(reason) = validate_remote_url(url) {
            self.notice = Some(reason.into());
            return vec![];
        }
        if self.op_is_running() {
            self.notice = Some(keymap::OP_RUNNING.into());
            return vec![];
        }
        let url = url.to_string();
        self.start_op(Ok(Some(GitOp::AddRemote { name, url })))
    }

    pub(crate) fn ask_remote_rename(&mut self) {
        if !self.remotes_action(keys::rename_remote_disabled) {
            return;
        }
        let Some(name) = self
            .branches
            .remotes
            .selected_remote()
            .map(|r| r.name.clone())
        else {
            return;
        };
        let input = LineInput::with_text(&name.display());
        self.branches.remotes.pending = Some(RemotePending::Rename(name));
        self.popups.push(Popup::Prompt {
            kind: PromptKind::RenameRemote,
            input,
        });
    }

    pub(crate) fn rename_remote_to(&mut self, text: &str) -> Vec<Effect> {
        let Some(RemotePending::Rename(from)) = self.branches.remotes.pending.take() else {
            return vec![];
        };
        let to = text.trim();
        if let Err(reason) = validate_remote_name(to) {
            self.notice = Some(reason.into());
            return vec![];
        }
        if self.op_is_running() {
            self.notice = Some(keymap::OP_RUNNING.into());
            return vec![];
        }
        let to = to.to_string();
        self.start_op(Ok(Some(GitOp::RenameRemote { from, to })))
    }

    /// Ask before removing the selected remote: its remote-tracking
    /// branches go with it.
    pub(crate) fn ask_remove_remote(&mut self) {
        if !self.remotes_action(keys::change_remote_disabled) {
            return;
        }
        let Some(name) = self
            .branches
            .remotes
            .selected_remote()
            .map(|r| r.name.clone())
        else {
            return;
        };
        let question = remove_remote_question(name.display(), GitOp::RemoveRemote(name));
        self.popups.push(Popup::Question(question));
    }

    /// Remove the remote the open question names, and only if one is open.
    pub(crate) fn confirm_remove_remote(&mut self) -> Vec<Effect> {
        self.answer_question(KeyScope::RemoveRemote, Answer::YOnly)
    }

    /// Closing a remote's prompt forgets what it was asked for.
    pub(crate) fn remotes_popup_closed(&mut self, closed: &Popup) {
        if matches!(
            closed,
            Popup::Prompt {
                kind: PromptKind::TrackBranch
                    | PromptKind::RemoteName
                    | PromptKind::RemoteUrl
                    | PromptKind::RenameRemote,
                ..
            }
        ) {
            self.branches.remotes.pending = None;
        }
    }
}

/// "Remove this remote?", carrying the op chosen when it was asked, so a
/// reload before the answer cannot change the target. `name` is the
/// remote's display name, already terminal-safe.
pub(crate) fn remove_remote_question(name: String, op: GitOp) -> ConfirmQuestion {
    ConfirmQuestion {
        scope: KeyScope::RemoveRemote,
        title: "Remove remote".into(),
        text: format!(
            "Remove remote {name}? Its remote-tracking branches are deleted, and \
             branches tracking it lose their upstream. (y/n)"
        ),
        answer: Answer::YOnly,
        then: Then::Op(op),
        range: false,
        head: None,
    }
}

#[cfg(test)]
mod tests {
    use crate::confirm_question::Then;
    use crate::effect::Effect;
    use crate::features::branches::BranchesTab;
    use crate::features::branches::remotes::RemoteLine;
    use crate::intent::AppIntent;
    use crate::keymap::{self, KeyScope};
    use crate::op::GitOp;
    use crate::popup::{Popup, PromptKind};
    use crate::test_support::{
        branches_loaded, branches_tag, finished, git_remote, log_load, on_remote_row, op_of, press,
        press_char, reload_remotes, remotes_tag, shift, three_branches, three_remotes, type_text,
        with_branches, with_remotes,
    };
    use crossterm::event::KeyCode;

    #[test]
    fn a_stale_or_other_repository_remotes_load_is_dropped() {
        let mut state = with_branches();
        let old = remotes_tag(&state.update(AppIntent::RefreshStatus));
        let newer = remotes_tag(&state.update(AppIntent::RefreshStatus));
        state.update(AppIntent::RemotesLoaded {
            tag: old,
            result: Ok(three_remotes()),
        });
        assert!(!state.branches.remotes.list.loaded);
        state.generation += 1;
        state.update(AppIntent::RemotesLoaded {
            tag: newer,
            result: Ok(three_remotes()),
        });
        assert!(!state.branches.remotes.list.loaded);
        assert_eq!(state.branches.remotes.remotes().count(), 0);
    }

    #[test]
    fn brackets_switch_the_tab_and_j_and_k_move_over_remotes_and_branches() {
        let mut state = with_remotes();
        assert_eq!(
            press_char(&mut state, ']'),
            vec![],
            "a header row has no log"
        );
        assert_eq!(state.branches.tab, BranchesTab::Remotes);
        assert_eq!(state.key_focus(), KeyScope::Remotes);
        assert_eq!(state.branches.log, Default::default());
        assert!(matches!(
            state.branches.remotes.list.selected_row(),
            Some(RemoteLine::Remote(r)) if r.name == "fork".into()
        ));

        assert_eq!(press_char(&mut state, 'j'), vec![], "origin's header");
        assert!(matches!(
            state.branches.remotes.list.selected_row(),
            Some(RemoteLine::Remote(r)) if r.name == "origin".into()
        ));
        let (tag, refname) = log_load(&press_char(&mut state, 'j')).unwrap();
        assert_eq!(refname, b"refs/remotes/origin/main");
        assert!(tag.user);
        press(&mut state, KeyCode::Down);
        press(&mut state, KeyCode::Down);
        assert!(matches!(
            state.branches.remotes.list.selected_row(),
            Some(RemoteLine::Remote(r)) if r.name == "stray".into()
        ));
        press_char(&mut state, 'k');
        press(&mut state, KeyCode::Up);
        assert_eq!(
            state.branches.remotes.selected_branch().unwrap().display(),
            "origin/main"
        );

        let (_, refname) = log_load(&press_char(&mut state, '[')).unwrap();
        assert_eq!(refname, b"refs/heads/main", "the local selection's log");
        assert_eq!(state.branches.tab, BranchesTab::Local);
        assert_eq!(state.key_focus(), KeyScope::Branches);
    }

    #[test]
    fn a_reload_on_one_tab_reloads_only_its_own_log() {
        let mut state = on_remote_row(2);
        let tag = branches_tag(&state.update(AppIntent::RefreshStatus));
        let effects = state.update(branches_loaded(tag, three_branches()));
        assert_eq!(log_load(&effects), None, "the local list is not shown");
        let effects = reload_remotes(&mut state, three_remotes());
        let (_, refname) = log_load(&effects).expect("the shown branch's log");
        assert_eq!(refname, b"refs/remotes/origin/main");
    }

    #[test]
    fn checking_out_a_remote_branch_asks_for_the_name_and_tracks_it() {
        let mut state = on_remote_row(0);
        assert_eq!(press_char(&mut state, ' '), vec![]);
        assert_eq!(state.notice, Some("Select a remote branch".into()));
        assert_eq!(state.popups, []);

        state.branches.remotes.list.move_by(3);
        press_char(&mut state, ' ');
        match state.popups.as_slice() {
            [Popup::Prompt { kind, input }] => {
                assert_eq!(*kind, PromptKind::TrackBranch);
                assert_eq!(input.text, "topic");
            }
            other => panic!("expected the name prompt, got {other:?}"),
        }
        assert_eq!(
            state.prompt_title(PromptKind::TrackBranch),
            "Check out origin/topic as"
        );
        // A reload while the user types moves the list under the prompt.
        reload_remotes(&mut state, vec![git_remote("origin", true, &["main"])]);
        type_text(&mut state, "-2");
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
        assert_eq!(
            op,
            GitOp::CheckoutTracking {
                name: "topic-2".into(),
                from: three_remotes()[1].branches[1].clone(),
            }
        );
        assert_eq!(state.branches.remotes.pending, None);
    }

    #[test]
    fn an_invalid_remote_name_or_url_leaves_a_notice_and_no_effect() {
        let mut state = on_remote_row(0);
        for bad in ["", "-x", "a b", "a..b", "x.lock"] {
            press_char(&mut state, 'a');
            type_text(&mut state, bad);
            assert_eq!(press(&mut state, KeyCode::Enter), vec![], "{bad:?}");
            assert!(state.notice.is_some(), "{bad:?}");
            assert_eq!(state.popups, [], "{bad:?}");
        }
        for bad in [
            "ext::sh -c x",
            "EXT::x",
            "fd::3",
            "ext://sh -c x",
            "EXT://x",
            "fd://3",
            "-oProxyCommand=x",
            "  ",
        ] {
            press_char(&mut state, 'a');
            type_text(&mut state, "fork2\n");
            assert_eq!(
                state.prompt_title(PromptKind::RemoteUrl),
                "URL of fork2",
                "{bad:?}"
            );
            type_text(&mut state, bad);
            assert_eq!(press(&mut state, KeyCode::Enter), vec![], "{bad:?}");
            assert!(state.notice.is_some(), "{bad:?}");
            assert_eq!(
                (state.held_seq(), &state.popups),
                (None, &vec![]),
                "{bad:?}"
            );
            assert_eq!(state.branches.remotes.pending, None);
        }
    }

    #[test]
    fn adding_a_remote_runs_only_the_op_and_never_fetches() {
        let mut state = on_remote_row(0);
        press_char(&mut state, 'a');
        type_text(&mut state, " fork2 \n");
        type_text(&mut state, "https://u:t@h/fork.git");
        let effects = press(&mut state, KeyCode::Enter);
        let (tag, op) = op_of(&effects);
        assert_eq!(
            op,
            GitOp::AddRemote {
                name: "fork2".into(),
                url: "https://u:t@h/fork.git".into(),
            }
        );
        assert_eq!(state.remote, None);

        let effects = state.update(finished(tag, op, Ok(())));
        remotes_tag(&effects);
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Remote { .. })),
            "{effects:?}"
        );
        assert_eq!(state.remote, None);
    }

    #[test]
    fn a_cancelled_remote_prompt_forgets_its_target() {
        let mut state = on_remote_row(0);
        press_char(&mut state, 'a');
        type_text(&mut state, "fork2\n");
        assert!(state.branches.remotes.pending.is_some());
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.branches.remotes.pending, None);
        assert_eq!(state.update(AppIntent::AddRemoteUrl("/x".into())), vec![]);
    }

    #[test]
    fn renaming_keeps_the_remote_selected_when_the_prompt_opened() {
        let mut state = on_remote_row(1);
        shift(&mut state, 'R');
        match state.popups.as_slice() {
            [Popup::Prompt { kind, input }] => {
                assert_eq!(*kind, PromptKind::RenameRemote);
                assert_eq!(input.text, "origin");
            }
            other => panic!("expected the rename prompt, got {other:?}"),
        }
        reload_remotes(&mut state, vec![git_remote("fork", true, &[])]);
        type_text(&mut state, "-old");
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
        assert_eq!(
            op,
            GitOp::RenameRemote {
                from: "origin".into(),
                to: "origin-old".into(),
            }
        );

        let mut state = on_remote_row(1);
        shift(&mut state, 'R');
        for _ in 0..6 {
            press(&mut state, KeyCode::Backspace);
        }
        type_text(&mut state, "-bad");
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert!(state.notice.is_some());
    }

    #[test]
    fn rename_and_remove_are_refused_for_an_option_like_name_or_an_unconfigured_group() {
        let mut state = on_remote_row(4);
        assert!(!state.branches.remotes.selected_remote().unwrap().configured);
        for key in ['d', 'R'] {
            let effects = if key == 'R' {
                shift(&mut state, key)
            } else {
                press_char(&mut state, key)
            };
            assert_eq!(effects, vec![]);
            assert_eq!(state.notice, Some("Not a configured remote".into()));
            assert_eq!(state.popups, []);
        }

        let mut state = with_branches();
        reload_remotes(&mut state, vec![git_remote("-f", true, &["x"])]);
        press_char(&mut state, ']');
        state.branches.remotes.list.move_by(1);
        for intent in [AppIntent::RemoveRemote, AppIntent::RenameRemote] {
            state.notice = None;
            assert_eq!(state.update(intent), vec![]);
            assert!(state.notice.unwrap().contains("like an option"));
            assert_eq!(state.popups, []);
        }
    }

    #[test]
    fn removing_a_remote_asks_and_only_y_removes_the_one_asked_about() {
        let mut state = on_remote_row(2);
        press_char(&mut state, 'd');
        assert_eq!(state.key_focus(), KeyScope::RemoveRemote);
        match state.popups.as_slice() {
            [Popup::Question(q)] => {
                assert!(q.text.starts_with("Remove remote origin?"), "{}", q.text);
                assert_eq!(q.then, Then::Op(GitOp::RemoveRemote("origin".into())));
            }
            other => panic!("expected the question, got {other:?}"),
        }
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.popups.len(), 1, "enter does not answer");
        assert_eq!(press_char(&mut state, 'q'), vec![]);
        assert!(!state.should_quit);

        // A reload under the question cannot change what it removes.
        reload_remotes(&mut state, vec![git_remote("fork", true, &[])]);
        let (_, op) = op_of(&press_char(&mut state, 'y'));
        assert_eq!(op, GitOp::RemoveRemote("origin".into()));
        assert_eq!(state.popups, []);

        for key in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut state = on_remote_row(2);
            press_char(&mut state, 'd');
            assert_eq!(press(&mut state, key), vec![]);
            assert_eq!(state.popups, [], "{key:?}");
            assert_eq!(state.update(AppIntent::ConfirmRemoveRemote), vec![]);
        }
    }

    #[test]
    fn remote_actions_wait_for_a_running_op() {
        let mut state = on_remote_row(2);
        state.hold_op(99);
        for intent in [
            AppIntent::TrackRemoteBranch,
            AppIntent::AddRemote,
            AppIntent::RenameRemote,
            AppIntent::RemoveRemote,
        ] {
            state.notice = None;
            assert_eq!(state.update(intent.clone()), vec![], "{intent:?}");
            assert_eq!(state.notice, Some(keymap::OP_RUNNING.into()), "{intent:?}");
            assert_eq!(state.popups, [], "{intent:?}");
        }
    }

    #[test]
    fn closing_a_remote_prompt_forgets_what_it_asked_for() {
        use crate::features::branches::remotes::RemotePending;
        use crate::model::{AppState, Context};
        use crate::popup::LineInput;
        for kind in [
            PromptKind::TrackBranch,
            PromptKind::RemoteName,
            PromptKind::RemoteUrl,
            PromptKind::RenameRemote,
        ] {
            let mut state = AppState::new(Context::Workspace);
            state.branches.remotes.pending = Some(RemotePending::Rename("origin".into()));
            state.popups.push(Popup::Prompt {
                kind,
                input: LineInput::default(),
            });
            assert_eq!(state.update(AppIntent::PopPopup), vec![], "{kind:?}");
            assert_eq!(state.popups, [], "{kind:?}");
            assert_eq!(state.branches.remotes.pending, None, "{kind:?}");
        }
    }
}
