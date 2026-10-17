//! The custom patch's side of `AppState`: picking a commit's files and
//! lines into it, the menu of rewrites and their questions, and what a
//! finished rewrite leaves behind.

use std::collections::BTreeSet;

use super::CustomPatchIntent;
use super::keys;
use super::state::{CustomPatch, FilePick, PatchAction, PatchChoice};
use crate::confirm_question::{Answer, ConfirmQuestion, Then};
use crate::effect::Effect;
use crate::features::commits::CommitsIntent;
use crate::hunk_view::HunkView;
use crate::intent::AppIntent;
use crate::keymap::{self, KeyScope};
use crate::layout::SidePanel;
use crate::menu;
use crate::model::{AppState, Context, Focus};
use crate::op::GitOp;
use crate::popup::{LineInput, Popup, PromptKind};
use crate::view::elide_right;
use kika_git::commit_files::CommitId;
use kika_git::patch::{FileKind, LineId, PatchError};
use kika_git::safe_text::terminal_safe;

/// The custom patch's half of `update`, which `commits::update` passes
/// `CommitsIntent::CustomPatch` to. A free function, so its name cannot
/// clash with handlers.
pub(crate) fn update(state: &mut AppState, intent: CustomPatchIntent) -> Vec<Effect> {
    match intent {
        CustomPatchIntent::ToggleFile => state.patch_toggle_file(),
        CustomPatchIntent::OpenMenu => {
            if state.context == Context::Workspace {
                state.popups.push(Popup::Menu {
                    id: menu::MenuId::CustomPatch,
                    cursor: 0,
                });
            }
        }
        CustomPatchIntent::Choose(choice) => state.choose_patch_action(choice),
        CustomPatchIntent::CommitMessage(message) => state.patch_commit_message(message),
        CustomPatchIntent::Confirm(action) => return state.run_patch_action(action),
        CustomPatchIntent::Reset => {
            if state.custom_patch.take().is_none() {
                state.notice = Some(NO_PATCH.into());
            }
        }
    }
    Vec::new()
}

/// Why resetting does nothing.
const NO_PATCH: &str = "No custom patch";

/// How much of a new commit's message the question shows.
const MESSAGE_SHOWN: usize = 30;

/// Why "New commit after source" asks again.
pub(crate) const MESSAGE_NEEDED: &str = "Type a message for the new commit";

impl AppState {
    /// Focus the Diff pane on the open commit's selected file, its lines
    /// to be picked. Only one commit's change: not a comparison, a first
    /// commit or a merge.
    pub(crate) fn focus_commit_diff(&mut self) -> Vec<Effect> {
        let Some(open) = &self.commits.open else {
            return vec![];
        };
        if let Some(reason) =
            keymap::list_guard(self, false).or_else(|| keymap::commit_source_disabled(self, open))
        {
            self.notice = Some(reason);
            return vec![];
        }
        let Some(file) = open.selected_file() else {
            return vec![];
        };
        self.focus_hunk_view(HunkView::for_commit(file.path.clone(), open.range.clone()));
        let effects = self.load_patch();
        self.remap();
        effects
    }

    /// Pick the selected commit file whole, or drop it.
    pub(crate) fn patch_toggle_file(&mut self) {
        if self.context != Context::Workspace || self.focus != Focus::Commits {
            return;
        }
        if let Some(reason) = keys::patch_file_disabled(self) {
            self.notice = Some(reason);
            return;
        }
        let Some(open) = &self.commits.open else {
            return;
        };
        let Some(file) = open.selected_file() else {
            return;
        };
        let path = file.path.clone();
        let mut patch = self
            .custom_patch
            .take()
            .unwrap_or_else(|| CustomPatch::new(open.range.clone(), open.short_oid.clone()));
        patch.toggle_file(&path);
        self.custom_patch = (!patch.is_empty()).then_some(patch);
    }

