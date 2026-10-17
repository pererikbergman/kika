//! The Branches feature's `update` handlers: loading the branch list and
//! the selected branch's log, moving between rows and tabs, checking out
//! the selected branch, creating, renaming and deleting one, and setting
//! or unsetting its upstream.

use super::keys;
use super::{BranchesIntent, BranchesTab, remotes, tags};
use crate::confirm_question::{Answer, ConfirmQuestion, Then};
use crate::effect::{Effect, Tag};
use crate::keymap::{self, KeyScope};
use crate::model::{AppState, Context};
use crate::op::GitOp;
use crate::popup::{LineInput, Popup, PromptKind, RangeKind};
use crate::refresh::{RefreshPlan, Scope};
use kika_git::branches::{Branch, BranchName, LogRow};

/// The branches subtree's half of `update`: core's one
/// `AppIntent::Branches` arm lands here, and each sub-feature's intent goes
/// on to its own `update`. A free function, so its name cannot clash with
/// handlers.
pub(crate) fn update(state: &mut AppState, intent: BranchesIntent) -> Vec<Effect> {
    match intent {
        BranchesIntent::Loaded { tag, result } => return state.branches_loaded(tag, result),
        BranchesIntent::LogLoaded {
            tag,
            refname,
            result,
        } => state.branch_log_loaded(tag, refname, result),
        BranchesIntent::SwitchTab => return state.switch_branches_tab(BranchesTab::next),
        BranchesIntent::PrevTab => return state.switch_branches_tab(BranchesTab::prev),
        BranchesIntent::SetUpstream => state.ask_upstream(),
        BranchesIntent::SetUpstreamTo(text) => return state.set_upstream_to(&text),
        BranchesIntent::UnsetUpstream => return state.unset_upstream(),
        BranchesIntent::Next => return state.move_branch(1),
        BranchesIntent::Prev => return state.move_branch(-1),
        BranchesIntent::MoveBy(delta) => return state.move_branch(delta),
        BranchesIntent::Checkout => return state.checkout_branch(),
        BranchesIntent::New => state.ask_branch_name(PromptKind::NewBranch),
        BranchesIntent::Rename => state.ask_branch_name(PromptKind::RenameBranch),
        BranchesIntent::Delete if state.branches.list.range_anchor.is_some() => {
            state.ask_delete_range(RangeKind::Branches)
        }
        BranchesIntent::Delete => state.ask_delete_branch(),
        BranchesIntent::CreateNamed(text) => return state.create_branch_named(&text),
        BranchesIntent::NewAtHead => state.ask_branch_at_head(),
        BranchesIntent::CreateAtHeadNamed(text) => {
            return state.create_branch_at_head_named(&text);
        }
        BranchesIntent::RenameTo(text) => return state.rename_branch_to(&text),
        BranchesIntent::ConfirmDelete { force } => return state.confirm_delete_branch(force),
        BranchesIntent::Remotes(i) => return remotes::update(state, i),
        BranchesIntent::Tags(i) => return tags::update(state, i),
    }
    Vec::new()
}

/// The Branches panel: loading the list and the selected branch's log,
/// moving the selection, checking out the selected branch, and changing
/// branches and their upstreams.
impl AppState {
    /// [`Self::reload_branches`], for a change noticed outside Kika: out
    /// of the busy count, and only one at a time.
    pub(crate) fn background_branches(&mut self) -> Vec<Effect> {
        if self.watch.branches_pending {
            return vec![];
        }
        self.load_branches(false)
    }

