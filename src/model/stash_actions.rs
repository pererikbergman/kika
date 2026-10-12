//! The Stash panel's side of `AppState`: loading the list, opening an
//! entry to its files, the actions on the selected entry, and stashing
//! from the Files panel's menu.

use super::{AppState, Context, Focus};
use crate::diff_pane::DiffPane;
use crate::effect::{Effect, Tag};
use crate::git::diff::DiffRequest;
use crate::git::ops::{GitOp, StashMode, StashSet};
use crate::git::stash::{StashEntry, StashFile, StashRef};
use crate::keymap;
use crate::menu;
use crate::popup::{DeleteRangeConfirm, DropStashConfirm, LineInput, Popup, PromptKind, RangeKind};
use crate::stash::{StashDraft, StashFilesList};

impl AppState {
    /// Whether a commit's or a stash's files are open: the Diff pane is
    /// theirs, not the selected working-tree file's.
    pub(super) fn files_open(&self) -> bool {
        self.commits.open.is_some() || self.stash.open.is_some()
    }

    /// Load the stash list again, keeping what it shows until it lands.
    pub(super) fn reload_stashes(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        let tag = self.background_tag();
        self.stash.begin(tag.seq);
        vec![Effect::LoadStashes { tag }]
    }

    /// Open the selected stash's files in place of the list. A user
    /// effect, so the busy indicator shows while git lists them.
    pub(super) fn open_stash_files(&mut self) -> Vec<Effect> {
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
        self.busy += 1;
        let tag = Tag {
            generation: self.generation,
            seq,
            user: true,
        };
        vec![Effect::LoadStashFiles { tag, entry }]
    }

    /// Show the first file's diff, or nothing for an entry with no files.
    pub(super) fn stash_files_loaded(
        &mut self,
        tag: Tag,
        result: Result<Vec<StashFile>, String>,
    ) -> Vec<Effect> {
        // Every user effect reports back once, stale or not.
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
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

    pub(super) fn move_stash_file(&mut self, delta: isize) -> Vec<Effect> {
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
    pub(super) fn stash_file_diff(&mut self) -> Vec<Effect> {
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
    pub(super) fn close_stash_files(&mut self) -> Vec<Effect> {
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
        if let Some(reason) = keymap::stash_action_disabled(self) {
            self.notice = Some(reason);
            return None;
        }
        self.stash.selected_entry().cloned()
    }

    /// Apply or pop the selected entry, as `make` says.
    pub(super) fn stash_op(&mut self, make: fn(StashRef) -> GitOp) -> Vec<Effect> {
        match self.stash_for_action() {
            Some(entry) => self.start_op(Ok(Some(make(entry.stash_ref())))),
            None => vec![],
        }
    }

    /// Ask before dropping the selected entry: it leaves the list for good.
    pub(super) fn ask_drop_stash(&mut self) {
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
    pub(super) fn stashes_range_confirm(&self) -> DeleteRangeConfirm {
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
    pub(super) fn confirm_drop_stash(&mut self) -> Vec<Effect> {
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
    pub(super) fn ask_rename_stash(&mut self) {
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
    pub(super) fn rename_stash_to(&mut self, text: &str) -> Vec<Effect> {
        let Some(stash) = self.stash.pending.take() else {
            return vec![];
        };
        let message = text.trim();
        if message.is_empty() {
            self.notice = Some("A stash message cannot be empty".into());
            return vec![];
        }
        if self.op_seq.is_some() {
            self.notice = Some(keymap::OP_RUNNING.into());
            return vec![];
        }
        self.start_op(Ok(Some(GitOp::StashRename {
            stash,
            message: message.to_string(),
        })))
    }

    /// Fix what a whole-tree stash takes, then ask for its message.
    pub(super) fn ask_stash_message(&mut self, mode: StashMode) {
        let disabled = match mode {
            StashMode::All => menu::stash_all_disabled,
            StashMode::IncludeUntracked => menu::stash_untracked_disabled,
            StashMode::Staged => menu::stash_staged_disabled,
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
    pub(super) fn ask_stash_file_message(&mut self) {
        self.ask_stash_draft(menu::stash_file_disabled, |state| {
            let (entries, from_range) = state.files_marked();
            let set = GitOp::stash_files_for(&entries)?;
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
    pub(super) fn stash_message_answered(&mut self, text: &str) -> Vec<Effect> {
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
        if self.op_seq.is_some() {
            self.notice = Some(keymap::OP_RUNNING.into());
            return vec![];
        }
        // A range's set is checked again: a reload while the message was
        // typed may have cleared the range or changed what its files need,
        // and a push must never take a set other than the one shown.
        if from_range {
            let (entries, still_range) = self.files_marked();
            let now = still_range
                .then(|| GitOp::stash_files_for(&entries))
                .flatten();
            let drafted = StashSet {
                mode,
                paths: paths.clone(),
                restore: restore.clone(),
                unstage: unstage.clone(),
            };
            if now.as_ref() != Some(&drafted) {
                self.notice = Some(super::STASH_SET_CHANGED.into());
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