    /// Pick `lines` of the focused commit file, or drop them when all are
    /// picked. Nothing reaches git.
    pub(crate) fn patch_toggle_lines(&mut self, lines: BTreeSet<LineId>) {
        let Some(view) = self.hunk_view() else {
            return;
        };
        let Some(range) = view.commit.clone() else {
            return;
        };
        if self.display_only() {
            self.notice = Some(keymap::DISPLAY_ONLY.into());
            return;
        }
        if self.diff_options.ignore_whitespace {
            self.notice = Some(keymap::WHITESPACE_HIDDEN.into());
            return;
        }
        let Some(file) = &view.patch else {
            return;
        };
        let refused = match file.kind {
            FileKind::Binary => Some(PatchError::Binary),
            FileKind::Renamed => Some(PatchError::Renamed),
            FileKind::Copied => Some(PatchError::Copied),
            FileKind::Unsupported => Some(PatchError::Unsupported),
            FileKind::Modified | FileKind::Added | FileKind::Deleted => None,
        };
        if let Some(error) = refused {
            self.notice = Some(error.in_commit_message().into());
            return;
        }
        if lines.is_empty() {
            return;
        }
        if let Some(reason) = keys::other_commit_disabled(self, &range) {
            self.notice = Some(reason);
            return;
        }
        let all = view.all_lines();
        let path = view.path.clone();
        let kind = file.kind;
        let short = self.commits.short_oid(&range.oid);
        let mut patch = self
            .custom_patch
            .clone()
            .unwrap_or_else(|| CustomPatch::new(range, short));
        patch.expand(&path, &all);
        patch.toggle_lines(&path, &lines);
        // A new or deleted file only moves whole.
        let partial =
            matches!(patch.file_state(&path), Some(FilePick::Lines(picked)) if picked != all);
        let refused = match kind {
            FileKind::Added if partial => Some(PatchError::PartialNewFile),
            FileKind::Deleted if partial => Some(PatchError::PartialDeletedFile),
            _ => None,
        };
        if let Some(error) = refused {
            self.notice = Some(error.message().into());
            return;
        }
        self.custom_patch = (!patch.is_empty()).then_some(patch);
    }

    /// A rewrite chosen from the patch menu: ask for the new commit's
    /// message first, else ask whether to go ahead.
    pub(crate) fn choose_patch_action(&mut self, choice: PatchChoice) {
        if self.context != Context::Workspace {
            return;
        }
        if let Some(reason) = keys::patch_rewrite_disabled(self, choice) {
            self.notice = Some(reason);
            return;
        }
        match keys::patch_action(self, choice) {
            Ok(PatchAction::NewCommit(_)) => self.ask_patch_message(),
            Ok(action) => {
                let question = self.patch_action_question(action);
                self.popups.push(Popup::Question(question));
            }
            Err(reason) => self.notice = Some(reason),
        }
    }

    fn ask_patch_message(&mut self) {
        self.popups.push(Popup::Prompt {
            kind: PromptKind::PatchCommitMessage,
            input: LineInput::default(),
        });
    }

    /// The new commit's message, typed: an empty one asks again.
    pub(crate) fn patch_commit_message(&mut self, message: String) {
        if self.context != Context::Workspace {
            return;
        }
        if message.trim().is_empty() {
            self.notice = Some(MESSAGE_NEEDED.into());
            self.ask_patch_message();
            return;
        }
        if let Some(reason) = keys::patch_rewrite_disabled(self, PatchChoice::NewCommit) {
            self.notice = Some(reason);
            return;
        }
        let action = PatchAction::NewCommit(message);
        let question = self.patch_action_question(action);
        self.popups.push(Popup::Question(question));
    }

