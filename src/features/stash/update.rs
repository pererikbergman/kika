//! The Stash panel's side of `AppState`: loading the list, opening an
//! entry to its files, the actions on the selected entry, and stashing
//! from the Files panel's menu.

use super::git::{StashEntry, StashFile, StashMode, StashRef};
use super::keys;
use super::list::{StashDraft, StashFilesList};
use super::push::{self, StashSet};
use crate::diff_pane::DiffPane;
use crate::effect::{Effect, Tag};
use crate::git::diff::DiffRequest;
use crate::keymap;
use crate::model::{AppState, Context, Focus};
use crate::op::GitOp;
use crate::popup::{DeleteRangeConfirm, DropStashConfirm, LineInput, Popup, PromptKind, RangeKind};

/// Said when a range's stash is refused because its files changed while
/// the message was typed.
const STASH_SET_CHANGED: &str = "Nothing stashed: the selected files changed";

impl AppState {
    /// Whether a commit's or a stash's files are open: the Diff pane is
    /// theirs, not the selected working-tree file's.
    pub(crate) fn files_open(&self) -> bool {
        self.commits.open.is_some() || self.stash.open.is_some()
    }

    /// Load the stash list again, keeping what it shows until it lands.
    pub(crate) fn reload_stashes(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        let tag = self.background_tag();
        self.stash.begin(tag.seq);
        vec![Effect::LoadStashes { tag }]
    }

    /// Open the selected stash's files in place of the list. A user
    /// effect, so the busy indicator shows while git lists them.
    pub(crate) fn open_stash_files(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace
            || self.focus != Focus::Stash
            || self.stash.open.is_some()
        {
            return vec![];
        }
        if let Some(reason) = keymap::list_guard(self, false) {
            self.notice = Some(reason);
            return vec![];
        }
        let Some(entry) = self.stash.selected_entry().cloned() else {
            return vec![];
        };
        let seq = self.take_seq();
        self.stash.open = Some(StashFilesList::begin(entry.stash_ref(), seq));
        let tag = self.user_tag(seq);
        vec![Effect::LoadStashFiles { tag, entry }]
    }

    /// Show the first file's diff, or nothing for an entry with no files.
    pub(crate) fn stash_files_loaded(
        &mut self,
        tag: Tag,
        result: Result<Vec<StashFile>, String>,
    ) -> Vec<Effect> {
        // Every user effect reports back once, stale or not.
        self.user_done(tag);
        if tag.generation != self.generation {
            return vec![];
        }
        let Some(open) = &mut self.stash.open else {
            return vec![];
        };
        if !open.loaded(tag.seq, result) {
            return vec![];
        }
        if open.selected_file().is_none() {
            self.diff = DiffPane::default();
            return vec![Effect::CancelDiff];
        }
        // Another stash's file: start at its top, whatever was shown.
        self.diff.path = None;
        self.stash_file_diff()
    }

    pub(crate) fn move_stash_file(&mut self, delta: isize) -> Vec<Effect> {
        let moved = self
            .stash
            .open
            .as_mut()
            .is_some_and(|open| open.move_by(delta));
        if moved {
            self.stash_file_diff()
        } else {
            vec![]
        }
    }

    /// Stream the selected stash file's diff, if one is selected.
    pub(crate) fn stash_file_diff(&mut self) -> Vec<Effect> {
        let request = self.stash.open.as_ref().and_then(|open| {
            let file = open.selected_file()?;
            Some(DiffRequest::for_commit_file(
                &file.range,
                &file.file,
                self.diff_options,
            ))
        });
        match request {
            Some(request) => self.stream_diff(request),
            None => vec![],
        }
    }

    /// Back to the list, still on the same entry, with the pane showing the
    /// selected working-tree file again.
    pub(crate) fn close_stash_files(&mut self) -> Vec<Effect> {
        if self.stash.open.take().is_none() {
            return vec![];
        }
        self.diff.path = None;
        match self.selected {
            Some(index) if index < self.files.len() => self.select(index),
            _ => {
                self.diff = DiffPane::default();
                vec![Effect::CancelDiff]
            }
        }
    }

    /// The selected entry, or why no action may run on it now, said in
    /// the bar. Its place and id are taken now, when the key is pressed.
    fn stash_for_action(&mut self) -> Option<StashEntry> {
        if self.context != Context::Workspace {
            return None;
        }
        if let Some(reason) = keys::stash_action_disabled(self) {
            self.notice = Some(reason);
            return None;
        }
        self.stash.selected_entry().cloned()
    }

    /// Apply or pop the selected entry, as `make` says.
    pub(crate) fn stash_op(&mut self, make: fn(StashRef) -> GitOp) -> Vec<Effect> {
        match self.stash_for_action() {
            Some(entry) => self.start_op(Ok(Some(make(entry.stash_ref())))),
            None => vec![],
        }
    }

    /// Ask before dropping the selected entry: it leaves the list for good.
    pub(crate) fn ask_drop_stash(&mut self) {
        let Some(entry) = self.stash_for_action() else {
            return;
        };
        let stash = entry.stash_ref();
        let confirm = DropStashConfirm {
            label: format!("{}: {}", stash.arg(), entry.subject),
            op: GitOp::StashDrop(stash),
        };
        self.popups.push(Popup::DropStash(confirm));
    }

    /// Every entry in the range, each with its place and id as listed
    /// now. Only the loaded list names them; the filter only chose which
    /// rows the range spans.
    pub(crate) fn stashes_range_confirm(&self) -> DeleteRangeConfirm {
        let stashes: Vec<StashRef> = self
            .stash
            .range()
            .into_iter()
            .map(|i| self.stash.entries[i].stash_ref())
            .collect();
        DeleteRangeConfirm {
            kind: RangeKind::Stashes,
            names: stashes.iter().map(StashRef::arg).collect(),
            skipped: vec![],
            op: GitOp::StashDrops(stashes),
        }
    }