    /// Load the local branches and the remotes together: whatever moves
    /// one can move the other. A background remotes load is not counted:
    /// the branch load beside it keeps a second pair from starting.
    pub(crate) fn load_branches(&mut self, user: bool) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        let seq = self.take_seq();
        self.branches.list.begin(seq);
        let branches = self.start_branch_load(seq, user);
        let remotes = self.take_seq();
        self.branches.remotes.list.begin(remotes);
        let remotes = self.effect_tag(remotes, user);
        let tags = self.take_seq();
        self.branches.tags.begin(tags);
        let tags = self.effect_tag(tags, user);
        vec![
            Effect::LoadBranches { tag: branches },
            Effect::LoadRemotes { tag: remotes },
            Effect::LoadTags { tag: tags },
        ]
    }

    /// A user load raises the busy count; a background one the watch's flag.
    fn start_branch_load(&mut self, seq: u64, user: bool) -> Tag {
        if !user {
            self.watch.branches_pending = true;
        }
        self.effect_tag(seq, user)
    }

    /// Every branch load reports back once, stale or not.
    fn end_branch_load(&mut self, tag: Tag) {
        self.user_done(tag);
        if !tag.user {
            self.watch.branches_pending = false;
        }
    }

    /// Show the list, and load the selected branch's log afresh: a
    /// reload means its commits may have moved.
    pub(crate) fn branches_loaded(
        &mut self,
        tag: Tag,
        result: Result<Vec<Branch>, String>,
    ) -> Vec<Effect> {
        self.end_branch_load(tag);
        let listed = result.is_ok();
        if !self.branches.list.loaded(tag, self.generation, result) {
            return vec![];
        }
        if listed {
            self.forge
                .set_branches(self.branches.list.rows.iter().map(|b| &b.name));
        }
        let mut effects = if self.branches.tab == BranchesTab::Local {
            self.load_branch_log(tag.user)
        } else {
            vec![]
        };
        if listed {
            effects.extend(self.apply_plan(RefreshPlan::of(&[Scope::PullRequests])));
        }
        effects
    }

    /// Start loading the log of the branch selected on the tab shown;
    /// with none selected, or a remote's header row, there is no log to
    /// show. `user` as for the list.
    pub(crate) fn load_branch_log(&mut self, user: bool) -> Vec<Effect> {
        let refname = match self.branches.tab {
            BranchesTab::Local => self.branches.selected_branch().map(|b| b.name.ref_bytes()),
            BranchesTab::Remotes => self
                .branches
                .remotes
                .selected_branch()
                .map(|b| b.refname.clone()),
            BranchesTab::Tags => self
                .branches
                .tags
                .selected_row()
                .map(|t| t.name.ref_bytes()),
        };
        let Some(refname) = refname else {
            self.branches.log = Default::default();
            return vec![];
        };
        let seq = self.take_seq();
        self.branches.log.begin(seq, refname.clone());
        let tag = self.start_branch_load(seq, user);
        vec![Effect::LoadBranchLog { tag, refname }]
    }

    pub(crate) fn branch_log_loaded(
        &mut self,
        tag: Tag,
        refname: Vec<u8>,
        result: Result<Vec<LogRow>, String>,
    ) {
        self.end_branch_load(tag);
        let log = &mut self.branches.log;
        if tag.generation != self.generation || log.for_ref.as_ref() != Some(&refname) {
            return;
        }
        log.apply(tag.seq, result);
    }

    /// Show the tab `step` gives, and the log of what is selected there.
    pub(crate) fn switch_branches_tab(
        &mut self,
        step: fn(BranchesTab) -> BranchesTab,
    ) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        self.branches.clear_range(self.branches.tab);
        self.branches.tab = step(self.branches.tab);
        self.load_branch_log(true)
    }

    pub(crate) fn move_branch(&mut self, delta: isize) -> Vec<Effect> {
        if self.branches.list.move_by(delta) {
            self.load_branch_log(true)
        } else {
            vec![]
        }
    }

    pub(crate) fn checkout_branch(&mut self) -> Vec<Effect> {
        match self.branch_for_action(keys::checkout_branch_disabled) {
            Some(name) => self.move_head_to_branch(GitOp::Checkout(name)),
            None => vec![],
        }
    }

    /// Run `op`, which puts `HEAD` on a branch. From a detached `HEAD`,
    /// first look for the commits it would leave behind and ask before
    /// leaving them; the branch tip is one of the refs the check keeps, so
    /// it passes no target. On a branch, nothing can be left behind.
    pub(crate) fn move_head_to_branch(&mut self, op: GitOp) -> Vec<Effect> {
        match self.detached_head() {
            Some(head) => self.check_left_behind(op, head, None),
            None => self.start_op(Ok(Some(op))),
        }
    }

    /// Ask for a branch name, keeping the selected branch for the answer.
    /// Renaming starts from the old name.
    pub(crate) fn ask_branch_name(&mut self, kind: PromptKind) {
        let disabled = match kind {
            // Creating a branch checks it out, which moves `HEAD`.
            PromptKind::NewBranch => keys::new_branch_disabled,
            _ => keymap::branch_action_disabled,
        };
        let Some(name) = self.branch_for_action(disabled) else {
            return;
        };
        let input = match kind {
            PromptKind::RenameBranch => LineInput::with_text(&name.display()),
            _ => LineInput::default(),
        };
        self.branches.pending = Some(name);
        self.popups.push(Popup::Prompt { kind, input });
    }

    /// Run the op a branch prompt's answer makes, on the branch kept when
    /// the prompt opened.
    fn named_branch_op(&mut self, text: &str, op: fn(BranchName, String) -> GitOp) -> Vec<Effect> {
        let Some(branch) = self.branches.pending.take() else {
            return vec![];
        };
        let Some(name) = self.branch_name_to_run(text) else {
            return vec![];
        };
        match op(branch, name) {
            // Creating a branch checks it out, which moves `HEAD`.
            op @ GitOp::CreateBranch { .. } => self.move_head_to_branch(op),
            op => self.start_op(Ok(Some(op))),
        }
    }

    /// A branch prompt's answer, trimmed, when an op may run with it now;
    /// else `None`, saying why in the bar.
    fn branch_name_to_run(&mut self, text: &str) -> Option<String> {
        let name = text.trim();
        if name.is_empty() {
            self.notice = Some("A branch needs a name".into());
            return None;
        }
        if self.op_is_running() {
            self.notice = Some(keymap::OP_RUNNING.into());
            return None;
        }
        Some(name.to_string())
    }

    /// Create the branch the prompt named, unless a bisect began while it
    /// was open.
    pub(crate) fn create_branch_named(&mut self, text: &str) -> Vec<Effect> {
        // A bisect may have begun while the prompt was open.
        if self.bisect_refuses() {
            self.branches.pending = None;
            return vec![];
        }
        self.named_branch_op(text, |from, name| GitOp::CreateBranch { name, from })
    }

    /// Ask for a name, keeping the detached `HEAD` for the answer.
    pub(crate) fn ask_branch_at_head(&mut self) {
        if let Some(reason) = keys::branch_at_head_disabled(self) {
            self.notice = Some(reason);
            return;
        }
        self.branches.pending_head = self.detached_head();
        self.popups.push(Popup::Prompt {
            kind: PromptKind::NewBranchAtHead,
            input: LineInput::default(),
        });
    }

    /// Create the branch the prompt named at the `HEAD` kept when it
    /// opened, and check it out. Runs nothing once `HEAD` has moved, or a
    /// bisect began, while the prompt was open.
    pub(crate) fn create_branch_at_head_named(&mut self, text: &str) -> Vec<Effect> {
        let Some(head) = self.branches.pending_head.take() else {
            return vec![];
        };
        if self.bisect_refuses() {
            return vec![];
        }
        let Some(name) = self.branch_name_to_run(text) else {
            return vec![];
        };
        if !self.head_still(Some(&head)) {
            return vec![];
        }
        self.start_op(Ok(Some(GitOp::CreateBranchAt { name, at: head })))
    }

    /// Rename the branch kept when the prompt opened.
    pub(crate) fn rename_branch_to(&mut self, text: &str) -> Vec<Effect> {
        self.named_branch_op(text, |from, to| GitOp::RenameBranch { from, to })
    }

    /// Ask before deleting the selected branch: plainly when `git branch
    /// -d` would take it, else warning that its commits may be lost.
    pub(crate) fn ask_delete_branch(&mut self) {
        if self
            .branch_for_action(keys::delete_branch_disabled)
            .is_none()
        {
            return;
        }
        let Some(branch) = self.branches.selected_branch() else {
            return;
        };
        let question = delete_branch_question(
            branch.name.display(),
            GitOp::DeleteBranch {
                name: branch.name.clone(),
                force: !branch.merged,
            },
        );
        self.popups.push(Popup::Question(question));
    }

    /// Delete as the open question asked: `force` must match it, so `y`
    /// never deletes unmerged commits; a mismatched key leaves it open.
    pub(crate) fn confirm_delete_branch(&mut self, force: bool) -> Vec<Effect> {
        let answer = if force { Answer::Force } else { Answer::YOnly };
        self.answer_question(KeyScope::DeleteBranch, answer)
    }

    /// Ask which remote branch the selected branch should track, starting
    /// from its upstream, or else the same name on `origin` or the first
    /// remote.
    pub(crate) fn ask_upstream(&mut self) {
        let Some(name) = self.branch_for_action(keymap::branch_action_disabled) else {
            return;
        };
        let upstream = self
            .branches
            .selected_branch()
            .and_then(|b| b.upstream.clone());
        let text = upstream.unwrap_or_else(|| {
            let configured = || self.branches.remotes.remotes().filter(|r| r.configured);
            let remote = configured()
                .find(|r| r.name.0 == b"origin")
                .or_else(|| configured().next());
            match remote {
                Some(remote) => format!("{}/{}", remote.name.display(), name.display()),
                None => name.display(),
            }
        });
        self.branches.pending = Some(name);
        self.popups.push(Popup::Prompt {
            kind: PromptKind::SetUpstream,
            input: LineInput::with_text(&text),
        });
    }

    /// Set the upstream of the branch kept when the prompt opened, to the
    /// loaded remote branch written exactly as typed.
    pub(crate) fn set_upstream_to(&mut self, text: &str) -> Vec<Effect> {
        let Some(branch) = self.branches.pending.take() else {
            return vec![];
        };
        let Some(upstream) = self.branches.remotes.branch_named(text.trim()).cloned() else {
            self.notice = Some("No such remote branch; fetch, or check the name".into());
            return vec![];
        };
        if self.op_is_running() {
            self.notice = Some(keymap::OP_RUNNING.into());
            return vec![];
        }
        self.start_op(Ok(Some(GitOp::SetUpstream { branch, upstream })))
    }

    /// Stop the selected branch tracking its upstream.
    pub(crate) fn unset_upstream(&mut self) -> Vec<Effect> {
        let op = self
            .branch_for_action(keys::unset_upstream_disabled)
            .map(GitOp::UnsetUpstream);
        self.start_op(Ok(op))
    }

    /// Closing a branch name or upstream prompt forgets the branch, or
    /// the `HEAD`, it was asked for.
    pub(crate) fn branches_popup_closed(&mut self, closed: &Popup) {
        if matches!(
            closed,
            Popup::Prompt {
                kind: PromptKind::NewBranchAtHead,
                ..
            }
        ) {
            self.branches.pending_head = None;
        }
        if matches!(
            closed,
            Popup::Prompt {
                kind: PromptKind::NewBranch | PromptKind::RenameBranch | PromptKind::SetUpstream,
                ..
            }
        ) {
            self.branches.pending = None;
        }
    }
}