    /// Run `action`, as the question asked. The plan is made again from
    /// the log as it is now, so a reload while asking is safe; `HEAD` is
    /// checked again just before git starts.
    pub(crate) fn run_patch_action(&mut self, action: PatchAction) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if matches!(&action, PatchAction::NewCommit(message) if message.trim().is_empty()) {
            self.notice = Some(MESSAGE_NEEDED.into());
            return vec![];
        }
        let plan = match keys::patch_action_disabled(self, &action) {
            Some(reason) => Err(reason),
            None => keys::patch_plan(self, &action),
        };
        let head = self
            .commits
            .list
            .rows
            .first()
            .and_then(|row| CommitId::parse(&row.oid));
        match (plan, self.custom_patch.clone(), head) {
            (Ok(plan), Some(picks), Some(orig_head)) => {
                self.run_op_where_it_belongs(GitOp::CustomPatch {
                    plan,
                    action,
                    picks,
                    orig_head,
                })
            }
            (Err(reason), ..) => {
                self.notice = Some(reason);
                vec![]
            }
            _ => {
                self.notice = Some(keys::PATCH_EMPTY.into());
                vec![]
            }
        }
    }

    /// The patch is in history now: drop it, and the source commit's
    /// files, whose commit was rewritten.
    pub(crate) fn patch_rewritten(&mut self) -> Vec<Effect> {
        self.custom_patch = None;
        if self.hunk_view().is_some_and(|view| view.commit.is_some()) {
            self.focus = Focus::Commits;
            self.expanded = SidePanel::Commits;
        }
        self.close_commit_files()
    }

    /// "Rewrite the history with the custom patch?", carrying `action`
    /// with its target or message fixed when asked. It rewrites commits
    /// the user may not be looking at, so only `y` answers.
    pub(crate) fn patch_action_question(&self, action: PatchAction) -> ConfirmQuestion {
        let text = self.patch_question(&action).unwrap_or_else(|| {
            // The fallback when the patch cannot be named.
            "Rewrite the history with the custom patch? (y/n)".into()
        });
        ConfirmQuestion {
            scope: KeyScope::PatchConfirm,
            title: "Custom patch".into(),
            text,
            answer: Answer::YOnly,
            then: Then::Intent(Box::new(AppIntent::Commits(CommitsIntent::CustomPatch(
                CustomPatchIntent::Confirm(action),
            )))),
            range: false,
            head: None,
        }
    }

    /// The question for `action`: what moves from where to where.
    fn patch_question(&self, action: &PatchAction) -> Option<String> {
        let patch = self.custom_patch.as_ref()?;
        let n = patch.files.len();
        let what = format!(
            "the custom patch ({n} {})",
            if n == 1 { "file" } else { "files" }
        );
        let source = &patch.short_oid;
        let question = match action {
            PatchAction::Remove => format!("Take {what} out of {source}?"),
            PatchAction::MoveTo(target) => {
                let target = self.commits.short_oid(target);
                format!("Move {what} from {source} into {target}?")
            }
            PatchAction::ToIndex => {
                format!("Take {what} out of {source} and stage it?")
            }
            PatchAction::NewCommit(message) => {
                // Cut so the question never grows past a small screen.
                let message = elide_right(&terminal_safe(message.as_bytes()), MESSAGE_SHOWN);
                format!("Take {what} out of {source} into a new commit after it, \"{message}\"?")
            }
        };
        // The answer keys first, so a screen too small for the whole box
        // still shows how to answer.
        Some(format!(
            "{question} (y/n)\nThe commits from there up are rewritten. If any step \
             fails, everything is put back."
        ))
    }
}

