use crate::commit_editor::CommitEditor;
use crate::diff_pane::DiffPane;
use crate::effect::{Effect, Tag};
use crate::git::FileEntry;
use crate::git::diff::{DiffChunk, DiffOptions, DiffRequest, DiffSide, MAX_CONTEXT, MIN_CONTEXT};
use crate::git::ops::GitOp;
use crate::git::patch::{self, Direction, FileKind, FilePatch, PatchSelection};
use crate::git::path::RepoPath;
use crate::git::status::{BranchStatus, EntryKind, RepoStatus};
use crate::handoff::HandOff;
use crate::hunk_view::{HunkView, NO_CHANGES};
use crate::intent::AppIntent;
use crate::keymap::{self, KeyScope, Resolution};
use crate::menu;
use crate::popup::{DiscardConfirm, DiscardWhat, ErrorPopup, LineInput, Popup};
use crate::refresh::{self, RefreshPlan, Scope, Selection};

/// What Kika is looking at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Context {
    /// The local working tree and index.
    Workspace,
    /// A remote pull/merge request.
    Review { number: u64 },
}

/// The commit popup. One popup at a time, so a state machine rather than a stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitFlow {
    Closed,
    /// Nothing is staged; asking whether to stage everything and commit.
    ConfirmStageAll,
    /// Reading `HEAD`'s message to amend it; `seq` is the load's.
    LoadingAmend {
        seq: u64,
    },
    Editing(CommitEditor),
    /// Asking whether to throw this typed message away.
    ConfirmDiscard(CommitEditor),
    /// `git commit` is running with this message; the text cannot change.
    Running(CommitEditor),
    /// `git commit` failed. The message is kept for another try.
    Failed {
        editor: CommitEditor,
        output: String,
    },
}

/// Which panel has the keys in the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Focus {
    Files,
    /// The Diff pane, working on the selected file's hunks.
    Diff(HunkView),
}

/// The single source of truth for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppState {
    pub context: Context,
    pub size: (u16, u16),
    pub should_quit: bool,
    /// Bumped when the repository changes, so results for the old one are dropped.
    pub generation: u64,
    /// Next sequence number to hand out to an effect.
    pub next_seq: u64,
    /// Sequence of the latest status load; only its result is applied.
    pub status_seq: Option<u64>,
    /// User-initiated effects still in flight.
    pub busy: u32,
    pub files: Vec<FileEntry>,
    /// False until the first status result arrives, to tell "loading" from "clean".
    pub files_loaded: bool,
    pub status_error: Option<String>,
    /// Branch, upstream and operations in progress. `None` until the first
    /// status load succeeds; a failed load keeps the last one.
    pub branch: Option<BranchStatus>,
    /// Index into `files` of the file whose diff is shown.
    pub selected: Option<usize>,
    pub diff: DiffPane,
    pub focus: Focus,
    /// Sequence of the Git operation in flight. Operations run one at a
    /// time so two quick presses never race for `index.lock`.
    pub op_seq: Option<u64>,
    /// The last operation's failure, or why one was refused.
    pub op_error: Option<String>,
    /// How the selection is kept when the pending status load lands.
    pub status_selection: Selection,
    pub help_open: bool,
    pub commit: CommitFlow,
    /// Why the last key did nothing, shown in the bottom bar until the next key.
    pub notice: Option<&'static str>,
    /// Menus, questions, prompts and errors, drawn bottom to top. The top
    /// one takes the keys; closing it shows the one below again.
    pub popups: Vec<Popup>,
    /// How diffs are drawn. Kept in memory for the session: it outlives file
    /// selection, focus changes and reloads.
    pub diff_options: DiffOptions,
}

impl AppState {
    pub fn new(context: Context) -> Self {
        Self {
            context,
            size: (0, 0),
            should_quit: false,
            generation: 0,
            next_seq: 0,
            status_seq: None,
            busy: 0,
            files: Vec::new(),
            files_loaded: false,
            status_error: None,
            branch: None,
            selected: None,
            diff: DiffPane::default(),
            focus: Focus::Files,
            op_seq: None,
            op_error: None,
            status_selection: Selection::KeepPath,
            help_open: false,
            commit: CommitFlow::Closed,
            notice: None,
            popups: Vec::new(),
            diff_options: DiffOptions::default(),
        }
    }

    /// Apply an intent and return the effects to run. Pure: no I/O, no rendering.
    pub fn update(&mut self, intent: AppIntent) -> Vec<Effect> {
        match intent {
            AppIntent::Key(key) => {
                self.notice = None;
                match keymap::resolve(keymap::registry(), self.key_focus(), self, key) {
                    Resolution::Run(binding) => return self.update(binding.intent.clone()),
                    Resolution::Disabled(reason) => self.notice = Some(reason),
                    // Only a prompt or the editor takes text; elsewhere
                    // unbound keys are dropped. A popup over the editor
                    // keeps the keys from it.
                    Resolution::Unbound => match self.popups.last_mut() {
                        Some(Popup::Prompt { input, .. }) => input.edit(key),
                        Some(_) => {}
                        None => {
                            if let CommitFlow::Editing(editor) = &mut self.commit {
                                editor.edit(key);
                            }
                        }
                    },
                }
            }
            AppIntent::Quit => {
                self.should_quit = true;
                // A hook or signing step may wait forever; quitting must not.
                if self.op_seq.is_some() {
                    return vec![Effect::InterruptGit];
                }
            }
            AppIntent::OpenHelp => self.help_open = true,
            AppIntent::CloseHelp => self.help_open = false,
            AppIntent::Resize { width, height } => {
                self.size = (width, height);
                return self.diff.demand(self.pane_rows());
            }
            AppIntent::RefreshStatus => {
                self.status_selection = Selection::KeepPath;
                return self.refresh_status();
            }
            AppIntent::StatusLoaded { tag, result } => return self.status_loaded(tag, result),
            AppIntent::SelectNext => return self.move_selection(1),
            AppIntent::SelectPrev => return self.move_selection(-1),
            AppIntent::ScrollDiff(by) => {
                let rows = self.pane_rows();
                if let Focus::Diff(view) = &mut self.focus {
                    view.scroll(by, rows);
                } else {
                    return self.diff.scroll(by, rows);
                }
            }
            AppIntent::DiffLoaded { tag, result } => self.diff_loaded(tag, result),
            AppIntent::DiffSlow { tag } => {
                if tag.generation == self.generation {
                    self.diff.slow(tag.seq);
                }
            }
            AppIntent::FocusDiff => return self.focus_diff(),
            AppIntent::FocusFiles => self.focus = Focus::Files,
            AppIntent::NextHunk => {
                let rows = self.pane_rows();
                if let Focus::Diff(view) = &mut self.focus {
                    view.next(rows);
                }
            }
            AppIntent::PrevHunk => {
                let rows = self.pane_rows();
                if let Focus::Diff(view) = &mut self.focus {
                    view.prev(rows);
                }
            }
            AppIntent::ToggleHunkStage => return self.toggle_hunk_stage(),
            AppIntent::EnterLineMode => self.with_lines(HunkView::enter_lines),
            AppIntent::ExitLineMode => self.with_lines(HunkView::exit_lines),
            AppIntent::NextLine { extend } => self.with_lines(|view| view.next_line(extend)),
            AppIntent::PrevLine { extend } => self.with_lines(|view| view.prev_line(extend)),
            AppIntent::ToggleLinesStage => return self.toggle_lines_stage(),
            AppIntent::ToggleDiffSide => return self.toggle_diff_side(),
            AppIntent::MoreContext => {
                let context = self.diff_options.context.saturating_add(1);
                return self.set_diff_options(DiffOptions {
                    context,
                    ..self.diff_options
                });
            }
            AppIntent::LessContext => {
                let context = self.diff_options.context.saturating_sub(1);
                return self.set_diff_options(DiffOptions {
                    context,
                    ..self.diff_options
                });
            }
            AppIntent::ToggleWhitespace => {
                return self.set_diff_options(DiffOptions {
                    ignore_whitespace: !self.diff_options.ignore_whitespace,
                    ..self.diff_options
                });
            }
            AppIntent::PatchLoaded { tag, result } => self.patch_loaded(tag, result),
            AppIntent::ToggleStage => {
                let op = self
                    .selected
                    .and_then(|i| self.files.get(i))
                    .and_then(GitOp::toggle_for);
                return self.start_op(Ok(op));
            }
            AppIntent::ToggleStageAll => return self.start_op(GitOp::toggle_all(&self.files)),
            AppIntent::Discard => self.ask_discard(),
            AppIntent::OpenCommit => self.open_commit(),
            AppIntent::ConfirmStageAll => {
                if self.commit == CommitFlow::ConfirmStageAll {
                    self.commit = CommitFlow::Editing(CommitEditor::new(true));
                }
            }
            AppIntent::CancelCommit => {
                let commit = std::mem::replace(&mut self.commit, CommitFlow::Closed);
                self.commit = match commit {
                    // An untouched amend has nothing typed to lose.
                    CommitFlow::Editing(editor)
                        if !editor.is_blank() && !editor.message_unchanged() =>
                    {
                        CommitFlow::ConfirmDiscard(editor)
                    }
                    CommitFlow::ConfirmStageAll
                    | CommitFlow::LoadingAmend { .. }
                    | CommitFlow::Editing(_) => CommitFlow::Closed,
                    other => other,
                };
            }
            AppIntent::DiscardCommit => {
                if matches!(self.commit, CommitFlow::ConfirmDiscard(_)) {
                    self.commit = CommitFlow::Closed;
                }
            }
            AppIntent::ResumeCommit => {
                let commit = std::mem::replace(&mut self.commit, CommitFlow::Closed);
                self.commit = match commit {
                    CommitFlow::ConfirmDiscard(editor) => CommitFlow::Editing(editor),
                    other => other,
                };
            }
            AppIntent::SubmitCommit => return self.submit_commit(),
            AppIntent::ToggleCommitField => {
                if let CommitFlow::Editing(editor) = &mut self.commit {
                    editor.toggle_field();
                }
            }
            AppIntent::OpenAmend => return self.open_amend(),
            AppIntent::HeadMessageLoaded { tag, result } => self.head_message_loaded(tag, result),
            AppIntent::ToggleNoVerify => {
                if let CommitFlow::Editing(editor) = &mut self.commit {
                    editor.no_verify = !editor.no_verify;
                }
            }
            AppIntent::EditCommitExternally => {
                if let CommitFlow::Editing(editor) = &self.commit {
                    let initial = editor.message();
                    return vec![Effect::HandOff(HandOff::EditCommitMessage { initial })];
                }
            }
            AppIntent::CommitMessageEdited { result } => {
                return self.commit_message_edited(result);
            }
            AppIntent::DismissCommitError => {
                let commit = std::mem::replace(&mut self.commit, CommitFlow::Closed);
                self.commit = match commit {
                    CommitFlow::Failed { editor, .. } => CommitFlow::Editing(editor),
                    other => other,
                };
            }
            AppIntent::OpFinished { tag, op, result } => return self.op_finished(tag, op, result),
            AppIntent::OpenMenu(id) => {
                if self.context == Context::Workspace {
                    self.popups.push(Popup::Menu { id, cursor: 0 });
                }
            }
            AppIntent::MenuNext => self.move_menu_cursor(1),
            AppIntent::MenuPrev => self.move_menu_cursor(-1),
            AppIntent::MenuSelect => return self.menu_select(),
            AppIntent::Ask(kind) => self.popups.push(Popup::Confirm(kind)),
            AppIntent::ConfirmYes => return self.answer(true),
            AppIntent::ConfirmNo => return self.answer(false),
            AppIntent::OpenPrompt(kind) => {
                if self.context == Context::Workspace {
                    self.popups.push(Popup::Prompt {
                        kind,
                        input: LineInput::default(),
                    });
                }
            }
            AppIntent::SubmitPrompt => {
                if let Some(Popup::Prompt { kind, input }) = self.popups.last() {
                    let submitted = kind.submit(input.text.clone());
                    self.popups.pop();
                    return self.update(submitted);
                }
            }
            AppIntent::GoToFile(text) => return self.go_to_file(&text),
            AppIntent::PopPopup => {
                self.popups.pop();
            }
            AppIntent::OpenInEditor => return self.open_in_editor(),
            AppIntent::EditHunk => return self.edit_hunk(),
            AppIntent::EditorClosed { result } => return self.editor_closed(result),
            AppIntent::HunkEdited { result } => return self.hunk_edited(result),
        }
        vec![]
    }

    /// The panel keys go to. Derived from the context until panels can
    /// take focus themselves.
    pub fn panel_focus(&self) -> KeyScope {
        match (&self.context, &self.focus) {
            (Context::Workspace, Focus::Files) => KeyScope::Files,
            (Context::Workspace, Focus::Diff(view)) if view.lines.is_some() => KeyScope::Lines,
            (Context::Workspace, Focus::Diff(_)) => KeyScope::Diff,
            (Context::Review { .. }, _) => KeyScope::Review,
        }
    }

    /// Where key lookup starts: the top popup, else the commit popup, else
    /// the help menu while one is open.
    pub fn key_focus(&self) -> KeyScope {
        match self.popups.last() {
            None => {}
            Some(Popup::Menu { .. }) => return KeyScope::Menu,
            Some(Popup::Confirm(_)) => return KeyScope::Confirm,
            Some(Popup::Prompt { .. }) => return KeyScope::Prompt,
            Some(Popup::Error(_)) => return KeyScope::Error,
            Some(Popup::Discard(_)) => return KeyScope::DiscardConfirm,
        }
        match self.commit {
            CommitFlow::Closed => {}
            CommitFlow::ConfirmStageAll => return KeyScope::CommitConfirm,
            CommitFlow::LoadingAmend { .. } => return KeyScope::CommitLoading,
            CommitFlow::ConfirmDiscard(_) => return KeyScope::CommitDiscard,
            CommitFlow::Editing(_) | CommitFlow::Running(_) => return KeyScope::CommitEditor,
            CommitFlow::Failed { .. } => return KeyScope::CommitError,
        }
        if self.help_open {
            KeyScope::Help
        } else {
            self.panel_focus()
        }
    }

    /// Text rows inside the Diff pane: the screen minus the status bar and borders.
    pub fn pane_rows(&self) -> usize {
        usize::from(self.size.1).saturating_sub(3).max(1)
    }

    fn move_menu_cursor(&mut self, delta: isize) {
        if let Some(Popup::Menu { id, cursor }) = self.popups.last_mut() {
            let last = menu::items(*id).len().saturating_sub(1);
            *cursor = cursor.saturating_add_signed(delta).min(last);
        }
    }

    /// Run the item under the top menu's cursor. A submenu opens on top;
    /// anything else closes every menu first, so what it opens is on top.
    fn menu_select(&mut self) -> Vec<Effect> {
        let Some(&Popup::Menu { id, cursor }) = self.popups.last() else {
            return vec![];
        };
        let Some(item) = menu::items(id).get(cursor) else {
            return vec![];
        };
        if let Some(reason) = (item.disabled)(self) {
            self.notice = Some(reason);
            return vec![];
        }
        if !matches!(item.intent, AppIntent::OpenMenu(_)) {
            while matches!(self.popups.last(), Some(Popup::Menu { .. })) {
                self.popups.pop();
            }
        }
        self.update(item.intent.clone())
    }

    /// Close the question on top and run its answer, if it has one.
    fn answer(&mut self, yes: bool) -> Vec<Effect> {
        match self.popups.pop() {
            Some(Popup::Confirm(kind)) => {
                let answer = if yes { Some(kind.yes()) } else { kind.no() };
                answer.map_or_else(Vec::new, |intent| self.update(intent))
            }
            Some(Popup::Discard(confirm)) if yes => self.start_op(Ok(Some(confirm.op))),
            Some(Popup::Discard(_)) => vec![],
            // Not a question: leave it where it was.
            other => {
                self.popups.extend(other);
                vec![]
            }
        }
    }

    /// Ask before throwing away the selected file, hunk or lines. Nothing
    /// is discarded here; a refusal says why in the bar instead of asking.
    fn ask_discard(&mut self) {
        if self.context != Context::Workspace || self.op_seq.is_some() {
            return;
        }
        match self.discard_confirm() {
            Ok(Some(confirm)) => self.popups.push(Popup::Discard(confirm)),
            Ok(None) => {}
            Err(reason) => self.notice = Some(reason),
        }
    }