/// "Delete this branch?", carrying the op chosen when it was asked.
/// `name` is the branch's display name, already terminal-safe. A branch
/// whose commits are not merged is asked about in stronger words, and
/// only a force delete (`D`) answers it.
pub(crate) fn delete_branch_question(name: String, op: GitOp) -> ConfirmQuestion {
    let (title, text, answer) = if matches!(op, GitOp::DeleteBranch { force: true, .. }) {
        (
            "Force delete branch",
            format!(
                "{name} is not fully merged; its commits may be lost.\n\
                 Press D to force delete, n or Esc to cancel"
            ),
            Answer::Force,
        )
    } else {
        (
            "Delete branch",
            format!("Delete branch {name}? (y/n)"),
            Answer::YOnly,
        )
    };
    ConfirmQuestion {
        scope: KeyScope::DeleteBranch,
        title: title.into(),
        text,
        answer,
        then: Then::Op(op),
        range: false,
        head: None,
    }
}

#[cfg(test)]
mod tests {
    use crate::confirm_question::Answer;
    use crate::effect::Effect;
    use crate::features::branches::BranchesIntent;
    use crate::features::branches::BranchesTab;
    use crate::intent::AppIntent;
    use crate::keymap::{KeyPress, KeyScope};
    use crate::model::{AppState, Context};
    use crate::op::GitOp;
    use crate::popup::{Popup, PromptKind};
    use crate::test_support::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use kika_git::branches::LogRow;

    fn log_row(subject: &str) -> LogRow {
        LogRow {
            short_oid: "abc1234".into(),
            date: "2024-01-01".into(),
            author: "a".into(),
            subject: subject.into(),
        }
    }