    /// Drop as the open question asked, and only if one is open.
    pub(crate) fn confirm_drop_stash(&mut self) -> Vec<Effect> {
        if !matches!(self.popups.last(), Some(Popup::DropStash(_))) {
            return vec![];
        }
        let Some(Popup::DropStash(confirm)) = self.popups.pop() else {
            return vec![];
        };
        self.start_op(Ok(Some(confirm.op)))
    }

    /// Ask for a new message, starting from the current one, keeping the
    /// entry for the answer.
    pub(crate) fn ask_rename_stash(&mut self) {
        let Some(entry) = self.stash_for_action() else {
            return;
        };
        self.stash.pending = Some(entry.stash_ref());
        self.popups.push(Popup::Prompt {
            kind: PromptKind::RenameStash,
            input: LineInput::with_text(&entry.subject),
        });
    }

    /// Rename the entry kept when the prompt opened.
    pub(crate) fn rename_stash_to(&mut self, text: &str) -> Vec<Effect> {
        let Some(stash) = self.stash.pending.take() else {
            return vec![];
        };
        let message = text.trim();
        if message.is_empty() {
            self.notice = Some("A stash message cannot be empty".into());
            return vec![];
        }
        if self.op_is_running() {
            self.notice = Some(keymap::OP_RUNNING.into());
            return vec![];
        }
        self.start_op(Ok(Some(GitOp::StashRename {
            stash,
            message: message.to_string(),
        })))
    }

    /// Fix what a whole-tree stash takes, then ask for its message.
    pub(crate) fn ask_stash_message(&mut self, mode: StashMode) {
        let disabled = match mode {
            StashMode::All => keys::stash_all_disabled,
            StashMode::IncludeUntracked => keys::stash_untracked_disabled,
            StashMode::Staged => keys::stash_staged_disabled,
        };
        self.ask_stash_draft(disabled, |_| {
            Some(StashDraft {
                mode,
                paths: vec![],
                restore: vec![],
                unstage: vec![],
                from_range: false,
            })
        });
    }

    /// Fix the marked files, then ask for the stash's message: the Files
    /// range when one is set there, else the file under the cursor. Only
    /// the loaded list names them; the filter only chose which rows the
    /// range spans.
    pub(crate) fn ask_stash_file_message(&mut self) {
        self.ask_stash_draft(keys::stash_file_disabled, |state| {
            let (entries, from_range) = state.files_marked();
            let set = push::stash_files_for(&entries)?;
            Some(StashDraft {
                mode: set.mode,
                paths: set.paths,
                restore: set.restore,
                unstage: set.unstage,
                from_range,
            })
        });
    }

    fn ask_stash_draft(
        &mut self,
        disabled: fn(&AppState) -> Option<crate::keymap::Reason>,
        draft: impl FnOnce(&AppState) -> Option<StashDraft>,
    ) {
        if self.context != Context::Workspace {
            return;
        }
        if let Some(reason) = disabled(self) {
            self.notice = Some(reason);
            return;
        }
        let Some(draft) = draft(self) else {
            return;
        };
        self.stash_draft = Some(draft);
        self.popups.push(Popup::Prompt {
            kind: PromptKind::StashMessage,
            input: LineInput::default(),
        });
    }

