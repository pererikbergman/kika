//! The custom patch's side of `AppState`: picking a commit's files and
//! lines into it, the menu of rewrites and their questions, and what a
//! finished rewrite leaves behind.

use std::collections::BTreeSet;

use super::{AppState, Context, Focus};
use crate::custom_patch::{CustomPatch, FilePick, PatchAction, PatchChoice};
use crate::effect::Effect;
use crate::git::commit_files::CommitId;
use crate::git::diff::terminal_safe;
use crate::git::ops::GitOp;
use crate::git::patch::{FileKind, LineId, PatchError};
use crate::hunk_view::HunkView;
use crate::keymap;
use crate::layout::SidePanel;
use crate::popup::{ConfirmKind, LineInput, Popup, PromptKind};
use crate::view::elide_right;

/// Why resetting does nothing.
pub(super) const NO_PATCH: &str = "No custom patch";

/// How much of a new commit's message the question shows.
const MESSAGE_SHOWN: usize = 30;

/// Why "New commit after source" asks again.
pub(crate) const MESSAGE_NEEDED: &str = "Type a message for the new commit";

impl AppState {
    /// Focus the Diff pane on the open commit's selected file, its lines
    /// to be picked. Only one commit's change: not a comparison, a first
    /// commit or a merge.
    pub(super) fn focus_commit_diff(&mut self) -> Vec<Effect> {
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
        self.focus = Focus::Diff(HunkView::for_commit(file.path.clone(), open.range.clone()));
        let effects = self.load_patch();
        self.remap();
        effects
    }

    /// Pick the selected commit file whole, or drop it.
    pub(super) fn patch_toggle_file(&mut self) {
        if self.context != Context::Workspace || self.focus != Focus::Commits {
            return;
        }
        if let Some(reason) = keymap::patch_file_disabled(self) {
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
    pub(super) fn patch_toggle_lines(&mut self, lines: BTreeSet<LineId>) {
        let Focus::Diff(view) = &self.focus else {
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
            self.notice = Some(error.message().into());
            return;
        }
        if lines.is_empty() {
            return;
        }
        if let Some(reason) = keymap::other_commit_disabled(self, &range) {
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
    pub(super) fn choose_patch_action(&mut self, choice: PatchChoice) {
        if self.context != Context::Workspace {
            return;
        }
        if let Some(reason) = keymap::patch_rewrite_disabled(self, choice) {
            self.notice = Some(reason);
            return;
        }
        match keymap::patch_action(self, choice) {
            Ok(PatchAction::NewCommit(_)) => self.ask_patch_message(),
            Ok(action) => self
                .popups
                .push(Popup::Confirm(ConfirmKind::CustomPatch(action))),
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
    pub(super) fn patch_commit_message(&mut self, message: String) {
        if self.context != Context::Workspace {
            return;
        }
        if message.trim().is_empty() {
            self.notice = Some(MESSAGE_NEEDED.into());
            self.ask_patch_message();
            return;
        }
        if let Some(reason) = keymap::patch_rewrite_disabled(self, PatchChoice::NewCommit) {
            self.notice = Some(reason);
            return;
        }
        let action = PatchAction::NewCommit(message);
        self.popups
            .push(Popup::Confirm(ConfirmKind::CustomPatch(action)));
    }

    /// Run `action`, as the question asked. The plan is made again from
    /// the log as it is now, so a reload while asking is safe; `HEAD` is
    /// checked again just before git starts.
    pub(super) fn run_patch_action(&mut self, action: PatchAction) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if matches!(&action, PatchAction::NewCommit(message) if message.trim().is_empty()) {
            self.notice = Some(MESSAGE_NEEDED.into());
            return vec![];
        }
        let plan = match keymap::patch_action_disabled(self, &action) {
            Some(reason) => Err(reason),
            None => keymap::patch_plan(self, &action),
        };
        let head = self
            .commits
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
                self.notice = Some(keymap::PATCH_EMPTY.into());
                vec![]
            }
        }
    }

    /// The patch is in history now: drop it, and the source commit's
    /// files, whose commit was rewritten.
    pub(super) fn patch_rewritten(&mut self) -> Vec<Effect> {
        self.custom_patch = None;
        if matches!(&self.focus, Focus::Diff(view) if view.commit.is_some()) {
            self.focus = Focus::Commits;
            self.expanded = SidePanel::Commits;
        }
        self.close_commit_files()
    }

    /// The question for `action`: what moves from where to where.
    pub(super) fn patch_question(&self, action: &PatchAction) -> Option<String> {
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