    /// What `d` would discard from the current focus, fixed now.
    fn discard_confirm(&self) -> Result<Option<DiscardConfirm>, &'static str> {
        let Some(entry) = self.selected.and_then(|i| self.files.get(i)) else {
            return Ok(None);
        };
        // Deleting a file is said as such, whatever was selected in it.
        let confirm = |what, path: &RepoPath, op: GitOp| DiscardConfirm {
            what: if matches!(op, GitOp::DeleteUntracked(_)) {
                DiscardWhat::Untracked
            } else {
                what
            },
            path: path.display().to_string(),
            op,
        };
        let view = match &self.focus {
            Focus::Files => {
                let op = GitOp::discard_for(entry)?;
                return Ok(op.map(|op| confirm(DiscardWhat::File, &entry.path, op)));
            }
            Focus::Diff(view) => view,
        };
        if self.diff_options.ignore_whitespace {
            return Err(keymap::WHITESPACE_HIDDEN);
        }
        if view.side == DiffSide::Index {
            return Err("Unstage first to discard");
        }
        if entry.kind == EntryKind::Conflicted {
            return Err("Resolve the conflict first");
        }
        let Some(file_patch) = &view.patch else {
            return Err(NO_CHANGES);
        };
        let (selection, what) = match view.lines {
            Some(_) => (
                PatchSelection::Lines(view.selected_lines()),
                DiscardWhat::Lines,
            ),
            None => (PatchSelection::Hunk(view.hunk), DiscardWhat::Hunk),
        };
        // The worktree side is the reverse patch's new side, as for unstaging.
        let built =
            patch::build(file_patch, &selection, Direction::Unstage).map_err(|e| e.message())?;
        // The file's whole change: discard the file itself, as staging
        // falls back to `git add`.
        let op = if patch::selects_everything(file_patch, &selection) {
            GitOp::discard_for(entry)?
        } else {
            Some(GitOp::ApplyWorktree { patch: built })
        };
        Ok(op.map(|op| confirm(what, &view.path, op)))
    }

    /// Hand the terminal to the editor on the selected file: at the
    /// selected change from the Diff pane, at the top from the Files panel.
    fn open_in_editor(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace || self.op_seq.is_some() {
            return vec![];
        }
        let Some(entry) = self.selected.and_then(|i| self.files.get(i)) else {
            return vec![];
        };
        if entry.worktree == 'D' || entry.index == 'D' {
            self.notice = Some("The file is deleted");
            return vec![];
        }
        let (path, line) = match &self.focus {
            Focus::Files => (entry.path.clone(), None),
            Focus::Diff(view) => (view.path.clone(), view.editor_line()),
        };
        vec![Effect::HandOff(HandOff::OpenFile { path, line })]
    }

    /// Hand the selected unstaged hunk to the editor; what comes back is
    /// staged. Only plain changes to a file: other headers need more than
    /// a recount to apply.
    fn edit_hunk(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace || self.op_seq.is_some() {
            return vec![];
        }
        let Focus::Diff(view) = &self.focus else {
            return vec![];
        };
        if view.lines.is_some() {
            return vec![];
        }
        if self.diff_options.ignore_whitespace {
            self.notice = Some(keymap::WHITESPACE_HIDDEN);
            return vec![];
        }
        if view.side == DiffSide::Index {
            self.notice = Some("Edit hunks from the unstaged view");
            return vec![];
        }
        let Some(file_patch) = &view.patch else {
            return vec![];
        };
        if file_patch.kind != FileKind::Modified {
            self.notice = Some("Only hunks of changed files can be edited");
            return vec![];
        }
        if file_patch.hunks.is_empty() {
            self.notice = Some(NO_CHANGES);
            return vec![];
        }
        match patch::build(
            file_patch,
            &PatchSelection::Hunk(view.hunk),
            Direction::Stage,
        ) {
            Ok(built) => vec![Effect::HandOff(HandOff::edit_hunk(&built))],
            Err(error) => {
                self.notice = Some(error.message());
                vec![]
            }
        }
    }

    /// Back from the editor: the file may have changed, so reload.
    fn editor_closed(&mut self, result: Result<(), String>) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        match result {
            Ok(()) => {
                self.status_selection = Selection::KeepPath;
                self.refresh_status()
            }
            Err(message) => {
                self.push_error(ErrorPopup::new(
                    "Could not open the editor",
                    &message,
                    "Check core.editor, $VISUAL or $EDITOR, then try again.",
                ));
                vec![]
            }
        }
    }

    /// Back from editing a hunk: stage what the user left, if anything.
    fn hunk_edited(&mut self, result: Result<Option<Vec<u8>>, String>) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        match result {
            Ok(Some(patch)) => self.start_op(Ok(Some(GitOp::ApplyEdited { patch }))),
            Ok(None) => {
                self.notice = Some("Edit cancelled");
                vec![]
            }
            Err(message) => {
                self.push_error(ErrorPopup::new(
                    "Could not edit the hunk",
                    &message,
                    "Nothing was staged. Fix the cause above, then try again.",
                ));
                vec![]
            }
        }
    }

    /// Select the first file whose path contains `text`, ignoring case.
    fn go_to_file(&mut self, text: &str) -> Vec<Effect> {
        let needle = text.trim().to_lowercase();
        if needle.is_empty() {
            return vec![];
        }
        let found = self
            .files
            .iter()
            .position(|f| f.path.display().to_lowercase().contains(&needle));
        match found {
            Some(index) => self.select(index),
            None => {
                self.notice = Some("No file matches");
                vec![]
            }
        }
    }

    /// Show an error, unless the same one is already on top: a repeated
    /// failure should not need dismissing twice.
    fn push_error(&mut self, error: ErrorPopup) {
        if self.popups.last() != Some(&Popup::Error(error.clone())) {
            self.popups.push(Popup::Error(error));
        }
    }

    fn take_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    fn move_selection(&mut self, delta: isize) -> Vec<Effect> {
        let Some(current) = self.selected else {
            return vec![];
        };
        let next = current
            .saturating_add_signed(delta)
            .min(self.files.len().saturating_sub(1));
        if next == current {
            return vec![];
        }
        self.select(next)
    }

    /// Select file `index` and start loading its diff. With the Diff pane
    /// focused, its patch reloads too, on the same side while the file is
    /// the same one.
    fn select(&mut self, index: usize) -> Vec<Effect> {
        self.selected = Some(index);
        let request = DiffRequest::for_entry(&self.files[index], self.diff_options);
        let seq = self.take_seq();
        let want = self.diff.begin(seq, &request.path, self.pane_rows());
        // Diff streams report many times, so they stay out of the busy count.
        let tag = Tag {
            generation: self.generation,
            seq,
            user: false,
        };
        let mut effects = vec![
            Effect::LoadDiff {
                tag,
                request: request.clone(),
                want,
            },
            Effect::DiffLoadingDelay { tag },
        ];
        if let Focus::Diff(view) = &mut self.focus {
            // A file that stopped (or started) being untracked has other
            // sides now, so the remembered one no longer applies.
            let untracked = request.side == DiffSide::Untracked;
            if view.path != request.path || (view.side == DiffSide::Untracked) != untracked {
                *view = HunkView::new(request.path, request.side);
            }
            effects.extend(self.load_patch());
        }
        effects
    }

    /// Take new diff options, with the context clamped to its limits, and
    /// reload the selected file's diff with them. `select` keeps the scroll
    /// and the hunk cursor, since the file is the same one.
    fn set_diff_options(&mut self, options: DiffOptions) -> Vec<Effect> {
        let options = DiffOptions {
            context: options.context.clamp(MIN_CONTEXT, MAX_CONTEXT),
            ..options
        };
        if options == self.diff_options {
            return vec![];
        }
        self.diff_options = options;
        match self.selected {
            Some(index) if index < self.files.len() => self.select(index),
            _ => vec![],
        }
    }

    /// Focus the Diff pane on the selected file, on the side its diff shows.
    fn focus_diff(&mut self) -> Vec<Effect> {
        let Some(entry) = self.selected.and_then(|i| self.files.get(i)) else {
            return vec![];
        };
        let request = DiffRequest::for_entry(entry, self.diff_options);
        self.focus = Focus::Diff(HunkView::new(request.path, request.side));
        self.load_patch()
    }

    /// Start loading the focused view's patch. A user effect, so the busy
    /// indicator shows and a refused key can say "Loading…".
    fn load_patch(&mut self) -> Vec<Effect> {
        let seq = self.take_seq();
        let Focus::Diff(view) = &mut self.focus else {
            return vec![];
        };
        view.seq = Some(seq);
        self.busy += 1;
        let tag = Tag {
            generation: self.generation,
            seq,
            user: true,
        };
        // Renames are off in patch diffs, so the original path is not needed.
        let request = DiffRequest {
            path: view.path.clone(),
            orig_path: None,
            side: view.side,
            options: self.diff_options,
        };
        vec![Effect::LoadPatch { tag, request }]
    }

    fn patch_loaded(&mut self, tag: Tag, result: Result<FilePatch, String>) {
        // Every user effect reports back once, stale or not.
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        let rows = self.pane_rows();
        let Focus::Diff(view) = &mut self.focus else {
            return;
        };
        if tag.generation != self.generation || view.seq != Some(tag.seq) {
            return;
        }
        view.seq = None;
        match result {
            Ok(patch) => view.loaded(patch, rows),
            Err(message) => {
                view.patch = None;
                self.push_error(ErrorPopup::new(
                    "Could not read the diff",
                    &message,
                    "Close this, fix the cause, then press r to retry.",
                ));
            }
        }
    }

    /// Change the focused view's line cursor, keeping it on screen.
    fn with_lines(&mut self, change: impl FnOnce(&mut HunkView)) {
        let rows = self.pane_rows();
        if let Focus::Diff(view) = &mut self.focus {
            change(view);
            view.scroll_to_cursor(rows);
        }
    }

    fn toggle_hunk_stage(&mut self) -> Vec<Effect> {
        match &self.focus {
            Focus::Diff(view) => self.toggle_selection_stage(PatchSelection::Hunk(view.hunk)),
            Focus::Files => vec![],
        }
    }

    fn toggle_lines_stage(&mut self) -> Vec<Effect> {
        match &self.focus {
            Focus::Diff(view) => {
                self.toggle_selection_stage(PatchSelection::Lines(view.selected_lines()))
            }
            Focus::Files => vec![],
        }
    }

    /// Stage the selected hunk or lines, or unstage them from the staged
    /// view. When that is the file's whole change, the file is staged
    /// instead: the same change, without a patch that could fail to apply.
    fn toggle_selection_stage(&mut self, selection: PatchSelection) -> Vec<Effect> {
        let Focus::Diff(view) = &self.focus else {
            return vec![];
        };
        if self.diff_options.ignore_whitespace {
            self.notice = Some(keymap::WHITESPACE_HIDDEN);
            return vec![];
        }
        let Some(file_patch) = &view.patch else {
            return vec![];
        };
        let direction = if view.side == DiffSide::Index {
            Direction::Unstage
        } else {
            Direction::Stage
        };
        let built = match patch::build(file_patch, &selection, direction) {
            Ok(built) => built,
            Err(error) => {
                self.notice = Some(error.message());
                return vec![];
            }
        };
        let staged = view.side == DiffSide::Index;
        let op = if patch::selects_everything(file_patch, &selection) {
            let path = vec![view.path.clone()];
            if staged {
                GitOp::Unstage(path)
            } else {
                GitOp::Stage(path)
            }
        } else {
            GitOp::ApplyCached {
                patch: built,
                reverse: staged,
            }
        };
        self.start_op(Ok(Some(op)))
    }

    /// Flip the focused view between unstaged and staged changes.
    fn toggle_diff_side(&mut self) -> Vec<Effect> {
        let Focus::Diff(view) = &mut self.focus else {
            return vec![];
        };
        let side = match view.side {
            DiffSide::Worktree => DiffSide::Index,
            DiffSide::Index => DiffSide::Worktree,
            DiffSide::Untracked => return vec![],
        };
        *view = HunkView::new(view.path.clone(), side);
        self.load_patch()
    }

    /// Keep the selection on the same file across a refresh when it still
    /// exists, else near the same place.
    fn reselect(&mut self, old_path: Option<RepoPath>) -> Vec<Effect> {
        if self.files.is_empty() {
            self.selected = None;
            self.diff = DiffPane::default();
            self.focus = Focus::Files;
            return vec![Effect::CancelDiff];
        }
        let index = old_path
            .and_then(|path| self.files.iter().position(|f| f.path == path))
            .or_else(|| self.selected.map(|i| i.min(self.files.len() - 1)))
            .unwrap_or(0);
        self.select(index)
    }

    fn diff_loaded(&mut self, tag: Tag, result: Result<DiffChunk, String>) {
        if tag.generation == self.generation {
            self.diff.apply(tag.seq, result);
        }
    }

    /// Run the operation a stage key chose. Only one runs at a time, and
    /// only on the local workspace.
    fn start_op(&mut self, op: Result<Option<GitOp>, String>) -> Vec<Effect> {
        if self.context != Context::Workspace || self.op_seq.is_some() {
            return vec![];
        }
        self.op_error = None;
        let op = match op {
            Ok(Some(op)) => op,
            Ok(None) => return vec![],
            Err(message) => {
                self.op_error = Some(message);
                return vec![];
            }
        };
        let seq = self.take_seq();
        self.op_seq = Some(seq);
        self.busy += 1;
        let tag = Tag {
            generation: self.generation,
            seq,
            user: true,
        };
        vec![Effect::RunOp { tag, op }]
    }

    /// Open the commit popup: straight to the editor when something is
    /// staged, else ask whether to stage everything first.
    fn open_commit(&mut self) {
        if self.context != Context::Workspace || self.commit != CommitFlow::Closed {
            return;
        }
        let staged = self.files.iter().any(|f| !matches!(f.index, '.' | '?'));
        self.commit = if staged {
            CommitFlow::Editing(CommitEditor::new(false))
        } else {
            CommitFlow::ConfirmStageAll
        };
    }

    /// Open the popup to amend `HEAD`, once its message is read. Nothing
    /// is asked about staging: with nothing staged, only the message changes.
    fn open_amend(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace || self.commit != CommitFlow::Closed {
            return vec![];
        }
        let seq = self.take_seq();
        self.busy += 1;
        self.commit = CommitFlow::LoadingAmend { seq };
        vec![Effect::LoadHeadMessage {
            tag: Tag {
                generation: self.generation,
                seq,
                user: true,
            },
        }]
    }

    fn head_message_loaded(&mut self, tag: Tag, result: Result<String, String>) {
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        let current = CommitFlow::LoadingAmend { seq: tag.seq };
        if tag.generation != self.generation || self.commit != current {
            return;
        }
        match result {
            Ok(message) => self.commit = CommitFlow::Editing(CommitEditor::amend(&message)),
            Err(message) => {
                self.commit = CommitFlow::Closed;
                self.push_error(ErrorPopup::new(
                    "Could not read the last commit",
                    &message,
                    "Nothing was changed. Fix the cause above, then try again.",
                ));
            }
        }
    }

    fn submit_commit(&mut self) -> Vec<Effect> {
        let CommitFlow::Editing(editor) = &self.commit else {
            return vec![];
        };
        if editor.subject.trim().is_empty() {
            self.notice = Some("Write a subject first");
            return vec![];
        }
        // Git keeps `HEAD`'s own message, byte for byte, when it is untouched.
        let message = (!editor.message_unchanged()).then(|| editor.message());
        self.start_commit(message)
    }

    /// Commit the editor's choices with `message`, and wait for git with
    /// the text kept.
    fn start_commit(&mut self, message: Option<String>) -> Vec<Effect> {
        let CommitFlow::Editing(editor) = &self.commit else {
            return vec![];
        };
        let editor = editor.clone();
        let op = GitOp::Commit {
            message,
            stage_all: editor.stage_all,
            amend: editor.amend.is_some(),
            no_verify: editor.no_verify,
        };
        let effects = self.start_op(Ok(Some(op)));
        if !effects.is_empty() {
            self.commit = CommitFlow::Running(editor);
        }
        effects
    }

    /// Back from writing the message in the editor: commit it as written,
    /// or keep the popup as it was when there is nothing to commit.
    fn commit_message_edited(&mut self, result: Result<Option<String>, String>) -> Vec<Effect> {
        let CommitFlow::Editing(editor) = &self.commit else {
            return vec![];
        };
        match result {
            Ok(Some(message)) => {
                self.commit = CommitFlow::Editing(CommitEditor {
                    stage_all: editor.stage_all,
                    no_verify: editor.no_verify,
                    amend: editor.amend.clone(),
                    ..CommitEditor::from_message(&message)
                });
                self.start_commit(Some(message))
            }
            Ok(None) => {
                self.notice = Some("Empty message; commit aborted");
                vec![]
            }
            Err(message) => {
                self.push_error(ErrorPopup::new(
                    "Could not open the editor",
                    &message,
                    "Your message is kept. Check core.editor, $VISUAL or $EDITOR, then try again.",
                ));
                vec![]
            }
        }
    }

    fn op_finished(&mut self, tag: Tag, op: GitOp, result: Result<(), String>) -> Vec<Effect> {
        // As with status loads: every user effect reports once, stale or not.
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        if Some(tag.seq) == self.op_seq {
            self.op_seq = None;
        }
        let commit = matches!(op, GitOp::Commit { .. });
        if tag.generation != self.generation {
            // The result is for another repository; give the message back.
            if commit {
                self.end_commit(None);
            }
            return vec![];
        }
        if commit {
            // A commit's failure shows in its own popup, not the status bar.
            self.end_commit(Some(result));
        } else {
            // A failed git command gets a popup; `op_error` is for refusals.
            self.op_error = None;
            if let Err(message) = result {
                self.push_error(ErrorPopup::new(
                    failure_title(&op),
                    &message,
                    "The file list was reloaded. Fix the cause above, then try again.",
                ));
            }
        }
        // Refresh even after a failure: a failed `add` of several paths can
        // still have changed the index.
        self.apply_plan(refresh::plan(&op))
    }

    /// Leave `Running`: closed on success, `Failed` with the output on
    /// failure, back to `Editing` when there is no result to show.
    fn end_commit(&mut self, result: Option<Result<(), String>>) {
        let commit = std::mem::replace(&mut self.commit, CommitFlow::Closed);
        let CommitFlow::Running(editor) = commit else {
            self.commit = commit;
            return;
        };
        self.commit = match result {
            Some(Ok(())) => CommitFlow::Closed,
            Some(Err(output)) => CommitFlow::Failed { editor, output },
            None => CommitFlow::Editing(editor),
        };
    }

    /// Reload what the plan says is stale. One status load serves both the
    /// files and the branch; reloading files reselects, which reloads the
    /// diff too.
    fn apply_plan(&mut self, plan: RefreshPlan) -> Vec<Effect> {
        if plan.scopes.contains(&Scope::Files) || plan.scopes.contains(&Scope::Branch) {
            self.status_selection = plan.selection;
            return self.refresh_status();
        }
        match self.selected {
            Some(index) if plan.scopes.contains(&Scope::Diff) => self.select(index),
            _ => vec![],
        }
    }

    fn refresh_status(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        self.status_seq = Some(seq);
        self.busy += 1;
        vec![Effect::LoadStatus {
            tag: Tag {
                generation: self.generation,
                seq,
                user: true,
            },
        }]
    }

    fn status_loaded(&mut self, tag: Tag, result: Result<RepoStatus, String>) -> Vec<Effect> {
        // Every user effect reports back exactly once, stale or not, so
        // decrement before the staleness check or the counter would leak.
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        if tag.generation != self.generation || Some(tag.seq) != self.status_seq {
            return vec![];
        }
        self.files_loaded = true;
        match result {
            Ok(status) => {
                let old_path = self
                    .selected
                    .and_then(|i| self.files.get(i))
                    .map(|f| f.path.clone());
                self.files = status.files;
                self.branch = Some(status.branch);
                self.status_error = None;
                match self.status_selection {
                    Selection::KeepPath => self.reselect(old_path),
                }
            }
            Err(message) => {
                self.push_error(ErrorPopup::new(
                    "Could not read the repository status",
                    &message,
                    "Close this, fix the cause, then press r to retry.",
                ));
                self.status_error = Some(message);
                vec![]
            }
        }
    }
}