    /// Stash what was drafted when the prompt opened, with the message
    /// typed, or git's own when it is empty.
    pub(crate) fn stash_message_answered(&mut self, text: &str) -> Vec<Effect> {
        let Some(StashDraft {
            mode,
            paths,
            restore,
            unstage,
            from_range,
        }) = self.stash_draft.take()
        else {
            return vec![];
        };
        if self.op_is_running() {
            self.notice = Some(keymap::OP_RUNNING.into());
            return vec![];
        }
        // A range's set is checked again: a reload while the message was
        // typed may have cleared the range or changed what its files need,
        // and a push must never take a set other than the one shown.
        if from_range {
            let (entries, still_range) = self.files_marked();
            let now = still_range
                .then(|| push::stash_files_for(&entries))
                .flatten();
            let drafted = StashSet {
                mode,
                paths: paths.clone(),
                restore: restore.clone(),
                unstage: unstage.clone(),
            };
            if now.as_ref() != Some(&drafted) {
                self.notice = Some(STASH_SET_CHANGED.into());
                return vec![];
            }
            self.files_anchor = None;
        }
        let message = Some(text.trim())
            .filter(|m| !m.is_empty())
            .map(str::to_string);
        self.start_op(Ok(Some(GitOp::StashPush {
            mode,
            message,
            paths,
            restore,
            unstage,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::FileEntry;
    use crate::git::commit_files::CommitRange;
    use crate::git::path::RepoPath;
    use crate::git::status::EntryKind;
    use crate::intent::AppIntent;
    use crate::keymap::KeyScope;
    use crate::list_filter::InputTarget;
    use crate::list_input::SELECTION_CLEARED;
    use crate::menu::MenuId;
    use crate::refresh;
    use crate::test_support::*;
    use crossterm::event::KeyCode;

    #[test]
    fn the_diff_cannot_take_focus_from_an_open_stash() {
        let mut state = on_stash_one();
        stash_opened(&mut state);
        assert_eq!(
            state.update(AppIntent::FocusPanel(crate::layout::Panel::Diff)),
            vec![]
        );
        assert_eq!(state.focus, Focus::Stash);
    }

    #[test]
    fn stashes_loaded_fills_the_list_and_a_stale_or_other_repositorys_is_ignored() {
        let (mut state, _) = with_files(three_files(), 24);
        let first = match state.reload_stashes().as_slice() {
            [Effect::LoadStashes { tag }] => *tag,
            other => panic!("{other:?}"),
        };
        assert!(!first.user, "a background load");
        let second = match state.reload_stashes().as_slice() {
            [Effect::LoadStashes { tag }] => *tag,
            other => panic!("{other:?}"),
        };
        let busy = state.busy_count();
        state.update(AppIntent::StashesLoaded {
            tag: first,
            result: Ok(three_stashes()),
        });
        assert!(state.stash.entries.is_empty(), "a stale seq is ignored");
        state.update(AppIntent::StashesLoaded {
            tag: Tag {
                generation: second.generation + 1,
                ..second
            },
            result: Ok(three_stashes()),
        });
        assert!(state.stash.entries.is_empty(), "another repository's");
        state.update(AppIntent::StashesLoaded {
            tag: second,
            result: Ok(three_stashes()),
        });
        assert_eq!(state.stash.entries, three_stashes());
        assert!(state.stash.loaded);
        assert_eq!(state.busy_count(), busy);
    }

    #[test]
    fn the_cursor_moves_through_the_stashes_and_stops_at_the_ends() {
        let mut state = on_stash_one();
        assert_eq!(state.stash.selected, 1);
        press_char(&mut state, 'j');
        press(&mut state, KeyCode::Down);
        assert_eq!(state.stash.selected, 2);
        press_char(&mut state, 'k');
        press(&mut state, KeyCode::Up);
        press(&mut state, KeyCode::Up);
        assert_eq!(state.stash.selected, 0);
    }

    #[test]
    fn stash_ops_and_a_stash_refresh_plan_reload_the_list() {
        let mut state = on_stash_one();
        let (tag, op) = op_of(&press_char(&mut state, ' '));
        let effects = state.update(finished(tag, op, Ok(())));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::LoadStashes { .. })),
            "{effects:?}"
        );
        let effects = state.apply_plan(refresh::plan(&GitOp::StashDrop(stash_ref(0, "a0"))));
        assert!(
            matches!(effects.as_slice(), [Effect::LoadStashes { .. }]),
            "{effects:?}"
        );
    }

    #[test]
    fn enter_opens_the_stash_as_a_user_load_then_shows_its_first_files_diff() {
        let mut state = on_stash_one();
        let busy = state.busy_count();

        let (tag, entry) = stash_files_load(&press(&mut state, KeyCode::Enter));

        assert_eq!(entry, three_stashes()[1]);
        assert!(tag.user);
        assert_eq!(state.busy_count(), busy + 1);
        assert_eq!(state.key_focus(), KeyScope::StashFiles);
        let effects = state.update(AppIntent::StashFilesLoaded {
            tag,
            result: Ok(tracked_and_untracked()),
        });
        assert_eq!(state.busy_count(), busy);
        let request = diff_request(&effects);
        assert_eq!(request.commit, Some(range_of("a1", "ba5e")));
        assert_eq!(request.path, "src/lib.rs".into());

        // The untracked file's diff is read from the untracked commit alone.
        let request = diff_request(&press_char(&mut state, 'j'));
        assert_eq!(
            request.commit,
            Some(CommitRange {
                oid: cid("d1"),
                parent: None
            })
        );
        assert_eq!(request.path, "notes.txt".into());
        assert_eq!(press(&mut state, KeyCode::Down), vec![]);
        let request = diff_request(&press(&mut state, KeyCode::Up));
        assert_eq!(request.path, "src/lib.rs".into());
        assert_eq!(state.stash.selected, 1);
    }

    #[test]
    fn a_stale_stash_file_list_is_dropped_and_an_empty_or_failed_one_empties_the_diff() {
        let mut state = on_stash_one();
        let (tag, _) = stash_files_load(&press(&mut state, KeyCode::Enter));
        let busy = state.busy_count();
        let stale = Tag {
            seq: tag.seq + 1,
            ..tag
        };
        let effects = state.update(AppIntent::StashFilesLoaded {
            tag: stale,
            result: Ok(tracked_and_untracked()),
        });
        assert_eq!(effects, vec![]);
        assert_eq!(state.busy_count(), busy - 1);
        assert!(!state.stash.open.as_ref().unwrap().loaded);

        for result in [Ok(vec![]), Err("bad object".to_string())] {
            let mut state = on_stash_one();
            let (tag, _) = stash_files_load(&press(&mut state, KeyCode::Enter));
            let effects = state.update(AppIntent::StashFilesLoaded { tag, result });
            assert_eq!(effects, vec![Effect::CancelDiff]);
            assert_eq!(state.diff, DiffPane::default());
        }
    }

    #[test]
    fn esc_or_backspace_closes_the_files_and_restores_the_worktree_diff() {
        for code in [KeyCode::Esc, KeyCode::Backspace] {
            let mut state = on_stash_one();
            stash_opened(&mut state);
            press_char(&mut state, 'j');

            let effects = press(&mut state, code);

            assert!(!state.should_quit, "{code:?}");
            assert_eq!(state.stash.open, None, "{code:?}");
            assert_eq!(state.stash.selected, 1, "{code:?}");
            assert_eq!(state.key_focus(), KeyScope::Stash, "{code:?}");
            let request = diff_request(&effects);
            assert_eq!((request.commit, request.path), (None, "a.rs".into()));
        }
    }

    #[test]
    fn while_a_stash_is_open_the_pane_stays_its_own() {
        // The diff cannot take focus: a stash's file has no hunks to stage.
        let mut state = on_stash_one();
        stash_opened(&mut state);
        assert_eq!(press_char(&mut state, 'l'), vec![]);
        assert_eq!(state.focus, Focus::Stash);

        // New diff options reload the stash's file.
        let request = diff_request(&state.update(AppIntent::ToggleWhitespace));
        assert!(request.options.ignore_whitespace);
        assert_eq!(request.commit, Some(range_of("a1", "ba5e")));

        // A status refresh does not load the worktree diff over it.
        let diff = state.diff.clone();
        let tag = refresh_tag(&mut state);
        let effects = state.update(AppIntent::StatusLoaded {
            tag,
            result: Ok(repo(vec![changed("b.rs", 'M', '.')])),
        });
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::LoadDiff { .. } | Effect::CancelDiff)),
            "{effects:?}"
        );
        assert_eq!(state.diff, diff);
    }

    #[test]
    fn focusing_another_panel_closes_the_stash_and_shows_the_worktree_diff() {
        let mut state = on_stash_one();
        stash_opened(&mut state);
        let request = diff_request(&press_char(&mut state, '2'));
        assert_eq!(state.stash.open, None);
        assert_eq!((request.commit, request.path), (None, "a.rs".into()));

        // From itself, it stays open.
        let mut state = on_stash_one();
        stash_opened(&mut state);
        assert_eq!(press_char(&mut state, '5'), vec![]);
        assert!(state.stash.open.is_some());
    }

    #[test]
    fn space_applies_and_g_pops_the_stash_under_the_cursor() {
        let mut state = on_stash_one();
        let (tag, op) = op_of(&press_char(&mut state, ' '));
        assert_eq!(op, GitOp::StashApply(stash_ref(1, "a1")));
        state.update(finished(tag, op, Ok(())));
        let (_, op) = op_of(&press_char(&mut state, 'g'));
        assert_eq!(op, GitOp::StashPop(stash_ref(1, "a1")));
    }

    #[test]
    fn d_asks_and_only_confirm_drops_the_stash_chosen_when_asked() {
        let mut state = on_stash_one();
        assert_eq!(press_char(&mut state, 'd'), vec![]);
        let asked = Popup::DropStash(DropStashConfirm {
            label: "stash@{1}: On main: one".into(),
            op: GitOp::StashDrop(stash_ref(1, "a1")),
        });
        assert_eq!(state.popups, std::slice::from_ref(&asked));
        assert_eq!(state.key_focus(), KeyScope::DropStash);

        // Enter is not an answer.
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.popups, [asked]);
        assert_eq!(state.held_seq(), None);
        // A reload under the question moves the list, not the answer.
        let mut moved = three_stashes();
        moved.insert(0, stash_entry(0, "f0", "new"));
        load_stashes(&mut state, moved);

        let (_, op) = op_of(&press_char(&mut state, 'y'));
        assert_eq!(op, GitOp::StashDrop(stash_ref(1, "a1")));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn slash_in_the_stash_panel_narrows_it_as_typed_and_enter_keeps_it() {
        let mut state = on_stash_one();
        let mut effects = press_char(&mut state, '/');
        let input = state.input.as_ref().expect("the input opens");
        assert_eq!(input.target, InputTarget::StashFilter);
        assert_eq!(input.target.prompt(), "/");
        assert_eq!(state.key_focus(), KeyScope::Input);
        for c in "zero".chars() {
            effects.extend(press_char(&mut state, c));
        }
        assert_eq!(state.stash.visible(), [0], "by subject");
        assert_eq!(state.stash.selected, 0, "the hidden cursor follows");
        for _ in 0..4 {
            effects.extend(press(&mut state, KeyCode::Backspace));
        }
        for c in "{2}".chars() {
            effects.extend(press_char(&mut state, c));
        }
        assert_eq!(state.stash.visible(), [2], "by its place");
        assert_eq!(state.stash.selected, 2);
        effects.extend(press(&mut state, KeyCode::Enter));
        assert_eq!(effects, vec![], "the filter asks git nothing");
        assert_eq!(state.input, None);
        assert_eq!(state.stash.filter, "{2}");
        assert_eq!(state.key_focus(), KeyScope::Stash);
        // `/` again starts from the kept text.
        press_char(&mut state, '/');
        assert_eq!(state.input.as_ref().unwrap().line.text, "{2}");
    }

    #[test]
    fn a_changed_stash_filter_clears_the_range_and_says_so() {
        let mut state = on_stash_one();
        press(&mut state, KeyCode::Char('V'));
        press_char(&mut state, 'j');
        assert_eq!(state.stash.range(), [1, 2]);
        press_char(&mut state, '/');
        press_char(&mut state, 'o');
        assert_eq!(state.stash.range_anchor, None);
        assert_eq!(state.notice.as_deref(), Some(SELECTION_CLEARED));
    }

    #[test]
    fn esc_in_the_stash_panel_clears_the_range_then_the_filter_then_quits() {
        let mut state = on_stash_one();
        press_char(&mut state, '/');
        type_text(&mut state, "main\n");
        press(&mut state, KeyCode::Char('V'));
        assert!(state.stash.range_anchor.is_some());
        assert_eq!(press(&mut state, KeyCode::Esc), vec![]);
        assert_eq!(state.stash.range_anchor, None);
        assert_eq!(state.stash.filter, "main");
        assert_eq!(press(&mut state, KeyCode::Esc), vec![]);
        assert_eq!(state.stash.filter, "");
        assert!(!state.should_quit);
        press(&mut state, KeyCode::Esc);
        assert!(state.should_quit);
    }

    /// The Stash panel with a range over all three stashes, the cursor on
    /// `stash@{2}`.
    fn stash_range_of_three() -> AppState {
        let mut state = on_stash_one();
        press_char(&mut state, 'k');
        press(&mut state, KeyCode::Char('V'));
        press_char(&mut state, 'j');
        press_char(&mut state, 'j');
        assert_eq!(state.stash.range(), [0, 1, 2]);
        state
    }

    #[test]
    fn d_over_a_stash_range_asks_once_and_only_y_drops_every_stash_in_it() {
        let mut state = stash_range_of_three();
        assert_eq!(press_char(&mut state, 'd'), vec![]);
        let confirm = match state.popups.as_slice() {
            [Popup::DeleteRange(confirm)] => confirm.clone(),
            other => panic!("expected the range question, got {other:?}"),
        };
        assert_eq!(confirm.title(), "Drop stashes");
        assert_eq!(
            confirm.question(),
            "Drop 3 stashes (stash@{0}, stash@{1}, stash@{2})? \
             They cannot be recovered from the stash list. (y/n)"
        );
        assert_eq!(state.key_focus(), KeyScope::DeleteRange);
        let refs = vec![stash_ref(0, "a0"), stash_ref(1, "a1"), stash_ref(2, "a2")];

        // Enter is not an answer.
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.popups.len(), 1);
        assert_eq!(state.held_seq(), None);
        // A reload under the question moves the list, not the answer.
        let mut moved = three_stashes();
        moved.insert(0, stash_entry(0, "f0", "new"));
        load_stashes(&mut state, moved);

        let (_, op) = op_of(&press_char(&mut state, 'y'));
        assert_eq!(op, GitOp::StashDrops(refs));
        assert_eq!(state.popups, []);
        assert_eq!(state.stash.range_anchor, None, "the range is done with");
    }

    #[test]
    fn n_or_esc_keeps_a_stash_range_and_drops_nothing() {
        for code in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut state = stash_range_of_three();
            press_char(&mut state, 'd');
            assert_eq!(press(&mut state, code), vec![], "{code:?}");
            assert_eq!(state.popups, [], "{code:?}");
            assert_eq!(state.held_seq(), None, "{code:?}");
        }
    }

    #[test]
    fn a_stash_range_refuses_the_one_row_actions_and_a_hidden_anchor_asks_nothing() {
        let mut state = stash_range_of_three();
        for code in [
            KeyCode::Char(' '),
            KeyCode::Char('g'),
            KeyCode::Char('R'),
            KeyCode::Enter,
        ] {
            state.notice = None;
            assert_eq!(press(&mut state, code), vec![], "{code:?}");
            assert_eq!(
                state.notice.as_deref(),
                Some(keymap::CLEAR_RANGE_FIRST),
                "{code:?}"
            );
            assert_eq!(
                (state.held_seq(), state.popups.len()),
                (None, 0),
                "{code:?}"
            );
            assert_eq!(state.stash.open, None, "{code:?}");
        }
        // Asked of the model directly, the guard still holds.
        state.update(AppIntent::ApplyStash);
        assert_eq!(state.held_seq(), None);
        state.update(AppIntent::OpenStashFiles);
        assert_eq!(state.stash.open, None);

        // The anchor hidden by a filter set under the range: nothing to ask.
        state.stash.filter = "{2}".into();
        assert!(state.stash.range().is_empty());
        state.update(AppIntent::DropStash);
        assert_eq!(state.popups, []);
        assert_eq!(state.notice.as_deref(), Some("No stash in the selection"));
    }

    #[test]
    fn the_stash_filter_never_reaches_git_and_a_range_drop_holds_only_listed_refs() {
        let hostile = "-x stash@{0}";
        let mut state = on_stash_one();
        // Every subject matches, so the filter keeps every row.
        let entries = ["a0", "a1", "a2"]
            .iter()
            .enumerate()
            .map(|(i, oid)| stash_entry(i, oid, "On main: -x stash@{0}"))
            .collect();
        load_stashes(&mut state, entries);
        let mut effects = press_char(&mut state, '/');
        for c in hostile.chars() {
            effects.extend(press_char(&mut state, c));
        }
        effects.extend(press(&mut state, KeyCode::Enter));
        assert_eq!(effects, vec![], "typing a filter returns no effects");
        assert_eq!(state.stash.filter, hostile);
        assert_eq!(state.stash.visible(), [0, 1, 2]);

        press_char(&mut state, 'k');
        press(&mut state, KeyCode::Char('V'));
        press_char(&mut state, 'j');
        press_char(&mut state, 'd');
        let ran = press_char(&mut state, 'y');
        let debug = format!("{ran:?}");
        assert!(!debug.contains("-x"), "{debug}");
        let (_, op) = op_of(&ran);
        assert_eq!(
            op,
            GitOp::StashDrops(vec![stash_ref(0, "a0"), stash_ref(1, "a1")])
        );
        for step in crate::op::op_steps(&op) {
            let argv: Vec<String> = crate::op::op_command(&step)
                .args()
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            assert_eq!(argv[..2], ["stash", "drop"]);
            assert!(argv[2] == "stash@{1}" || argv[2] == "stash@{0}", "{argv:?}");
            assert_eq!(argv.len(), 3);
        }
    }

    #[test]
    fn n_or_esc_keeps_the_stash_and_confirm_needs_the_question() {
        for code in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut state = on_stash_one();
            press_char(&mut state, 'd');
            assert_eq!(press(&mut state, code), vec![], "{code:?}");
            assert_eq!(state.popups, [], "{code:?}");
            assert_eq!(state.held_seq(), None, "{code:?}");
        }
        let mut state = on_stash_one();
        assert_eq!(state.update(AppIntent::ConfirmDropStash), vec![]);
        assert_eq!(state.held_seq(), None);
    }

    #[test]
    fn rename_starts_from_the_subject_and_renames_the_stash_chosen_then() {
        let mut state = on_stash_one();
        press(&mut state, KeyCode::Char('R'));
        match state.popups.as_slice() {
            [Popup::Prompt { kind, input }] => {
                assert_eq!(*kind, PromptKind::RenameStash);
                assert_eq!(input.text, "On main: one");
            }
            other => panic!("expected the rename prompt, got {other:?}"),
        }
        assert_eq!(
            state.prompt_title(PromptKind::RenameStash),
            "Rename stash@{1}"
        );
        // The cursor moving under the prompt does not change the entry.
        state.update(AppIntent::StashNext);

        type_text(&mut state, "!");
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
        assert_eq!(
            op,
            GitOp::StashRename {
                stash: stash_ref(1, "a1"),
                message: "On main: one!".into()
            }
        );
        assert_eq!(state.stash.pending, None);
    }

    #[test]
    fn an_empty_rename_is_refused_and_a_cancelled_one_forgets_the_stash() {
        let mut state = on_stash_one();
        press(&mut state, KeyCode::Char('R'));
        // What Enter on a blanked prompt submits.
        state.popups.clear();
        assert_eq!(state.update(AppIntent::RenameStashTo("   ".into())), vec![]);
        assert_eq!(state.notice, Some("A stash message cannot be empty".into()));
        assert_eq!(state.held_seq(), None);

        press(&mut state, KeyCode::Char('R'));
        assert!(state.stash.pending.is_some());
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.stash.pending, None);
        assert_eq!(state.update(AppIntent::RenameStashTo("x".into())), vec![]);
    }

    #[test]
    fn with_no_stashes_the_keys_say_so() {
        let (mut state, _) = with_files(three_files(), 24);
        load_stashes(&mut state, vec![]);
        press_char(&mut state, '5');
        for code in [
            KeyCode::Enter,
            KeyCode::Char(' '),
            KeyCode::Char('g'),
            KeyCode::Char('d'),
            KeyCode::Char('R'),
        ] {
            state.notice = None;
            assert_eq!(press(&mut state, code), vec![], "{code:?}");
            assert_eq!(state.notice, Some("No stashes".into()), "{code:?}");
        }
        assert_eq!(state.popups, []);
    }

    /// Choose item `index` of the Stash menu from the Files panel.
    fn stash_menu_item(state: &mut AppState, index: usize) -> Vec<Effect> {
        press_char(state, 's');
        assert_eq!(
            state.popups,
            [Popup::Menu {
                id: MenuId::Stash,
                cursor: 0
            }]
        );
        for _ in 0..index {
            press_char(state, 'j');
        }
        press(state, KeyCode::Enter)
    }

    #[test]
    fn stash_this_file_takes_the_file_chosen_from_the_menu_even_after_a_refresh() {
        let (mut state, _) = with_files(three_files(), 24);
        select_path(&mut state, "b.rs");
        assert_eq!(stash_menu_item(&mut state, 3), vec![]);
        assert!(matches!(
            state.popups.as_slice(),
            [Popup::Prompt {
                kind: PromptKind::StashMessage,
                ..
            }]
        ));
        assert_eq!(
            state.prompt_title(PromptKind::StashMessage),
            "Stash message (optional)"
        );
        // A refresh while typing moves the cursor to another file.
        let tag = refresh_tag(&mut state);
        state.update(AppIntent::StatusLoaded {
            tag,
            result: Ok(repo(vec![changed("a.rs", '.', 'M')])),
        });
        assert_eq!(state.selected, Some(0));

        type_text(&mut state, " wip ");
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
        assert_eq!(
            op,
            GitOp::StashPush {
                mode: StashMode::All,
                message: Some("wip".into()),
                paths: vec!["b.rs".into()],
                restore: vec![],
                unstage: vec![],
            }
        );
        assert_eq!(state.stash_draft, None);
    }

    #[test]
    fn each_whole_tree_item_stashes_its_mode_and_an_empty_message_is_gits_own() {
        for (index, mode) in [
            (0, StashMode::All),
            (1, StashMode::IncludeUntracked),
            (2, StashMode::Staged),
        ] {
            let (mut state, _) = with_files(three_files(), 24);
            stash_menu_item(&mut state, index);
            let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
            assert_eq!(
                op,
                GitOp::StashPush {
                    mode,
                    message: None,
                    paths: vec![],
                    restore: vec![],
                    unstage: vec![],
                },
                "{index}"
            );
        }
    }

    #[test]
    fn stashing_an_untracked_file_takes_untracked_files_too() {
        let (mut state, _) = with_files(three_files(), 24);
        select_path(&mut state, "c.txt");
        stash_menu_item(&mut state, 3);
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
        assert_eq!(
            op,
            GitOp::StashPush {
                mode: StashMode::IncludeUntracked,
                message: None,
                paths: vec!["c.txt".into()],
                restore: vec![],
                unstage: vec![],
            }
        );
    }

    #[test]
    fn a_disabled_stash_item_says_why_and_a_cancelled_prompt_forgets_the_draft() {
        let (mut state, _) = with_files(vec![untracked("new.txt")], 24);
        assert_eq!(stash_menu_item(&mut state, 0), vec![]);
        assert_eq!(state.notice, Some("No changes to stash".into()));
        assert_eq!(state.stash_draft, None);

        let (mut state, _) = with_files(three_files(), 24);
        stash_menu_item(&mut state, 0);
        assert!(state.stash_draft.is_some());
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.stash_draft, None);
        assert_eq!(state.popups, []);
        assert_eq!(
            state.update(AppIntent::StashMessageAnswered("x".into())),
            vec![]
        );
    }

    #[test]
    fn the_stash_key_on_a_clean_tree_says_so() {
        let (mut state, _) = with_files(vec![], 24);
        state.focus = Focus::Files;
        assert_eq!(press_char(&mut state, 's'), vec![]);
        assert_eq!(state.notice, Some("No changes to stash".into()));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn stashing_selected_files_is_offered_over_a_range_but_waits_for_ops_and_conflicts() {
        let (mut state, _) = with_files(three_files(), 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        assert_eq!(keys::stash_file_disabled(&state), None);
        state.hold_op(1);
        assert_eq!(
            keys::stash_file_disabled(&state),
            Some(keymap::OP_RUNNING.into())
        );

        let files = vec![changed("a.rs", '.', 'M'), conflicted("c.rs")];
        let (mut state, _) = with_files(files, 24);
        press_char(&mut state, 'V');
        assert_eq!(
            keys::stash_file_disabled(&state),
            Some("Resolve conflicts first".into())
        );
    }

    fn renamed(path: &str, old: &str) -> FileEntry {
        FileEntry {
            orig_path: Some(old.into()),
            kind: EntryKind::Renamed,
            ..changed(path, 'R', '.')
        }
    }

    #[test]
    fn a_range_with_a_rename_and_an_untracked_file_stashes_the_rename_whole() {
        let files = vec![renamed("new.rs", "old.rs"), untracked("u.txt")];
        let (mut state, _) = with_files(files, 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        assert_eq!(keys::stash_file_disabled(&state), None);
        stash_menu_item(&mut state, 3);
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
        assert_eq!(
            op,
            GitOp::StashPush {
                mode: StashMode::IncludeUntracked,
                message: None,
                paths: vec!["new.rs".into(), "u.txt".into()],
                restore: vec!["old.rs".into()],
                unstage: vec![],
            }
        );
    }

    #[test]
    fn a_rename_whose_old_path_is_taken_again_is_refused_with_a_reason() {
        // The file at the old path is listed but not marked.
        let files = vec![renamed("new.rs", "old.rs"), untracked("old.rs")];
        let (mut state, _) = with_files(files, 24);
        let reason = keys::STASH_RENAME_TAKEN;
        assert_eq!(keys::stash_file_disabled(&state), Some(reason.into()));
        // Marked too, it is refused for the same reason.
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        assert_eq!(keys::stash_file_disabled(&state), Some(reason.into()));
        assert_eq!(stash_menu_item(&mut state, 3), vec![]);
        assert_eq!(state.notice, Some(reason.into()));
        assert_eq!(state.stash_draft, None);
        assert!(state.files_anchor.is_some());
    }

    #[test]
    fn a_rename_whose_old_directory_is_now_a_file_is_refused_with_a_reason() {
        let reason = Some(keys::STASH_RENAME_TAKEN.into());
        for taken in ["d", "d/e"] {
            let files = vec![renamed("new.rs", "d/e/old.rs"), untracked(taken)];
            let (state, _) = with_files(files, 24);
            assert_eq!(keys::stash_file_disabled(&state), reason, "{taken}");
        }
        // A case-insensitive filesystem would fold these onto the old path
        // or its directory, so they are refused too.
        for (old, taken) in [("Old.rs", "old.rs"), ("D/old.rs", "d")] {
            let files = vec![renamed("new.rs", old), untracked(taken)];
            let (state, _) = with_files(files, 24);
            assert_eq!(keys::stash_file_disabled(&state), reason, "{taken}");
        }
        // A file beside the old path, inside its directory, is fine.
        let files = vec![renamed("new.rs", "d/e/old.rs"), untracked("d/e/other.rs")];
        let (state, _) = with_files(files, 24);
        assert_eq!(keys::stash_file_disabled(&state), None);
    }

    #[test]
    fn a_staged_deletion_whose_path_is_taken_again_is_refused_with_a_reason() {
        let reason = Some(keys::STASH_DELETED_TAKEN.into());
        // An untracked file at the path, or folded onto it or a directory
        // it needs by a case-insensitive filesystem.
        for taken in ["gone.rs", "Gone.rs"] {
            let files = vec![changed("gone.rs", 'D', '.'), untracked(taken)];
            let (state, _) = with_files(files, 24);
            assert_eq!(keys::stash_file_disabled(&state), reason, "{taken}");
        }
        for taken in ["d", "D"] {
            let files = vec![changed("d/gone.rs", 'D', '.'), untracked(taken)];
            let (state, _) = with_files(files, 24);
            assert_eq!(keys::stash_file_disabled(&state), reason, "{taken}");
        }
        // Marked as well, it is still refused, with the same reason.
        let files = vec![changed("gone.rs", 'D', '.'), untracked("gone.rs")];
        let (mut state, _) = with_files(files, 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        assert_eq!(keys::stash_file_disabled(&state), reason);
        assert_eq!(stash_menu_item(&mut state, 3), vec![]);
        assert_eq!(state.notice, reason);
        assert_eq!(state.stash_draft, None);
    }

    #[test]
    fn a_lone_staged_deletion_is_stashed_with_its_path_unstaged_first() {
        // Its own entry does not count as taking its path, nor does a file
        // beside it.
        let files = vec![changed("d/gone.rs", 'D', '.'), untracked("d/other.rs")];
        let (mut state, _) = with_files(files, 24);
        assert_eq!(keys::stash_file_disabled(&state), None);
        stash_menu_item(&mut state, 3);
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
        assert_eq!(
            op,
            GitOp::StashPush {
                mode: StashMode::All,
                message: None,
                paths: vec!["d/gone.rs".into()],
                restore: vec![],
                unstage: vec!["d/gone.rs".into()],
            }
        );
    }

    #[test]
    fn stashing_a_range_pushes_its_files_and_drops_the_range() {
        let (mut state, _) = with_files(three_files(), 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        assert_eq!(state.files_range(), [0, 1]);
        stash_menu_item(&mut state, 3);
        let draft = state.stash_draft.clone().unwrap();
        assert!(draft.from_range);
        type_text(&mut state, "pair");
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
        assert_eq!(
            op,
            GitOp::StashPush {
                mode: StashMode::All,
                message: Some("pair".into()),
                paths: vec!["a.rs".into(), "b.rs".into()],
                restore: vec![],
                unstage: vec![],
            }
        );
        assert_eq!(state.files_anchor, None, "the range is done with");
    }

    #[test]
    fn a_range_with_an_untracked_file_stashes_with_untracked_and_cancel_keeps_it() {
        let (mut state, _) = with_files(three_files(), 24);
        press_char(&mut state, 'j');
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        stash_menu_item(&mut state, 3);
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.stash_draft, None);
        assert!(
            state.files_anchor.is_some(),
            "a cancelled prompt keeps the range"
        );

        stash_menu_item(&mut state, 3);
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
        assert_eq!(
            op,
            GitOp::StashPush {
                mode: StashMode::IncludeUntracked,
                message: None,
                paths: vec!["b.rs".into(), "c.txt".into()],
                restore: vec![],
                unstage: vec![],
            }
        );
        assert_eq!(state.files_anchor, None);
    }

    #[test]
    fn a_filtered_range_stashes_only_the_files_shown_and_never_the_filter_text() {
        let files = vec![
            changed("src/model.rs", '.', 'M'),
            changed("src/view.rs", '.', 'M'),
            changed("docs/mode.md", '.', 'M'),
            changed("src/main.rs", '.', 'M'),
        ];
        let (mut state, _) = with_files(files, 24);
        type_text(&mut state, "/mod");
        press(&mut state, KeyCode::Enter);
        assert_eq!(state.files_visible(), [0, 2]);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        assert_eq!(state.files_range(), [0, 2]);
        stash_menu_item(&mut state, 3);
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
        let GitOp::StashPush { mode, paths, .. } = op else {
            panic!("{op:?}")
        };
        assert_eq!(mode, StashMode::All);
        assert_eq!(
            paths,
            [RepoPath::from("src/model.rs"), "docs/mode.md".into()]
        );
    }

    #[test]
    fn with_no_range_the_stash_takes_only_the_file_under_the_cursor() {
        let (mut state, _) = with_files(three_files(), 24);
        press_char(&mut state, 'j');
        stash_menu_item(&mut state, 3);
        assert!(!state.stash_draft.as_ref().unwrap().from_range);
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
        assert_eq!(
            op,
            GitOp::StashPush {
                mode: StashMode::All,
                message: None,
                paths: vec!["b.rs".into()],
                restore: vec![],
                unstage: vec![],
            }
        );
    }

    #[test]
    fn a_range_with_a_staged_deletion_and_an_untracked_file_is_refused() {
        let files = vec![changed("gone.rs", 'D', '.'), untracked("u.txt")];
        let (mut state, _) = with_files(files, 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        assert_eq!(stash_menu_item(&mut state, 3), vec![]);
        assert_eq!(state.notice, Some(keys::STASH_APART.into()));
        assert_eq!(state.stash_draft, None);
    }

    #[test]
    fn a_reload_that_clears_the_range_under_the_prompt_stashes_nothing() {
        let files = vec![
            changed("a.rs", '.', 'M'),
            changed("b.rs", '.', 'M'),
            changed("c.rs", '.', 'M'),
        ];
        let (mut state, _) = with_files(files.clone(), 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        stash_menu_item(&mut state, 3);
        reload(
            &mut state,
            vec![
                changed("a.rs", '.', 'M'),
                untracked("ab.rs"),
                changed("b.rs", '.', 'M'),
            ],
        );
        assert_eq!(state.files_anchor, None);
        type_text(&mut state, "late");
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice, Some(STASH_SET_CHANGED.into()));
        assert_eq!(state.stash_draft, None);
        assert_eq!(state.popups, []);
    }

    #[test]
    fn a_range_whose_files_changed_kind_under_the_prompt_stashes_nothing() {
        let (mut state, _) = with_files(
            vec![changed("a.rs", '.', 'M'), changed("b.rs", '.', 'M')],
            24,
        );
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        stash_menu_item(&mut state, 3);
        // Same paths, so the range stays, but b.rs is now staged as
        // deleted: the push would now need it unstaged first.
        reload(
            &mut state,
            vec![changed("a.rs", '.', 'M'), changed("b.rs", 'D', '.')],
        );
        assert!(state.files_anchor.is_some());
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice, Some(STASH_SET_CHANGED.into()));

        let (mut state, _) = with_files(
            vec![changed("a.rs", '.', 'M'), changed("b.rs", 'M', '.')],
            24,
        );
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        stash_menu_item(&mut state, 3);
        // b.rs's change moved to the worktree: same set, same push.
        reload(
            &mut state,
            vec![changed("a.rs", '.', 'M'), changed("b.rs", '.', 'M')],
        );
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
        assert!(matches!(op, GitOp::StashPush { .. }), "same set, same push");

        let (mut state, _) = with_files(
            vec![changed("a.rs", '.', 'M'), changed("b.rs", '.', 'M')],
            24,
        );
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        stash_menu_item(&mut state, 3);
        // b.rs became untracked: the push would now need -u.
        reload(
            &mut state,
            vec![changed("a.rs", '.', 'M'), untracked("b.rs")],
        );
        assert!(state.files_anchor.is_some());
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice, Some(STASH_SET_CHANGED.into()));
        assert!(state.files_anchor.is_some(), "the range stays to try again");
    }
}