    #[test]
    fn the_branches_panel_takes_its_own_keys() {
        let state = with_branches();
        assert_eq!(state.panel_focus(), KeyScope::Branches);
        assert_eq!(state.key_focus(), KeyScope::Branches);
    }

    #[test]
    fn move_by_steps_through_the_shown_branches_with_one_log_load() {
        let mut state = with_branches();
        let effects = state.update(AppIntent::Branches(BranchesIntent::MoveBy(5)));
        assert_eq!(selected_name(&state), "wip", "clamped at the end");
        assert!(log_load(&effects).is_some(), "{effects:?}");
        assert_eq!(
            state.update(AppIntent::Branches(BranchesIntent::MoveBy(1))),
            vec![]
        );

        state.branches.list.set_filter("p".into());
        let effects = state.update(AppIntent::Branches(BranchesIntent::MoveBy(-5)));
        assert_eq!(selected_name(&state), "topic", "main is hidden");
        assert!(log_load(&effects).is_some(), "{effects:?}");
    }

    #[test]
    fn a_reload_keeps_the_selected_branch_by_name() {
        let mut state = with_branches();
        state.update(AppIntent::Branches(BranchesIntent::Next));
        assert_eq!(selected_name(&state), "topic");

        let tag = branches_tag(&state.update(AppIntent::RefreshStatus));
        let mut moved = three_branches();
        moved.swap(1, 2);
        state.update(branches_loaded(tag, moved));

        assert_eq!(selected_name(&state), "topic");
        assert_eq!(state.branches.list.selected, 2);
    }

    #[test]
    fn the_forge_sees_the_names_of_an_applied_load_only() {
        use kika_git::branches::BranchName;
        let mut state = with_branches();
        let names = |state: &AppState| state.forge.branches.clone();
        let listed: Vec<BranchName> = ["main", "topic", "wip"].map(Into::into).into();
        assert_eq!(names(&state), listed);

        // An applied load: its UTF-8 names, in list order.
        let tag = branches_tag(&state.update(AppIntent::RefreshStatus));
        let mut branches = vec![
            git_branch("new", false, true),
            git_branch("main", true, true),
        ];
        branches.push(git_branch("x", false, true));
        branches[2].name = BranchName::from_bytes(b"x\xff");
        state.update(branches_loaded(tag, branches));
        let applied: Vec<BranchName> = ["new", "main"].map(Into::into).into();
        assert_eq!(names(&state), applied);

        // Neither a failed load, nor a stale one, nor one from another
        // repository changes what the forge sees.
        let tag = branches_tag(&state.update(AppIntent::RefreshStatus));
        state.update(AppIntent::Branches(BranchesIntent::Loaded {
            tag,
            result: Err("broken".into()),
        }));
        let old = branches_tag(&state.update(AppIntent::RefreshStatus));
        let mut other_repo = branches_tag(&state.update(AppIntent::RefreshStatus));
        state.update(branches_loaded(old, three_branches()));
        other_repo.generation += 1;
        state.update(branches_loaded(other_repo, three_branches()));
        assert_eq!(names(&state), applied);
    }

    #[test]
    fn a_stale_branch_list_or_log_is_dropped() {
        let mut state = with_branches();
        let old = branches_tag(&state.update(AppIntent::RefreshStatus));
        let new = branches_tag(&state.update(AppIntent::RefreshStatus));
        state.update(branches_loaded(old, vec![git_branch("stale", true, true)]));
        assert_eq!(state.branches.list.rows, three_branches());

        let mut other_repo = new;
        other_repo.generation += 1;
        state.update(branches_loaded(other_repo, vec![]));
        assert_eq!(state.branches.list.rows, three_branches());

        let (first, refname) =
            log_load(&state.update(AppIntent::Branches(BranchesIntent::Next))).unwrap();
        let (second, _) =
            log_load(&state.update(AppIntent::Branches(BranchesIntent::Next))).unwrap();
        state.update(AppIntent::Branches(BranchesIntent::LogLoaded {
            tag: first,
            refname,
            result: Ok(vec![log_row("topic's")]),
        }));
        assert!(
            state.branches.log.rows.is_empty(),
            "the log of wip is awaited"
        );
        state.update(AppIntent::Branches(BranchesIntent::LogLoaded {
            tag: second,
            refname: b"refs/heads/wip".to_vec(),
            result: Ok(vec![log_row("wip's")]),
        }));
        assert_eq!(state.branches.log.rows, [log_row("wip's")]);
    }

    #[test]
    fn moving_the_selection_loads_the_new_branch_log() {
        let mut state = with_branches();
        let (tag, refname) =
            log_load(&state.update(AppIntent::Branches(BranchesIntent::Next))).unwrap();
        assert_eq!(refname, b"refs/heads/topic");
        assert!(tag.user);
        assert_eq!(
            state.branches.log.for_ref.as_deref(),
            Some(&b"refs/heads/topic"[..])
        );
        // At the end there is nowhere to go, so nothing loads.
        state.update(AppIntent::Branches(BranchesIntent::Next));
        assert_eq!(
            state.update(AppIntent::Branches(BranchesIntent::Next)),
            vec![]
        );
    }