/// The custom patch: picking from a commit, and the menu of rewrites.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bisect;
    use crate::effect::Tag;
    use crate::handoff::HandOff;
    use crate::intent::AppIntent;
    use crate::keymap::{KeyPress, KeyScope, Keymap};
    use crate::menu::{self, MenuId};
    use crate::test_support::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use kika_git::commit_files::CommitFile;
    use kika_git::log::{CommitRow, LogPage};
    use kika_git::status::EntryKind;

    /// Row `i`'s full id.
    fn oid(i: usize) -> String {
        format!("{:040x}", i + 1)
    }

    fn id(i: usize) -> CommitId {
        CommitId::parse(&oid(i)).unwrap()
    }

    /// Four commits on `main`, row 0 `HEAD`, none of them a branch's
    /// other than `main`; an untracked file only; the Commits panel
    /// focused on row `selected`.
    fn history(selected: usize) -> AppState {
        let (mut state, _) = with_files(vec![untracked("u.txt")], 24);
        let rows = (0..4)
            .map(|i| CommitRow {
                oid: oid(i),
                short_oid: oid(i)[..7].to_string(),
                parents: vec![oid(i + 1)],
                author: "Ann".into(),
                time: 0,
                subject: format!("commit {i}"),
                subject_altered: false,
            })
            .collect();
        state.commits.begin_page(900, 0);
        state.commits.page_loaded(
            Tag {
                generation: state.generation,
                seq: 900,
                user: false,
            },
            state.generation,
            0,
            Ok((LogPage { rows, done: true }, 1_000)),
        );
        state.commits.tips = Some(vec![]);
        state.focus = Focus::Commits;
        state.expanded = SidePanel::Commits;
        state.commits.list.selected = selected;
        state
    }

    /// Open row `row`'s files, listing `a.rs` and `b.rs`.
    fn open_files(state: &mut AppState, row: usize) {
        state.commits.list.selected = row;
        let effects = press(state, KeyCode::Enter);
        let tag = match effects.as_slice() {
            [Effect::LoadCommitFiles { tag, .. }] => *tag,
            other => panic!("{other:?}"),
        };
        let file = |path: &str| CommitFile {
            status: 'M',
            path: path.into(),
            orig_path: None,
        };
        state.update(AppIntent::Commits(CommitsIntent::FilesLoaded {
            tag,
            result: Ok(vec![file("a.rs"), file("b.rs")]),
        }));
    }

    /// Row 2's `a.rs` in the Diff pane, its patch `TWO_HUNKS`.
    fn picking() -> AppState {
        let mut state = history(0);
        open_files(&mut state, 2);
        let (tag, request) = patch_load(&press(&mut state, KeyCode::Char('l')));
        assert_eq!(request.commit.as_ref().map(|r| r.oid.clone()), Some(id(2)));
        state.update(patch_loaded(tag, patch_of(TWO_HUNKS)));
        state
    }

    fn lines(ids: &[LineId]) -> Option<FilePick> {
        Some(FilePick::Lines(ids.iter().copied().collect()))
    }

    fn picked(state: &AppState, path: &str) -> Option<FilePick> {
        state.custom_patch.as_ref()?.file_state(&path.into())
    }

    #[test]
    fn space_in_a_commits_hunk_picks_its_lines_and_never_stages() {
        let mut state = picking();
        assert_eq!(state.key_focus(), KeyScope::Diff);

        assert_eq!(press(&mut state, KeyCode::Char(' ')), vec![]);
        assert_eq!(
            picked(&state, "a.rs"),
            lines(&[LineId::removed(1), LineId::added(1), LineId::added(2)])
        );
        assert_eq!(state.custom_patch.as_ref().unwrap().source.oid, id(2));
        assert_eq!(state.held_seq(), None);

        assert_eq!(press(&mut state, KeyCode::Char(' ')), vec![]);
        assert_eq!(state.custom_patch, None);
    }

    #[test]
    fn space_in_line_mode_picks_only_the_range() {
        let mut state = picking();
        press(&mut state, KeyCode::Char('v'));
        assert_eq!(state.key_focus(), KeyScope::Lines);
        assert_eq!(press(&mut state, KeyCode::Char(' ')), vec![]);
        assert_eq!(picked(&state, "a.rs"), lines(&[LineId::removed(1)]));
        // The next line joins it; the hunk's other line stays out.
        press(&mut state, KeyCode::Char('j'));
        press(&mut state, KeyCode::Char(' '));
        assert_eq!(
            picked(&state, "a.rs"),
            lines(&[LineId::removed(1), LineId::added(1)])
        );
        assert_eq!(state.held_seq(), None);
    }

    #[test]
    fn the_working_tree_keys_say_not_in_a_commits_change() {
        let mut state = picking();
        for c in ['t', 'd', 'e'] {
            assert_eq!(
                key_reason(&state, KeyScope::Diff, c).as_deref(),
                Some(keymap::NOT_IN_COMMIT),
                "{c}"
            );
        }
        assert_eq!(key_reason(&state, KeyScope::Diff, ' '), None);
        press(&mut state, KeyCode::Char('v'));
        assert_eq!(
            key_reason(&state, KeyScope::Lines, 'd').as_deref(),
            Some(keymap::NOT_IN_COMMIT)
        );
        // Sent as intents, they do nothing either.
        for intent in [
            toggle_diff_side_intent(),
            AppIntent::Discard,
            edit_hunk_intent(),
        ] {
            state.notice = None;
            assert_eq!(state.update(intent.clone()), vec![], "{intent:?}");
            assert!(state.popups.is_empty(), "{intent:?}");
            assert_eq!(state.notice.as_deref(), Some(keymap::NOT_IN_COMMIT));
        }
    }

    #[test]
    fn a_renamed_or_binary_file_in_a_commits_change_names_the_custom_patch() {
        for (bytes, kind, text) in [
            (
                TWO_HUNKS,
                Some(FileKind::Renamed),
                "Renamed file: it cannot go into the custom patch",
            ),
            (
                BINARY,
                None,
                "Binary file: it cannot go into the custom patch",
            ),
        ] {
            let mut state = history(0);
            open_files(&mut state, 2);
            let (tag, _) = patch_load(&press(&mut state, KeyCode::Char('l')));
            let mut patch = patch_of(bytes);
            if let Some(kind) = kind {
                patch.kind = kind;
            }
            state.update(patch_loaded(tag, patch));

            state.patch_toggle_lines([LineId::added(1)].into());

            assert_eq!(state.notice.as_deref(), Some(text));
            assert_eq!(state.custom_patch, None);
        }
    }

    #[test]
    fn a_renamed_commit_file_cannot_be_picked_whole_and_says_custom_patch() {
        let mut state = history(0);
        state.commits.list.selected = 2;
        let tag = match press(&mut state, KeyCode::Enter).as_slice() {
            [Effect::LoadCommitFiles { tag, .. }] => *tag,
            other => panic!("{other:?}"),
        };
        state.update(AppIntent::Commits(CommitsIntent::FilesLoaded {
            tag,
            result: Ok(vec![CommitFile {
                status: 'R',
                path: "new.rs".into(),
                orig_path: Some("old.rs".into()),
            }]),
        }));

        assert_eq!(
            keys::patch_file_disabled(&state).as_deref(),
            Some("Renamed file: it cannot go into the custom patch")
        );
    }

    #[test]
    fn space_in_the_commit_files_picks_the_whole_file_and_drops_it() {
        let mut state = history(0);
        open_files(&mut state, 2);
        assert_eq!(state.key_focus(), KeyScope::CommitFiles);
        assert_eq!(press(&mut state, KeyCode::Char(' ')), vec![]);
        assert_eq!(picked(&state, "a.rs"), Some(FilePick::Whole));
        press(&mut state, KeyCode::Char(' '));
        assert_eq!(state.custom_patch, None);
    }

    #[test]
    fn picking_from_a_second_commit_leaves_the_patch_and_says_reset_it_first() {
        let mut state = history(0);
        open_files(&mut state, 2);
        press(&mut state, KeyCode::Char(' '));
        let before = state.custom_patch.clone();
        press(&mut state, KeyCode::Esc);
        open_files(&mut state, 1);

        press(&mut state, KeyCode::Char(' '));

        assert_eq!(state.custom_patch, before);
        let short = &oid(2)[..7];
        assert_eq!(
            state.notice.as_deref(),
            Some(format!("The custom patch holds changes from {short}; reset it first").as_str())
        );
    }

    #[test]
    fn the_patch_menu_has_five_items_and_reset_drops_the_patch() {
        let mut state = history(0);
        open_files(&mut state, 2);
        press(&mut state, KeyCode::Char(' '));
        state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(
            state.popups,
            [Popup::Menu {
                id: MenuId::CustomPatch,
                cursor: 0
            }]
        );
        let labels: Vec<_> = menu::items(MenuId::CustomPatch)
            .iter()
            .map(|item| item.label)
            .collect();
        assert_eq!(
            labels,
            [
                "Remove from commit",
                "Move to selected commit",
                "Move to index",
                "New commit after source…",
                "Reset patch"
            ]
        );
        if let Some(Popup::Menu { cursor, .. }) = state.popups.last_mut() {
            *cursor = 4;
        }
        assert_eq!(state.update(AppIntent::MenuSelect), vec![]);
        assert_eq!(state.custom_patch, None);
        assert!(state.popups.is_empty());
    }

    /// Row 2's `a.rs` picked whole, files closed, the cursor on `row`.
    fn with_patch(row: usize) -> AppState {
        let mut state = history(0);
        open_files(&mut state, 2);
        press(&mut state, KeyCode::Char(' '));
        press(&mut state, KeyCode::Esc);
        state.commits.list.selected = row;
        state
    }

    fn the_op(effects: &[Effect]) -> GitOp {
        match effects {
            [Effect::HandOff(HandOff::Attached { op, .. })] => op.clone(),
            other => panic!("expected one attached op, got {other:?}"),
        }
    }

    #[test]
    fn a_rewrite_asks_and_only_y_runs_it() {
        for (choice, action, row) in [
            (PatchChoice::Remove, PatchAction::Remove, 0),
            (PatchChoice::ToIndex, PatchAction::ToIndex, 0),
            (PatchChoice::MoveToSelected, PatchAction::MoveTo(id(0)), 0),
            (PatchChoice::MoveToSelected, PatchAction::MoveTo(id(3)), 3),
        ] {
            let mut state = with_patch(row);
            state.update(AppIntent::Commits(CommitsIntent::CustomPatch(
                CustomPatchIntent::Choose(choice),
            )));
            assert_eq!(
                state.popups,
                [Popup::Question(state.patch_action_question(action.clone()))]
            );
            assert_eq!(state.key_focus(), KeyScope::PatchConfirm);
            // Enter answers nothing.
            assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
            assert_eq!(state.popups.len(), 1);

            let op = the_op(&press(&mut state, KeyCode::Char('y')));

            let GitOp::CustomPatch {
                plan,
                action: ran,
                picks,
                orig_head,
            } = op
            else {
                panic!("{op:?}");
            };
            assert_eq!(ran, action);
            assert_eq!(orig_head, id(0));
            assert_eq!(Some(picks), state.custom_patch.clone());
            let edits: Vec<_> = plan
                .steps
                .iter()
                .filter(|s| s.action == kika_git::rebase_plan::TodoAction::Edit)
                .map(|s| s.commit.clone())
                .collect();
            let want = match &action {
                PatchAction::MoveTo(target) if *target == id(3) => vec![id(3)],
                PatchAction::MoveTo(target) => vec![id(2), target.clone()],
                PatchAction::ToIndex => vec![id(2), id(0)],
                _ => vec![id(2)],
            };
            assert_eq!(edits, want, "{action:?}");
        }
    }

    #[test]
    fn during_a_bisect_no_patch_rewrite_asks_or_runs() {
        for choice in [
            PatchChoice::Remove,
            PatchChoice::MoveToSelected,
            PatchChoice::ToIndex,
            PatchChoice::NewCommit,
        ] {
            let mut idle = with_patch(0);
            idle.update(AppIntent::Commits(CommitsIntent::CustomPatch(
                CustomPatchIntent::Choose(choice),
            )));
            assert_eq!(idle.popups.len(), 1, "{choice:?} asks without a bisect");

            let mut state = with_patch(0);
            start_bisecting(&mut state);
            let effects = state.update(AppIntent::Commits(CommitsIntent::CustomPatch(
                CustomPatchIntent::Choose(choice),
            )));
            assert_eq!(effects, vec![], "{choice:?}");
            assert_eq!(state.notice, Some(bisect::RESET_FIRST.into()), "{choice:?}");
            assert_eq!(state.popups, [], "{choice:?}");
            assert_eq!(state.held_seq(), None, "{choice:?}");
        }
    }

    #[test]
    fn no_closes_the_question_and_keeps_the_patch() {
        let mut state = with_patch(0);
        state.update(AppIntent::Commits(CommitsIntent::CustomPatch(
            CustomPatchIntent::Choose(PatchChoice::Remove),
        )));
        assert_eq!(press(&mut state, KeyCode::Char('n')), vec![]);
        assert!(state.popups.is_empty());
        assert!(state.custom_patch.is_some());
    }

    #[test]
    fn a_new_commit_asks_for_its_message_first_and_refuses_an_empty_one() {
        let mut state = with_patch(0);
        state.update(AppIntent::Commits(CommitsIntent::CustomPatch(
            CustomPatchIntent::Choose(PatchChoice::NewCommit),
        )));
        assert!(matches!(
            state.popups.as_slice(),
            [Popup::Prompt {
                kind: PromptKind::PatchCommitMessage,
                ..
            }]
        ));
        press(&mut state, KeyCode::Char(' '));
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice.as_deref(), Some(super::MESSAGE_NEEDED));
        assert!(matches!(
            state.popups.as_slice(),
            [Popup::Prompt {
                kind: PromptKind::PatchCommitMessage,
                ..
            }]
        ));

        for c in "Split".chars() {
            state.update(AppIntent::Key(KeyPress::new(
                KeyCode::Char(c),
                KeyModifiers::SHIFT,
            )));
        }
        press(&mut state, KeyCode::Enter);
        assert_eq!(
            state.popups,
            [Popup::Question(state.patch_action_question(
                PatchAction::NewCommit("Split".into())
            ))]
        );
        let Popup::Question(q) = state.popups[0].clone() else {
            panic!("{:?}", state.popups);
        };
        let question = q.text;
        assert!(question.contains("\"Split\""), "{question}");
        let op = the_op(&press(&mut state, KeyCode::Char('y')));
        assert!(matches!(
            op,
            GitOp::CustomPatch { action: PatchAction::NewCommit(ref m), .. } if m == "Split"
        ));
    }

    #[test]
    fn a_successful_rewrite_drops_the_patch_and_a_failed_one_keeps_it() {
        let mut state = with_patch(0);
        state.update(AppIntent::Commits(CommitsIntent::CustomPatch(
            CustomPatchIntent::Choose(PatchChoice::Remove),
        )));
        let effects = press(&mut state, KeyCode::Char('y'));
        let (tag, op) = match effects.as_slice() {
            [Effect::HandOff(HandOff::Attached { tag, op })] => (*tag, op.clone()),
            other => panic!("{other:?}"),
        };
        state.update(AppIntent::OpFinished {
            tag,
            op: op.clone(),
            result: Err("conflict".into()),
        });
        assert!(state.custom_patch.is_some());
        assert!(matches!(state.popups.last(), Some(Popup::Error(_))));

        state.popups.clear();
        let effects = state.update(AppIntent::Commits(CommitsIntent::CustomPatch(
            CustomPatchIntent::Choose(PatchChoice::Remove),
        )));
        assert_eq!(effects, vec![]);
        let (tag, op) = match press(&mut state, KeyCode::Char('y')).as_slice() {
            [Effect::HandOff(HandOff::Attached { tag, op })] => (*tag, op.clone()),
            other => panic!("{other:?}"),
        };
        state.update(AppIntent::OpFinished {
            tag,
            op,
            result: Ok(()),
        });
        assert_eq!(state.custom_patch, None);
    }

    #[test]
    fn a_rewrite_says_why_not_in_order() {
        use kika_git::repo_state::Operation;
        let why = |state: &AppState, choice| {
            keys::patch_rewrite_disabled(state, choice).map(String::from)
        };
        let mut state = with_patch(2);
        let picks = state.custom_patch.take();
        state.hold_op(1);
        state.branch.as_mut().unwrap().in_progress = vec![Operation::Rebase];
        state.files.rows.push(changed("t.rs", '.', 'M'));
        state.commits.list.filter = "x".into();
        state.commits.list.rows[2].parents = vec![oid(3), oid(9)];

        let mut reasons = vec![];
        reasons.push(why(&state, PatchChoice::MoveToSelected));
        state.custom_patch = picks;
        reasons.push(why(&state, PatchChoice::MoveToSelected));
        state.free_op();
        reasons.push(why(&state, PatchChoice::MoveToSelected));
        state.branch.as_mut().unwrap().in_progress.clear();
        reasons.push(why(&state, PatchChoice::MoveToSelected));
        state.files.rows.retain(|f| f.kind == EntryKind::Untracked);
        reasons.push(why(&state, PatchChoice::MoveToSelected));
        state.commits.list.filter.clear();
        reasons.push(why(&state, PatchChoice::MoveToSelected));
        state.commits.list.rows[2].parents.clear();
        reasons.push(why(&state, PatchChoice::MoveToSelected));
        state.commits.list.rows[2].parents = vec![oid(3)];
        reasons.push(why(&state, PatchChoice::MoveToSelected));
        reasons.push(why(&state, PatchChoice::Remove));

        let want: Vec<Option<String>> = [
            Some(keys::PATCH_EMPTY),
            Some(keymap::OP_RUNNING),
            Some("Finish or abort the operation in progress"),
            Some("Commit or stash your changes first"),
            Some(keymap::CLEAR_LOG_FILTER),
            Some("Cannot rewrite a merge commit"),
            Some("Cannot rewrite the first commit"),
            Some(keys::TARGET_IS_SOURCE),
            None,
        ]
        .into_iter()
        .map(|r| r.map(String::from))
        .collect();
        assert_eq!(reasons, want);

        // A log narrowed to a path is refused the same way.
        state.commits.query.path = Some("a.rs".into());
        assert_eq!(
            why(&state, PatchChoice::Remove).as_deref(),
            Some(keymap::CLEAR_LOG_FILTER)
        );
    }

    #[test]
    fn a_repository_file_cannot_rebind_the_patch_menu() {
        use crate::keymap::{KeysTable, REPO_LOCKED, Trust};
        assert!(REPO_LOCKED.contains(&"workspace.patch_menu"));
        for id in [
            "workspace.patch_menu",
            "patch_confirm.yes",
            "patch_confirm.no",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
            let table: KeysTable = [(id.to_string(), vec!["enter".to_string()])].into();
            let repo = Keymap::build(&Keymap::default(), &table, Trust::Repo);
            assert!(repo.is_err(), "{id}");
            let global = Keymap::build(&Keymap::default(), &table, Trust::Global);
            assert!(global.is_ok(), "{id}: {global:?}");
        }
    }
}