/// What the error popup calls a failed operation.
fn failure_title(op: &GitOp) -> &'static str {
    match op {
        GitOp::Stage(_) => "Could not stage",
        GitOp::Unstage(_) => "Could not unstage",
        GitOp::StageAll => "Could not stage everything",
        GitOp::UnstageAll => "Could not unstage everything",
        GitOp::Commit { amend: true, .. } => "Could not amend",
        GitOp::Commit { .. } => "Could not commit",
        GitOp::ApplyCached { reverse: false, .. } => "Could not stage the change",
        GitOp::ApplyCached { reverse: true, .. } => "Could not unstage the change",
        GitOp::DiscardWorktree(_) => "Could not discard the changes",
        GitOp::DeleteUntracked(_) => "Could not delete the file",
        GitOp::ApplyWorktree { .. } => "Could not discard the change",
        GitOp::ApplyEdited { .. } => "Could not stage the edited hunk",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit_editor::{CommitEditor, Field};
    use crate::diff_pane::DIFF_PREFETCH;
    use crate::git::diff::{DiffLine, DiffSide, LineKind};
    use crate::git::status::{BranchStatus, EntryKind, Head, RepoStatus};
    use crate::intent::Scroll;
    use crate::keymap::KeyPress;
    use crate::menu::MenuId;
    use crate::popup::{ConfirmKind, DiscardConfirm, DiscardWhat, PromptKind};
    use crossterm::event::{KeyCode, KeyModifiers};

    fn untracked(path: &str) -> FileEntry {
        FileEntry {
            path: path.into(),
            orig_path: None,
            index: '?',
            worktree: '?',
            kind: EntryKind::Untracked,
        }
    }

    fn on_branch(name: &str) -> BranchStatus {
        BranchStatus {
            head: Head::Branch(name.into()),
            upstream: None,
            ahead_behind: None,
            in_progress: vec![],
        }
    }

    /// A status load of `files` on branch `main`.
    fn repo(files: Vec<FileEntry>) -> RepoStatus {
        RepoStatus {
            branch: on_branch("main"),
            files,
        }
    }

    fn loaded(tag: Tag, paths: &[&str]) -> AppIntent {
        AppIntent::StatusLoaded {
            tag,
            result: Ok(repo(paths.iter().map(|p| untracked(p)).collect())),
        }
    }

    fn refresh_tag(state: &mut AppState) -> Tag {
        match state.update(AppIntent::RefreshStatus).as_slice() {
            [Effect::LoadStatus { tag }] => *tag,
            other => panic!("expected one LoadStatus, got {other:?}"),
        }
    }

    #[test]
    fn quitting_while_an_op_runs_interrupts_git() {
        let mut state = AppState::new(Context::Workspace);
        state.op_seq = Some(4);
        assert_eq!(state.update(AppIntent::Quit), vec![Effect::InterruptGit]);
        assert!(state.should_quit);
    }

    #[test]
    fn quit_sets_should_quit() {
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(state.update(AppIntent::Quit), vec![]);
        assert!(state.should_quit);
    }

    #[test]
    fn resize_records_size() {
        let mut state = AppState::new(Context::Workspace);
        state.update(AppIntent::Resize {
            width: 120,
            height: 40,
        });
        assert_eq!(state.size, (120, 40));
    }

    #[test]
    fn refresh_in_workspace_requests_a_status_load_with_increasing_seq() {
        let mut state = AppState::new(Context::Workspace);
        state.generation = 7;

        let first = refresh_tag(&mut state);
        let second = refresh_tag(&mut state);

        assert_eq!(
            first,
            Tag {
                generation: 7,
                seq: 0,
                user: true
            }
        );
        assert_eq!(
            second,
            Tag {
                generation: 7,
                seq: 1,
                user: true
            }
        );
    }

    #[test]
    fn refresh_in_review_does_nothing() {
        let mut state = AppState::new(Context::Review { number: 3 });
        assert_eq!(state.update(AppIntent::RefreshStatus), vec![]);
        assert_eq!(state.busy, 0);
    }

    #[test]
    fn latest_status_result_is_applied() {
        let mut state = AppState::new(Context::Workspace);
        let tag = refresh_tag(&mut state);

        state.update(loaded(tag, &["a.txt"]));

        assert!(state.files_loaded);
        assert_eq!(state.files, [untracked("a.txt")]);
    }

    #[test]
    fn result_for_superseded_seq_is_dropped() {
        let mut state = AppState::new(Context::Workspace);
        let old = refresh_tag(&mut state);
        let new = refresh_tag(&mut state);

        state.update(loaded(old, &["stale.txt"]));
        assert!(!state.files_loaded);
        assert_eq!(state.files, []);

        state.update(loaded(new, &["fresh.txt"]));
        assert_eq!(state.files, [untracked("fresh.txt")]);

        state.update(loaded(old, &["stale.txt"]));
        assert_eq!(state.files, [untracked("fresh.txt")]);
    }

    #[test]
    fn result_from_older_generation_is_dropped() {
        let mut state = AppState::new(Context::Workspace);
        let tag = refresh_tag(&mut state);
        state.generation += 1;

        state.update(loaded(tag, &["other-repo.txt"]));

        assert!(!state.files_loaded);
        assert_eq!(state.files, []);
    }

    fn loaded_on(tag: Tag, branch: &str) -> AppIntent {
        AppIntent::StatusLoaded {
            tag,
            result: Ok(RepoStatus {
                branch: on_branch(branch),
                files: vec![],
            }),
        }
    }

    #[test]
    fn the_branch_is_unknown_until_the_first_load() {
        assert_eq!(AppState::new(Context::Workspace).branch, None);
    }

    #[test]
    fn a_fresh_status_load_sets_the_branch() {
        let mut state = AppState::new(Context::Workspace);
        let tag = refresh_tag(&mut state);
        state.update(loaded_on(tag, "topic"));
        assert_eq!(state.branch, Some(on_branch("topic")));
    }

    #[test]
    fn a_stale_status_load_leaves_the_branch_alone() {
        let mut state = AppState::new(Context::Workspace);
        let old = refresh_tag(&mut state);
        let new = refresh_tag(&mut state);
        state.update(loaded_on(new, "fresh"));
        state.update(loaded_on(old, "stale"));
        assert_eq!(state.branch, Some(on_branch("fresh")));

        let tag = refresh_tag(&mut state);
        state.generation += 1;
        state.update(loaded_on(tag, "other-repo"));
        assert_eq!(state.branch, Some(on_branch("fresh")));
    }

    #[test]
    fn a_failed_status_load_keeps_the_old_branch() {
        let mut state = AppState::new(Context::Workspace);
        let tag = refresh_tag(&mut state);
        state.update(loaded_on(tag, "main"));
        let tag = refresh_tag(&mut state);
        state.update(AppIntent::StatusLoaded {
            tag,
            result: Err("boom".into()),
        });
        assert_eq!(state.branch, Some(on_branch("main")));
        assert_eq!(state.status_error.as_deref(), Some("boom"));
    }

    #[test]
    fn a_branch_only_plan_loads_status_once() {
        let (mut state, _) = with_files(three_files(), 24);
        let busy = state.busy;
        let effects = state.apply_plan(RefreshPlan {
            scopes: [Scope::Branch].into(),
            selection: Selection::KeepPath,
        });
        assert!(
            matches!(effects.as_slice(), [Effect::LoadStatus { .. }]),
            "{effects:?}"
        );
        assert_eq!(state.busy, busy + 1);
    }

    #[test]
    fn error_result_is_recorded_and_cleared_by_the_next_success() {
        let mut state = AppState::new(Context::Workspace);
        let tag = refresh_tag(&mut state);
        state.update(AppIntent::StatusLoaded {
            tag,
            result: Err("fatal: not a git repository".into()),
        });
        assert!(state.files_loaded);
        assert_eq!(
            state.status_error.as_deref(),
            Some("fatal: not a git repository")
        );

        let tag = refresh_tag(&mut state);
        state.update(loaded(tag, &["a.txt"]));
        assert_eq!(state.status_error, None);
        assert_eq!(state.files, [untracked("a.txt")]);
    }

    #[test]
    fn busy_rises_per_refresh_and_falls_per_result_even_when_stale() {
        let mut state = AppState::new(Context::Workspace);
        let old = refresh_tag(&mut state);
        assert_eq!(state.busy, 1);
        let new = refresh_tag(&mut state);
        assert_eq!(state.busy, 2);

        state.update(loaded(old, &[]));
        assert_eq!(state.busy, 1);
        state.update(loaded(new, &[]));
        assert_eq!(state.busy, 0);
    }

    #[test]
    fn busy_never_underflows() {
        let mut state = AppState::new(Context::Workspace);
        let tag = Tag {
            generation: 0,
            seq: 0,
            user: true,
        };
        state.update(loaded(tag, &[]));
        assert_eq!(state.busy, 0);
    }

    #[test]
    fn non_user_results_leave_busy_alone() {
        let mut state = AppState::new(Context::Workspace);
        let mut tag = refresh_tag(&mut state);
        tag.user = false;
        state.update(loaded(tag, &[]));
        assert_eq!(state.busy, 1);
    }

    fn changed(path: &str, index: char, worktree: char) -> FileEntry {
        FileEntry {
            path: path.into(),
            orig_path: None,
            index,
            worktree,
            kind: EntryKind::Changed,
        }
    }

    /// A workspace of `height` rows whose status load listed `files`, with
    /// the first file's diff already requested. Returns that request's effects.
    fn with_files(files: Vec<FileEntry>, height: u16) -> (AppState, Vec<Effect>) {
        let mut state = AppState::new(Context::Workspace);
        state.update(AppIntent::Resize { width: 80, height });
        let tag = refresh_tag(&mut state);
        let effects = state.update(AppIntent::StatusLoaded {
            tag,
            result: Ok(repo(files)),
        });
        (state, effects)
    }

    fn three_files() -> Vec<FileEntry> {
        vec![
            changed("a.rs", '.', 'M'),
            changed("b.rs", 'M', '.'),
            untracked("c.txt"),
        ]
    }

    fn diff_tag(effects: &[Effect]) -> Tag {
        match effects {
            [
                Effect::LoadDiff { tag, .. },
                Effect::DiffLoadingDelay { tag: delay },
            ] => {
                assert_eq!(tag, delay);
                *tag
            }
            other => panic!("expected LoadDiff + DiffLoadingDelay, got {other:?}"),
        }
    }

    fn line(text: &str) -> DiffLine {
        DiffLine {
            kind: LineKind::Context,
            text: text.into(),
        }
    }

    fn chunk(tag: Tag, n: usize, done: bool) -> AppIntent {
        AppIntent::DiffLoaded {
            tag,
            result: Ok(DiffChunk {
                lines: (0..n).map(|i| line(&format!("{}:{i}", tag.seq))).collect(),
                done,
            }),
        }
    }

    #[test]
    fn a_status_load_selects_the_first_file_and_requests_its_diff() {
        // Height 24: 21 diff rows.
        let (state, effects) = with_files(three_files(), 24);
        assert_eq!(state.selected, Some(0));
        let tag = diff_tag(&effects);
        assert_eq!(
            effects[0],
            Effect::LoadDiff {
                tag,
                request: DiffRequest {
                    path: "a.rs".into(),
                    orig_path: None,
                    side: DiffSide::Worktree,
                    options: Default::default(),
                },
                want: 21 + DIFF_PREFETCH,
            }
        );
        assert!(!tag.user, "diff streams stay out of the busy count");
        assert!(state.diff.pending);
    }

    #[test]
    fn select_next_loads_the_next_file_and_its_chunk_replaces_the_lines() {
        let (mut state, effects) = with_files(three_files(), 24);
        let first = diff_tag(&effects);
        state.update(chunk(first, 3, true));
        assert_eq!(state.diff.lines.len(), 3);

        let effects = state.update(AppIntent::SelectNext);
        let second = diff_tag(&effects);
        assert!(second.seq > first.seq);
        assert_eq!(state.selected, Some(1));
        match &effects[0] {
            Effect::LoadDiff { request, .. } => {
                assert_eq!(
                    request,
                    &DiffRequest::for_entry(&state.files[1], DiffOptions::default())
                )
            }
            other => panic!("{other:?}"),
        }
        // The old lines stay until the new ones arrive.
        assert_eq!(state.diff.lines.len(), 3);

        state.update(chunk(second, 2, true));
        let seq = second.seq;
        assert_eq!(
            state.diff.lines,
            [line(&format!("{seq}:0")), line(&format!("{seq}:1"))]
        );
        assert!(!state.diff.pending);
        assert!(state.diff.done);
    }

    #[test]
    fn later_chunks_append() {
        let (mut state, effects) = with_files(three_files(), 24);
        let tag = diff_tag(&effects);
        state.update(chunk(tag, 5, false));
        state.update(chunk(tag, 4, true));
        assert_eq!(state.diff.lines.len(), 9);
    }

    #[test]
    fn selection_stops_at_the_ends_of_the_list() {
        let (mut state, _) = with_files(three_files(), 24);
        assert_eq!(state.update(AppIntent::SelectPrev), vec![]);
        assert_eq!(state.selected, Some(0));

        state.update(AppIntent::SelectNext);
        state.update(AppIntent::SelectNext);
        assert_eq!(state.update(AppIntent::SelectNext), vec![]);
        assert_eq!(state.selected, Some(2));

        diff_tag(&state.update(AppIntent::SelectPrev));
        assert_eq!(state.selected, Some(1));
    }

    #[test]
    fn selection_does_nothing_without_files() {
        let (mut state, effects) = with_files(vec![], 24);
        assert_eq!(effects, vec![Effect::CancelDiff]);
        assert_eq!(state.selected, None);
        assert_eq!(state.update(AppIntent::SelectNext), vec![]);
        assert_eq!(state.update(AppIntent::SelectPrev), vec![]);
    }

    #[test]
    fn superseded_diff_results_never_appear() {
        let (mut state, effects) = with_files(three_files(), 24);
        let first = diff_tag(&effects);
        let second = diff_tag(&state.update(AppIntent::SelectNext));
        let before = state.diff.clone();

        state.update(chunk(first, 3, true));
        state.update(AppIntent::DiffSlow { tag: first });
        assert_eq!(state.diff, before);

        let current_generation = Tag {
            generation: second.generation + 1,
            ..second
        };
        state.generation += 1;
        let after_bump = state.diff.clone();
        state.update(chunk(second, 3, true));
        state.update(AppIntent::DiffSlow { tag: second });
        assert_eq!(state.diff, after_bump);
        // Sanity: the tag for the new generation would have applied.
        state.update(chunk(current_generation, 1, true));
        assert_eq!(state.diff.lines.len(), 1);
    }

    #[test]
    fn a_refresh_keeps_the_selected_path_and_its_scroll() {
        let (mut state, effects) = with_files(three_files(), 10);
        state.update(chunk(diff_tag(&effects), 50, true));
        state.update(AppIntent::SelectNext);
        let tag = diff_tag(&state.update(AppIntent::SelectNext));
        state.update(chunk(tag, 50, true));
        state.update(AppIntent::ScrollDiff(Scroll::LineDown));
        assert_eq!(state.selected, Some(2));

        // c.txt moved to the front; the selection follows it.
        let status = refresh_tag(&mut state);
        let effects = state.update(AppIntent::StatusLoaded {
            tag: status,
            result: Ok(repo(vec![untracked("c.txt"), changed("a.rs", '.', 'M')])),
        });
        diff_tag(&effects);
        assert_eq!(state.selected, Some(0));
        assert_eq!(state.diff.scroll, 1);
    }

    #[test]
    fn a_refresh_that_drops_the_selected_file_clamps_and_resets_scroll() {
        let (mut state, _) = with_files(three_files(), 10);
        state.update(AppIntent::SelectNext);
        let tag = diff_tag(&state.update(AppIntent::SelectNext));
        state.update(chunk(tag, 50, true));
        state.update(AppIntent::ScrollDiff(Scroll::LineDown));

        let status = refresh_tag(&mut state);
        let effects = state.update(AppIntent::StatusLoaded {
            tag: status,
            result: Ok(repo(vec![
                changed("a.rs", '.', 'M'),
                changed("z.rs", '.', 'M'),
            ])),
        });
        diff_tag(&effects);
        assert_eq!(state.selected, Some(1));
        assert_eq!(state.diff.path, Some("z.rs".into()));
        assert_eq!(state.diff.scroll, 0);
    }

    #[test]
    fn a_refresh_to_a_clean_tree_clears_the_diff_and_cancels() {
        let (mut state, effects) = with_files(three_files(), 10);
        state.update(chunk(diff_tag(&effects), 5, true));

        let status = refresh_tag(&mut state);
        let effects = state.update(loaded(status, &[]));

        assert_eq!(effects, vec![Effect::CancelDiff]);
        assert_eq!(state.selected, None);
        assert_eq!(state.diff, DiffPane::default());
    }

    #[test]
    fn a_failed_refresh_keeps_the_selection_and_diff() {
        let (mut state, effects) = with_files(three_files(), 10);
        state.update(chunk(diff_tag(&effects), 5, true));
        let status = refresh_tag(&mut state);
        let effects = state.update(AppIntent::StatusLoaded {
            tag: status,
            result: Err("boom".into()),
        });
        assert_eq!(effects, vec![]);
        assert_eq!(state.selected, Some(0));
        assert_eq!(state.diff.lines.len(), 5);
    }

    #[test]
    fn a_diff_error_is_stored_and_ends_the_stream() {
        let (mut state, effects) = with_files(three_files(), 10);
        let tag = diff_tag(&effects);
        state.update(AppIntent::DiffLoaded {
            tag,
            result: Err("fatal: oops".into()),
        });
        assert_eq!(state.diff.error.as_deref(), Some("fatal: oops"));
        assert!(state.diff.done);
        assert!(!state.diff.pending);
        assert_eq!(
            state.update(AppIntent::ScrollDiff(Scroll::PageDown)),
            vec![]
        );
    }

    #[test]
    fn scrolling_past_the_prefetch_window_demands_more() {
        // Height 13: 10 rows, so the first request wants 210 lines.
        let (mut state, effects) = with_files(three_files(), 13);
        let tag = diff_tag(&effects);
        state.update(chunk(tag, 210, false));
        assert_eq!(state.diff.requested, 210);

        // Still inside the window: no effect.
        assert_eq!(state.update(AppIntent::ScrollDiff(Scroll::LineUp)), vec![]);
        assert_eq!(state.diff.scroll, 0);

        let effects = state.update(AppIntent::ScrollDiff(Scroll::PageDown));
        assert_eq!(state.diff.scroll, 10);
        assert_eq!(
            effects,
            vec![Effect::DiffDemand {
                seq: tag.seq,
                want: 220
            }]
        );
        assert_eq!(state.diff.requested, 220);

        // The same position again asks for nothing new.
        state.update(AppIntent::ScrollDiff(Scroll::LineUp));
        assert_eq!(
            state.update(AppIntent::ScrollDiff(Scroll::LineDown)),
            vec![]
        );
    }

    #[test]
    fn a_finished_diff_demands_nothing_and_scroll_is_clamped() {
        let (mut state, effects) = with_files(three_files(), 13);
        state.update(chunk(diff_tag(&effects), 15, true));

        for _ in 0..5 {
            assert_eq!(
                state.update(AppIntent::ScrollDiff(Scroll::PageDown)),
                vec![]
            );
        }
        assert_eq!(state.diff.scroll, 14);
        state.update(AppIntent::ScrollDiff(Scroll::PageUp));
        state.update(AppIntent::ScrollDiff(Scroll::PageUp));
        assert_eq!(state.diff.scroll, 0);
    }

    #[test]
    fn a_taller_terminal_demands_more_lines() {
        let (mut state, effects) = with_files(three_files(), 13);
        let tag = diff_tag(&effects);
        state.update(chunk(tag, 210, false));
        let effects = state.update(AppIntent::Resize {
            width: 80,
            height: 53,
        });
        assert_eq!(
            effects,
            vec![Effect::DiffDemand {
                seq: tag.seq,
                want: 250
            }]
        );
        assert_eq!(
            state.update(AppIntent::Resize {
                width: 80,
                height: 13
            }),
            vec![]
        );
    }

    #[test]
    fn slow_before_any_chunk_shows_loading() {
        let (mut state, effects) = with_files(three_files(), 13);
        let tag = diff_tag(&effects);
        state.update(AppIntent::DiffSlow { tag });
        assert!(state.diff.show_loading);

        state.update(chunk(tag, 1, true));
        assert!(!state.diff.show_loading);
    }

    #[test]
    fn slow_after_the_first_chunk_does_nothing() {
        let (mut state, effects) = with_files(three_files(), 13);
        let tag = diff_tag(&effects);
        state.update(chunk(tag, 1, false));
        state.update(AppIntent::DiffSlow { tag });
        assert!(!state.diff.show_loading);
    }

    #[test]
    fn pane_rows_never_drops_below_one() {
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(state.pane_rows(), 1);
        state.update(AppIntent::Resize {
            width: 1,
            height: 2,
        });
        assert_eq!(state.pane_rows(), 1);
    }

    fn press(state: &mut AppState, code: KeyCode) -> Vec<Effect> {
        state.update(AppIntent::Key(KeyPress::new(code, KeyModifiers::NONE)))
    }

    #[test]
    fn a_bound_key_runs_its_intent() {
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(press(&mut state, KeyCode::Char('r')).len(), 1);
        assert_eq!(state.busy, 1);
        press(&mut state, KeyCode::Char('q'));
        assert!(state.should_quit);
    }

    #[test]
    fn question_mark_opens_help_and_closes_it_again() {
        let mut state = AppState::new(Context::Workspace);
        press(&mut state, KeyCode::Char('?'));
        assert!(state.help_open);
        assert_eq!(state.key_focus(), KeyScope::Help);
        press(&mut state, KeyCode::Char('?'));
        assert!(!state.help_open);
    }

    #[test]
    fn esc_closes_help_instead_of_quitting() {
        let mut state = AppState::new(Context::Workspace);
        press(&mut state, KeyCode::Char('?'));
        press(&mut state, KeyCode::Esc);
        assert!(!state.help_open);
        assert!(!state.should_quit);
        press(&mut state, KeyCode::Esc);
        assert!(state.should_quit);
    }

    #[test]
    fn panel_keys_do_nothing_while_help_is_open_but_q_still_quits() {
        let (mut state, _) = with_files(three_files(), 24);
        press(&mut state, KeyCode::Char('?'));
        assert_eq!(press(&mut state, KeyCode::Char('j')), vec![]);
        assert_eq!(state.selected, Some(0));
        press(&mut state, KeyCode::Char('q'));
        assert!(state.should_quit);
    }

    #[test]
    fn space_while_an_op_runs_shows_why_and_does_nothing() {
        let (mut state, _) = with_files(three_files(), 24);
        state.op_seq = Some(99);
        let busy = state.busy;
        assert_eq!(press(&mut state, KeyCode::Char(' ')), vec![]);
        assert_eq!(state.notice, Some("An operation is running"));
        assert_eq!(state.busy, busy);
    }

    #[test]
    fn space_without_a_selection_says_so() {
        let (mut state, _) = with_files(vec![], 24);
        assert_eq!(press(&mut state, KeyCode::Char(' ')), vec![]);
        assert_eq!(state.notice, Some("No file selected"));
    }

    #[test]
    fn a_with_a_conflict_shows_the_reason_as_a_notice_not_an_error() {
        let conflict = FileEntry {
            kind: EntryKind::Conflicted,
            ..changed("both.rs", 'U', 'U')
        };
        let (mut state, _) = with_files(vec![changed("a.rs", '.', 'M'), conflict], 24);
        assert_eq!(press(&mut state, KeyCode::Char('a')), vec![]);
        assert_eq!(
            state.notice,
            Some("Resolve conflicts before staging everything")
        );
        assert_eq!(state.op_error, None);
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn the_next_key_clears_the_notice() {
        let (mut state, _) = with_files(vec![], 24);
        press(&mut state, KeyCode::Char(' '));
        assert!(state.notice.is_some());
        press(&mut state, KeyCode::Char('x'));
        assert_eq!(state.notice, None);
    }

    #[test]
    fn space_on_a_stageable_file_runs_the_op() {
        let (mut state, _) = with_files(three_files(), 24);
        let (_, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        assert_eq!(op, GitOp::Stage(vec!["a.rs".into()]));
        assert_eq!(state.notice, None);
    }

    fn op_of(effects: &[Effect]) -> (Tag, GitOp) {
        match effects {
            [Effect::RunOp { tag, op }] => (*tag, op.clone()),
            other => panic!("expected one RunOp, got {other:?}"),
        }
    }

    fn select_path(state: &mut AppState, path: &str) {
        while state.files[state.selected.unwrap()].path != path.into() {
            state.update(AppIntent::SelectNext);
        }
    }

    #[test]
    fn toggle_stage_on_an_unstaged_file_runs_one_user_op_and_raises_busy() {
        let (mut state, _) = with_files(three_files(), 24);
        let busy = state.busy;

        let (tag, op) = op_of(&state.update(AppIntent::ToggleStage));

        assert_eq!(op, GitOp::Stage(vec!["a.rs".into()]));
        assert!(tag.user);
        assert_eq!(tag.generation, state.generation);
        assert_eq!(state.op_seq, Some(tag.seq));
        assert_eq!(state.busy, busy + 1);
    }

    #[test]
    fn toggle_stage_on_a_staged_file_unstages_it() {
        let (mut state, _) = with_files(three_files(), 24);
        select_path(&mut state, "b.rs");
        let (_, op) = op_of(&state.update(AppIntent::ToggleStage));
        assert_eq!(op, GitOp::Unstage(vec!["b.rs".into()]));
    }

    #[test]
    fn toggle_stage_all_stages_or_unstages_everything() {
        let (mut state, _) = with_files(three_files(), 24);
        let (_, op) = op_of(&state.update(AppIntent::ToggleStageAll));
        assert_eq!(op, GitOp::StageAll);

        let (mut state, _) = with_files(vec![changed("b.rs", 'M', '.')], 24);
        let (_, op) = op_of(&state.update(AppIntent::ToggleStageAll));
        assert_eq!(op, GitOp::UnstageAll);
    }

    #[test]
    fn toggle_stage_all_with_conflicts_refuses_with_an_error() {
        let conflict = FileEntry {
            kind: EntryKind::Conflicted,
            ..changed("both.rs", 'U', 'U')
        };
        let (mut state, _) = with_files(vec![changed("a.rs", '.', 'M'), conflict], 24);
        let busy = state.busy;

        assert_eq!(state.update(AppIntent::ToggleStageAll), vec![]);

        assert_eq!(
            state.op_error.as_deref(),
            Some("Resolve conflicts before staging everything")
        );
        assert_eq!(state.op_seq, None);
        assert_eq!(state.busy, busy);
    }

    #[test]
    fn toggle_stage_on_a_conflicted_file_does_nothing() {
        let conflict = FileEntry {
            kind: EntryKind::Conflicted,
            ..changed("both.rs", 'U', 'U')
        };
        let (mut state, _) = with_files(vec![conflict], 24);
        assert_eq!(state.update(AppIntent::ToggleStage), vec![]);
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn a_second_toggle_while_one_is_in_flight_does_nothing() {
        let (mut state, _) = with_files(three_files(), 24);
        op_of(&state.update(AppIntent::ToggleStage));
        let busy = state.busy;

        assert_eq!(state.update(AppIntent::ToggleStage), vec![]);
        assert_eq!(state.update(AppIntent::ToggleStageAll), vec![]);
        assert_eq!(state.busy, busy);
    }

    #[test]
    fn toggles_do_nothing_in_review_or_without_files() {
        let mut review = AppState::new(Context::Review { number: 1 });
        review.files = three_files();
        review.selected = Some(0);
        let (mut clean, _) = with_files(vec![], 24);
        for state in [&mut review, &mut clean] {
            assert_eq!(state.update(AppIntent::ToggleStage), vec![]);
            assert_eq!(state.update(AppIntent::ToggleStageAll), vec![]);
            assert_eq!(state.op_seq, None);
            assert_eq!(state.op_error, None);
        }
    }

    #[test]
    fn the_next_stage_key_clears_an_old_error() {
        let (mut state, _) = with_files(three_files(), 24);
        state.op_error = Some("old".into());
        op_of(&state.update(AppIntent::ToggleStage));
        assert_eq!(state.op_error, None);
    }

    fn finished(tag: Tag, op: GitOp, result: Result<(), String>) -> AppIntent {
        AppIntent::OpFinished { tag, op, result }
    }

    #[test]
    fn a_finished_op_refreshes_status_and_keeps_the_selected_path() {
        let (mut state, _) = with_files(three_files(), 24);
        select_path(&mut state, "b.rs");
        let (tag, op) = op_of(&state.update(AppIntent::ToggleStage));
        let busy = state.busy;

        let effects = state.update(finished(tag, op, Ok(())));
        // The op's slot is released and the status load takes one.
        assert_eq!(state.busy, busy);
        assert_eq!(state.op_seq, None);
        let status = match effects.as_slice() {
            [Effect::LoadStatus { tag }] => *tag,
            other => panic!("expected one LoadStatus, got {other:?}"),
        };

        // b.rs is now unstaged and moved to the front.
        let effects = state.update(AppIntent::StatusLoaded {
            tag: status,
            result: Ok(repo(vec![
                changed("b.rs", '.', 'M'),
                changed("a.rs", '.', 'M'),
            ])),
        });
        diff_tag(&effects);
        assert_eq!(state.selected, Some(0));
        match &effects[0] {
            Effect::LoadDiff { request, .. } => assert_eq!(
                request,
                &DiffRequest {
                    path: "b.rs".into(),
                    orig_path: None,
                    side: DiffSide::Worktree,
                    options: Default::default(),
                }
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_failed_op_records_the_error_and_still_refreshes() {
        let (mut state, _) = with_files(three_files(), 24);
        let (tag, op) = op_of(&state.update(AppIntent::ToggleStage));

        let effects = state.update(finished(tag, op, Err("fatal: index.lock".into())));

        match state.popups.as_slice() {
            [Popup::Error(error)] => {
                assert_eq!(error.title, "Could not stage");
                assert_eq!(error.message, "fatal: index.lock");
                assert!(!error.next.is_empty());
            }
            other => panic!("expected one error popup, got {other:?}"),
        }
        assert_eq!(state.op_error, None);
        assert_eq!(state.op_seq, None);
        assert!(
            matches!(effects.as_slice(), [Effect::LoadStatus { .. }]),
            "{effects:?}"
        );
    }

    #[test]
    fn a_successful_op_clears_an_earlier_error() {
        let (mut state, _) = with_files(three_files(), 24);
        let (tag, op) = op_of(&state.update(AppIntent::ToggleStage));
        state.op_error = Some("left over".into());
        state.update(finished(tag, op, Ok(())));
        assert_eq!(state.op_error, None);
    }

    #[test]
    fn a_stale_generation_op_lowers_busy_and_does_nothing_else() {
        let (mut state, _) = with_files(three_files(), 24);
        let (tag, op) = op_of(&state.update(AppIntent::ToggleStage));
        let busy = state.busy;
        state.generation += 1;

        let effects = state.update(finished(tag, op, Err("boom".into())));

        assert_eq!(effects, vec![]);
        assert_eq!(state.busy, busy - 1);
        assert_eq!(state.op_error, None);
        assert_eq!(state.op_seq, None, "the slot is freed even when stale");
    }

    #[test]
    fn a_diff_only_plan_reloads_the_selected_diff() {
        let (mut state, _) = with_files(three_files(), 24);
        select_path(&mut state, "b.rs");
        let effects = state.apply_plan(RefreshPlan {
            scopes: [Scope::Diff].into(),
            selection: Selection::KeepPath,
        });
        diff_tag(&effects);
        assert_eq!(state.selected, Some(1));
        assert_eq!(state.diff.path, Some("b.rs".into()));
    }

    fn ctrl(state: &mut AppState, c: char) -> Vec<Effect> {
        state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char(c),
            KeyModifiers::CONTROL,
        )))
    }

    fn type_text(state: &mut AppState, text: &str) {
        for c in text.chars() {
            let code = if c == '\n' {
                KeyCode::Enter
            } else {
                KeyCode::Char(c)
            };
            press(state, code);
        }
    }

    fn editor(state: &AppState) -> &CommitEditor {
        match &state.commit {
            CommitFlow::Editing(editor) => editor,
            other => panic!("expected Editing, got {other:?}"),
        }
    }

    /// A workspace with a staged file, the commit popup open on it.
    fn editing() -> AppState {
        let (mut state, _) = with_files(three_files(), 24);
        press(&mut state, KeyCode::Char('c'));
        editor(&state);
        state
    }

    #[test]
    fn typing_a_message_and_ctrl_s_runs_the_commit() {
        let mut state = editing();
        let busy = state.busy;
        type_text(&mut state, "subj\nline 1\nline 2");

        let (tag, op) = op_of(&ctrl(&mut state, 's'));

        assert_eq!(
            op,
            GitOp::Commit {
                message: Some("subj\n\nline 1\nline 2\n".into()),
                stage_all: false,
                amend: false,
                no_verify: false,
            }
        );
        assert!(tag.user);
        assert_eq!(state.op_seq, Some(tag.seq));
        assert_eq!(state.busy, busy + 1);
        assert!(matches!(&state.commit, CommitFlow::Running(e) if e.subject == "subj"));
    }

    #[test]
    fn an_empty_subject_is_refused_with_a_notice() {
        let mut state = editing();
        type_text(&mut state, "  ");
        assert_eq!(ctrl(&mut state, 's'), vec![]);
        assert_eq!(state.notice, Some("Write a subject first"));
        assert_eq!(state.op_seq, None);
        assert_eq!(editor(&state).subject, "  ");
    }

    #[test]
    fn with_nothing_staged_c_asks_to_stage_all_and_y_opens_the_editor() {
        let files = vec![changed("a.rs", '.', 'M'), untracked("n.txt")];
        let (mut state, _) = with_files(files, 24);
        press(&mut state, KeyCode::Char('c'));
        assert_eq!(state.commit, CommitFlow::ConfirmStageAll);
        assert_eq!(state.key_focus(), KeyScope::CommitConfirm);

        press(&mut state, KeyCode::Char('y'));
        assert!(editor(&state).stage_all);
        type_text(&mut state, "all");
        let (_, op) = op_of(&ctrl(&mut state, 's'));
        assert_eq!(
            op,
            GitOp::Commit {
                message: Some("all\n".into()),
                stage_all: true,
                amend: false,
                no_verify: false,
            }
        );
    }

    #[test]
    fn enter_also_confirms_staging_everything() {
        let (mut state, _) = with_files(vec![changed("a.rs", '.', 'M')], 24);
        press(&mut state, KeyCode::Char('c'));
        press(&mut state, KeyCode::Enter);
        assert!(editor(&state).stage_all);
    }

    #[test]
    fn n_or_esc_declines_staging_everything_and_nothing_runs() {
        for key in [KeyCode::Char('n'), KeyCode::Esc] {
            let (mut state, _) = with_files(vec![changed("a.rs", '.', 'M')], 24);
            press(&mut state, KeyCode::Char('c'));
            assert_eq!(press(&mut state, key), vec![]);
            assert_eq!(state.commit, CommitFlow::Closed);
            assert!(!state.should_quit);
        }
    }

    #[test]
    fn a_staged_file_opens_the_editor_directly() {
        let state = editing();
        assert!(!editor(&state).stage_all);
        assert_eq!(state.key_focus(), KeyScope::CommitEditor);
    }

    #[test]
    fn c_with_a_conflict_or_no_files_is_disabled_with_a_reason() {
        let conflict = FileEntry {
            kind: EntryKind::Conflicted,
            ..changed("both.rs", 'U', 'U')
        };
        let (mut state, _) = with_files(vec![changed("a.rs", 'M', '.'), conflict], 24);
        press(&mut state, KeyCode::Char('c'));
        assert_eq!(state.notice, Some("Resolve conflicts before committing"));
        assert_eq!(state.commit, CommitFlow::Closed);

        let (mut state, _) = with_files(vec![], 24);
        press(&mut state, KeyCode::Char('c'));
        assert_eq!(state.notice, Some("Nothing to commit"));

        let (mut state, _) = with_files(three_files(), 24);
        state.op_seq = Some(5);
        press(&mut state, KeyCode::Char('c'));
        assert_eq!(state.notice, Some("An operation is running"));
        assert_eq!(state.commit, CommitFlow::Closed);
    }

    #[test]
    fn q_and_question_mark_are_typed_and_esc_cancels_while_editing() {
        let mut state = editing();
        type_text(&mut state, "q?");
        assert!(!state.should_quit);
        assert!(!state.help_open);
        assert_eq!(editor(&state).subject, "q?");

        // The message is not empty, so Esc asks before throwing it away.
        assert_eq!(press(&mut state, KeyCode::Esc), vec![]);
        assert!(matches!(state.commit, CommitFlow::ConfirmDiscard(_)));
        assert_eq!(state.key_focus(), KeyScope::CommitDiscard);
        assert!(!state.should_quit);
    }

    #[test]
    fn esc_on_an_empty_message_closes_at_once() {
        let mut state = editing();
        type_text(&mut state, " \n ");
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.commit, CommitFlow::Closed);
    }

    #[test]
    fn y_discards_the_message_and_n_or_esc_goes_back_to_it() {
        for key in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut state = editing();
            type_text(&mut state, "keep me");
            press(&mut state, KeyCode::Esc);
            assert_eq!(press(&mut state, key), vec![]);
            assert_eq!(editor(&state).subject, "keep me");
        }
        let mut state = editing();
        type_text(&mut state, "drop me");
        press(&mut state, KeyCode::Esc);
        // Other keys, Enter included, neither discard nor quit.
        press(&mut state, KeyCode::Enter);
        press(&mut state, KeyCode::Char('q'));
        assert!(matches!(state.commit, CommitFlow::ConfirmDiscard(_)));
        assert!(!state.should_quit);
        press(&mut state, KeyCode::Char('y'));
        assert_eq!(state.commit, CommitFlow::Closed);
    }

    #[test]
    fn ctrl_c_quits_from_the_editor() {
        let mut state = editing();
        type_text(&mut state, "abc");
        assert_eq!(ctrl(&mut state, 'c'), vec![]);
        assert!(state.should_quit);
    }

    #[test]
    fn ctrl_c_quits_while_the_commit_runs_and_interrupts_git() {
        let (mut state, _, _) = submitted();
        assert_eq!(ctrl(&mut state, 'c'), vec![Effect::InterruptGit]);
        assert!(state.should_quit);
    }

    #[test]
    fn tab_moves_between_the_fields() {
        let mut state = editing();
        type_text(&mut state, "s");
        press(&mut state, KeyCode::Tab);
        assert_eq!(editor(&state).field, Field::Body);
        state.update(AppIntent::ToggleCommitField);
        assert_eq!(editor(&state).field, Field::Subject);
    }

    /// Submit "subj" with a body and return the op's tag and op.
    fn submitted() -> (AppState, Tag, GitOp) {
        let mut state = editing();
        type_text(&mut state, "subj\nbody");
        let (tag, op) = op_of(&ctrl(&mut state, 's'));
        (state, tag, op)
    }

    #[test]
    fn keys_do_nothing_while_the_commit_runs() {
        let (mut state, _, _) = submitted();
        let before = state.commit.clone();
        type_text(&mut state, "xq");
        press(&mut state, KeyCode::Esc);
        assert_eq!(ctrl(&mut state, 's'), vec![]);
        assert_eq!(state.commit, before);
        assert_eq!(state.notice, Some("An operation is running"));
        assert!(!state.should_quit);
    }

    #[test]
    fn a_successful_commit_closes_the_popup_and_refreshes() {
        let (mut state, tag, op) = submitted();
        let effects = state.update(finished(tag, op, Ok(())));
        assert_eq!(state.commit, CommitFlow::Closed);
        assert_eq!(state.op_seq, None);
        assert!(
            matches!(effects.as_slice(), [Effect::LoadStatus { .. }]),
            "{effects:?}"
        );
    }

    #[test]
    fn a_failed_commit_shows_its_output_and_esc_returns_to_the_kept_message() {
        let (mut state, tag, op) = submitted();
        let effects = state.update(finished(tag, op, Err("lint failed".into())));

        assert_eq!(state.op_error, None);
        assert!(
            matches!(effects.as_slice(), [Effect::LoadStatus { .. }]),
            "{effects:?}"
        );
        match &state.commit {
            CommitFlow::Failed { editor, output } => {
                assert_eq!(output, "lint failed");
                assert_eq!(editor.subject, "subj");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(state.key_focus(), KeyScope::CommitError);
        // Other keys are dropped while the error shows.
        press(&mut state, KeyCode::Char('q'));
        assert!(!state.should_quit);

        press(&mut state, KeyCode::Esc);
        let editor = editor(&state);
        assert_eq!(editor.subject, "subj");
        assert_eq!(editor.body, ["body"]);
    }

    #[test]
    fn enter_also_dismisses_the_commit_error() {
        let (mut state, tag, op) = submitted();
        state.update(finished(tag, op, Err("no".into())));
        press(&mut state, KeyCode::Enter);
        editor(&state);
    }

    #[test]
    fn a_stale_commit_result_returns_to_editing_without_a_refresh() {
        let (mut state, tag, op) = submitted();
        let busy = state.busy;
        state.generation += 1;
        let effects = state.update(finished(tag, op, Err("boom".into())));
        assert_eq!(effects, vec![]);
        assert_eq!(state.busy, busy - 1);
        assert_eq!(state.op_seq, None);
        assert_eq!(editor(&state).subject, "subj");
    }

    fn head_load(effects: &[Effect]) -> Tag {
        match effects {
            [Effect::LoadHeadMessage { tag }] => *tag,
            other => panic!("expected one LoadHeadMessage, got {other:?}"),
        }
    }

    /// `A` on a clean tree with commits: the popup is loading HEAD's message.
    fn loading_amend() -> (AppState, Tag) {
        let (mut state, _) = with_files(vec![], 24);
        let busy = state.busy;
        let tag = head_load(&press(&mut state, KeyCode::Char('A')));
        assert!(tag.user);
        assert_eq!(state.busy, busy + 1);
        assert_eq!(state.commit, CommitFlow::LoadingAmend { seq: tag.seq });
        assert_eq!(state.key_focus(), KeyScope::CommitLoading);
        (state, tag)
    }

    fn head_loaded(tag: Tag, result: Result<&str, &str>) -> AppIntent {
        AppIntent::HeadMessageLoaded {
            tag,
            result: result.map(Into::into).map_err(Into::into),
        }
    }

    /// Amending HEAD's "subj\n\nbody", the popup open on it.
    fn amending() -> AppState {
        let (mut state, tag) = loading_amend();
        let busy = state.busy;
        assert_eq!(state.update(head_loaded(tag, Ok("subj\n\nbody"))), vec![]);
        assert_eq!(state.busy, busy - 1);
        state
    }

    #[test]
    fn a_shift_a_loads_heads_message_into_the_popup() {
        let state = amending();
        let editor = editor(&state);
        assert_eq!(editor.subject, "subj");
        assert_eq!(editor.body, ["body"]);
        assert!(editor.amend.is_some());
        assert!(!editor.stage_all, "an amend never stages everything");
        assert_eq!(state.key_focus(), KeyScope::CommitEditor);
    }

    #[test]
    fn an_untouched_amend_keeps_heads_message() {
        let mut state = amending();
        let (_, op) = op_of(&ctrl(&mut state, 's'));
        assert_eq!(
            op,
            GitOp::Commit {
                message: None,
                stage_all: false,
                amend: true,
                no_verify: false,
            }
        );
        assert!(matches!(state.commit, CommitFlow::Running(_)));
    }

    #[test]
    fn an_edited_amend_commits_the_new_message() {
        let mut state = amending();
        type_text(&mut state, "!");
        let (_, op) = op_of(&ctrl(&mut state, 's'));
        assert_eq!(
            op,
            GitOp::Commit {
                message: Some("subj!\n\nbody\n".into()),
                stage_all: false,
                amend: true,
                no_verify: false,
            }
        );
    }

    #[test]
    fn a_failed_head_load_closes_the_popup_and_says_why() {
        let (mut state, tag) = loading_amend();
        state.update(head_loaded(tag, Err("fatal: bad HEAD")));
        assert_eq!(state.commit, CommitFlow::Closed);
        match state.popups.last() {
            Some(Popup::Error(error)) => {
                assert_eq!(error.title, "Could not read the last commit")
            }
            other => panic!("expected an error popup, got {other:?}"),
        }
    }

    #[test]
    fn esc_while_loading_closes_and_the_late_result_is_ignored() {
        let (mut state, tag) = loading_amend();
        let busy = state.busy;
        assert_eq!(press(&mut state, KeyCode::Esc), vec![]);
        assert_eq!(state.commit, CommitFlow::Closed);
        assert!(!state.should_quit);

        state.update(head_loaded(tag, Ok("late")));
        assert_eq!(state.commit, CommitFlow::Closed);
        assert_eq!(state.busy, busy - 1);
    }

    #[test]
    fn a_stale_head_load_is_ignored_but_still_lowers_busy() {
        let (mut state, tag) = loading_amend();
        // Another user load in flight, so two decrements stay above zero.
        state.busy += 1;
        let busy = state.busy;
        state.update(head_loaded(
            Tag {
                seq: tag.seq + 99,
                ..tag
            },
            Ok("other"),
        ));
        assert_eq!(state.commit, CommitFlow::LoadingAmend { seq: tag.seq });
        state.update(head_loaded(
            Tag {
                generation: tag.generation + 1,
                ..tag
            },
            Ok("other"),
        ));
        assert_eq!(state.commit, CommitFlow::LoadingAmend { seq: tag.seq });
        assert_eq!(state.busy, busy - 2);
    }

    #[test]
    fn esc_on_an_untouched_amend_closes_at_once() {
        let mut state = amending();
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.commit, CommitFlow::Closed);
    }

    #[test]
    fn a_shift_a_is_refused_on_an_unborn_branch_or_while_an_op_runs() {
        let (mut state, _) = with_files(vec![], 24);
        state.branch = Some(BranchStatus {
            head: Head::Unborn("main".into()),
            ..on_branch("main")
        });
        assert_eq!(press(&mut state, KeyCode::Char('A')), vec![]);
        assert_eq!(state.notice, Some("No commit to amend"));
        assert_eq!(state.commit, CommitFlow::Closed);

        let (mut state, _) = with_files(three_files(), 24);
        state.op_seq = Some(7);
        assert_eq!(press(&mut state, KeyCode::Char('A')), vec![]);
        assert_eq!(state.notice, Some("An operation is running"));
        assert_eq!(state.commit, CommitFlow::Closed);
    }

    #[test]
    fn open_amend_does_nothing_with_a_popup_already_open() {
        let mut state = editing();
        assert_eq!(state.update(AppIntent::OpenAmend), vec![]);
        assert!(editor(&state).amend.is_none());
    }

    #[test]
    fn ctrl_n_toggles_skipping_hooks_and_the_commit_carries_it() {
        let mut state = editing();
        assert!(!editor(&state).no_verify);
        ctrl(&mut state, 'n');
        assert!(editor(&state).no_verify);
        ctrl(&mut state, 'n');
        assert!(!editor(&state).no_verify);
        ctrl(&mut state, 'n');
        type_text(&mut state, "subj");
        let (_, op) = op_of(&ctrl(&mut state, 's'));
        assert!(
            matches!(
                op,
                GitOp::Commit {
                    no_verify: true,
                    ..
                }
            ),
            "{op:?}"
        );

        let (_, _, op) = submitted();
        assert!(
            matches!(
                op,
                GitOp::Commit {
                    no_verify: false,
                    ..
                }
            ),
            "{op:?}"
        );
    }

    #[test]
    fn ctrl_e_hands_the_current_message_to_the_editor() {
        let mut state = editing();
        type_text(&mut state, "subj\nbody");
        let before = state.commit.clone();
        assert_eq!(
            ctrl(&mut state, 'e'),
            vec![Effect::HandOff(HandOff::EditCommitMessage {
                initial: "subj\n\nbody\n".into()
            })]
        );
        assert_eq!(state.commit, before);
    }

    #[test]
    fn a_message_from_the_editor_commits_right_away_keeping_the_choices() {
        let mut state = amending();
        ctrl(&mut state, 'n');
        let busy = state.busy;
        let effects = state.update(AppIntent::CommitMessageEdited {
            result: Ok(Some("x\n\ny\n".into())),
        });
        let (tag, op) = op_of(&effects);
        assert_eq!(
            op,
            GitOp::Commit {
                message: Some("x\n\ny\n".into()),
                stage_all: false,
                amend: true,
                no_verify: true,
            }
        );
        assert_eq!(state.op_seq, Some(tag.seq));
        assert_eq!(state.busy, busy + 1);
        match &state.commit {
            CommitFlow::Running(editor) => {
                assert_eq!(editor.subject, "x");
                assert_eq!(editor.body, ["y"]);
                assert!(editor.no_verify && editor.amend.is_some());
            }
            other => panic!("expected Running, got {other:?}"),
        }
    }

    #[test]
    fn an_emptied_message_aborts_the_commit_and_keeps_the_text() {
        let mut state = editing();
        type_text(&mut state, "keep me");
        let before = state.commit.clone();
        let effects = state.update(AppIntent::CommitMessageEdited { result: Ok(None) });
        assert_eq!(effects, vec![]);
        assert_eq!(state.commit, before);
        assert_eq!(state.notice, Some("Empty message; commit aborted"));
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn an_editor_failure_shows_an_error_over_the_kept_text() {
        let mut state = editing();
        type_text(&mut state, "keep me");
        let effects = state.update(AppIntent::CommitMessageEdited {
            result: Err("The editor exited with 3".into()),
        });
        assert_eq!(effects, vec![]);
        assert!(matches!(state.popups.last(), Some(Popup::Error(_))));
        press(&mut state, KeyCode::Esc);
        assert_eq!(editor(&state).subject, "keep me");
    }

    #[test]
    fn an_edited_message_with_no_popup_open_is_dropped() {
        let (mut state, _) = with_files(three_files(), 24);
        let effects = state.update(AppIntent::CommitMessageEdited {
            result: Ok(Some("x\n".into())),
        });
        assert_eq!(effects, vec![]);
        assert_eq!(state.commit, CommitFlow::Closed);
        assert_eq!(state.op_seq, None);
    }

    fn menu(id: MenuId, cursor: usize) -> Popup {
        Popup::Menu { id, cursor }
    }

    #[test]
    fn a_submenu_opens_on_its_parent_and_esc_goes_back_one_level() {
        let (mut state, _) = with_files(three_files(), 24);
        press(&mut state, KeyCode::Char('m'));
        assert_eq!(state.popups, [menu(MenuId::Actions, 0)]);
        assert_eq!(state.key_focus(), KeyScope::Menu);

        // "Stage…" is the first item.
        press(&mut state, KeyCode::Enter);
        assert_eq!(
            state.popups,
            [menu(MenuId::Actions, 0), menu(MenuId::Stage, 0)]
        );

        press(&mut state, KeyCode::Esc);
        assert_eq!(state.popups, [menu(MenuId::Actions, 0)]);
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.popups, []);
        assert!(!state.should_quit);
        assert_eq!(state.key_focus(), KeyScope::Files);
    }

    #[test]
    fn the_menu_cursor_moves_and_stops_at_the_ends() {
        let (mut state, _) = with_files(three_files(), 24);
        state.update(AppIntent::OpenMenu(MenuId::Actions));
        press(&mut state, KeyCode::Char('k'));
        assert_eq!(state.popups, [menu(MenuId::Actions, 0)]);
        for _ in 0..10 {
            press(&mut state, KeyCode::Char('j'));
        }
        assert_eq!(state.popups, [menu(MenuId::Actions, 5)]);
        press(&mut state, KeyCode::Up);
        assert_eq!(state.popups, [menu(MenuId::Actions, 4)]);
        // The file selection did not move.
        assert_eq!(state.selected, Some(0));
    }

    #[test]
    fn a_disabled_menu_item_says_why_and_leaves_the_stack_alone() {
        let (mut state, _) = with_files(vec![], 24);
        state.update(AppIntent::OpenMenu(MenuId::Actions));
        state.update(AppIntent::OpenMenu(MenuId::Stage));
        let before = state.popups.clone();

        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);

        assert_eq!(state.notice, Some("No file selected"));
        assert_eq!(state.popups, before);
    }

    #[test]
    fn selecting_a_stage_item_runs_it_and_closes_every_menu() {
        let (mut state, _) = with_files(three_files(), 24);
        press(&mut state, KeyCode::Char('m'));
        press(&mut state, KeyCode::Enter);
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
        assert_eq!(op, GitOp::Stage(vec!["a.rs".into()]));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn menus_and_prompts_only_open_on_the_workspace() {
        let mut state = AppState::new(Context::Review { number: 1 });
        state.update(AppIntent::OpenMenu(MenuId::Actions));
        state.update(AppIntent::OpenPrompt(PromptKind::GoToFile));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn the_amend_item_follows_commit_and_opens_the_amend() {
        let (mut state, _) = with_files(vec![], 24);
        let items = menu::items(MenuId::Actions);
        assert_eq!(items[1].label, "Commit…");
        assert_eq!(items[2].label, "Amend last commit…");
        state.update(AppIntent::OpenMenu(MenuId::Actions));
        state.update(AppIntent::MenuNext);
        state.update(AppIntent::MenuNext);
        head_load(&state.update(AppIntent::MenuSelect));
        assert_eq!(state.popups, []);
        assert!(matches!(state.commit, CommitFlow::LoadingAmend { .. }));

        state.branch = None;
        state.commit = CommitFlow::Closed;
        state.update(AppIntent::OpenMenu(MenuId::Actions));
        state.update(AppIntent::MenuNext);
        state.update(AppIntent::MenuNext);
        assert_eq!(state.update(AppIntent::MenuSelect), vec![]);
        assert_eq!(state.notice, Some("No commit to amend"));
    }

    #[test]
    fn the_quit_item_asks_first_and_y_quits() {
        let (mut state, _) = with_files(three_files(), 24);
        state.update(AppIntent::OpenMenu(MenuId::Actions));
        for _ in 0..5 {
            state.update(AppIntent::MenuNext);
        }
        state.update(AppIntent::MenuSelect);
        assert_eq!(state.popups, [Popup::Confirm(ConfirmKind::Quit)]);
        assert_eq!(state.key_focus(), KeyScope::Confirm);
        assert!(!state.should_quit);

        press(&mut state, KeyCode::Char('y'));
        assert!(state.should_quit);
        assert_eq!(state.popups, []);
    }

    #[test]
    fn n_or_esc_declines_the_confirm_without_quitting() {
        for key in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut state = AppState::new(Context::Workspace);
            state.update(AppIntent::Ask(ConfirmKind::Quit));
            assert_eq!(press(&mut state, key), vec![]);
            assert_eq!(state.popups, []);
            assert!(!state.should_quit);
        }
    }

    fn two_txt_files() -> AppState {
        with_files(vec![untracked("a.txt"), untracked("b.txt")], 24).0
    }

    fn prompt_text(state: &AppState) -> &str {
        match state.popups.last() {
            Some(Popup::Prompt { input, .. }) => &input.text,
            other => panic!("expected a prompt on top, got {other:?}"),
        }
    }

    #[test]
    fn the_go_to_file_prompt_selects_a_match_ignoring_case() {
        let mut state = two_txt_files();
        state.update(AppIntent::OpenPrompt(PromptKind::GoToFile));
        assert_eq!(state.key_focus(), KeyScope::Prompt);

        state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('B'),
            KeyModifiers::SHIFT,
        )));
        assert_eq!(prompt_text(&state), "B");
        let effects = press(&mut state, KeyCode::Enter);

        let tag = diff_tag(&effects);
        assert!(tag.seq > 0);
        assert_eq!(state.selected, Some(1));
        assert_eq!(state.diff.path, Some("b.txt".into()));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn q_and_question_mark_are_typed_into_the_prompt_and_esc_cancels() {
        let mut state = two_txt_files();
        state.update(AppIntent::OpenPrompt(PromptKind::GoToFile));
        type_text(&mut state, "q?b");
        assert_eq!(prompt_text(&state), "q?b");
        assert!(!state.should_quit);
        assert!(!state.help_open);

        assert_eq!(press(&mut state, KeyCode::Esc), vec![]);
        assert_eq!(state.popups, []);
        assert_eq!(state.selected, Some(0));
        assert!(!state.should_quit);
    }

    #[test]
    fn going_to_a_missing_file_says_so_and_empty_text_does_nothing() {
        let mut state = two_txt_files();
        assert_eq!(state.update(AppIntent::GoToFile("zzz".into())), vec![]);
        assert_eq!(state.notice, Some("No file matches"));
        assert_eq!(state.selected, Some(0));

        state.notice = None;
        assert_eq!(state.update(AppIntent::GoToFile("  ".into())), vec![]);
        assert_eq!(state.notice, None);
        assert_eq!(state.selected, Some(0));
    }

    fn failed_stage(state: &mut AppState, message: &str) -> Vec<Effect> {
        let (tag, op) = op_of(&state.update(AppIntent::ToggleStage));
        state.update(finished(tag, op, Err(message.into())))
    }

    #[test]
    fn the_same_error_is_never_pushed_twice_in_a_row() {
        let (mut state, _) = with_files(three_files(), 24);
        failed_stage(&mut state, "fatal: index.lock");
        failed_stage(&mut state, "fatal: index.lock");
        assert_eq!(state.popups.len(), 1);
        failed_stage(&mut state, "fatal: other");
        assert_eq!(state.popups.len(), 2);
    }

    #[test]
    fn a_stale_failed_op_pushes_no_error() {
        let (mut state, _) = with_files(three_files(), 24);
        let (tag, op) = op_of(&state.update(AppIntent::ToggleStage));
        state.generation += 1;
        state.update(finished(tag, op, Err("boom".into())));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn a_failed_unstage_all_names_what_failed() {
        let (mut state, _) = with_files(vec![changed("b.rs", 'M', '.')], 24);
        let (tag, op) = op_of(&state.update(AppIntent::ToggleStageAll));
        state.update(finished(tag, op, Err("no".into())));
        assert!(
            matches!(state.popups.as_slice(), [Popup::Error(e)] if e.title == "Could not unstage everything"),
            "{:?}",
            state.popups
        );
    }

    #[test]
    fn a_failed_status_load_keeps_its_error_and_shows_a_popup() {
        let mut state = AppState::new(Context::Workspace);
        let tag = refresh_tag(&mut state);
        state.update(AppIntent::StatusLoaded {
            tag,
            result: Err("fatal: not a git repository".into()),
        });
        assert_eq!(
            state.status_error.as_deref(),
            Some("fatal: not a git repository")
        );
        match state.popups.as_slice() {
            [Popup::Error(error)] => {
                assert_eq!(error.title, "Could not read the repository status");
                assert_eq!(error.message, "fatal: not a git repository");
                assert_eq!(
                    error.next,
                    "Close this, fix the cause, then press r to retry."
                );
            }
            other => panic!("expected one error popup, got {other:?}"),
        }
        assert_eq!(state.key_focus(), KeyScope::Error);
    }

    /// Fail a status load while `state` is in some popup.
    fn fail_status(state: &mut AppState) {
        let tag = refresh_tag(state);
        state.update(AppIntent::StatusLoaded {
            tag,
            result: Err("boom".into()),
        });
    }

    #[test]
    fn an_error_over_the_commit_editor_keeps_the_typed_text() {
        let mut state = editing();
        type_text(&mut state, "half a mess");
        fail_status(&mut state);
        assert_eq!(state.key_focus(), KeyScope::Error);
        // Keys meant for the error never reach the message.
        type_text(&mut state, "xyz");
        assert_eq!(editor(&state).subject, "half a mess");

        press(&mut state, KeyCode::Esc);
        assert_eq!(state.popups, []);
        assert_eq!(state.key_focus(), KeyScope::CommitEditor);
        type_text(&mut state, "!");
        assert_eq!(editor(&state).subject, "half a mess!");
    }

    #[test]
    fn an_error_over_a_prompt_keeps_the_typed_text() {
        let mut state = two_txt_files();
        state.update(AppIntent::OpenPrompt(PromptKind::GoToFile));
        type_text(&mut state, "b.t");
        fail_status(&mut state);
        assert_eq!(state.key_focus(), KeyScope::Error);

        press(&mut state, KeyCode::Enter);
        assert_eq!(state.key_focus(), KeyScope::Prompt);
        assert_eq!(prompt_text(&state), "b.t");
    }

    const TWO_HUNKS: &[u8] = b"diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n\
        @@ -1,2 +1,3 @@\n-a\n+b\n+c\n d\n\
        @@ -9 +10 @@\n-e\n+f\n";
    const ONE_HUNK: &[u8] = b"diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n\
        @@ -9 +9 @@\n-e\n+f\n";
    const BINARY: &[u8] = b"diff --git a/a.rs b/a.rs\nindex 1..2 100644\n\
        Binary files a/a.rs and b/a.rs differ\n";

    fn patch_of(bytes: &[u8]) -> FilePatch {
        patch::parse(bytes).unwrap()
    }

    /// The one `LoadPatch` among `effects`.
    fn patch_load(effects: &[Effect]) -> (Tag, DiffRequest) {
        let loads: Vec<_> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::LoadPatch { tag, request } => Some((*tag, request.clone())),
                _ => None,
            })
            .collect();
        match loads.as_slice() {
            [load] => load.clone(),
            other => panic!("expected one LoadPatch in {effects:?}, got {other:?}"),
        }
    }

    fn view(state: &AppState) -> &HunkView {
        match &state.focus {
            Focus::Diff(view) => view,
            Focus::Files => panic!("expected diff focus"),
        }
    }

    /// `files` loaded, the first selected, Enter pressed and its patch
    /// answered with `bytes`.
    fn focused(files: Vec<FileEntry>, bytes: &[u8]) -> AppState {
        let (mut state, _) = with_files(files, 24);
        let (tag, _) = patch_load(&press(&mut state, KeyCode::Enter));
        state.update(AppIntent::PatchLoaded {
            tag,
            result: Ok(patch_of(bytes)),
        });
        state
    }

    #[test]
    fn enter_focuses_the_diff_on_the_entry_side_and_loads_the_patch() {
        let (mut state, _) = with_files(three_files(), 24);
        let busy = state.busy;
        let (tag, request) = patch_load(&press(&mut state, KeyCode::Enter));
        assert!(tag.user);
        assert_eq!(state.busy, busy + 1);
        assert_eq!(
            request,
            DiffRequest {
                path: "a.rs".into(),
                orig_path: None,
                side: DiffSide::Worktree,
                options: Default::default(),
            }
        );
        assert_eq!(view(&state).side, DiffSide::Worktree);
        assert_eq!(view(&state).seq, Some(tag.seq));
        assert_eq!(state.panel_focus(), KeyScope::Diff);

        // A staged-only file opens on the staged side.
        let (mut state, _) = with_files(vec![changed("b.rs", 'M', '.')], 24);
        let (_, request) = patch_load(&press(&mut state, KeyCode::Enter));
        assert_eq!(request.side, DiffSide::Index);
    }

    #[test]
    fn enter_without_a_file_says_so() {
        let (mut state, _) = with_files(vec![], 24);
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice, Some("No file selected"));
        assert_eq!(state.focus, Focus::Files);
    }

    #[test]
    fn a_loaded_patch_lowers_busy_and_shows() {
        let state = focused(three_files(), TWO_HUNKS);
        assert_eq!(state.busy, 0);
        assert_eq!(view(&state).seq, None);
        assert_eq!(view(&state).hunk_count(), 2);
    }

    #[test]
    fn j_and_k_move_between_hunks_and_stop_at_the_ends() {
        let mut state = focused(three_files(), TWO_HUNKS);
        press(&mut state, KeyCode::Char('k'));
        assert_eq!(view(&state).hunk, 0);
        press(&mut state, KeyCode::Char('j'));
        press(&mut state, KeyCode::Down);
        assert_eq!(view(&state).hunk, 1);
        press(&mut state, KeyCode::Up);
        assert_eq!(view(&state).hunk, 0);
        // The file selection never moved.
        assert_eq!(state.selected, Some(0));
    }

    #[test]
    fn space_on_one_of_several_hunks_applies_a_patch() {
        let mut state = focused(three_files(), TWO_HUNKS);
        press(&mut state, KeyCode::Char('j'));
        let (tag, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        assert!(tag.user);
        assert_eq!(
            op,
            GitOp::ApplyCached {
                patch: b"diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -9 +9 @@\n-e\n+f\n"
                    .to_vec(),
                reverse: false,
            }
        );
    }

    #[test]
    fn space_in_the_staged_view_applies_the_patch_in_reverse() {
        let mut state = focused(vec![changed("a.rs", 'M', '.')], TWO_HUNKS);
        assert_eq!(view(&state).side, DiffSide::Index);
        let (_, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        assert!(
            matches!(op, GitOp::ApplyCached { reverse: true, .. }),
            "{op:?}"
        );
    }

    #[test]
    fn space_on_the_only_hunk_stages_or_unstages_the_file() {
        let mut state = focused(three_files(), ONE_HUNK);
        let (_, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        assert_eq!(op, GitOp::Stage(vec!["a.rs".into()]));

        let mut state = focused(vec![changed("a.rs", 'M', '.')], ONE_HUNK);
        let (_, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        assert_eq!(op, GitOp::Unstage(vec!["a.rs".into()]));
    }

    #[test]
    fn space_on_a_binary_file_says_why_and_runs_nothing() {
        let mut state = focused(three_files(), BINARY);
        assert_eq!(press(&mut state, KeyCode::Char(' ')), vec![]);
        assert_eq!(
            state.notice,
            Some("Binary file: stage it from the Files panel")
        );
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn space_is_refused_in_order_of_what_matters_most() {
        let mut state = focused(three_files(), TWO_HUNKS);
        state.op_seq = Some(1);
        press(&mut state, KeyCode::Char(' '));
        assert_eq!(state.notice, Some("An operation is running"));
        state.op_seq = None;

        let mut loading = state.clone();
        press(&mut loading, KeyCode::Char('t'));
        press(&mut loading, KeyCode::Char(' '));
        assert_eq!(loading.notice, Some("Loading…"));

        for (kind, reason) in [
            (EntryKind::Conflicted, "Resolve the conflict first"),
            (
                EntryKind::Renamed,
                "Renamed file: stage it from the Files panel",
            ),
            (
                EntryKind::Copied,
                "Renamed file: stage it from the Files panel",
            ),
        ] {
            let mut odd = state.clone();
            odd.files[0].kind = kind;
            assert_eq!(press(&mut odd, KeyCode::Char(' ')), vec![]);
            assert_eq!(odd.notice, Some(reason), "{kind:?}");
        }

        let mut empty = focused(three_files(), b"");
        press(&mut empty, KeyCode::Char(' '));
        assert_eq!(empty.notice, Some("No changes in this view"));
    }

    #[test]
    fn a_submodule_is_refused_by_the_patch_engine() {
        let submodule = b"diff --git a/s b/s\nindex 1..2 160000\n--- a/s\n+++ b/s\n\
            @@ -1 +1 @@\n-Subproject commit 1\n+Subproject commit 2\n";
        let mut state = focused(three_files(), submodule);
        assert_eq!(press(&mut state, KeyCode::Char(' ')), vec![]);
        assert_eq!(
            state.notice,
            Some("This change can only be staged from the Files panel")
        );
    }

    #[test]
    fn t_flips_the_side_and_reloads() {
        let mut state = focused(three_files(), TWO_HUNKS);
        press(&mut state, KeyCode::Char('j'));
        let (_, request) = patch_load(&press(&mut state, KeyCode::Char('t')));
        assert_eq!(request.side, DiffSide::Index);
        assert_eq!(view(&state).side, DiffSide::Index);
        assert_eq!(view(&state).hunk, 0);
        assert_eq!(view(&state).patch, None);
        let (_, request) = patch_load(&press(&mut state, KeyCode::Char('t')));
        assert_eq!(request.side, DiffSide::Worktree);
    }

    #[test]
    fn t_on_an_untracked_file_says_nothing_is_staged() {
        let mut state = focused(vec![untracked("n.txt")], ONE_HUNK);
        assert_eq!(view(&state).side, DiffSide::Untracked);
        assert_eq!(press(&mut state, KeyCode::Char('t')), vec![]);
        assert_eq!(state.notice, Some("Untracked file: nothing is staged"));
    }

    #[test]
    fn a_stale_patch_load_is_dropped_but_busy_still_falls() {
        let (mut state, _) = with_files(three_files(), 24);
        let (old, _) = patch_load(&press(&mut state, KeyCode::Enter));
        let (new, _) = patch_load(&press(&mut state, KeyCode::Char('t')));
        assert_eq!(state.busy, 2);

        state.update(AppIntent::PatchLoaded {
            tag: old,
            result: Ok(patch_of(TWO_HUNKS)),
        });
        assert_eq!(state.busy, 1);
        assert_eq!(view(&state).patch, None);
        assert_eq!(view(&state).seq, Some(new.seq));

        // Another repository's result is dropped too.
        state.generation += 1;
        state.update(AppIntent::PatchLoaded {
            tag: new,
            result: Ok(patch_of(TWO_HUNKS)),
        });
        assert_eq!(state.busy, 0);
        assert_eq!(view(&state).patch, None);
    }

    #[test]
    fn a_failed_patch_load_shows_an_error_popup() {
        let (mut state, _) = with_files(three_files(), 24);
        let (tag, _) = patch_load(&press(&mut state, KeyCode::Enter));
        state.update(AppIntent::PatchLoaded {
            tag,
            result: Err("fatal: bad".into()),
        });
        match state.popups.as_slice() {
            [Popup::Error(error)] => {
                assert_eq!(error.title, "Could not read the diff");
                assert_eq!(error.message, "fatal: bad");
            }
            other => panic!("expected one error popup, got {other:?}"),
        }
        assert_eq!(view(&state).seq, None);
        assert_eq!(state.busy, 0);
    }

    #[test]
    fn a_failed_hunk_stage_names_what_failed() {
        let mut state = focused(three_files(), TWO_HUNKS);
        let (tag, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        state.update(finished(tag, op, Err("error: patch failed".into())));
        assert!(
            matches!(state.popups.as_slice(), [Popup::Error(e)] if e.title == "Could not stage the change"),
            "{:?}",
            state.popups
        );

        let mut state = focused(vec![changed("a.rs", 'M', '.')], TWO_HUNKS);
        let (tag, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        state.update(finished(tag, op, Err("no".into())));
        assert!(
            matches!(state.popups.as_slice(), [Popup::Error(e)] if e.title == "Could not unstage the change"),
            "{:?}",
            state.popups
        );
    }

    #[test]
    fn after_a_hunk_is_staged_the_patch_reloads_and_the_cursor_is_clamped() {
        let mut state = focused(three_files(), TWO_HUNKS);
        press(&mut state, KeyCode::Char('j'));
        let (tag, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        let status = match state.update(finished(tag, op, Ok(()))).as_slice() {
            [Effect::LoadStatus { tag }] => *tag,
            other => panic!("expected one LoadStatus, got {other:?}"),
        };
        let effects = state.update(AppIntent::StatusLoaded {
            tag: status,
            result: Ok(repo(vec![changed("a.rs", 'M', 'M')])),
        });
        let (tag, request) = patch_load(&effects);
        // Same file, so the same side.
        assert_eq!(request.side, DiffSide::Worktree);
        assert_eq!(view(&state).hunk, 1);
        state.update(AppIntent::PatchLoaded {
            tag,
            result: Ok(patch_of(ONE_HUNK)),
        });
        assert_eq!(view(&state).hunk, 0);
        assert_eq!(state.busy, 0);
    }

    #[test]
    fn a_file_that_stops_being_untracked_takes_its_new_side() {
        let mut state = focused(vec![untracked("n.txt")], ONE_HUNK);
        let (tag, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        assert_eq!(op, GitOp::Stage(vec!["n.txt".into()]));
        let status = match state.update(finished(tag, op, Ok(()))).as_slice() {
            [Effect::LoadStatus { tag }] => *tag,
            other => panic!("{other:?}"),
        };
        let effects = state.update(AppIntent::StatusLoaded {
            tag: status,
            result: Ok(repo(vec![changed("n.txt", 'A', '.')])),
        });
        assert_eq!(patch_load(&effects).1.side, DiffSide::Index);
    }

    #[test]
    fn a_clean_tree_returns_focus_to_the_files() {
        let mut state = focused(three_files(), ONE_HUNK);
        let tag = refresh_tag(&mut state);
        state.update(loaded(tag, &[]));
        assert_eq!(state.focus, Focus::Files);
    }

    #[test]
    fn esc_h_or_left_in_the_diff_go_back_to_the_files_without_quitting() {
        for key in [KeyCode::Esc, KeyCode::Char('h'), KeyCode::Left] {
            let mut state = focused(three_files(), TWO_HUNKS);
            assert_eq!(press(&mut state, key), vec![]);
            assert_eq!(state.focus, Focus::Files, "{key:?}");
            assert!(!state.should_quit);
            assert_eq!(state.key_focus(), KeyScope::Files);
        }
    }

    #[test]
    fn scrolling_in_the_diff_focus_moves_the_hunk_view() {
        let mut state = focused(three_files(), TWO_HUNKS);
        let before = state.diff.clone();
        assert_eq!(
            state.update(AppIntent::ScrollDiff(Scroll::LineDown)),
            vec![]
        );
        assert_eq!(view(&state).scroll, 1);
        assert_eq!(state.diff, before);
    }

    const HEAD: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n";

    #[test]
    fn v_enters_line_mode_and_esc_or_v_leaves_it_without_leaving_the_diff() {
        for leave in [KeyCode::Esc, KeyCode::Char('v')] {
            let mut state = focused(three_files(), TWO_HUNKS);
            press(&mut state, KeyCode::Char('v'));
            assert!(view(&state).lines.is_some());
            assert_eq!(state.panel_focus(), KeyScope::Lines);
            assert_eq!(state.key_focus(), KeyScope::Lines);
            assert_eq!(press(&mut state, leave), vec![]);
            assert_eq!(view(&state).lines, None, "{leave:?}");
            assert_eq!(state.panel_focus(), KeyScope::Diff);
        }
    }

    #[test]
    fn in_line_mode_j_and_k_move_between_lines_not_hunks() {
        let mut state = focused(three_files(), TWO_HUNKS);
        press(&mut state, KeyCode::Char('v'));
        press(&mut state, KeyCode::Char('j'));
        press(&mut state, KeyCode::Char('j'));
        press(&mut state, KeyCode::Char('j'));
        assert_eq!(view(&state).hunk, 0);
        assert_eq!(view(&state).selected_range(), [2]);
        press(&mut state, KeyCode::Char('k'));
        state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('K'),
            KeyModifiers::SHIFT,
        )));
        assert_eq!(view(&state).selected_range(), [0, 1]);
    }

    #[test]
    fn space_in_line_mode_applies_the_selected_lines_per_side() {
        let mut state = focused(three_files(), TWO_HUNKS);
        press(&mut state, KeyCode::Char('v'));
        press(&mut state, KeyCode::Char('j'));
        let (tag, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        assert!(tag.user);
        assert_eq!(
            op,
            GitOp::ApplyCached {
                patch: format!("{HEAD}@@ -1,2 +1,3 @@\n a\n+b\n d\n").into_bytes(),
                reverse: false,
            }
        );

        let mut state = focused(vec![changed("a.rs", 'M', '.')], TWO_HUNKS);
        press(&mut state, KeyCode::Char('v'));
        press(&mut state, KeyCode::Char('j'));
        let (_, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        assert_eq!(
            op,
            GitOp::ApplyCached {
                patch: format!("{HEAD}@@ -1,2 +1,3 @@\n+b\n c\n d\n").into_bytes(),
                reverse: true,
            }
        );
    }

    #[test]
    fn selecting_every_line_of_the_file_stages_or_unstages_the_file() {
        for (index, worktree, expected) in [
            ('.', 'M', GitOp::Stage(vec!["a.rs".into()])),
            ('M', '.', GitOp::Unstage(vec!["a.rs".into()])),
        ] {
            let mut state = focused(vec![changed("a.rs", index, worktree)], ONE_HUNK);
            press(&mut state, KeyCode::Char('v'));
            press(&mut state, KeyCode::Char('J'));
            let (_, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
            assert_eq!(op, expected);
        }
    }

    #[test]
    fn a_refused_line_selection_says_why_and_runs_nothing() {
        let new_file = b"diff --git a/n.txt b/n.txt\nnew file mode 100644\n\
            --- /dev/null\n+++ b/n.txt\n@@ -0,0 +1,2 @@\n+x\n+y\n";
        let mut state = focused(vec![untracked("n.txt")], new_file);
        press(&mut state, KeyCode::Char('v'));
        assert_eq!(press(&mut state, KeyCode::Char(' ')), vec![]);
        assert_eq!(
            state.notice,
            Some("New file: press Esc, then Space for the whole file")
        );
        assert_eq!(state.op_seq, None);
        assert_eq!(state.busy, 0);
    }

    #[test]
    fn t_leaves_line_mode_and_v_is_refused_without_hunks() {
        let mut state = focused(three_files(), TWO_HUNKS);
        press(&mut state, KeyCode::Char('v'));
        press(&mut state, KeyCode::Char('t'));
        assert_eq!(view(&state).lines, None);
        assert_eq!(state.panel_focus(), KeyScope::Diff);

        let mut empty = focused(three_files(), b"");
        assert_eq!(press(&mut empty, KeyCode::Char('v')), vec![]);
        assert_eq!(empty.notice, Some("No changes in this view"));
        assert_eq!(view(&empty).lines, None);
    }

    #[test]
    fn a_failed_lines_stage_names_the_change() {
        let mut state = focused(three_files(), TWO_HUNKS);
        press(&mut state, KeyCode::Char('v'));
        let (tag, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        state.update(finished(tag, op, Err("error: patch failed".into())));
        assert!(
            matches!(state.popups.as_slice(), [Popup::Error(e)] if e.title == "Could not stage the change"),
            "{:?}",
            state.popups
        );
    }

    fn discard_popup(state: &AppState) -> &DiscardConfirm {
        match state.popups.as_slice() {
            [Popup::Discard(confirm)] => confirm,
            other => panic!("expected one discard popup, got {other:?}"),
        }
    }

    #[test]
    fn d_on_a_modified_file_asks_and_y_discards_it() {
        let (mut state, _) = with_files(three_files(), 24);
        assert_eq!(press(&mut state, KeyCode::Char('d')), vec![]);
        assert_eq!(
            discard_popup(&state),
            &DiscardConfirm {
                what: DiscardWhat::File,
                path: "a.rs".into(),
                op: GitOp::DiscardWorktree(vec!["a.rs".into()]),
            }
        );
        assert_eq!(state.key_focus(), KeyScope::DiscardConfirm);
        assert_eq!(state.op_seq, None);

        let (tag, op) = op_of(&press(&mut state, KeyCode::Char('y')));
        assert_eq!(op, GitOp::DiscardWorktree(vec!["a.rs".into()]));
        assert!(tag.user);
        assert_eq!(state.popups, []);
    }

    #[test]
    fn n_or_esc_on_the_discard_question_runs_nothing() {
        for key in [KeyCode::Char('n'), KeyCode::Esc, KeyCode::Enter] {
            let (mut state, _) = with_files(three_files(), 24);
            press(&mut state, KeyCode::Char('d'));
            assert_eq!(press(&mut state, key), vec![], "{key:?}");
            assert_eq!(state.op_seq, None, "{key:?}");
            assert!(!state.should_quit, "{key:?}");
            if key == KeyCode::Enter {
                // Enter never discards, and never answers at all.
                assert_eq!(state.popups.len(), 1);
            } else {
                assert_eq!(state.popups, [], "{key:?}");
            }
        }
    }

    #[test]
    fn the_op_is_fixed_when_asked_so_a_refresh_cannot_change_it() {
        let (mut state, _) = with_files(three_files(), 24);
        press(&mut state, KeyCode::Char('d'));
        let tag = refresh_tag(&mut state);
        state.update(loaded(tag, &["other.txt"]));
        let (_, op) = op_of(&press(&mut state, KeyCode::Char('y')));
        assert_eq!(op, GitOp::DiscardWorktree(vec!["a.rs".into()]));
    }

    #[test]
    fn d_on_an_untracked_file_asks_to_delete_it() {
        let (mut state, _) = with_files(three_files(), 24);
        select_path(&mut state, "c.txt");
        press(&mut state, KeyCode::Char('d'));
        assert_eq!(
            discard_popup(&state),
            &DiscardConfirm {
                what: DiscardWhat::Untracked,
                path: "c.txt".into(),
                op: GitOp::DeleteUntracked(vec!["c.txt".into()]),
            }
        );
    }

    #[test]
    fn d_on_a_conflicted_or_fully_staged_file_says_why_and_asks_nothing() {
        let conflict = FileEntry {
            kind: EntryKind::Conflicted,
            ..changed("both.rs", 'U', 'U')
        };
        for (file, reason) in [
            (conflict, "Resolve the conflict first"),
            (changed("b.rs", 'M', '.'), "No unstaged changes to discard"),
        ] {
            let (mut state, _) = with_files(vec![file], 24);
            assert_eq!(press(&mut state, KeyCode::Char('d')), vec![]);
            assert_eq!(state.notice, Some(reason));
            assert_eq!(state.popups, []);
        }
    }

    #[test]
    fn discard_does_nothing_in_review_or_while_an_op_runs() {
        let mut review = AppState::new(Context::Review { number: 1 });
        review.files = three_files();
        review.selected = Some(0);
        let (mut running, _) = with_files(three_files(), 24);
        running.op_seq = Some(9);
        for state in [&mut review, &mut running] {
            assert_eq!(state.update(AppIntent::Discard), vec![]);
            assert_eq!(state.popups, []);
        }
    }

    #[test]
    fn d_on_one_of_two_hunks_asks_to_reverse_apply_it_to_the_worktree() {
        let mut state = focused(three_files(), TWO_HUNKS);
        press(&mut state, KeyCode::Char('j'));
        press(&mut state, KeyCode::Char('d'));
        let expected = patch::build(
            &patch_of(TWO_HUNKS),
            &PatchSelection::Hunk(1),
            Direction::Unstage,
        )
        .unwrap();
        assert_eq!(
            discard_popup(&state),
            &DiscardConfirm {
                what: DiscardWhat::Hunk,
                path: "a.rs".into(),
                op: GitOp::ApplyWorktree { patch: expected },
            }
        );
        let (_, op) = op_of(&press(&mut state, KeyCode::Char('y')));
        assert!(matches!(op, GitOp::ApplyWorktree { .. }), "{op:?}");
    }

    #[test]
    fn d_on_the_only_hunk_discards_the_whole_file() {
        let mut state = focused(three_files(), ONE_HUNK);
        press(&mut state, KeyCode::Char('d'));
        let confirm = discard_popup(&state);
        assert_eq!(confirm.what, DiscardWhat::Hunk);
        assert_eq!(confirm.op, GitOp::DiscardWorktree(vec!["a.rs".into()]));

        let new_file = b"diff --git a/n.txt b/n.txt\nnew file mode 100644\n\
            --- /dev/null\n+++ b/n.txt\n@@ -0,0 +1,2 @@\n+x\n+y\n";
        let mut state = focused(vec![untracked("n.txt")], new_file);
        press(&mut state, KeyCode::Char('d'));
        let confirm = discard_popup(&state);
        assert_eq!(confirm.what, DiscardWhat::Untracked);
        assert_eq!(confirm.op, GitOp::DeleteUntracked(vec!["n.txt".into()]));
    }

    #[test]
    fn d_in_line_mode_asks_to_discard_the_selected_lines() {
        let mut state = focused(three_files(), TWO_HUNKS);
        press(&mut state, KeyCode::Char('v'));
        press(&mut state, KeyCode::Char('j'));
        press(&mut state, KeyCode::Char('d'));
        let expected = patch::build(
            &patch_of(TWO_HUNKS),
            &PatchSelection::Lines([patch::LineId::added(1)].into()),
            Direction::Unstage,
        )
        .unwrap();
        assert_eq!(
            discard_popup(&state),
            &DiscardConfirm {
                what: DiscardWhat::Lines,
                path: "a.rs".into(),
                op: GitOp::ApplyWorktree { patch: expected },
            }
        );
    }

    #[test]
    fn d_in_the_staged_view_or_without_a_patch_says_why_and_asks_nothing() {
        let mut state = focused(vec![changed("a.rs", 'M', '.')], TWO_HUNKS);
        assert_eq!(press(&mut state, KeyCode::Char('d')), vec![]);
        assert_eq!(state.notice, Some("Unstage first to discard"));
        assert_eq!(state.popups, []);

        let mut binary = focused(three_files(), BINARY);
        assert_eq!(press(&mut binary, KeyCode::Char('d')), vec![]);
        assert_eq!(binary.notice, Some(patch::PatchError::Binary.message()));
        assert_eq!(binary.popups, []);

        // Still loading: no patch yet.
        let (mut loading, _) = with_files(three_files(), 24);
        press(&mut loading, KeyCode::Enter);
        assert_eq!(press(&mut loading, KeyCode::Char('d')), vec![]);
        assert_eq!(loading.notice, Some("Loading…"));
        assert_eq!(loading.popups, []);

        // The patch failed to load: nothing to discard from.
        let (tag, _) = patch_load(&press(&mut state, KeyCode::Char('t')));
        state.update(AppIntent::PatchLoaded {
            tag,
            result: Err("fatal: bad".into()),
        });
        state.popups.clear();
        assert_eq!(state.update(AppIntent::Discard), vec![]);
        assert_eq!(state.notice, Some(NO_CHANGES));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn a_failed_discard_names_what_failed() {
        for (op, title) in [
            (
                GitOp::DiscardWorktree(vec!["a.rs".into()]),
                "Could not discard the changes",
            ),
            (
                GitOp::DeleteUntracked(vec!["a.rs".into()]),
                "Could not delete the file",
            ),
            (
                GitOp::ApplyWorktree { patch: vec![] },
                "Could not discard the change",
            ),
            (
                GitOp::ApplyEdited { patch: vec![] },
                "Could not stage the edited hunk",
            ),
        ] {
            assert_eq!(failure_title(&op), title);
        }
    }

    fn hand_off(effects: &[Effect]) -> &HandOff {
        match effects {
            [Effect::HandOff(job)] => job,
            other => panic!("expected one HandOff, got {other:?}"),
        }
    }

    fn open_file(path: &str, line: Option<u32>) -> HandOff {
        HandOff::OpenFile {
            path: path.into(),
            line,
        }
    }

    #[test]
    fn o_in_the_files_panel_opens_the_selected_file_at_the_top() {
        let (mut state, _) = with_files(three_files(), 24);
        let effects = press(&mut state, KeyCode::Char('o'));
        assert_eq!(hand_off(&effects), &open_file("a.rs", None));
        assert_eq!(state.op_seq, None);
        assert_eq!(state.busy, 0);
    }

    #[test]
    fn o_in_the_diff_opens_at_the_selected_change_on_the_worktree_side() {
        let mut state = focused(three_files(), TWO_HUNKS);
        // Hunk 0 starts with `-a`, which sat at new line 1.
        let effects = press(&mut state, KeyCode::Char('o'));
        assert_eq!(hand_off(&effects), &open_file("a.rs", Some(1)));
        press(&mut state, KeyCode::Char('j'));
        let effects = press(&mut state, KeyCode::Char('o'));
        assert_eq!(hand_off(&effects), &open_file("a.rs", Some(10)));
        assert_eq!(view(&state).editor_line(), Some(10));

        // Line mode follows the cursor: `+b` then `+c`.
        press(&mut state, KeyCode::Char('k'));
        press(&mut state, KeyCode::Char('v'));
        press(&mut state, KeyCode::Char('j'));
        let effects = press(&mut state, KeyCode::Char('o'));
        assert_eq!(hand_off(&effects), &open_file("a.rs", Some(1)));
        press(&mut state, KeyCode::Char('j'));
        let effects = press(&mut state, KeyCode::Char('o'));
        assert_eq!(hand_off(&effects), &open_file("a.rs", Some(2)));
    }

    #[test]
    fn o_in_the_staged_view_opens_without_a_line() {
        let mut state = focused(vec![changed("a.rs", 'M', '.')], TWO_HUNKS);
        assert_eq!(view(&state).side, DiffSide::Index);
        let effects = press(&mut state, KeyCode::Char('o'));
        assert_eq!(hand_off(&effects), &open_file("a.rs", None));
    }

    #[test]
    fn o_on_a_file_deleted_from_the_worktree_says_so_and_opens_nothing() {
        for (index, worktree) in [('.', 'D'), ('D', '.')] {
            let (mut state, _) = with_files(vec![changed("gone.rs", index, worktree)], 24);
            assert_eq!(press(&mut state, KeyCode::Char('o')), vec![]);
            assert_eq!(state.notice, Some("The file is deleted"));
        }
        let mut state = focused(vec![changed("gone.rs", '.', 'D')], ONE_HUNK);
        assert_eq!(press(&mut state, KeyCode::Char('o')), vec![]);
        assert_eq!(state.notice, Some("The file is deleted"));
    }

    #[test]
    fn e_hands_the_selected_hunk_to_the_editor_split_at_its_first_hunk_line() {
        let mut state = focused(three_files(), TWO_HUNKS);
        press(&mut state, KeyCode::Char('j'));
        let effects = press(&mut state, KeyCode::Char('e'));
        let expected = patch::build(
            &patch_of(TWO_HUNKS),
            &PatchSelection::Hunk(1),
            Direction::Stage,
        )
        .unwrap();
        match hand_off(&effects) {
            HandOff::EditHunk { header, hunk } => {
                assert_eq!([header.as_slice(), hunk.as_slice()].concat(), expected);
                assert_eq!(header.as_slice(), HEAD.as_bytes());
                assert!(hunk.starts_with(b"@@ "), "{hunk:?}");
            }
            other => panic!("expected an EditHunk, got {other:?}"),
        }
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn e_refuses_the_staged_side_other_kinds_and_a_running_op() {
        let mut staged = focused(vec![changed("a.rs", 'M', '.')], TWO_HUNKS);
        assert_eq!(press(&mut staged, KeyCode::Char('e')), vec![]);
        assert_eq!(staged.notice, Some("Edit hunks from the unstaged view"));

        let new_file = b"diff --git a/n.txt b/n.txt\nnew file mode 100644\n\
            --- /dev/null\n+++ b/n.txt\n@@ -0,0 +1,2 @@\n+x\n+y\n";
        let mut added = focused(vec![untracked("n.txt")], new_file);
        assert_eq!(press(&mut added, KeyCode::Char('e')), vec![]);
        assert_eq!(
            added.notice,
            Some("Only hunks of changed files can be edited")
        );

        let mut running = focused(three_files(), TWO_HUNKS);
        running.op_seq = Some(5);
        assert_eq!(running.update(AppIntent::EditHunk), vec![]);
        assert_eq!(running.update(AppIntent::OpenInEditor), vec![]);

        // Still loading: no patch to edit yet.
        let (mut loading, _) = with_files(three_files(), 24);
        press(&mut loading, KeyCode::Enter);
        assert_eq!(loading.update(AppIntent::EditHunk), vec![]);

        // From the Files panel there is no hunk.
        let (mut files, _) = with_files(three_files(), 24);
        assert_eq!(files.update(AppIntent::EditHunk), vec![]);
    }

    #[test]
    fn an_edited_hunk_is_staged_with_one_apply_edited() {
        let mut state = focused(three_files(), TWO_HUNKS);
        let effects = state.update(AppIntent::HunkEdited {
            result: Ok(Some(b"edited".to_vec())),
        });
        let (tag, op) = op_of(&effects);
        assert!(tag.user);
        assert_eq!(
            op,
            GitOp::ApplyEdited {
                patch: b"edited".to_vec()
            }
        );
        assert_eq!(state.op_seq, Some(tag.seq));
    }

    #[test]
    fn a_cancelled_edit_says_so_and_runs_nothing() {
        let mut state = focused(three_files(), TWO_HUNKS);
        assert_eq!(
            state.update(AppIntent::HunkEdited { result: Ok(None) }),
            vec![]
        );
        assert_eq!(state.notice, Some("Edit cancelled"));
        assert_eq!(state.op_seq, None);
        assert_eq!(state.popups, []);
    }

    #[test]
    fn a_failed_edit_or_editor_shows_an_error_popup() {
        for (intent, title) in [
            (
                AppIntent::HunkEdited {
                    result: Err("no editor".into()),
                },
                "Could not edit the hunk",
            ),
            (
                AppIntent::EditorClosed {
                    result: Err("no editor".into()),
                },
                "Could not open the editor",
            ),
        ] {
            let mut state = focused(three_files(), TWO_HUNKS);
            assert_eq!(state.update(intent), vec![]);
            match state.popups.as_slice() {
                [Popup::Error(error)] => {
                    assert_eq!(error.title, title);
                    assert_eq!(error.message, "no editor");
                }
                other => panic!("expected one error popup, got {other:?}"),
            }
            assert_eq!(state.op_seq, None);
        }
    }

    #[test]
    fn a_closed_editor_reloads_the_status() {
        let (mut state, _) = with_files(three_files(), 24);
        let effects = state.update(AppIntent::EditorClosed { result: Ok(()) });
        assert!(
            matches!(effects.as_slice(), [Effect::LoadStatus { tag }] if tag.user),
            "{effects:?}"
        );
        assert_eq!(state.busy, 1);
    }

    #[test]
    fn editor_intents_do_nothing_outside_the_workspace() {
        let mut review = AppState::new(Context::Review { number: 1 });
        review.files = three_files();
        review.selected = Some(0);
        for intent in [
            AppIntent::OpenInEditor,
            AppIntent::EditHunk,
            AppIntent::EditorClosed { result: Ok(()) },
            AppIntent::EditorClosed {
                result: Err("x".into()),
            },
            AppIntent::HunkEdited {
                result: Ok(Some(b"p".to_vec())),
            },
            AppIntent::HunkEdited { result: Ok(None) },
        ] {
            assert_eq!(review.update(intent.clone()), vec![], "{intent:?}");
        }
        assert_eq!(review.popups, []);
        assert_eq!(review.notice, None);
    }

    /// The `LoadDiff` request among `effects`, checking it comes with its delay.
    fn diff_request(effects: &[Effect]) -> DiffRequest {
        diff_tag(&effects[..2]);
        match &effects[0] {
            Effect::LoadDiff { request, .. } => request.clone(),
            other => panic!("expected LoadDiff, got {other:?}"),
        }
    }

    fn press_char(state: &mut AppState, c: char) -> Vec<Effect> {
        press(state, KeyCode::Char(c))
    }

    #[test]
    fn more_and_less_context_reload_the_selected_diff_with_the_new_size() {
        let (mut state, _) = with_files(three_files(), 24);
        let effects = state.update(AppIntent::MoreContext);
        assert_eq!(state.diff_options.context, 4);
        assert_eq!(effects.len(), 2, "{effects:?}");
        assert_eq!(diff_request(&effects).options.context, 4);

        let effects = state.update(AppIntent::LessContext);
        let effects2 = state.update(AppIntent::LessContext);
        assert_eq!(state.diff_options.context, 2);
        assert_eq!(diff_request(&effects).options.context, 3);
        assert_eq!(diff_request(&effects2).options.context, 2);
    }

    #[test]
    fn a_context_change_in_diff_focus_reloads_the_patch_with_the_same_options() {
        let mut state = focused(three_files(), TWO_HUNKS);
        let effects = state.update(AppIntent::MoreContext);
        let (_, patch) = patch_load(&effects);
        assert_eq!(diff_request(&effects).options, state.diff_options);
        assert_eq!(patch.options, state.diff_options);
        assert_eq!(patch.options.context, 4);
        assert_eq!(view(&state).seq, Some(patch_load(&effects).0.seq));
    }

    #[test]
    fn context_stops_at_its_limits_and_the_keys_say_why() {
        use crate::git::diff::{MAX_CONTEXT, MIN_CONTEXT};
        let (mut state, _) = with_files(three_files(), 24);
        state.diff_options.context = MAX_CONTEXT;
        for c in ['+', '='] {
            assert_eq!(press_char(&mut state, c), vec![], "{c}");
            assert_eq!(state.notice, Some("Already at the most context"));
        }
        assert_eq!(state.update(AppIntent::MoreContext), vec![]);
        assert_eq!(state.diff_options.context, MAX_CONTEXT);

        state.diff_options.context = MIN_CONTEXT;
        assert_eq!(press_char(&mut state, '-'), vec![]);
        assert_eq!(state.notice, Some("Already at the least context"));
        assert_eq!(state.update(AppIntent::LessContext), vec![]);
        assert_eq!(state.diff_options.context, MIN_CONTEXT);
    }

    #[test]
    fn the_keys_change_context_and_whitespace_from_files_diff_and_lines() {
        let (mut state, _) = with_files(three_files(), 24);
        assert_eq!(
            diff_request(&press_char(&mut state, '+')).options.context,
            4
        );
        assert_eq!(
            diff_request(&press_char(&mut state, '=')).options.context,
            5
        );
        assert_eq!(
            diff_request(&press_char(&mut state, '-')).options.context,
            4
        );
        let request = diff_request(&press_char(&mut state, 'w'));
        assert!(request.options.ignore_whitespace);

        let mut state = focused(three_files(), TWO_HUNKS);
        press_char(&mut state, 'v');
        assert_eq!(state.panel_focus(), KeyScope::Lines);
        let effects = press_char(&mut state, '+');
        assert_eq!(patch_load(&effects).1.options.context, 4);
        let effects = press_char(&mut state, 'w');
        assert!(patch_load(&effects).1.options.ignore_whitespace);
    }

    #[test]
    fn without_a_selected_file_the_options_change_but_nothing_loads() {
        let (mut state, _) = with_files(vec![], 24);
        assert_eq!(state.update(AppIntent::MoreContext), vec![]);
        assert_eq!(state.update(AppIntent::ToggleWhitespace), vec![]);
        assert_eq!(state.update(AppIntent::LessContext), vec![]);
        assert_eq!(state.update(AppIntent::LessContext), vec![]);
        assert_eq!(
            state.diff_options,
            DiffOptions {
                context: 2,
                ignore_whitespace: true
            }
        );
    }

    #[test]
    fn toggling_whitespace_flips_it_and_reloads() {
        let mut state = focused(three_files(), TWO_HUNKS);
        let effects = state.update(AppIntent::ToggleWhitespace);
        assert!(state.diff_options.ignore_whitespace);
        assert!(diff_request(&effects).options.ignore_whitespace);
        assert!(patch_load(&effects).1.options.ignore_whitespace);

        let effects = state.update(AppIntent::ToggleWhitespace);
        assert!(!state.diff_options.ignore_whitespace);
        assert!(!patch_load(&effects).1.options.ignore_whitespace);
    }

    #[test]
    fn hidden_whitespace_refuses_every_key_that_applies_a_patch() {
        const REASON: &str = "Whitespace is hidden; press w to stage";
        let mut state = focused(three_files(), TWO_HUNKS);
        let effects = state.update(AppIntent::ToggleWhitespace);
        let (tag, _) = patch_load(&effects);
        state.update(AppIntent::PatchLoaded {
            tag,
            result: Ok(patch_of(TWO_HUNKS)),
        });
        for c in [' ', 'v', 'd', 'e'] {
            assert_eq!(press_char(&mut state, c), vec![], "{c:?}");
            assert_eq!(state.notice, Some(REASON), "{c:?}");
            assert_eq!(state.popups, [], "{c:?}");
            assert_eq!(state.panel_focus(), KeyScope::Diff, "{c:?}");
        }

        // Line mode, entered before whitespace was hidden.
        state.update(AppIntent::ToggleWhitespace);
        let (tag, _) = patch_load(&state.update(AppIntent::FocusDiff));
        state.update(AppIntent::PatchLoaded {
            tag,
            result: Ok(patch_of(TWO_HUNKS)),
        });
        press_char(&mut state, 'v');
        assert_eq!(state.panel_focus(), KeyScope::Lines);
        let (tag, _) = patch_load(&state.update(AppIntent::ToggleWhitespace));
        state.update(AppIntent::PatchLoaded {
            tag,
            result: Ok(patch_of(TWO_HUNKS)),
        });
        for c in [' ', 'd'] {
            assert_eq!(press_char(&mut state, c), vec![], "{c:?}");
            assert_eq!(state.notice, Some(REASON), "{c:?}");
            assert_eq!(state.popups, [], "{c:?}");
        }

        // The intents themselves refuse too, whatever sent them.
        for intent in [
            AppIntent::ToggleHunkStage,
            AppIntent::ToggleLinesStage,
            AppIntent::Discard,
            AppIntent::EditHunk,
        ] {
            assert_eq!(state.update(intent.clone()), vec![], "{intent:?}");
        }
        assert_eq!(state.popups, []);
    }

    #[test]
    fn hidden_whitespace_leaves_file_level_stage_and_discard_alone() {
        let (mut state, _) = with_files(three_files(), 24);
        state.update(AppIntent::ToggleWhitespace);
        let effects = press_char(&mut state, ' ');
        assert!(
            matches!(effects.as_slice(), [Effect::RunOp { .. }]),
            "{effects:?}"
        );
        state.op_seq = None;
        press_char(&mut state, 'd');
        assert!(
            matches!(state.popups.as_slice(), [Popup::Discard(_)]),
            "{:?}",
            state.popups
        );
    }

    #[test]
    fn a_context_change_keeps_the_scroll_of_the_streamed_diff() {
        let (mut state, effects) = with_files(three_files(), 24);
        state.update(chunk(diff_tag(&effects), 50, true));
        for _ in 0..7 {
            state.update(AppIntent::ScrollDiff(Scroll::LineDown));
        }
        assert_eq!(state.diff.scroll, 7);

        let effects = state.update(AppIntent::MoreContext);
        assert_eq!(state.diff.scroll, 7);
        let tag = diff_tag(&effects);
        state.update(chunk(tag, 8, true));
        assert_eq!(state.diff.scroll, 7);
    }

    const THREE_HUNKS: &[u8] = b"diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n\
        @@ -1 +1 @@\n-a\n+b\n\
        @@ -9 +9 @@\n-e\n+f\n\
        @@ -20 +20 @@\n-g\n+h\n";

    #[test]
    fn a_context_change_keeps_the_selected_hunk() {
        let mut state = focused(three_files(), THREE_HUNKS);
        state.update(AppIntent::NextHunk);
        state.update(AppIntent::NextHunk);
        assert_eq!(view(&state).hunk, 2);

        let effects = state.update(AppIntent::MoreContext);
        assert_eq!(view(&state).hunk, 2);
        let (tag, _) = patch_load(&effects);
        state.update(AppIntent::PatchLoaded {
            tag,
            result: Ok(patch_of(THREE_HUNKS)),
        });
        assert_eq!(view(&state).hunk, 2);
    }

    #[test]
    fn changed_options_last_for_the_session() {
        let (mut state, _) = with_files(three_files(), 24);
        state.update(AppIntent::MoreContext);
        state.update(AppIntent::ToggleWhitespace);
        let changed = state.diff_options;
        assert_ne!(changed, DiffOptions::default());

        assert_eq!(
            diff_request(&state.update(AppIntent::SelectNext)).options,
            changed
        );
        let (_, request) = patch_load(&state.update(AppIntent::FocusDiff));
        assert_eq!(request.options, changed);
        state.update(AppIntent::FocusFiles);
        assert_eq!(
            diff_request(&state.update(AppIntent::SelectPrev)).options,
            changed
        );

        let tag = refresh_tag(&mut state);
        let effects = state.update(AppIntent::StatusLoaded {
            tag,
            result: Ok(repo(three_files())),
        });
        assert_eq!(diff_request(&effects).options, changed);
        assert_eq!(state.diff_options, changed);
    }
}