    #[test]
    fn j_and_k_move_the_branch_selection_and_shift_j_scrolls_its_log() {
        let mut state = with_branches();
        press_char(&mut state, 'j');
        assert_eq!(selected_name(&state), "topic");
        let (tag, refname) = log_load(&press_char(&mut state, 'j')).unwrap();
        state.update(AppIntent::Branches(BranchesIntent::LogLoaded {
            tag,
            refname,
            result: Ok(vec![log_row("1"), log_row("2")]),
        }));
        state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('J'),
            KeyModifiers::SHIFT,
        )));
        assert_eq!(state.branches.log.scroll, 1);
        press_char(&mut state, 'k');
        assert_eq!(selected_name(&state), "topic");
        assert_eq!(state.branches.log.scroll, 0, "another branch's log");
    }

    #[test]
    fn checking_out_runs_the_op_and_its_finish_reloads_status_and_branches() {
        let mut state = with_branches();
        state.update(AppIntent::Branches(BranchesIntent::Next));
        let (tag, op) = op_of(&press_char(&mut state, ' '));
        assert_eq!(op, GitOp::Checkout("topic".into()));

        let effects = state.update(finished(tag, op, Ok(())));

        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::LoadStatus { .. })),
            "{effects:?}"
        );
        branches_tag(&effects);
    }

    #[test]
    fn checking_out_or_deleting_the_current_branch_is_refused() {
        let mut state = with_branches();
        assert_eq!(
            state.update(AppIntent::Branches(BranchesIntent::Checkout)),
            vec![]
        );
        assert_eq!(state.notice, Some("Already on this branch".into()));
        assert_eq!(
            state.update(AppIntent::Branches(BranchesIntent::Delete)),
            vec![]
        );
        assert_eq!(
            state.notice,
            Some("Cannot delete the current branch".into())
        );
        assert_eq!(state.popups, []);
        assert_eq!(state.held_seq(), None);
    }

    #[test]
    fn branch_actions_are_refused_while_an_op_runs_or_with_nothing_selected() {
        let mut state = with_branches();
        state.update(AppIntent::Branches(BranchesIntent::Next));
        state.hold_op(99);
        for intent in [
            AppIntent::Branches(BranchesIntent::Checkout),
            AppIntent::Branches(BranchesIntent::New),
            AppIntent::Branches(BranchesIntent::Rename),
            AppIntent::Branches(BranchesIntent::Delete),
        ] {
            state.notice = None;
            assert_eq!(state.update(intent.clone()), vec![], "{intent:?}");
            assert_eq!(
                state.notice,
                Some("An operation is running".into()),
                "{intent:?}"
            );
        }
        assert_eq!(state.popups, []);

        let mut empty = AppState::new(Context::Workspace);
        empty.update(AppIntent::Branches(BranchesIntent::Checkout));
        assert_eq!(empty.notice, Some("No branch selected".into()));
    }

    #[test]
    fn deleting_a_merged_branch_asks_and_y_deletes_without_force() {
        let mut state = with_branches();
        state.update(AppIntent::Branches(BranchesIntent::Next));
        press_char(&mut state, 'd');
        assert_eq!(state.key_focus(), KeyScope::DeleteBranch);
        // `D` answers only the question about an unmerged branch.
        state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('D'),
            KeyModifiers::SHIFT,
        )));
        assert_eq!(state.held_seq(), None);
        assert_eq!(state.popups.len(), 1);

        let (_, op) = op_of(&press_char(&mut state, 'y'));

        assert_eq!(
            op,
            GitOp::DeleteBranch {
                name: "topic".into(),
                force: false
            }
        );
        assert_eq!(state.popups, []);
    }

    #[test]
    fn deleting_an_unmerged_branch_ignores_y_and_d_forces() {
        let mut state = with_branches();
        state.update(AppIntent::Branches(BranchesIntent::Next));
        state.update(AppIntent::Branches(BranchesIntent::Next));
        press_char(&mut state, 'd');
        match state.popups.as_slice() {
            [Popup::Question(q)] => assert_eq!(q.answer, Answer::Force),
            other => panic!("expected the delete question, got {other:?}"),
        }

        assert_eq!(press_char(&mut state, 'y'), vec![]);
        assert_eq!(state.popups.len(), 1, "y leaves the question open");
        assert_eq!(state.held_seq(), None);

        let (_, op) = op_of(&state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('D'),
            KeyModifiers::SHIFT,
        ))));
        assert_eq!(
            op,
            GitOp::DeleteBranch {
                name: "wip".into(),
                force: true
            }
        );
    }

    #[test]
    fn n_or_esc_keeps_the_branch() {
        for key in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut state = with_branches();
            state.update(AppIntent::Branches(BranchesIntent::Next));
            press_char(&mut state, 'd');
            assert_eq!(press(&mut state, key), vec![]);
            assert_eq!(state.popups, []);
            assert!(!state.should_quit);
            assert_eq!(state.held_seq(), None);
        }
    }

    #[test]
    fn the_create_prompt_uses_the_branch_selected_when_it_opened() {
        let mut state = with_branches();
        state.update(AppIntent::Branches(BranchesIntent::Next));
        press_char(&mut state, 'n');
        assert_eq!(
            state.prompt_title(PromptKind::NewBranch),
            "New branch from topic"
        );
        // A reload while the user types moves the list under the prompt.
        let tag = branches_tag(&state.update(AppIntent::RefreshStatus));
        state.update(branches_loaded(tag, vec![git_branch("main", true, true)]));

        type_text(&mut state, "fresh");
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));

        assert_eq!(
            op,
            GitOp::CreateBranch {
                name: "fresh".into(),
                from: "topic".into()
            }
        );
        assert_eq!(state.branches.pending, None);
    }

    #[test]
    fn the_rename_prompt_starts_from_the_old_name_and_renames_that_branch() {
        let mut state = with_branches();
        state.update(AppIntent::Branches(BranchesIntent::Next));
        state.update(AppIntent::Branches(BranchesIntent::Next));
        press(&mut state, KeyCode::Char('R'));
        match state.popups.as_slice() {
            [Popup::Prompt { kind, input }] => {
                assert_eq!(*kind, PromptKind::RenameBranch);
                assert_eq!((input.text.as_str(), input.col), ("wip", 3));
            }
            other => panic!("expected the rename prompt, got {other:?}"),
        }
        let tag = branches_tag(&state.update(AppIntent::RefreshStatus));
        state.update(branches_loaded(tag, three_branches()[..2].to_vec()));

        type_text(&mut state, "-2");
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));

        assert_eq!(
            op,
            GitOp::RenameBranch {
                from: "wip".into(),
                to: "wip-2".into()
            }
        );
    }

    #[test]
    fn an_empty_name_is_refused_and_a_cancelled_prompt_forgets_the_branch() {
        let mut state = with_branches();
        press_char(&mut state, 'n');
        type_text(&mut state, "  ");
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice, Some("A branch needs a name".into()));
        assert_eq!(state.held_seq(), None);

        press_char(&mut state, 'n');
        assert!(state.branches.pending.is_some());
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.branches.pending, None);
        assert_eq!(
            state.update(AppIntent::Branches(BranchesIntent::CreateNamed("x".into()))),
            vec![]
        );
    }

    #[test]
    fn switching_repository_resets_the_panel() {
        let mut state = with_branches().with_location(Some("/r/a".into()), "/r/a".into());
        state.branches.tab = BranchesTab::Remotes;
        let effects = state.update(AppIntent::OpenRepo("/r/b".into()));
        assert!(!state.branches.list.loaded);
        assert_eq!(state.branches.list.rows, []);
        assert_eq!(state.branches.log, Default::default());
        assert_eq!(state.branches.tab, BranchesTab::Local);
        assert!(!state.branches.remotes.list.loaded);
        assert!(
            effects.iter().any(
                |e| matches!(e, Effect::LoadRemotes { tag } if tag.generation == state.generation)
            ),
            "{effects:?}"
        );
    }

    #[test]
    fn set_upstream_starts_from_the_upstream_or_origin_and_needs_a_listed_branch() {
        let mut state = with_remotes();
        state.update(AppIntent::Branches(BranchesIntent::Next));
        press_char(&mut state, 'u');
        match state.popups.as_slice() {
            [Popup::Prompt { kind, input }] => {
                assert_eq!(*kind, PromptKind::SetUpstream);
                assert_eq!(input.text, "origin/topic");
            }
            other => panic!("expected the upstream prompt, got {other:?}"),
        }
        assert_eq!(
            state.prompt_title(PromptKind::SetUpstream),
            "Upstream of topic"
        );
        // A name no fetch has listed only says so.
        type_text(&mut state, "x");
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(
            state.notice,
            Some("No such remote branch; fetch, or check the name".into())
        );
        assert_eq!(state.branches.pending, None);

        press_char(&mut state, 'u');
        for _ in 0.."origin/topic".len() {
            press(&mut state, KeyCode::Backspace);
        }
        type_text(&mut state, "origin/main");
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
        assert_eq!(
            op,
            GitOp::SetUpstream {
                branch: "topic".into(),
                upstream: three_remotes()[1].branches[0].clone(),
            }
        );

        // An upstream already set is where the prompt starts.
        let mut state = with_remotes();
        state.branches.list.rows[0].upstream = Some("fork/x".into());
        press_char(&mut state, 'u');
        assert!(matches!(
            state.popups.as_slice(),
            [Popup::Prompt { input, .. }] if input.text == "fork/x"
        ));
    }

    #[test]
    fn unset_upstream_needs_an_upstream() {
        let mut state = with_remotes();
        assert_eq!(shift(&mut state, 'U'), vec![]);
        assert_eq!(state.notice, Some("No upstream to unset".into()));
        state.branches.list.rows[0].upstream = Some("origin/main".into());
        let (_, op) = op_of(&shift(&mut state, 'U'));
        assert_eq!(op, GitOp::UnsetUpstream("main".into()));
    }

    #[test]
    fn a_failed_delete_says_the_branch_list_was_reloaded() {
        let mut state = with_branches();
        state.update(AppIntent::Branches(BranchesIntent::Next));
        press_char(&mut state, 'd');
        let (tag, op) = op_of(&press_char(&mut state, 'y'));
        state.update(finished(tag, op, Err("error: not fully merged".into())));
        match state.popups.as_slice() {
            [Popup::Error(error)] => {
                assert_eq!(error.title, "Could not delete the branch");
                assert!(error.next.starts_with("The branch list was reloaded"));
            }
            other => panic!("expected an error popup, got {other:?}"),
        }
    }

    #[test]
    fn a_finished_delete_reloads_only_the_list() {
        let mut state = with_branches();
        state.update(AppIntent::Branches(BranchesIntent::Next));
        press_char(&mut state, 'd');
        let (tag, op) = op_of(&press_char(&mut state, 'y'));
        let effects = state.update(finished(tag, op, Ok(())));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::LoadStatus { .. })),
            "a delete changes no files: {effects:?}"
        );
        branches_tag(&effects);
    }

    #[test]
    fn closing_a_branch_prompt_forgets_its_branch() {
        use crate::popup::LineInput;
        for kind in [
            PromptKind::NewBranch,
            PromptKind::RenameBranch,
            PromptKind::SetUpstream,
        ] {
            let mut state = AppState::new(Context::Workspace);
            state.branches.pending = Some("topic".into());
            state.popups.push(Popup::Prompt {
                kind,
                input: LineInput::default(),
            });
            assert_eq!(state.update(AppIntent::PopPopup), vec![], "{kind:?}");
            assert_eq!(state.popups, [], "{kind:?}");
            assert_eq!(state.branches.pending, None, "{kind:?}");
        }
    }

    /// `detached_on_commit`, with `HEAD` on a full id the log has not
    /// loaded, so the prompt shortens it itself.
    fn detached_on_hex() -> (AppState, kika_git::commit_files::CommitId) {
        let mut state = detached_on_commit();
        let hex = "fedcba9".repeat(6)[..40].to_string();
        state.branch.as_mut().unwrap().head = kika_git::status::Head::Detached(hex.clone());
        (
            state,
            kika_git::commit_files::CommitId::parse(&hex).unwrap(),
        )
    }

    #[test]
    fn shift_n_on_a_detached_head_makes_a_branch_there() {
        let (mut state, head) = detached_on_hex();
        press(&mut state, KeyCode::Char('N'));
        assert!(matches!(
            state.popups.as_slice(),
            [Popup::Prompt {
                kind: PromptKind::NewBranchAtHead,
                ..
            }]
        ));
        assert_eq!(state.branches.pending_head, Some(head.clone()));
        assert_eq!(
            state.prompt_title(PromptKind::NewBranchAtHead),
            "New branch at fedcba9"
        );

        type_text(&mut state, " topic ");
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));

        assert_eq!(
            op,
            GitOp::CreateBranchAt {
                name: "topic".into(),
                at: head
            }
        );
        assert_eq!(state.branches.pending_head, None);
        assert_eq!(state.popups, []);
    }

    #[test]
    fn a_branch_at_head_runs_nothing_once_head_has_moved() {
        // Detached elsewhere, then onto a branch.
        let moved = [
            kika_git::status::Head::Detached("c2".into()),
            on_branch("main").head,
        ];
        for head in moved {
            let (mut state, _) = detached_on_hex();
            press(&mut state, KeyCode::Char('N'));
            type_text(&mut state, "topic");
            state.branch.as_mut().unwrap().head = head.clone();
            assert_eq!(press(&mut state, KeyCode::Enter), vec![], "{head:?}");
            let notice = state.notice.as_deref();
            assert_eq!(notice, Some("HEAD moved; try again"), "{head:?}");
            assert_eq!(state.held_seq(), None, "{head:?}");
            assert_eq!(state.branches.pending_head, None, "{head:?}");
        }
    }

    #[test]
    fn a_branch_at_head_needs_a_name_and_a_free_op_slot() {
        let (mut state, _) = detached_on_hex();
        press(&mut state, KeyCode::Char('N'));
        type_text(&mut state, "  ");
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice.as_deref(), Some("A branch needs a name"));
        assert_eq!(state.branches.pending_head, None);

        press(&mut state, KeyCode::Char('N'));
        type_text(&mut state, "topic");
        state.hold_op(7);
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice.as_deref(), Some(crate::keymap::OP_RUNNING));
        assert_eq!(state.held_seq(), Some(7));
    }

    #[test]
    fn a_bisect_begun_while_the_prompt_was_open_refuses_the_branch() {
        let (mut state, _) = detached_on_hex();
        press(&mut state, KeyCode::Char('N'));
        type_text(&mut state, "topic");
        let branch = state.branch.as_mut().unwrap();
        branch
            .in_progress
            .push(kika_git::repo_state::Operation::Bisect);
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.held_seq(), None);
        assert!(state.notice.is_some());
        assert_eq!(state.branches.pending_head, None);
    }

    #[test]
    fn esc_closes_the_branch_at_head_prompt_and_forgets_head() {
        let (mut state, _) = detached_on_hex();
        press(&mut state, KeyCode::Char('N'));
        assert!(state.branches.pending_head.is_some());
        assert_eq!(press(&mut state, KeyCode::Esc), vec![]);
        assert_eq!(state.popups, []);
        assert_eq!(state.branches.pending_head, None);
    }

    /// The one popup in `state`: a leave-commits question, as its text.
    fn leave_question(state: &AppState) -> String {
        assert_eq!(state.key_focus(), KeyScope::LeaveCommits);
        match state.popups.as_slice() {
            [Popup::Question(ask)] => {
                assert_eq!(ask.title, "Leave commits behind");
                ask.text.clone()
            }
            other => panic!("expected the leave-commits question, got {other:?}"),
        }
    }

    /// `detached_on_branch_list` after `space` on `main` and its check
    /// came back with `result`.
    fn checkout_checked(
        result: Result<Vec<kika_git::commit_files::CommitId>, String>,
    ) -> (AppState, Vec<Effect>) {
        let mut state = detached_on_branch_list();
        let (tag, op) = check_to(&press_char(&mut state, ' '), None);
        let effects = state.update(left_behind_checked(tag, op, result));
        (state, effects)
    }

    #[test]
    fn from_a_detached_head_checking_out_a_branch_checks_for_commits_left_behind() {
        let mut state = detached_on_branch_list();
        let busy = state.busy_count();
        let effects = state.update(AppIntent::Branches(BranchesIntent::Checkout));
        let (tag, op) = check_to(&effects, None);
        assert_eq!(op, GitOp::Checkout("main".into()));
        assert!(tag.user);
        assert_eq!(state.busy_count(), busy + 1);
        assert_eq!(state.held_seq(), None);
        assert_eq!(state.popups, []);
    }

    #[test]
    fn commits_left_behind_ask_and_only_y_checks_out_the_branch() {
        let (state, effects) = checkout_checked(Ok(ids(2)));
        assert_eq!(effects, vec![]);
        assert_eq!(state.held_seq(), None);
        let text = leave_question(&state);
        assert!(
            text.starts_with(
                "Check out main and leave 2 commits that no branch or tag reaches behind (a0, a1)?"
            ),
            "{text}"
        );

        let mut yes = state.clone();
        let (tag, op) = op_of(&press_char(&mut yes, 'y'));
        assert_eq!(op, GitOp::Checkout("main".into()));
        assert_eq!(yes.held_seq(), Some(tag.seq));
        assert_eq!(yes.popups, []);

        for code in [KeyCode::Enter, KeyCode::Char('n'), KeyCode::Esc] {
            let mut no = state.clone();
            assert_eq!(press(&mut no, code), vec![], "{code:?}");
            assert_eq!(no.held_seq(), None, "{code:?}");
            assert_eq!(no.branch, state.branch, "{code:?}: HEAD stays");
            if code == KeyCode::Enter {
                // Enter is no answer: the question stays open.
                assert_eq!(no.popups, state.popups);
            } else {
                assert_eq!(no.popups, [], "{code:?}");
            }
        }
    }

    #[test]
    fn with_nothing_left_behind_the_branch_checkout_runs_at_once() {
        let (state, effects) = checkout_checked(Ok(vec![]));
        let (tag, op) = op_of(&effects);
        assert_eq!(op, GitOp::Checkout("main".into()));
        assert_eq!(state.held_seq(), Some(tag.seq));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn a_failed_check_shows_why_and_checks_out_nothing() {
        let (state, effects) = checkout_checked(Err("fatal: bad".into()));
        assert_eq!(effects, vec![]);
        assert_eq!(state.held_seq(), None);
        let [Popup::Error(error)] = state.popups.as_slice() else {
            panic!("{:?}", state.popups)
        };
        assert_eq!(error.title, "Could not check for commits left behind");
        assert!(error.message.contains("fatal: bad"));
    }

    #[test]
    fn y_checks_out_nothing_once_head_has_moved() {
        let (state, _) = checkout_checked(Ok(ids(1)));
        for head in [
            kika_git::status::Head::Detached("c2".into()),
            kika_git::status::Head::Branch("topic".into()),
        ] {
            let mut moved = state.clone();
            moved.branch.as_mut().unwrap().head = head.clone();
            assert_eq!(press_char(&mut moved, 'y'), vec![], "{head:?}");
            assert_eq!(moved.held_seq(), None, "{head:?}");
            assert_eq!(moved.popups, [], "{head:?}");
            assert_eq!(moved.notice, Some("HEAD moved; try again".into()));
        }
    }

    #[test]
    fn a_bisect_started_during_the_check_refuses_the_branch_checkout() {
        for result in [Ok(ids(2)), Ok(vec![])] {
            let mut state = detached_on_branch_list();
            let (tag, op) = check_to(&press_char(&mut state, ' '), None);
            start_bisecting(&mut state);
            let effects = state.update(left_behind_checked(tag, op, result.clone()));
            assert_eq!(effects, vec![], "{result:?}");
            assert_eq!(state.popups, [], "{result:?}");
            assert_eq!(state.held_seq(), None, "{result:?}");
            assert_eq!(
                state.notice,
                Some(crate::bisect::RESET_FIRST.into()),
                "{result:?}"
            );
        }
    }

    #[test]
    fn from_a_detached_head_creating_a_branch_asks_before_leaving_commits() {
        let mut state = detached_on_branch_list();
        press_char(&mut state, 'n');
        type_text(&mut state, "fresh");
        let (tag, op) = check_to(&press(&mut state, KeyCode::Enter), None);
        let create = GitOp::CreateBranch {
            name: "fresh".into(),
            from: "main".into(),
        };
        assert_eq!(op, create);
        assert_eq!(state.branches.pending, None);

        state.update(left_behind_checked(tag, op, Ok(ids(1))));
        let text = leave_question(&state);
        assert!(
            text.starts_with(
                "Check out fresh and leave 1 commit that no branch or tag reaches behind (a0)?"
            ),
            "{text}"
        );
        for code in [KeyCode::Enter, KeyCode::Char('n'), KeyCode::Esc] {
            let mut no = state.clone();
            assert_eq!(press(&mut no, code), vec![], "{code:?}");
            assert_eq!(no.held_seq(), None, "{code:?}");
        }
        let (_, op) = op_of(&press_char(&mut state, 'y'));
        assert_eq!(op, create);

        // Nothing left behind: the branch is made at once.
        let mut state = detached_on_branch_list();
        press_char(&mut state, 'n');
        type_text(&mut state, "fresh");
        let (tag, op) = check_to(&press(&mut state, KeyCode::Enter), None);
        let (_, op) = op_of(&state.update(left_behind_checked(tag, op, Ok(vec![]))));
        assert_eq!(op, create);
    }

    #[test]
    fn on_a_branch_creating_one_runs_with_no_check() {
        let mut state = with_branches();
        state.branch = Some(on_branch("main"));
        press_char(&mut state, 'n');
        type_text(&mut state, "fresh");
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
        assert!(matches!(op, GitOp::CreateBranch { .. }), "{op:?}");
    }

    #[test]
    fn on_a_branch_checking_out_another_runs_with_no_check() {
        let mut state = with_branches();
        state.branch = Some(on_branch("main"));
        state.update(AppIntent::Branches(BranchesIntent::Next));
        let (_, op) = op_of(&press_char(&mut state, ' '));
        assert_eq!(op, GitOp::Checkout("topic".into()));
    }

    // Switching worktrees needs no check: it points Kika at another
    // worktree and moves no `HEAD`, so nothing is left behind.
}
