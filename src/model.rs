use crate::ansi::StyledRow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::blame_view::{BlameView, CommitView};
use crate::commit_editor::CommitEditor;
use crate::config::ConfigError;
use crate::custom_command::{CommandFailure, CustomCommand, OutputMode, Target};
use crate::diff_pane::DiffPane;
use crate::effect::{Effect, Tag};
use crate::external_diff::{self, ExternalRenderer, map_rows};
use crate::git::FileEntry;
use crate::git::blame::BlameLine;
use crate::git::diff::{
    DiffChunk, DiffLine, DiffOptions, DiffRequest, DiffSide, MAX_CONTEXT, MIN_CONTEXT, sanitize,
};
use crate::git::ops::GitOp;
use crate::git::patch::{self, Direction, FileKind, FilePatch, PatchSelection};
use crate::git::path::RepoPath;
use crate::git::prompts::{PromptFacts, needs_terminal};
use crate::git::refs::RefsSnapshot;
use crate::git::status::{BranchStatus, EntryKind, RepoStatus};
use crate::git::submodule::Submodule;
use crate::handoff::HandOff;
use crate::hunk_view::{HunkView, NO_CHANGES};
use crate::intent::{AppIntent, Scroll};
use crate::keymap::{self, KeyPattern, KeyScope, Keymap, Resolution};
use crate::layout::{self, Panel, ScreenMode, SidePanel};
use crate::menu;
use crate::popup::{DiscardConfirm, DiscardWhat, ErrorPopup, LineInput, Popup, PromptKind};
use crate::recent_list::{RecentList, RecentRow};
use crate::refresh::{self, RefreshPlan, Scope, Selection};
use crate::rendered_view::RenderedDiff;
use crate::submodule_list::SubmoduleList;
use crate::watch::{WatchEvent, WatchState};

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

/// What a repository showed when a submodule was entered from it, put
/// back as it was on the way out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentRepo {
    /// The entered submodule's path, relative to this repository.
    pub path: RepoPath,
    pub files: Vec<FileEntry>,
    pub files_loaded: bool,
    pub branch: Option<BranchStatus>,
    pub selected: Option<usize>,
}

/// What a repository showed when another was opened from the recent list,
/// put back when it is opened again. Only the file list, the selection
/// and which pane had focus: cursors and scroll would be stale by then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParkedRepo {
    /// The submodules it was inside, as `AppState::repo_stack`.
    pub repo_stack: Vec<ParentRepo>,
    pub files: Vec<FileEntry>,
    pub files_loaded: bool,
    pub branch: Option<BranchStatus>,
    pub selected: Option<usize>,
    /// The Diff pane had focus; Files or a blame count as Files.
    pub diff_focused: bool,
}

/// A custom command waiting for its prompts. It holds a copy of the
/// command and the selection taken when its key was pressed, so a config
/// reload or a refresh while the user types cannot change what runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCommand {
    pub command: CustomCommand,
    pub target: Target,
    /// One per prompt answered so far, in order.
    pub answers: Vec<String>,
}

/// Which panel has the keys in the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Focus {
    Status,
    Files,
    Branches,
    Commits,
    Stash,
    /// The Diff pane, working on the selected file's hunks.
    Diff(HunkView),
    /// A blame of the selected file, in the Diff pane's place.
    Blame(BlameView),
}

impl Focus {
    /// The focus of a side panel. Only Files has state of its own.
    pub fn side(panel: SidePanel) -> Self {
        match panel {
            SidePanel::Status => Focus::Status,
            SidePanel::Files => Focus::Files,
            SidePanel::Branches => Focus::Branches,
            SidePanel::Commits => Focus::Commits,
            SidePanel::Stash => Focus::Stash,
        }
    }

    /// Where it is drawn. The blame takes the diff's place.
    pub fn panel(&self) -> Panel {
        match self {
            Focus::Status => Panel::Side(SidePanel::Status),
            Focus::Files => Panel::Side(SidePanel::Files),
            Focus::Branches => Panel::Side(SidePanel::Branches),
            Focus::Commits => Panel::Side(SidePanel::Commits),
            Focus::Stash => Panel::Side(SidePanel::Stash),
            Focus::Diff(_) | Focus::Blame(_) => Panel::Diff,
        }
    }

    /// Whether a side panel has the keys, so the diff pane shows the
    /// selected file's diff.
    pub fn is_side(&self) -> bool {
        matches!(self.panel(), Panel::Side(_))
    }
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
    /// The side panel last focused. It stays expanded while the diff has
    /// focus, and is where focus goes back to.
    pub expanded: SidePanel,
    /// How much of the screen the focused panel takes; kept across focus
    /// changes.
    pub screen_mode: ScreenMode,
    /// Sequence of the Git operation in flight. Operations run one at a
    /// time so two quick presses never race for `index.lock`.
    pub op_seq: Option<u64>,
    /// The running commit has the terminal, so Ctrl+C goes to git, not Kika.
    pub commit_on_terminal: bool,
    /// The last operation's failure, or why one was refused.
    pub op_error: Option<String>,
    /// How the selection is kept when the pending status load lands.
    pub status_selection: Selection,
    pub help_open: bool,
    /// The first help row shown, for a list taller than the screen.
    pub help_scroll: usize,
    pub commit: CommitFlow,
    /// Why the last key did nothing, shown in the bottom bar until the next key.
    pub notice: Option<&'static str>,
    /// Menus, questions, prompts and errors, drawn bottom to top. The top
    /// one takes the keys; closing it shows the one below again.
    pub popups: Vec<Popup>,
    /// How diffs are drawn. Kept in memory for the session: it outlives file
    /// selection, focus changes and reloads.
    pub diff_options: DiffOptions,
    /// Background loads that notice changes made outside Kika.
    pub watch: WatchState,
    /// The repositories entered from, outermost first; the current one is
    /// a submodule of the last. Empty in a top-level repository.
    pub repo_stack: Vec<ParentRepo>,
    /// The diff renderers configured for the session; empty until found.
    pub renderers: Vec<ExternalRenderer>,
    /// Index into `renderers` of the one the Diff pane uses; `None` for
    /// the built-in view. Kept for the session.
    pub renderer: Option<usize>,
    /// The shown diff as the active renderer drew it.
    pub rendered: Option<RenderedDiff>,
    /// The keys in force, after the user's config files.
    pub keymap: Keymap,
    /// The config errors last shown, so a reload that finds the same
    /// broken file does not show them again.
    pub config_error: Option<String>,
    /// The top-level repository open now; `None` outside one.
    pub repo: Option<PathBuf>,
    /// Where Kika was started, which `git init` makes a repository.
    pub launch: PathBuf,
    /// The recent repositories, newest first, as last read from disk.
    pub recent: Vec<PathBuf>,
    /// Repositories switched away from, by top level, to restore on return.
    pub parked: HashMap<PathBuf, ParkedRepo>,
    /// The custom command whose prompts are open.
    pub pending_command: Option<PendingCommand>,
    /// A custom command with captured output is running. One at a time,
    /// so their results cannot pile up.
    pub command_running: bool,
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
            expanded: SidePanel::Files,
            screen_mode: ScreenMode::Normal,
            op_seq: None,
            commit_on_terminal: false,
            op_error: None,
            status_selection: Selection::KeepPath,
            help_open: false,
            help_scroll: 0,
            commit: CommitFlow::Closed,
            notice: None,
            popups: Vec::new(),
            diff_options: DiffOptions::default(),
            watch: WatchState::default(),
            repo_stack: Vec::new(),
            renderers: Vec::new(),
            renderer: None,
            rendered: None,
            keymap: Keymap::default(),
            config_error: None,
            repo: None,
            launch: PathBuf::new(),
            recent: Vec::new(),
            parked: HashMap::new(),
            pending_command: None,
            command_running: false,
        }
    }

    /// Where Kika runs: the repository's top level, if it started in one,
    /// and the directory it started in.
    pub fn with_location(self, repo: Option<PathBuf>, launch: PathBuf) -> Self {
        Self {
            repo,
            launch,
            ..self
        }
    }

    /// Apply an intent and return the effects to run. Pure: no I/O, no rendering.
    pub fn update(&mut self, intent: AppIntent) -> Vec<Effect> {
        match intent {
            AppIntent::Key(key) => {
                self.notice = None;
                let focus = self.key_focus();
                match keymap::resolve(keymap::registry(), &self.keymap, focus, self, key) {
                    Resolution::Run(binding) => return self.update(binding.intent.clone()),
                    Resolution::Command(index) => return self.start_command(index),
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
            AppIntent::OpenHelp => {
                self.help_open = true;
                self.help_scroll = 0;
            }
            AppIntent::ScrollHelp(by) => self.scroll_help(by),
            AppIntent::CloseHelp => self.help_open = false,
            AppIntent::Resize { width, height } => {
                let cols = self.pane_cols();
                self.size = (width, height);
                let mut effects = self.diff.demand(self.pane_rows());
                if self.pane_cols() != cols {
                    effects.extend(self.render_shown());
                }
                return effects;
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
                if self.rendered_shown().is_some()
                    && let Some(rendered) = &mut self.rendered
                {
                    rendered.scroll(by, rows);
                } else if let Focus::Diff(view) = &mut self.focus {
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
            AppIntent::FocusLeft => {
                let cols = self.pane_cols();
                self.focus = Focus::side(self.expanded);
                let effects = self.render_if_stale();
                return self.rerender_if_resized(cols, effects);
            }
            AppIntent::FocusPanel(panel) => {
                let cols = self.pane_cols();
                let effects = self.focus_panel(panel);
                return self.rerender_if_resized(cols, effects);
            }
            AppIntent::CycleFocus { back } => {
                let cols = self.pane_cols();
                let effects = self.cycle_focus(back);
                return self.rerender_if_resized(cols, effects);
            }
            AppIntent::CycleScreenMode => {
                let cols = self.pane_cols();
                self.screen_mode = self.screen_mode.next();
                return self.rerender_if_resized(cols, vec![]);
            }
            AppIntent::NextHunk => {
                let rows = self.pane_rows();
                if let Focus::Diff(view) = &mut self.focus {
                    view.next(rows);
                }
                self.follow_cursor();
            }
            AppIntent::PrevHunk => {
                let rows = self.pane_rows();
                if let Focus::Diff(view) = &mut self.focus {
                    view.prev(rows);
                }
                self.follow_cursor();
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
            AppIntent::PatchLoaded { tag, result } => {
                self.patch_loaded(tag, result);
                self.remap();
            }
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
            AppIntent::InterruptCommit => {
                let intent = match self.commit {
                    CommitFlow::Running(_) => AppIntent::Quit,
                    _ => AppIntent::CancelCommit,
                };
                return self.update(intent);
            }
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
            AppIntent::CommitProbed { tag, op, facts } => {
                return self.commit_probed(tag, op, facts);
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
            AppIntent::PopPopup => match self.popups.pop() {
                // Closing the welcome shows what git says about the launch
                // directory, as Kika did before there was a welcome.
                Some(Popup::Recent(_)) if self.repo.is_none() => return self.refresh_status(),
                // Cancelling a command's prompt cancels the command.
                Some(Popup::Prompt {
                    kind: PromptKind::Command,
                    ..
                }) => self.pending_command = None,
                _ => {}
            },
            AppIntent::CommandPromptAnswered(text) => return self.command_answered(text),
            AppIntent::CommandFinished {
                title,
                output,
                result,
            } => return self.command_finished(&title, output, result),
            AppIntent::CancelCommand => {
                // The flag stays set until the command answers.
                if self.command_running {
                    return vec![Effect::CancelCommand];
                }
            }
            AppIntent::OpenInEditor => return self.open_in_editor(),
            AppIntent::EditHunk => return self.edit_hunk(),
            AppIntent::EditorClosed { result } => return self.editor_closed(result),
            AppIntent::HunkEdited { result } => return self.hunk_edited(result),
            AppIntent::Watch(event) => return self.watch(event),
            AppIntent::RefsLoaded { tag, result } => return self.refs_loaded(tag, result),
            AppIntent::FetchFinished { tag, result } => return self.fetch_finished(tag, result),
            AppIntent::OpenSubmodules => return self.open_submodules(),
            AppIntent::SubmodulesLoaded { tag, result } => self.submodules_loaded(tag, result),
            AppIntent::SubmoduleNext => self.move_submodule_cursor(1),
            AppIntent::SubmodulePrev => self.move_submodule_cursor(-1),
            AppIntent::InitSubmodule => {
                let op = self
                    .selected_submodule()
                    .map(|s| GitOp::SubmoduleInit(s.path.clone()));
                return self.start_op(Ok(op));
            }
            AppIntent::UpdateSubmodule => {
                let op = self
                    .selected_submodule()
                    .map(|s| GitOp::SubmoduleUpdate(s.path.clone()));
                return self.start_op(Ok(op));
            }
            AppIntent::EnterSubmodule => return self.enter_submodule(),
            AppIntent::LeaveSubmodule => return self.leave_submodule(),
            AppIntent::OpenBlame => return self.open_blame(),
            AppIntent::BlameLoaded { tag, result } => self.blame_loaded(tag, result),
            AppIntent::BlameNext => self.with_blame(|view, rows| view.move_by(1, rows)),
            AppIntent::BlamePrev => self.with_blame(|view, rows| view.move_by(-1, rows)),
            AppIntent::BlamePageDown => self.with_blame(|view, rows| view.page(true, rows)),
            AppIntent::BlamePageUp => self.with_blame(|view, rows| view.page(false, rows)),
            AppIntent::OpenBlameCommit => return self.open_blame_commit(),
            AppIntent::CommitLoaded { tag, result } => self.commit_loaded(tag, result),
            AppIntent::CloseBlameCommit => {
                if let Focus::Blame(view) = &mut self.focus {
                    view.commit = None;
                }
            }
            AppIntent::ScrollCommit(by) => {
                let rows = self.pane_rows();
                if let Focus::Blame(BlameView {
                    commit: Some(commit),
                    ..
                }) = &mut self.focus
                {
                    commit.scroll(by, rows);
                }
            }
            AppIntent::CloseBlame => {
                if matches!(self.focus, Focus::Blame(_)) {
                    self.focus = Focus::side(self.expanded);
                    return self.render_if_stale();
                }
            }
            AppIntent::CycleRenderer => return self.cycle_renderer(),
            AppIntent::RenderersFound(renderers) => {
                self.renderers = renderers;
                if self.renderer.is_some_and(|i| i >= self.renderers.len()) {
                    self.renderer = None;
                    self.rendered = None;
                }
            }
            AppIntent::DiffRendered { tag, result } => self.diff_rendered(tag, result),
            AppIntent::ConfigLoaded {
                repo,
                keymap,
                errors,
            } => self.config_loaded(repo, keymap, errors),
            AppIntent::Start => return self.start(),
            AppIntent::OpenRecent => return self.open_recent(),
            AppIntent::RecentLoaded(list) => {
                self.recent = list;
                self.move_recent_cursor(0);
            }
            AppIntent::RecentNext => self.move_recent_cursor(1),
            AppIntent::RecentPrev => self.move_recent_cursor(-1),
            AppIntent::RecentSelect => return self.recent_select(),
            AppIntent::OpenRepo(path) => return self.open_repo(path),
            AppIntent::InitRepo => return self.init_repo(),
            AppIntent::RepoInitialised { tag, result } => {
                return self.repo_initialised(tag, result);
            }
        }
        vec![]
    }

    /// Use the keys just read. A broken file is shown once: reloading on
    /// every focus-in must not bring the same popup back each time.
    ///
    /// A result read for another repository is dropped: the user switched
    /// while it loaded, and its keys and errors are not the open one's.
    fn config_loaded(&mut self, repo: Option<PathBuf>, keymap: Keymap, errors: Vec<ConfigError>) {
        if repo != self.repo {
            return;
        }
        self.keymap = keymap;
        if errors.is_empty() {
            self.config_error = None;
            return;
        }
        let text = errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        if self.config_error.as_deref() != Some(text.as_str()) {
            self.push_error(ErrorPopup::new(
                "Config error",
                &text,
                "Kika ignored this file. Fix it, then come back to Kika to reload it.",
            ));
            self.config_error = Some(text);
        }
    }

    /// Move the help list by a line or a page, never past its last screenful.
    /// The popup's inside is as tall as the diff pane's.
    fn scroll_help(&mut self, by: Scroll) {
        let rows = self.pane_rows();
        let focus = self.panel_focus();
        // As help lists them: the built-in keys, then a rule and the
        // user's commands, if there are any.
        let commands = keymap::available_commands(&self.keymap, focus).len();
        let rule = usize::from(commands > 0);
        let total = keymap::available(keymap::registry(), &self.keymap, focus, self).len()
            + rule
            + commands;
        let scroll = match by {
            Scroll::LineDown => self.help_scroll.saturating_add(1),
            Scroll::LineUp => self.help_scroll.saturating_sub(1),
            Scroll::PageDown => self.help_scroll.saturating_add(rows),
            Scroll::PageUp => self.help_scroll.saturating_sub(rows),
        };
        self.help_scroll = scroll.min(total.saturating_sub(rows));
    }

    /// The panel keys go to. Panels with no keys of their own yet use the
    /// workspace's.
    pub fn panel_focus(&self) -> KeyScope {
        match (&self.context, &self.focus) {
            (Context::Workspace, Focus::Files) => KeyScope::Files,
            (
                Context::Workspace,
                Focus::Status | Focus::Branches | Focus::Commits | Focus::Stash,
            ) => KeyScope::Workspace,
            (Context::Workspace, Focus::Diff(view)) if view.lines.is_some() => KeyScope::Lines,
            (Context::Workspace, Focus::Diff(_)) => KeyScope::Diff,
            (Context::Workspace, Focus::Blame(view)) if view.commit.is_some() => {
                KeyScope::BlameCommit
            }
            (Context::Workspace, Focus::Blame(_)) => KeyScope::Blame,
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
            Some(Popup::Submodules(_)) => return KeyScope::Submodules,
            Some(Popup::Recent(_)) => return KeyScope::Recent,
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

    /// Columns a renderer may fill: the Diff pane as the view lays it out
    /// for this focus and screen mode, minus its borders and the cursor
    /// gutter. Where the mode hides the diff, its width in Normal.
    pub fn pane_cols(&self) -> u16 {
        let screen = ratatui::layout::Rect::new(0, 0, self.size.0, self.size.1);
        let [main, _] = layout::screen_layout(screen);
        let panel = self.focus.panel();
        let diff = layout::workspace_layout(main, panel, self.expanded, self.screen_mode)
            .rect(Panel::Diff)
            .or_else(|| {
                layout::workspace_layout(main, panel, self.expanded, ScreenMode::Normal)
                    .rect(Panel::Diff)
            })
            .unwrap_or(main);
        diff.width.saturating_sub(3).max(1)
    }

    /// Render again when a focus or mode change gave the diff another
    /// width than `cols`, so a renderer's columns match the pane.
    fn rerender_if_resized(&mut self, cols: u16, mut effects: Vec<Effect>) -> Vec<Effect> {
        let pending = self.rendered.as_ref().is_some_and(|r| r.seq.is_some());
        if self.pane_cols() != cols && self.active_renderer().is_some() && !pending {
            effects.extend(self.render_shown());
        }
        effects
    }

    /// The renderer the Diff pane uses, if not the built-in view.
    pub fn active_renderer(&self) -> Option<&ExternalRenderer> {
        self.renderer.and_then(|i| self.renderers.get(i))
    }

    /// The diff the Diff pane shows: the selected file's from the Files
    /// panel, the focused view's side with the Diff pane focused (renames
    /// off, as its patch has them), nothing behind a blame.
    fn shown_request(&self) -> Option<DiffRequest> {
        match &self.focus {
            Focus::Status | Focus::Files | Focus::Branches | Focus::Commits | Focus::Stash => self
                .selected
                .and_then(|i| self.files.get(i))
                .map(|entry| DiffRequest::for_entry(entry, self.diff_options)),
            Focus::Diff(view) => Some(DiffRequest {
                path: view.path.clone(),
                orig_path: None,
                side: view.side,
                options: self.diff_options,
            }),
            Focus::Blame(_) => None,
        }
    }

    /// The rendered diff, when a renderer is active and it is of the
    /// shown file and side.
    pub fn rendered_for_shown(&self) -> Option<&RenderedDiff> {
        self.active_renderer()?;
        let request = self.shown_request()?;
        self.rendered
            .as_ref()
            .filter(|r| r.path == request.path && r.side == request.side)
    }

    /// The rendered diff when the Diff pane draws it. With the pane
    /// focused, only while the selection has rows: a cursor on rows the
    /// renderer did not draw would lie, so the built-in view is drawn.
    pub fn rendered_shown(&self) -> Option<&RenderedDiff> {
        let rendered = self.rendered_for_shown()?;
        match &self.focus {
            Focus::Diff(view) if rendered.error.is_none() && !rendered.rendering() => {
                let picked = view.picked();
                rendered
                    .shows(view.hunk, picked.as_deref())
                    .then_some(rendered)
            }
            _ => Some(rendered),
        }
    }

    /// Step through built-in, each renderer, and back to built-in.
    fn cycle_renderer(&mut self) -> Vec<Effect> {
        if self.renderers.is_empty() {
            self.notice = Some(external_diff::NO_RENDERER);
            return vec![];
        }
        self.renderer = match self.renderer {
            None => Some(0),
            Some(i) if i + 1 < self.renderers.len() => Some(i + 1),
            Some(_) => None,
        };
        // Another renderer's rows must not show under this one's name.
        self.rendered = None;
        self.render_shown()
    }

    /// Run the active renderer on the shown diff. A user effect, so the
    /// busy indicator shows while it runs.
    fn render_shown(&mut self) -> Vec<Effect> {
        let Some(renderer) = self.active_renderer().cloned() else {
            return vec![];
        };
        let Some(request) = self.shown_request() else {
            return vec![];
        };
        let seq = self.take_seq();
        match &mut self.rendered {
            Some(r) if r.path == request.path && r.side == request.side => {
                r.seq = Some(seq);
                r.error = None;
            }
            _ => self.rendered = Some(RenderedDiff::new(seq, request.path.clone(), request.side)),
        }
        self.busy += 1;
        let tag = Tag {
            generation: self.generation,
            seq,
            user: true,
        };
        vec![Effect::RenderDiff {
            tag,
            request,
            renderer,
            cols: self.pane_cols(),
        }]
    }

    /// Render again only if what is rendered is not what is shown, such
    /// as after the Diff pane gives focus back on another side.
    fn render_if_stale(&mut self) -> Vec<Effect> {
        if self.active_renderer().is_some() && self.rendered_for_shown().is_none() {
            self.render_shown()
        } else {
            vec![]
        }
    }

    fn diff_rendered(&mut self, tag: Tag, result: Result<Vec<StyledRow>, String>) {
        // Every user effect reports back once, stale or not.
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        if tag.generation != self.generation {
            return;
        }
        if let Some(rendered) = &mut self.rendered
            && rendered.apply(tag.seq, result)
        {
            self.remap();
        }
    }

    /// Map the rendered rows to the focused view's patch, when both are
    /// of the same file and side, then bring the selection into view.
    fn remap(&mut self) {
        let Some(rendered) = &mut self.rendered else {
            return;
        };
        rendered.map = match &self.focus {
            Focus::Diff(view) if view.path == rendered.path && view.side == rendered.side => view
                .patch
                .as_ref()
                .map(|patch| map_rows(patch, &rendered.rows))
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        self.follow_cursor();
    }

    /// Keep the rows of the selected hunk or lines inside the pane.
    fn follow_cursor(&mut self) {
        let rows = self.pane_rows();
        let (Focus::Diff(view), Some(rendered)) = (&self.focus, &mut self.rendered) else {
            return;
        };
        let picked = view.picked();
        let selection = rendered.mapped_rows(view.hunk, picked.as_deref());
        rendered.scroll_to(&selection, rows);
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
            // Bound only in Files and the diff; nothing to discard elsewhere.
            _ => return Ok(None),
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
            // Bound only in Files and the diff; nothing to open elsewhere.
            _ => return vec![],
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

    /// Start the user's command `index` on the current selection: ask its
    /// prompts first, if it has any.
    fn start_command(&mut self, index: usize) -> Vec<Effect> {
        let Some(command) = self.keymap.commands.get(index).cloned() else {
            return vec![];
        };
        let Some(target) = self.command_target() else {
            self.notice = Some("No file selected");
            return vec![];
        };
        if self.command_running {
            self.notice = Some("A command is running");
            return vec![];
        }
        // Say now what the selection lacks, not after the prompts.
        let blanks = vec![String::new(); command.prompts.len()];
        if let Err(reason) = command.expand(&target, &blanks) {
            self.notice = Some(reason);
            return vec![];
        }
        self.ask_or_run(PendingCommand {
            command,
            target,
            answers: Vec::new(),
        })
    }

    /// What a command in the focused panel runs on: the selected file, and
    /// in the Diff pane its line, in the blame the cursor's line and commit.
    fn command_target(&self) -> Option<Target> {
        match &self.focus {
            // Commands bind only in Files, the diff and the blame.
            Focus::Status | Focus::Branches | Focus::Commits | Focus::Stash => None,
            Focus::Files => {
                let entry = self.selected.and_then(|i| self.files.get(i))?;
                Some(Target {
                    path: entry.path.clone(),
                    line: None,
                    commit: None,
                })
            }
            Focus::Diff(view) => Some(Target {
                path: view.path.clone(),
                line: view.editor_line(),
                commit: None,
            }),
            Focus::Blame(view) => {
                let line = view.selected();
                Some(Target {
                    path: view.path.clone(),
                    line: line.and_then(|l| u32::try_from(l.line_no).ok()),
                    commit: line
                        .filter(|l| !l.oid.is_empty() && !l.is_uncommitted())
                        .map(|l| l.oid.clone()),
                })
            }
        }
    }

    /// Open the next unanswered prompt, or run the command once all are.
    fn ask_or_run(&mut self, pending: PendingCommand) -> Vec<Effect> {
        if pending.answers.len() < pending.command.prompts.len() {
            self.pending_command = Some(pending);
            self.popups.push(Popup::Prompt {
                kind: PromptKind::Command,
                input: LineInput::default(),
            });
            return vec![];
        }
        self.run_command(pending)
    }

    fn command_answered(&mut self, text: String) -> Vec<Effect> {
        let Some(mut pending) = self.pending_command.take() else {
            return vec![];
        };
        pending.answers.push(text);
        self.ask_or_run(pending)
    }

    /// Hand the terminal to the command, or run it in the background when
    /// its output is captured.
    fn run_command(&mut self, pending: PendingCommand) -> Vec<Effect> {
        let PendingCommand {
            command,
            target,
            answers,
        } = pending;
        let argv = match command.expand(&target, &answers) {
            Ok(argv) => argv,
            Err(reason) => {
                self.notice = Some(reason);
                return vec![];
            }
        };
        // A popup title: one line, safe to draw.
        let title = sanitize(command.description.as_bytes());
        match command.output {
            OutputMode::Terminal => vec![Effect::HandOff(HandOff::RunCommand { title, argv })],
            output => {
                self.command_running = true;
                self.busy += 1;
                vec![Effect::RunCommand {
                    title,
                    argv,
                    output,
                }]
            }
        }
    }

    /// Show what the command printed or why it failed, then reload: it
    /// may have changed the repository. Only a command that could not run
    /// points at the config file.
    fn command_finished(
        &mut self,
        title: &str,
        output: OutputMode,
        result: Result<String, CommandFailure>,
    ) -> Vec<Effect> {
        if output != OutputMode::Terminal && self.command_running {
            self.command_running = false;
            self.busy = self.busy.saturating_sub(1);
        }
        match result {
            Err(CommandFailure::Launch(message)) => self.push_error(ErrorPopup::new(
                format!("{title} failed"),
                &message,
                "Check the command in your config file, then try again.",
            )),
            Err(CommandFailure::Exited(message)) => self.push_error(ErrorPopup::new(
                format!("{title} failed"),
                &message,
                "Press Esc to close",
            )),
            Err(CommandFailure::Cancelled) => self.notice = Some("Command cancelled"),
            Ok(text) if output == OutputMode::Popup => {
                let text = if text.is_empty() {
                    "(no output)"
                } else {
                    &text
                };
                self.push_error(ErrorPopup::new(title, text, "Press Esc to close"));
            }
            Ok(_) => {}
        }
        self.status_selection = Selection::KeepPath;
        self.refresh_status()
    }

    /// The title of a prompt: a command's comes from the user's config,
    /// made one line and safe to draw.
    pub fn prompt_title(&self, kind: PromptKind) -> String {
        let pending = self
            .pending_command
            .as_ref()
            .filter(|_| kind == PromptKind::Command);
        pending
            .and_then(|p| p.command.prompts.get(p.answers.len()))
            .map_or_else(|| kind.title().to_string(), |t| sanitize(t.as_bytes()))
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
            self.remap();
        }
        effects.extend(self.render_shown());
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

    /// Focus `panel`. A side panel expands, and closes a blame on the way.
    /// The diff keeps its hunk view when it already has focus.
    fn focus_panel(&mut self, panel: Panel) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        match panel {
            Panel::Side(side) => {
                self.focus = Focus::side(side);
                self.expanded = side;
                // Leaving the diff or a blame, the pane shows the selected file.
                self.render_if_stale()
            }
            Panel::Diff if self.focus.is_side() => self.focus_diff(),
            Panel::Diff => vec![],
        }
    }

    /// Focus the next panel in screen order, the diff after the side
    /// panels. The diff is skipped with no file to show.
    fn cycle_focus(&mut self, back: bool) -> Vec<Effect> {
        let mut order: Vec<Panel> = SidePanel::ALL.into_iter().map(Panel::Side).collect();
        if self.selected.and_then(|i| self.files.get(i)).is_some() {
            order.push(Panel::Diff);
        }
        let here = self.focus.panel();
        let at = order.iter().position(|&p| p == here).unwrap_or(0);
        let step = if back { order.len() - 1 } else { 1 };
        let next = order[(at + step) % order.len()];
        self.focus_panel(next)
    }

    /// Focus the Diff pane on the selected file, on the side its diff shows.
    fn focus_diff(&mut self) -> Vec<Effect> {
        let Some(entry) = self.selected.and_then(|i| self.files.get(i)) else {
            return vec![];
        };
        let request = DiffRequest::for_entry(entry, self.diff_options);
        self.focus = Focus::Diff(HunkView::new(request.path, request.side));
        let mut effects = self.load_patch();
        self.remap();
        effects.extend(self.render_shown());
        effects
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
        self.follow_cursor();
    }

    fn toggle_hunk_stage(&mut self) -> Vec<Effect> {
        match &self.focus {
            Focus::Diff(view) => self.toggle_selection_stage(PatchSelection::Hunk(view.hunk)),
            _ => vec![],
        }
    }

    fn toggle_lines_stage(&mut self) -> Vec<Effect> {
        match &self.focus {
            Focus::Diff(view) => {
                self.toggle_selection_stage(PatchSelection::Lines(view.selected_lines()))
            }
            _ => vec![],
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
        let mut effects = self.load_patch();
        self.remap();
        effects.extend(self.render_shown());
        effects
    }

    /// Keep the selection on the same file across a refresh when it still
    /// exists, else near the same place.
    fn reselect(&mut self, old_path: Option<RepoPath>) -> Vec<Effect> {
        if self.files.is_empty() {
            self.selected = None;
            self.diff = DiffPane::default();
            self.rendered = None;
            // Nothing is left for the diff or a blame to show.
            if !self.focus.is_side() {
                self.focus = Focus::side(self.expanded);
            }
            return vec![Effect::CancelDiff];
        }
        let index = old_path
            .and_then(|path| self.files.iter().position(|f| f.path == path))
            .or_else(|| self.selected.map(|i| i.min(self.files.len() - 1)))
            .unwrap_or(0);
        self.select(index)
    }

    /// Blame the selected file from the Files panel. A user effect, so the
    /// busy indicator shows while git works.
    fn open_blame(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace || self.focus != Focus::Files {
            return vec![];
        }
        if let Some(reason) = keymap::blame_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let Some(path) = self
            .selected
            .and_then(|i| self.files.get(i))
            .map(|f| f.path.clone())
        else {
            return vec![];
        };
        let seq = self.take_seq();
        self.busy += 1;
        self.focus = Focus::Blame(BlameView::new(path.clone(), seq));
        let tag = Tag {
            generation: self.generation,
            seq,
            user: true,
        };
        vec![Effect::LoadBlame { tag, path }]
    }

    fn blame_loaded(&mut self, tag: Tag, result: Result<Vec<BlameLine>, String>) {
        // Every user effect reports back once, stale or not.
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        let Focus::Blame(view) = &mut self.focus else {
            return;
        };
        if tag.generation != self.generation || view.seq != Some(tag.seq) {
            return;
        }
        view.loaded(result);
    }

    /// Change the blame's cursor, given the pane's rows.
    fn with_blame(&mut self, change: impl FnOnce(&mut BlameView, usize)) {
        let rows = self.pane_rows();
        if let Focus::Blame(view) = &mut self.focus
            && view.commit.is_none()
        {
            change(view, rows);
        }
    }

    /// Open the commit that last changed the cursor's line. A line edited
    /// in the worktree has no commit to show.
    fn open_blame_commit(&mut self) -> Vec<Effect> {
        let Focus::Blame(view) = &self.focus else {
            return vec![];
        };
        let Some(line) = view.selected().filter(|_| view.commit.is_none()) else {
            return vec![];
        };
        if line.is_uncommitted() {
            self.notice = Some("Not committed yet");
            return vec![];
        }
        let oid = line.oid.clone();
        let seq = self.take_seq();
        if let Focus::Blame(view) = &mut self.focus {
            view.commit = Some(CommitView::new(oid.clone(), seq));
        }
        self.busy += 1;
        let tag = Tag {
            generation: self.generation,
            seq,
            user: true,
        };
        vec![Effect::LoadCommit { tag, oid }]
    }

    fn commit_loaded(&mut self, tag: Tag, result: Result<Vec<DiffLine>, String>) {
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        if tag.generation != self.generation {
            return;
        }
        if let Focus::Blame(BlameView {
            commit: Some(commit),
            ..
        }) = &mut self.focus
            && commit.seq == Some(tag.seq)
        {
            commit.loaded(result);
        }
    }

    fn diff_loaded(&mut self, tag: Tag, result: Result<DiffChunk, String>) {
        if tag.generation == self.generation {
            self.diff.apply(tag.seq, result);
        }
    }

    /// Run the operation a stage key chose. Only one runs at a time, and
    /// only on the local workspace.
    fn start_op(&mut self, op: Result<Option<GitOp>, String>) -> Vec<Effect> {
        self.start_op_as(op, |tag, op| Effect::RunOp { tag, op })
    }

    /// [`Self::start_op`], with `effect` saying what work starts it: a
    /// commit is probed first, everything else runs at once.
    fn start_op_as(
        &mut self,
        op: Result<Option<GitOp>, String>,
        effect: fn(Tag, GitOp) -> Effect,
    ) -> Vec<Effect> {
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
        vec![effect(tag, op)]
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
        // Whether it needs the terminal is read first; see `commit_probed`.
        let effects = self.start_op_as(Ok(Some(op)), |tag, op| Effect::ProbeCommit { tag, op });
        if !effects.is_empty() {
            self.commit = CommitFlow::Running(editor);
        }
        effects
    }

    /// Run the probed commit on the terminal when it may ask something,
    /// else detached. `busy` and `op_seq` stay as they are until
    /// `OpFinished`; a probe for another repository or op ends the commit
    /// here, giving the message back.
    fn commit_probed(&mut self, tag: Tag, op: GitOp, facts: Option<PromptFacts>) -> Vec<Effect> {
        if tag.generation != self.generation || self.op_seq != Some(tag.seq) {
            if tag.user {
                self.busy = self.busy.saturating_sub(1);
            }
            if self.op_seq == Some(tag.seq) {
                self.op_seq = None;
            }
            self.end_commit(None);
            return vec![];
        }
        if needs_terminal(&op, facts.as_ref()) {
            self.commit_on_terminal = true;
            vec![Effect::HandOff(HandOff::Commit { tag, op })]
        } else {
            vec![Effect::RunOp { tag, op }]
        }
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
                let error = if matches!(op, GitOp::ApplyEdited { .. }) {
                    self.edited_hunk_error(&message)
                } else {
                    ErrorPopup::new(
                        failure_title(&op),
                        &message,
                        "The file list was reloaded. Fix the cause above, then try again.",
                    )
                };
                self.push_error(error);
            }
        }
        // Refresh even after a failure: a failed `add` of several paths can
        // still have changed the index.
        self.apply_plan(refresh::plan(&op))
    }

    /// Git's words alone do not say what was wrong with the user's edit or
    /// how to retry, so lead with that and name the key from their keymap.
    pub(crate) fn edited_hunk_error(&self, message: &str) -> ErrorPopup {
        let key = keymap::registry()
            .iter()
            .find(|b| b.id == "diff.edit_hunk")
            .and_then(|binding| self.keymap.keys(binding).first())
            .map(KeyPattern::label);
        let next = match key {
            Some(key) => format!("Press {key} to edit the hunk again."),
            None => "Edit the hunk again to retry.".to_string(),
        };
        ErrorPopup::new(
            failure_title(&GitOp::ApplyEdited { patch: vec![] }),
            &format!("{EDITED_HUNK_INVALID}\n{message}"),
            next,
        )
    }

    /// Leave `Running`: closed on success, `Failed` with the output on
    /// failure, back to `Editing` when there is no result to show.
    fn end_commit(&mut self, result: Option<Result<(), String>>) {
        self.commit_on_terminal = false;
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
    /// diff too. The submodule list reloads only while it is shown.
    fn apply_plan(&mut self, plan: RefreshPlan) -> Vec<Effect> {
        let mut effects =
            if plan.scopes.contains(&Scope::Files) || plan.scopes.contains(&Scope::Branch) {
                self.status_selection = plan.selection;
                self.refresh_status()
            } else {
                match self.selected {
                    Some(index) if plan.scopes.contains(&Scope::Diff) => self.select(index),
                    _ => vec![],
                }
            };
        if plan.scopes.contains(&Scope::Submodules) {
            effects.extend(self.reload_submodules());
        }
        effects
    }

    /// The Submodules popup, wherever it is on the stack.
    fn submodule_list(&mut self) -> Option<&mut SubmoduleList> {
        self.popups.iter_mut().rev().find_map(|popup| match popup {
            Popup::Submodules(list) => Some(list),
            _ => None,
        })
    }

    /// The submodule under the popup's cursor.
    pub fn selected_submodule(&self) -> Option<&Submodule> {
        self.popups.iter().rev().find_map(|popup| match popup {
            Popup::Submodules(list) => list.selected(),
            _ => None,
        })
    }

    fn move_submodule_cursor(&mut self, delta: isize) {
        if let Some(list) = self.submodule_list() {
            list.move_cursor(delta);
        }
    }

    /// Show the submodules, loading them afresh.
    fn open_submodules(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        let seq = self.take_seq();
        self.popups
            .push(Popup::Submodules(SubmoduleList::loading(seq)));
        self.load_submodules(seq)
    }

    /// Load the list again for the open popup, keeping what it shows
    /// until the result lands. Nothing while the popup is closed.
    fn reload_submodules(&mut self) -> Vec<Effect> {
        if self.submodule_list().is_none() {
            return vec![];
        }
        let seq = self.take_seq();
        if let Some(list) = self.submodule_list() {
            list.seq = seq;
        }
        self.load_submodules(seq)
    }

    fn load_submodules(&mut self, seq: u64) -> Vec<Effect> {
        self.busy += 1;
        vec![Effect::LoadSubmodules {
            tag: Tag {
                generation: self.generation,
                seq,
                user: true,
            },
        }]
    }

    fn submodules_loaded(&mut self, tag: Tag, result: Result<Vec<Submodule>, String>) {
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        if tag.generation != self.generation {
            return;
        }
        if let Some(list) = self.submodule_list()
            && list.seq == tag.seq
        {
            list.loaded(result);
        }
    }

    /// The paths from the launch repository to the current one.
    fn nested(&self) -> Vec<RepoPath> {
        self.repo_stack.iter().map(|p| p.path.clone()).collect()
    }

    /// Work in the selected submodule as if Kika had started there. This
    /// repository's list and selection are kept for the way back.
    fn enter_submodule(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if let Some(reason) = keymap::enter_submodule_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let Some(path) = self.selected_submodule().map(|s| s.path.clone()) else {
            return vec![];
        };
        // A loaded status, which listing submodules needs, means a repository.
        let Some(root) = self.repo.clone() else {
            return vec![];
        };
        self.repo_stack.push(ParentRepo {
            path,
            files: std::mem::take(&mut self.files),
            files_loaded: self.files_loaded,
            branch: self.branch.take(),
            selected: self.selected.take(),
        });
        self.switch_repo();
        self.files_loaded = false;
        let mut effects = vec![self.set_repo(&root)];
        effects.extend(self.refresh_status());
        effects
    }

    /// Back to the repository this one was entered from, showing what it
    /// showed then while a fresh status load catches up.
    fn leave_submodule(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if let Some(reason) = keymap::leave_submodule_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let Some(root) = self.repo.clone() else {
            return vec![];
        };
        let Some(parent) = self.repo_stack.pop() else {
            return vec![];
        };
        self.switch_repo();
        self.files = parent.files;
        self.files_loaded = parent.files_loaded;
        self.branch = parent.branch;
        let mut effects = vec![self.set_repo(&root)];
        if let Some(index) = parent.selected.filter(|&i| i < self.files.len()) {
            effects.extend(self.select(index));
        }
        effects.extend(self.refresh_status());
        effects
    }

    /// Point git at `root` and the submodules entered from it.
    fn set_repo(&self, root: &Path) -> Effect {
        Effect::SetRepo {
            root: root.to_path_buf(),
            nested: self.nested(),
        }
    }

    /// Record the repository Kika started in and load it, or welcome the
    /// user with the recent list and the offer to make one here.
    fn start(&mut self) -> Vec<Effect> {
        match self.repo.clone() {
            Some(path) => {
                let mut effects = vec![Effect::RecordRecent { path }];
                effects.extend(self.refresh_status());
                effects
            }
            None => self.open_recent(),
        }
    }

    /// Show the recent list, offering `git init` outside a repository,
    /// and read the list afresh.
    fn open_recent(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if !matches!(self.popups.last(), Some(Popup::Recent(_))) {
            let list = RecentList::new(self.repo.is_none());
            self.popups.push(Popup::Recent(list));
        }
        vec![Effect::LoadRecent]
    }

    /// The open recent list and its rows. The list can change under the
    /// cursor when it is read again, so the rows are worked out each time.
    fn recent_rows(&self) -> Option<(&RecentList, Vec<RecentRow<'_>>)> {
        match self.popups.last() {
            Some(Popup::Recent(list)) => {
                let rows = list.rows(&self.recent, self.repo.as_deref());
                Some((list, rows))
            }
            _ => None,
        }
    }

    fn move_recent_cursor(&mut self, delta: isize) {
        let Some((_, rows)) = self.recent_rows() else {
            return;
        };
        let len = rows.len();
        if let Some(Popup::Recent(list)) = self.popups.last_mut() {
            list.move_cursor(delta, len);
        }
    }

    fn recent_select(&mut self) -> Vec<Effect> {
        let Some((list, rows)) = self.recent_rows() else {
            return vec![];
        };
        match rows.get(list.cursor) {
            Some(RecentRow::Init) => self.init_repo(),
            Some(RecentRow::Repo(path)) => {
                let open = AppIntent::OpenRepo(path.to_path_buf());
                self.update(open)
            }
            None => vec![],
        }
    }

    /// Make the launch directory a repository. Only offered outside one,
    /// where git already runs in the launch directory.
    fn init_repo(&mut self) -> Vec<Effect> {
        if let Some(reason) = keymap::init_repo_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let seq = self.take_seq();
        self.busy += 1;
        vec![Effect::InitRepo {
            tag: Tag {
                generation: self.generation,
                seq,
                user: true,
            },
        }]
    }

    fn repo_initialised(&mut self, tag: Tag, result: Result<PathBuf, String>) -> Vec<Effect> {
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        if tag.generation != self.generation {
            return vec![];
        }
        match result {
            Ok(path) => self.open_repo(path),
            Err(message) => {
                self.push_error(ErrorPopup::new(
                    "Could not initialise a repository",
                    &message,
                    "Close this, fix the cause, then try again.",
                ));
                vec![]
            }
        }
    }

    /// Switch the whole workspace to the repository at `path`. The one
    /// left is parked, so coming back shows its files, selection and pane
    /// at once while a fresh status load catches up.
    fn open_repo(&mut self, path: PathBuf) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if self.op_seq.is_some() {
            self.notice = Some("Wait for the operation to finish");
            return vec![];
        }
        if self.repo.as_ref() == Some(&path) && self.repo_stack.is_empty() {
            self.popups
                .retain(|popup| !matches!(popup, Popup::Recent(_)));
            return vec![];
        }
        let parked = if self.repo.as_ref() == Some(&path) {
            // Inside one of its submodules: back to its top, as it was
            // when the first submodule was entered.
            let stack = std::mem::take(&mut self.repo_stack);
            stack.into_iter().next().map(|top| ParkedRepo {
                repo_stack: Vec::new(),
                files: top.files,
                files_loaded: top.files_loaded,
                branch: top.branch,
                selected: top.selected,
                diff_focused: false,
            })
        } else {
            if let Some(current) = self.repo.take() {
                let here = self.park();
                self.parked.insert(current, here);
            }
            self.parked.remove(&path)
        };
        self.switch_repo();
        self.repo = Some(path.clone());
        let parked = parked.unwrap_or(ParkedRepo {
            repo_stack: Vec::new(),
            files: Vec::new(),
            files_loaded: false,
            branch: None,
            selected: None,
            diff_focused: false,
        });
        self.repo_stack = parked.repo_stack;
        self.files = parked.files;
        self.files_loaded = parked.files_loaded;
        self.branch = parked.branch;
        let mut effects = vec![self.set_repo(&path), Effect::RecordRecent { path }];
        if let Some(index) = parked.selected.filter(|&i| i < self.files.len()) {
            if parked.diff_focused {
                let request = DiffRequest::for_entry(&self.files[index], self.diff_options);
                self.focus = Focus::Diff(HunkView::new(request.path, request.side));
            }
            effects.extend(self.select(index));
        }
        effects.extend(self.refresh_status());
        // The keys follow the repository: its own file replaces the last one's.
        effects.push(Effect::LoadConfig {
            repo: self.repo.clone(),
        });
        effects
    }

    /// What the current repository shows, taken out of the state to keep.
    fn park(&mut self) -> ParkedRepo {
        ParkedRepo {
            repo_stack: std::mem::take(&mut self.repo_stack),
            files: std::mem::take(&mut self.files),
            files_loaded: self.files_loaded,
            branch: self.branch.take(),
            selected: self.selected.take(),
            diff_focused: matches!(self.focus, Focus::Diff(_)),
        }
    }

    /// Forget what belonged to the repository being left: results still in
    /// flight for it are dropped by the new generation.
    fn switch_repo(&mut self) {
        self.generation += 1;
        self.popups.clear();
        self.pending_command = None;
        // The side panel stays; the diff or blame belonged to the old repository.
        if !self.focus.is_side() {
            self.focus = Focus::side(self.expanded);
        }
        self.files.clear();
        self.branch = None;
        self.selected = None;
        self.diff = DiffPane::default();
        self.rendered = None;
        self.status_error = None;
        self.op_error = None;
        self.status_selection = Selection::KeepPath;
        // The refs seen belong to the repository being left; the next
        // snapshot is the new one's baseline, not a change.
        self.watch.refs = None;
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
        } else {
            self.watch.status_pending = false;
        }
        if tag.generation != self.generation || Some(tag.seq) != self.status_seq {
            return vec![];
        }
        // Nobody asked for a background load, so it neither interrupts with
        // an error nor reloads a diff that has not changed.
        if !tag.user {
            match &result {
                Err(_) => return vec![],
                Ok(status) if self.status_unchanged(status) => return vec![],
                Ok(_) => {}
            }
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

/// Watching for changes made outside Kika. None of it counts as busy.
impl AppState {
    fn watch(&mut self, event: WatchEvent) -> Vec<Effect> {
        let mut effects = Vec::new();
        // The user may have edited a config file while away, in any context.
        if event == WatchEvent::FocusGained {
            effects.push(Effect::LoadConfig {
                repo: self.repo.clone(),
            });
        }
        // Outside a repository there is nothing to watch until one opens.
        if self.context != Context::Workspace || self.repo.is_none() {
            return effects;
        }
        match event {
            WatchEvent::PollRefs => effects.extend(self.snapshot_refs()),
            WatchEvent::PollFiles => effects.extend(self.background_status()),
            WatchEvent::FocusGained => {
                effects.extend(self.background_status());
                effects.extend(self.snapshot_refs());
            }
            WatchEvent::Fetch => {
                if self.watch.start_fetch() {
                    effects.push(Effect::Fetch {
                        tag: self.background_tag(),
                    });
                }
            }
        }
        effects
    }

    fn snapshot_refs(&mut self) -> Vec<Effect> {
        if !self.watch.start_refs() {
            return vec![];
        }
        vec![Effect::SnapshotRefs {
            tag: self.background_tag(),
        }]
    }

    /// Moved refs mean a commit, checkout or fetch happened elsewhere.
    fn refs_loaded(&mut self, tag: Tag, result: Result<RefsSnapshot, String>) -> Vec<Effect> {
        self.watch.refs_pending = false;
        // A failed snapshot is tried again on the next poll.
        let Ok(snapshot) = result else {
            return vec![];
        };
        if tag.generation != self.generation || !self.watch.refs_changed(snapshot) {
            return vec![];
        }
        self.background_status()
    }

    /// A fetch can move remote refs and so ahead/behind. A failed one,
    /// say offline, is tried again on the next interval.
    fn fetch_finished(&mut self, tag: Tag, result: Result<(), String>) -> Vec<Effect> {
        self.watch.fetch_pending = false;
        if tag.generation != self.generation || result.is_err() {
            return vec![];
        }
        self.background_status()
    }

    /// Reload `git status` quietly. Skipped while the user is waiting on
    /// git, whose own reload is coming, or while one is already running.
    /// It takes `status_seq`, so a user refresh started later wins.
    fn background_status(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace
            || self.busy > 0
            || self.op_seq.is_some()
            || self.watch.status_pending
        {
            return vec![];
        }
        let tag = self.background_tag();
        self.status_seq = Some(tag.seq);
        self.status_selection = Selection::KeepPath;
        self.watch.status_pending = true;
        vec![Effect::LoadStatus { tag }]
    }

    fn background_tag(&mut self) -> Tag {
        Tag {
            generation: self.generation,
            seq: self.take_seq(),
            user: false,
        }
    }

    /// Whether `status` shows what is already on screen. After an error
    /// the screen is out of date, so anything is news.
    fn status_unchanged(&self, status: &RepoStatus) -> bool {
        self.status_error.is_none()
            && self.files == status.files
            && self.branch.as_ref() == Some(&status.branch)
    }
}

/// The first line of a rejected edited hunk's error, above git's message.
pub(crate) const EDITED_HUNK_INVALID: &str = "The edited hunk is not a patch git can apply. \
Keep the @@ line as it was and start each line with a space, + or -.";

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
        GitOp::SubmoduleInit(_) => "Could not initialize the submodule",
        GitOp::SubmoduleUpdate(_) => "Could not update the submodule",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit_editor::{CommitEditor, Field};
    use crate::diff_pane::DIFF_PREFETCH;
    use crate::git::diff::{DiffLine, DiffSide, LineKind};
    use crate::git::status::{BranchStatus, EntryKind, Head, RepoStatus};
    use crate::git::submodule::SubmoduleState;
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

    /// The commit a submit asked to probe before running it.
    fn probe_of(effects: &[Effect]) -> (Tag, GitOp) {
        match effects {
            [Effect::ProbeCommit { tag, op }] => (*tag, op.clone()),
            other => panic!("expected one ProbeCommit, got {other:?}"),
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

        let (tag, op) = probe_of(&ctrl(&mut state, 's'));

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
        let (_, op) = probe_of(&ctrl(&mut state, 's'));
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
    fn ctrl_c_in_the_editor_asks_before_discarding_a_typed_message() {
        let mut state = editing();
        type_text(&mut state, "abc");
        assert_eq!(ctrl(&mut state, 'c'), vec![]);
        assert!(!state.should_quit);
        assert!(matches!(state.commit, CommitFlow::ConfirmDiscard(_)));

        // Nothing typed: nothing to lose, so it just closes.
        let mut state = editing();
        ctrl(&mut state, 'c');
        assert_eq!(state.commit, CommitFlow::Closed);
        assert!(!state.should_quit);
    }

    #[test]
    fn interrupt_commit_cancels_while_editing_and_quits_while_running() {
        let mut state = editing();
        type_text(&mut state, "abc");
        assert_eq!(state.update(AppIntent::InterruptCommit), vec![]);
        assert!(matches!(state.commit, CommitFlow::ConfirmDiscard(_)));
        assert!(!state.should_quit);

        let (mut state, _, _) = submitted();
        assert_eq!(
            state.update(AppIntent::InterruptCommit),
            vec![Effect::InterruptGit]
        );
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
        let (tag, op) = probe_of(&ctrl(&mut state, 's'));
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

    fn probed(tag: Tag, op: &GitOp, facts: Option<PromptFacts>) -> AppIntent {
        AppIntent::CommitProbed {
            tag,
            op: op.clone(),
            facts,
        }
    }

    fn signing() -> Option<PromptFacts> {
        Some(PromptFacts {
            signs: true,
            hooks: vec![],
        })
    }

    /// Commits as each way of submitting makes them: typed, amended with
    /// HEAD's message, with hooks skipped, and from the editor.
    fn submissions() -> Vec<(AppState, Tag, GitOp)> {
        let mut amend = amending();
        let (tag, op) = probe_of(&ctrl(&mut amend, 's'));
        let amended = (amend, tag, op);

        let mut skip = editing();
        ctrl(&mut skip, 'n');
        type_text(&mut skip, "subj");
        let (tag, op) = probe_of(&ctrl(&mut skip, 's'));
        let skipped = (skip, tag, op);

        let mut edited = editing();
        let effects = edited.update(AppIntent::CommitMessageEdited {
            result: Ok(Some("x\n".into())),
        });
        let (tag, op) = probe_of(&effects);
        let from_editor = (edited, tag, op);

        vec![submitted(), amended, skipped, from_editor]
    }

    #[test]
    fn a_submitted_commit_is_probed_once_and_counts_as_running() {
        for (state, tag, op) in submissions() {
            assert!(matches!(op, GitOp::Commit { .. }), "{op:?}");
            assert!(tag.user);
            assert_eq!(state.op_seq, Some(tag.seq));
            assert!(matches!(state.commit, CommitFlow::Running(_)));
        }
    }

    #[test]
    fn a_commit_that_signs_is_handed_the_terminal() {
        for (mut state, tag, op) in submissions() {
            let busy = state.busy;
            let effects = state.update(probed(tag, &op, signing()));
            assert_eq!(
                effects,
                vec![Effect::HandOff(HandOff::Commit {
                    tag,
                    op: op.clone()
                })]
            );
            assert_eq!(state.busy, busy, "OpFinished still settles busy");
            assert_eq!(state.op_seq, Some(tag.seq));
            assert!(matches!(state.commit, CommitFlow::Running(_)));
        }
    }

    #[test]
    fn only_a_handed_over_commit_is_marked_as_on_the_terminal_until_it_ends() {
        let (mut state, tag, op) = submitted();
        assert!(!state.commit_on_terminal);
        state.update(probed(tag, &op, signing()));
        assert!(state.commit_on_terminal);
        state.update(finished(tag, op, Err("no".into())));
        assert!(!state.commit_on_terminal);

        let (mut state, tag, op) = submitted();
        state.update(probed(tag, &op, Some(PromptFacts::default())));
        assert!(!state.commit_on_terminal);
    }

    #[test]
    fn a_commit_with_no_signing_and_no_hooks_runs_detached() {
        for (mut state, tag, op) in submissions() {
            let busy = state.busy;
            let effects = state.update(probed(tag, &op, Some(PromptFacts::default())));
            assert_eq!(effects, vec![Effect::RunOp { tag, op }]);
            assert_eq!(state.busy, busy);
            assert_eq!(state.op_seq, Some(tag.seq));
        }
    }

    #[test]
    fn a_failed_probe_hands_the_terminal_over() {
        let (mut state, tag, op) = submitted();
        assert_eq!(
            state.update(probed(tag, &op, None)),
            vec![Effect::HandOff(HandOff::Commit { tag, op })]
        );
    }

    #[test]
    fn a_handed_over_commit_ends_like_a_detached_one() {
        let (mut state, tag, op) = submitted();
        state.update(probed(tag, &op, signing()));
        let effects = state.update(finished(tag, op, Ok(())));
        assert_eq!(state.commit, CommitFlow::Closed);
        assert_eq!(state.op_seq, None);
        assert!(!effects.is_empty(), "the status is reloaded");
    }

    #[test]
    fn a_stale_probe_gives_the_message_back_and_frees_the_slot() {
        for (mut state, tag, op) in submissions() {
            state.generation += 1;
            let effects = state.update(probed(tag, &op, signing()));
            assert_eq!(effects, vec![]);
            assert_eq!(state.busy, 0);
            assert_eq!(state.op_seq, None);
            editor(&state);
        }
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
        let (_, op) = probe_of(&ctrl(&mut state, 's'));
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
        let (_, op) = probe_of(&ctrl(&mut state, 's'));
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
        let (_, op) = probe_of(&ctrl(&mut state, 's'));
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
        let (tag, op) = probe_of(&effects);
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
        assert_eq!(state.popups, [menu(MenuId::Actions, 7)]);
        press(&mut state, KeyCode::Up);
        assert_eq!(state.popups, [menu(MenuId::Actions, 6)]);
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
        for _ in 0..7 {
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
            other => panic!("expected diff focus, got {other:?}"),
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

    const GARBAGE: &str = "error: patch with only garbage at <stdin>:4";

    /// Stage an edited hunk with `keymap` in force, let git reject it, and
    /// return the one popup that leaves.
    fn rejected_edit(keymap: Keymap) -> ErrorPopup {
        let mut state = focused(three_files(), TWO_HUNKS);
        state.keymap = keymap;
        let effects = state.update(AppIntent::HunkEdited {
            result: Ok(Some(b"edited".to_vec())),
        });
        let (tag, op) = op_of(&effects);
        state.update(finished(tag, op, Err(GARBAGE.into())));
        match state.popups.as_slice() {
            [Popup::Error(error)] => error.clone(),
            other => panic!("expected one error popup, got {other:?}"),
        }
    }

    #[test]
    fn a_rejected_edit_explains_itself_above_git_message() {
        let error = rejected_edit(Keymap::default());
        assert_eq!(error.title, "Could not stage the edited hunk");
        let (first, rest) = error.message.split_once('\n').expect("two parts");
        assert_eq!(first, EDITED_HUNK_INVALID);
        assert_eq!(rest, GARBAGE);
    }

    #[test]
    fn a_rejected_edit_names_the_key_to_edit_again() {
        let error = rejected_edit(Keymap::default());
        assert_eq!(error.next, "Press e to edit the hunk again.");
        let error = rejected_edit(keymap::remap(&[("diff.edit_hunk", &["E"])]));
        assert_eq!(error.next, "Press E to edit the hunk again.");
        let error = rejected_edit(keymap::remap(&[("diff.edit_hunk", &[])]));
        assert_eq!(error.next, "Edit the hunk again to retry.");
    }

    #[test]
    fn other_failed_ops_keep_the_plain_message_and_hint() {
        let (mut state, _) = with_files(three_files(), 24);
        let tag = Tag {
            generation: state.generation,
            seq: 1,
            user: true,
        };
        let op = GitOp::ApplyCached {
            patch: vec![],
            reverse: false,
        };
        state.update(finished(tag, op, Err(GARBAGE.into())));
        match state.popups.as_slice() {
            [Popup::Error(error)] => {
                assert_eq!(error.title, "Could not stage the change");
                assert_eq!(error.message, GARBAGE);
                assert_eq!(
                    error.next,
                    "The file list was reloaded. Fix the cause above, then try again."
                );
            }
            other => panic!("expected one error popup, got {other:?}"),
        }
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
        state.update(AppIntent::FocusLeft);
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

    fn blame_line(oid: &str, line_no: usize) -> crate::git::blame::BlameLine {
        crate::git::blame::BlameLine {
            oid: oid.into(),
            author: "Ann".into(),
            date: "2024-01-02".into(),
            line_no,
            text: format!("line {line_no}"),
        }
    }

    const OID: &str = "abcdef1234567890abcdef1234567890abcdef12";

    /// Press `b` on a tracked, modified `a.rs` in a `height`-row terminal.
    fn blaming(height: u16) -> (AppState, Tag) {
        let (mut state, _) = with_files(vec![changed("a.rs", '.', 'M')], height);
        let effects = press(&mut state, KeyCode::Char('b'));
        match effects.as_slice() {
            [Effect::LoadBlame { tag, path }] => {
                assert_eq!(path, &RepoPath::from("a.rs"));
                assert!(tag.user);
                let tag = *tag;
                (state, tag)
            }
            other => panic!("expected one LoadBlame, got {other:?}"),
        }
    }

    #[test]
    fn help_scrolls_within_its_list_and_opens_at_the_top() {
        let (mut state, _) = with_files(three_files(), 24);
        press(&mut state, KeyCode::Char('?'));
        let total = keymap::available(
            keymap::registry(),
            &state.keymap,
            state.panel_focus(),
            &state,
        )
        .len();
        press(&mut state, KeyCode::Char('j'));
        assert_eq!(state.help_scroll, 1);
        press(&mut state, KeyCode::Char('k'));
        press(&mut state, KeyCode::Char('k'));
        assert_eq!(state.help_scroll, 0);
        for _ in 0..10 {
            press(&mut state, KeyCode::PageDown);
        }
        assert_eq!(state.help_scroll, total - state.pane_rows());
        press(&mut state, KeyCode::Esc);
        press(&mut state, KeyCode::Char('?'));
        assert_eq!(state.help_scroll, 0);
        assert!(!state.should_quit);
    }

    #[test]
    fn digits_focus_and_expand_the_side_panels() {
        let (mut state, _) = with_files(three_files(), 24);
        for (key, focus, side) in [
            ('1', Focus::Status, SidePanel::Status),
            ('3', Focus::Branches, SidePanel::Branches),
            ('4', Focus::Commits, SidePanel::Commits),
            ('5', Focus::Stash, SidePanel::Stash),
            ('2', Focus::Files, SidePanel::Files),
        ] {
            assert_eq!(press(&mut state, KeyCode::Char(key)), vec![], "{key}");
            assert_eq!(state.focus, focus, "{key}");
            assert_eq!(state.expanded, side, "{key}");
        }
        // The new side panels have no keys of their own yet.
        state.update(AppIntent::FocusPanel(crate::layout::Panel::Side(
            SidePanel::Commits,
        )));
        assert_eq!(state.key_focus(), KeyScope::Workspace);
    }

    #[test]
    fn zero_l_and_right_focus_the_diff_and_h_left_and_esc_go_back_to_the_last_side_panel() {
        for right in [KeyCode::Char('0'), KeyCode::Char('l'), KeyCode::Right] {
            for left in [KeyCode::Char('h'), KeyCode::Left, KeyCode::Esc] {
                let (mut state, _) = with_files(three_files(), 24);
                press(&mut state, KeyCode::Char('1'));
                patch_load(&press(&mut state, right));
                assert!(matches!(state.focus, Focus::Diff(_)), "{right:?}");
                // The side panel stays expanded behind the diff.
                assert_eq!(state.expanded, SidePanel::Status);
                assert_eq!(press(&mut state, left), vec![], "{left:?}");
                assert_eq!(state.focus, Focus::Status, "{right:?} {left:?}");
                assert!(!state.should_quit);
            }
        }
    }

    #[test]
    fn the_diff_cannot_take_focus_without_a_selected_file() {
        let (mut state, _) = with_files(vec![], 24);
        assert_eq!(press(&mut state, KeyCode::Char('l')), vec![]);
        assert_eq!(state.focus, Focus::Files);
        assert_eq!(state.notice, Some("No file selected"));
    }

    #[test]
    fn tab_cycles_the_panels_in_screen_order_and_shift_tab_goes_back() {
        use crate::layout::Panel::{Diff, Side};
        use crate::layout::SidePanel::*;
        let (mut state, _) = with_files(three_files(), 24);
        press(&mut state, KeyCode::Char('1'));
        let mut seen = vec![];
        for _ in 0..7 {
            press(&mut state, KeyCode::Tab);
            seen.push(state.focus.panel());
        }
        assert_eq!(
            seen,
            [
                Side(Files),
                Side(Branches),
                Side(Commits),
                Side(Stash),
                Diff,
                Side(Status),
                Side(Files)
            ]
        );
        let mut seen = vec![];
        for _ in 0..3 {
            state.update(AppIntent::Key(KeyPress::new(
                KeyCode::BackTab,
                KeyModifiers::SHIFT,
            )));
            seen.push(state.focus.panel());
        }
        assert_eq!(seen, [Side(Status), Diff, Side(Stash)]);
    }

    #[test]
    fn tab_skips_the_diff_with_no_file_selected() {
        let (mut state, _) = with_files(vec![], 24);
        press(&mut state, KeyCode::Char('5'));
        press(&mut state, KeyCode::Tab);
        assert_eq!(state.focus, Focus::Status);
        state.update(AppIntent::CycleFocus { back: true });
        assert_eq!(state.focus, Focus::Stash);
    }

    #[test]
    fn focusing_the_diff_again_keeps_the_hunk_view() {
        for key in [KeyCode::Char('0'), KeyCode::Char('l'), KeyCode::Right] {
            let mut state = focused(three_files(), TWO_HUNKS);
            press(&mut state, KeyCode::Char('j'));
            press(&mut state, KeyCode::Char('t'));
            let before = view(&state).clone();
            assert_eq!(press(&mut state, key), vec![], "{key:?}");
            assert_eq!(view(&state), &before, "{key:?}");

            // In line mode too.
            let mut state = focused(three_files(), TWO_HUNKS);
            press(&mut state, KeyCode::Char('v'));
            press(&mut state, KeyCode::Char('j'));
            let before = view(&state).clone();
            assert!(before.lines.is_some());
            assert_eq!(press(&mut state, key), vec![], "{key:?}");
            assert_eq!(view(&state), &before, "{key:?}");
        }
    }

    #[test]
    fn a_digit_closes_an_open_blame_and_focuses_that_panel() {
        let mut state = blamed(5, 24);
        press(&mut state, KeyCode::Char('3'));
        assert_eq!(state.focus, Focus::Branches);
        assert_eq!(state.expanded, SidePanel::Branches);

        // Back from a blame goes where it was opened from.
        let mut state = blamed(5, 24);
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.focus, Focus::Files);
    }

    #[test]
    fn z_cycles_the_screen_mode_and_it_survives_focus_changes() {
        let (mut state, _) = with_files(three_files(), 24);
        assert_eq!(state.screen_mode, ScreenMode::Normal);
        press(&mut state, KeyCode::Char('z'));
        assert_eq!(state.screen_mode, ScreenMode::Half);
        press(&mut state, KeyCode::Char('1'));
        patch_load(&press(&mut state, KeyCode::Char('l')));
        assert_eq!(state.screen_mode, ScreenMode::Half);
        press(&mut state, KeyCode::Char('z'));
        assert_eq!(state.screen_mode, ScreenMode::Full);
        press(&mut state, KeyCode::Char('h'));
        assert_eq!(state.screen_mode, ScreenMode::Full);
        press(&mut state, KeyCode::Char('z'));
        assert_eq!(state.screen_mode, ScreenMode::Normal);
    }

    #[test]
    fn a_clean_tree_leaves_a_side_panel_focused() {
        let (mut state, _) = with_files(three_files(), 24);
        press(&mut state, KeyCode::Char('4'));
        let tag = refresh_tag(&mut state);
        state.update(loaded(tag, &[]));
        assert_eq!(state.focus, Focus::Commits);
    }

    /// A blame of `n` lines, all by commit `OID` but the uncommitted last.
    fn blamed(n: usize, height: u16) -> AppState {
        let (mut state, tag) = blaming(height);
        let zero = "0".repeat(40);
        let lines = (1..=n)
            .map(|i| blame_line(if i == n { &zero } else { OID }, i))
            .collect();
        state.update(AppIntent::BlameLoaded {
            tag,
            result: Ok(lines),
        });
        state
    }

    fn blame(state: &AppState) -> &crate::blame_view::BlameView {
        match &state.focus {
            Focus::Blame(view) => view,
            other => panic!("expected the blame focused, got {other:?}"),
        }
    }

    #[test]
    fn b_on_a_tracked_modified_file_opens_its_blame_and_loads_it_once() {
        let (state, _) = blaming(24);
        let view = blame(&state);
        assert_eq!(view.path, RepoPath::from("a.rs"));
        assert!(!view.loaded);
        assert_eq!(state.panel_focus(), KeyScope::Blame);
        assert_eq!(state.busy, 1);
    }

    #[test]
    fn b_on_an_untracked_or_deleted_file_says_why_and_does_nothing() {
        for entry in [
            untracked("new.txt"),
            changed("gone.rs", '.', 'D'),
            changed("staged-gone.rs", 'D', '.'),
        ] {
            let (mut state, _) = with_files(vec![entry.clone()], 24);
            let effects = press(&mut state, KeyCode::Char('b'));
            assert_eq!(effects, vec![], "{entry:?}");
            assert_eq!(state.focus, Focus::Files, "{entry:?}");
            assert!(state.notice.is_some(), "{entry:?}");
        }
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(press(&mut state, KeyCode::Char('b')), vec![]);
        assert_eq!(state.notice, Some("No file selected"));
    }

    #[test]
    fn a_stale_blame_result_is_ignored_but_still_lowers_busy() {
        let (mut state, tag) = blaming(24);
        let stale = Tag {
            seq: tag.seq + 100,
            ..tag
        };
        state.update(AppIntent::BlameLoaded {
            tag: stale,
            result: Ok(vec![blame_line(OID, 1)]),
        });
        assert!(!blame(&state).loaded);
        assert_eq!(state.busy, 0);

        let old_generation = Tag {
            generation: tag.generation + 1,
            ..tag
        };
        state.update(AppIntent::BlameLoaded {
            tag: old_generation,
            result: Ok(vec![blame_line(OID, 1)]),
        });
        assert!(!blame(&state).loaded);

        state.update(AppIntent::BlameLoaded {
            tag,
            result: Ok(vec![blame_line(OID, 1)]),
        });
        assert!(blame(&state).loaded);
        assert_eq!(blame(&state).lines.len(), 1);
    }

    #[test]
    fn a_blame_result_after_leaving_the_blame_is_dropped() {
        let (mut state, tag) = blaming(24);
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.focus, Focus::Files);
        state.update(AppIntent::BlameLoaded {
            tag,
            result: Ok(vec![blame_line(OID, 1)]),
        });
        assert_eq!(state.focus, Focus::Files);
        assert_eq!(state.busy, 0);
    }

    #[test]
    fn the_blame_cursor_stays_inside_the_lines_and_the_visible_rows() {
        // Height 10: 7 rows in the pane.
        let mut state = blamed(20, 10);
        let rows = state.pane_rows();
        assert_eq!(rows, 7);
        let in_view = |state: &AppState| {
            let view = blame(state);
            assert!(view.cursor < view.lines.len());
            assert!(view.scroll <= view.cursor && view.cursor < view.scroll + rows);
            view.cursor
        };
        press(&mut state, KeyCode::Char('k'));
        assert_eq!(in_view(&state), 0);
        for _ in 0..8 {
            press(&mut state, KeyCode::Char('j'));
        }
        assert_eq!(in_view(&state), 8);
        assert_eq!(blame(&state).scroll, 2);
        press(&mut state, KeyCode::Down);
        press(&mut state, KeyCode::Up);
        assert_eq!(in_view(&state), 8);
        press(&mut state, KeyCode::PageDown);
        assert_eq!(in_view(&state), 15);
        press(&mut state, KeyCode::PageDown);
        assert_eq!(in_view(&state), 19);
        press(&mut state, KeyCode::Char('j'));
        assert_eq!(in_view(&state), 19);
        press(&mut state, KeyCode::PageUp);
        assert_eq!(in_view(&state), 12);
        for _ in 0..3 {
            press(&mut state, KeyCode::PageUp);
        }
        assert_eq!(in_view(&state), 0);
        assert_eq!(blame(&state).scroll, 0);
    }

    #[test]
    fn moving_in_an_empty_or_unloaded_blame_does_nothing() {
        let (mut state, _) = blaming(24);
        for code in [KeyCode::Char('j'), KeyCode::PageDown, KeyCode::Enter] {
            assert_eq!(press(&mut state, code), vec![]);
        }
        assert_eq!(blame(&state).cursor, 0);
        assert_eq!(blame(&state).commit, None);
    }

    #[test]
    fn enter_opens_the_lines_commit_and_esc_goes_back_to_the_same_line_then_files() {
        let mut state = blamed(5, 24);
        press(&mut state, KeyCode::Char('j'));
        press(&mut state, KeyCode::Char('j'));
        let tag = match press(&mut state, KeyCode::Enter).as_slice() {
            [Effect::LoadCommit { tag, oid }] => {
                assert_eq!(oid, OID);
                assert!(tag.user);
                *tag
            }
            other => panic!("expected one LoadCommit, got {other:?}"),
        };
        assert_eq!(state.panel_focus(), KeyScope::BlameCommit);
        assert_eq!(state.busy, 1);
        // The blame's keys are not the commit's: Enter does nothing more.
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);

        let lines = vec![line("commit abc"), line("+added")];
        state.update(AppIntent::CommitLoaded {
            tag,
            result: Ok(lines.clone()),
        });
        assert_eq!(state.busy, 0);
        let commit = blame(&state).commit.as_ref().expect("commit open");
        assert!(commit.loaded);
        assert_eq!(commit.lines, lines);

        press(&mut state, KeyCode::Esc);
        assert_eq!(state.panel_focus(), KeyScope::Blame);
        assert_eq!(blame(&state).cursor, 2);
        assert_eq!(blame(&state).commit, None);

        press(&mut state, KeyCode::Esc);
        assert_eq!(state.focus, Focus::Files);
        assert!(!state.should_quit);
    }

    #[test]
    fn enter_on_an_uncommitted_line_says_so_and_loads_nothing() {
        let mut state = blamed(3, 24);
        press(&mut state, KeyCode::Char('j'));
        press(&mut state, KeyCode::Char('j'));
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice, Some("Not committed yet"));
        assert_eq!(blame(&state).commit, None);
        assert_eq!(state.panel_focus(), KeyScope::Blame);
    }

    #[test]
    fn a_stale_commit_result_is_ignored() {
        let mut state = blamed(3, 24);
        let tag = match press(&mut state, KeyCode::Enter).as_slice() {
            [Effect::LoadCommit { tag, .. }] => *tag,
            other => panic!("expected one LoadCommit, got {other:?}"),
        };
        state.update(AppIntent::CommitLoaded {
            tag: Tag {
                seq: tag.seq + 100,
                ..tag
            },
            result: Ok(vec![line("x")]),
        });
        assert!(!blame(&state).commit.as_ref().unwrap().loaded);
        assert_eq!(state.busy, 0);
        // Closed before it landed: the result has nowhere to go.
        press(&mut state, KeyCode::Esc);
        state.update(AppIntent::CommitLoaded {
            tag,
            result: Ok(vec![line("x")]),
        });
        assert_eq!(blame(&state).commit, None);
    }

    #[test]
    fn the_commit_view_scrolls_inside_its_lines() {
        let mut state = blamed(3, 10);
        let tag = match press(&mut state, KeyCode::Enter).as_slice() {
            [Effect::LoadCommit { tag, .. }] => *tag,
            other => panic!("expected one LoadCommit, got {other:?}"),
        };
        state.update(AppIntent::CommitLoaded {
            tag,
            result: Ok((0..20).map(|i| line(&i.to_string())).collect()),
        });
        let scroll = |state: &AppState| blame(state).commit.as_ref().unwrap().scroll;
        press(&mut state, KeyCode::Char('j'));
        press(&mut state, KeyCode::Down);
        assert_eq!(scroll(&state), 2);
        press(&mut state, KeyCode::Char('k'));
        assert_eq!(scroll(&state), 1);
        press(&mut state, KeyCode::PageDown);
        press(&mut state, KeyCode::PageDown);
        // 20 lines in 7 rows: the last page starts at 13.
        assert_eq!(scroll(&state), 13);
        press(&mut state, KeyCode::PageUp);
        press(&mut state, KeyCode::Up);
        assert_eq!(scroll(&state), 5);
        // The blame cursor did not move.
        assert_eq!(blame(&state).cursor, 0);
    }

    #[test]
    fn workspace_diff_keys_are_refused_while_the_blame_is_open() {
        let mut state = blamed(3, 24);
        let options = state.diff_options;
        for c in ['J', 'K', '-', '+', 'w'] {
            assert_eq!(press(&mut state, KeyCode::Char(c)), vec![], "{c}");
            assert!(state.notice.is_some(), "{c}");
        }
        assert_eq!(state.diff_options, options);
        assert_eq!(blame(&state).cursor, 0);
        // Still refused with the commit open.
        press(&mut state, KeyCode::Enter);
        assert_eq!(press(&mut state, KeyCode::Char('w')), vec![]);
        assert_eq!(state.diff_options, options);
    }

    fn submodule(path: &str, state: SubmoduleState) -> Submodule {
        Submodule {
            path: path.into(),
            oid: "1234567890abcdef".into(),
            state,
        }
    }

    fn submodules_tag(effects: &[Effect]) -> Tag {
        match effects {
            [Effect::LoadSubmodules { tag }] => *tag,
            other => panic!("expected one LoadSubmodules, got {other:?}"),
        }
    }

    fn submodule_list(state: &AppState) -> &SubmoduleList {
        match state.popups.last() {
            Some(Popup::Submodules(list)) => list,
            other => panic!("expected the Submodules popup, got {other:?}"),
        }
    }

    /// Three files with `b.rs` selected, and the Submodules popup open
    /// listing `subs`.
    fn with_submodules(subs: Vec<Submodule>) -> AppState {
        let (state, _) = with_files(three_files(), 24);
        let mut state = state.with_location(Some(TOP.into()), TOP.into());
        state.update(AppIntent::SelectNext);
        let tag = submodules_tag(&press_char(&mut state, 'S'));
        state.update(AppIntent::SubmodulesLoaded {
            tag,
            result: Ok(subs),
        });
        state
    }

    #[test]
    fn s_opens_the_submodules_popup_and_loads_it_once() {
        let (mut state, _) = with_files(three_files(), 24);
        let busy = state.busy;
        let tag = submodules_tag(&press_char(&mut state, 'S'));
        assert!(tag.user);
        assert_eq!(tag.generation, state.generation);
        assert_eq!(state.busy, busy + 1);
        assert_eq!(state.key_focus(), KeyScope::Submodules);
        assert_eq!(*submodule_list(&state), SubmoduleList::loading(tag.seq));

        let subs = vec![submodule("sub", SubmoduleState::UpToDate)];
        state.update(AppIntent::SubmodulesLoaded {
            tag,
            result: Ok(subs.clone()),
        });
        assert_eq!(submodule_list(&state).items, Some(subs));
        assert_eq!(state.busy, busy);
    }

    #[test]
    fn the_actions_menu_opens_the_submodules_popup() {
        let (mut state, _) = with_files(three_files(), 24);
        state.update(AppIntent::OpenMenu(MenuId::Actions));
        for _ in 0..5 {
            state.update(AppIntent::MenuNext);
        }
        submodules_tag(&state.update(AppIntent::MenuSelect));
        assert!(matches!(state.popups[..], [Popup::Submodules(_)]));
    }

    #[test]
    fn a_stale_or_unwanted_submodule_load_is_dropped_but_still_lowers_busy() {
        let (mut state, _) = with_files(three_files(), 24);
        let old = submodules_tag(&press_char(&mut state, 'S'));
        assert_eq!(state.busy, 1);
        let subs = vec![submodule("sub", SubmoduleState::UpToDate)];
        let stale_seq = Tag {
            seq: old.seq + 100,
            ..old
        };
        state.update(AppIntent::SubmodulesLoaded {
            tag: stale_seq,
            result: Ok(subs.clone()),
        });
        let stale_generation = Tag {
            generation: old.generation + 1,
            ..old
        };
        state.update(AppIntent::SubmodulesLoaded {
            tag: stale_generation,
            result: Ok(subs.clone()),
        });
        assert_eq!(submodule_list(&state).items, None);
        assert_eq!(state.busy, 0);

        // Closed before the answer: nothing comes back.
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.popups, []);
        state.update(AppIntent::SubmodulesLoaded {
            tag: old,
            result: Ok(subs),
        });
        assert_eq!(state.popups, []);
        assert!(!state.should_quit);
    }

    #[test]
    fn j_and_k_move_the_submodule_cursor_within_bounds() {
        let mut state = with_submodules(vec![
            submodule("a", SubmoduleState::UpToDate),
            submodule("b", SubmoduleState::OutOfDate),
        ]);
        press_char(&mut state, 'k');
        assert_eq!(submodule_list(&state).cursor, 0);
        for _ in 0..3 {
            press_char(&mut state, 'j');
        }
        assert_eq!(submodule_list(&state).cursor, 1);
        press(&mut state, KeyCode::Up);
        assert_eq!(submodule_list(&state).cursor, 0);
        press(&mut state, KeyCode::Down);
        assert_eq!(submodule_list(&state).cursor, 1);
        // The file selection did not move.
        assert_eq!(state.selected, Some(1));
    }

    #[test]
    fn i_and_u_run_one_op_on_the_selected_submodule() {
        let mut state = with_submodules(vec![
            submodule("a", SubmoduleState::Uninitialized),
            submodule("libs/b", SubmoduleState::Uninitialized),
        ]);
        press_char(&mut state, 'j');
        let (tag, op) = op_of(&press_char(&mut state, 'i'));
        assert_eq!(op, GitOp::SubmoduleInit("libs/b".into()));

        // One at a time.
        assert_eq!(press_char(&mut state, 'u'), vec![]);
        assert_eq!(state.notice, Some("An operation is running"));
        state.update(AppIntent::OpFinished {
            tag,
            op,
            result: Ok(()),
        });

        let (_, op) = op_of(&press_char(&mut state, 'u'));
        assert_eq!(op, GitOp::SubmoduleUpdate("libs/b".into()));
    }

    #[test]
    fn a_finished_submodule_op_reloads_the_files_and_the_open_list() {
        let mut state = with_submodules(vec![submodule("a", SubmoduleState::Uninitialized)]);
        let (tag, op) = op_of(&press_char(&mut state, 'u'));
        let effects = state.update(AppIntent::OpFinished {
            tag,
            op: op.clone(),
            result: Ok(()),
        });
        let seq = match effects.as_slice() {
            [Effect::LoadStatus { .. }, Effect::LoadSubmodules { tag }] => tag.seq,
            other => panic!("expected a status and a submodule load, got {other:?}"),
        };
        let list = submodule_list(&state);
        assert_eq!(list.seq, seq);
        // What it showed stays until the reload lands.
        assert!(list.items.is_some());

        // Closed: only the files reload.
        let (tag, op) = op_of(&state.update(AppIntent::UpdateSubmodule));
        state.popups.clear();
        let effects = state.update(AppIntent::OpFinished {
            tag,
            op,
            result: Ok(()),
        });
        assert!(
            matches!(effects.as_slice(), [Effect::LoadStatus { .. }]),
            "{effects:?}"
        );
    }

    /// The top level the submodule tests run in.
    const TOP: &str = "/r/top";

    /// The submodule path of a `SetRepo` from the top level [`TOP`].
    fn set_repo(effect: &Effect) -> &[RepoPath] {
        match effect {
            Effect::SetRepo { root, nested } => {
                assert_eq!(root, Path::new(TOP));
                nested
            }
            other => panic!("expected SetRepo, got {other:?}"),
        }
    }

    #[test]
    fn enter_on_a_checked_out_submodule_switches_to_it() {
        let mut state = with_submodules(vec![
            submodule("a", SubmoduleState::Uninitialized),
            submodule("sub", SubmoduleState::UpToDate),
        ]);
        let generation = state.generation;
        let files = state.files.clone();
        let branch = state.branch.clone();
        press_char(&mut state, 'j');

        let effects = press(&mut state, KeyCode::Enter);

        assert_eq!(
            state.repo_stack,
            [ParentRepo {
                path: "sub".into(),
                files,
                files_loaded: true,
                branch,
                selected: Some(1),
            }]
        );
        assert_eq!(state.generation, generation + 1);
        assert_eq!(state.files, []);
        assert!(!state.files_loaded);
        assert_eq!(state.branch, None);
        assert_eq!(state.selected, None);
        assert_eq!(state.popups, []);
        assert_eq!(state.focus, Focus::Files);
        assert_eq!(state.diff, DiffPane::default());
        match effects.as_slice() {
            [set, Effect::LoadStatus { tag }] => {
                assert_eq!(set_repo(set), [RepoPath::from("sub")]);
                assert_eq!(tag.generation, state.generation);
            }
            other => panic!("expected SetRepo then LoadStatus, got {other:?}"),
        }
    }

    #[test]
    fn enter_on_an_uninitialized_submodule_only_says_why() {
        let mut state = with_submodules(vec![submodule("a", SubmoduleState::Uninitialized)]);
        let before = state.clone();
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice, Some("Update the submodule first"));
        assert_eq!(
            AppState {
                notice: None,
                ..state
            },
            before
        );
    }

    #[test]
    fn enter_on_a_conflicted_submodule_says_why_and_stays_in_the_parent() {
        let mut state = with_submodules(vec![submodule("a", SubmoduleState::Conflicted)]);
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice, Some("Resolve the submodule conflict first"));
        assert_eq!(state.repo_stack, []);
        assert!(matches!(state.popups[..], [Popup::Submodules(_)]));
    }

    #[test]
    fn enter_waits_for_a_running_op() {
        let mut state = with_submodules(vec![submodule("a", SubmoduleState::UpToDate)]);
        state.op_seq = Some(99);
        assert_eq!(state.update(AppIntent::EnterSubmodule), vec![]);
        assert_eq!(state.notice, Some("Wait for the operation to finish"));
        assert_eq!(state.repo_stack, []);
    }

    #[test]
    fn backspace_restores_the_parent_exactly_and_reloads_it() {
        let mut state = with_submodules(vec![submodule("sub", SubmoduleState::OutOfDate)]);
        let files = state.files.clone();
        let branch = state.branch.clone();
        press(&mut state, KeyCode::Enter);
        let tag = refresh_tag(&mut state);
        state.update(loaded(tag, &["inner.txt"]));
        let generation = state.generation;

        let effects = press(&mut state, KeyCode::Backspace);

        assert_eq!(state.repo_stack, []);
        assert_eq!(state.generation, generation + 1);
        assert_eq!(state.files, files);
        assert_eq!(state.selected, Some(1));
        assert_eq!(state.branch, branch);
        assert!(state.files_loaded);
        match effects.as_slice() {
            [
                set,
                Effect::LoadDiff { request, tag, .. },
                Effect::DiffLoadingDelay { .. },
                Effect::LoadStatus { tag: status },
            ] => {
                assert_eq!(set_repo(set), []);
                assert_eq!(request.path, "b.rs".into());
                assert_eq!(tag.generation, state.generation);
                assert_eq!(status.generation, state.generation);
            }
            other => panic!("expected SetRepo, the diff, then status, got {other:?}"),
        }
        assert_eq!(state.status_selection, Selection::KeepPath);
    }

    #[test]
    fn nested_submodules_stack_their_paths() {
        let mut state = with_submodules(vec![submodule("outer", SubmoduleState::UpToDate)]);
        press(&mut state, KeyCode::Enter);
        let tag = submodules_tag(&state.update(AppIntent::OpenSubmodules));
        state.update(AppIntent::SubmodulesLoaded {
            tag,
            result: Ok(vec![submodule("inner", SubmoduleState::UpToDate)]),
        });
        let effects = press(&mut state, KeyCode::Enter);
        assert_eq!(
            set_repo(&effects[0]),
            [RepoPath::from("outer"), RepoPath::from("inner")]
        );
        let effects = press(&mut state, KeyCode::Backspace);
        assert_eq!(set_repo(&effects[0]), [RepoPath::from("outer")]);
    }

    #[test]
    fn backspace_outside_a_submodule_is_disabled() {
        let (mut state, _) = with_files(three_files(), 24);
        let before = state.clone();
        assert_eq!(press(&mut state, KeyCode::Backspace), vec![]);
        assert_eq!(state.notice, Some("Not in a submodule"));
        assert_eq!(
            AppState {
                notice: None,
                ..state
            },
            before
        );
    }

    #[test]
    fn results_for_the_parent_are_dropped_inside_the_submodule() {
        let mut state = with_submodules(vec![submodule("sub", SubmoduleState::UpToDate)]);
        let parent = refresh_tag(&mut state);
        press(&mut state, KeyCode::Enter);
        state.update(loaded(parent, &["parent.txt"]));
        assert_eq!(state.files, []);
        assert!(!state.files_loaded);
    }

    // Startup and the recent repositories.

    fn outside(launch: &str) -> AppState {
        let mut state = AppState::new(Context::Workspace).with_location(None, launch.into());
        state.update(AppIntent::Resize {
            width: 80,
            height: 24,
        });
        state
    }

    fn recent_list(state: &AppState) -> &RecentList {
        match state.popups.last() {
            Some(Popup::Recent(list)) => list,
            other => panic!("expected the Recent popup, got {other:?}"),
        }
    }

    #[test]
    fn start_in_a_repository_records_it_and_loads_its_status() {
        let mut state =
            AppState::new(Context::Workspace).with_location(Some("/r/a".into()), "/r/a/d".into());
        let effects = state.update(AppIntent::Start);
        match effects.as_slice() {
            [Effect::RecordRecent { path }, Effect::LoadStatus { .. }] => {
                assert_eq!(path, Path::new("/r/a"));
            }
            other => panic!("expected RecordRecent and LoadStatus, got {other:?}"),
        }
        assert_eq!(state.popups, []);
    }

    #[test]
    fn start_outside_a_repository_welcomes_with_the_init_row_and_no_status() {
        let mut state = outside("/w");
        let effects = state.update(AppIntent::Start);
        assert_eq!(effects, [Effect::LoadRecent]);
        assert_eq!(*recent_list(&state), RecentList::new(true));
        assert_eq!(state.key_focus(), KeyScope::Recent);
        assert_eq!(state.busy, 0);
    }

    #[test]
    fn closing_the_welcome_loads_the_status_to_show_gits_error() {
        let mut state = outside("/w");
        state.update(AppIntent::Start);
        let effects = press(&mut state, KeyCode::Esc);
        assert!(
            matches!(effects.as_slice(), [Effect::LoadStatus { .. }]),
            "{effects:?}"
        );
        assert_eq!(state.popups, []);
        assert!(!state.should_quit);
    }

    #[test]
    fn shift_o_opens_the_list_from_files_and_diff_without_the_init_row() {
        let (state, _) = with_files(three_files(), 24);
        let mut state = state.with_location(Some("/r/a".into()), "/r/a".into());
        let o = AppIntent::Key(KeyPress::new(KeyCode::Char('O'), KeyModifiers::SHIFT));
        assert_eq!(state.update(o.clone()), [Effect::LoadRecent]);
        assert_eq!(*recent_list(&state), RecentList::new(false));
        press(&mut state, KeyCode::Esc);
        // Closing it in a repository asks for nothing.
        assert_eq!(state.popups, []);

        press(&mut state, KeyCode::Enter);
        assert!(matches!(state.focus, Focus::Diff(_)));
        assert_eq!(state.update(o), [Effect::LoadRecent]);
        assert_eq!(state.key_focus(), KeyScope::Recent);
    }

    #[test]
    fn the_actions_menu_opens_the_recent_list() {
        let (mut state, _) = with_files(three_files(), 24);
        state.update(AppIntent::OpenMenu(MenuId::Actions));
        for _ in 0..6 {
            state.update(AppIntent::MenuNext);
        }
        assert_eq!(state.update(AppIntent::MenuSelect), [Effect::LoadRecent]);
        assert!(matches!(state.popups[..], [Popup::Recent(_)]));
    }

    #[test]
    fn the_cursor_moves_over_the_rows_and_enter_opens_the_repository_under_it() {
        let mut state = outside("/w");
        state.update(AppIntent::Start);
        state.update(AppIntent::RecentLoaded(vec!["/r/a".into(), "/r/b".into()]));
        press_char(&mut state, 'k');
        for _ in 0..5 {
            press_char(&mut state, 'j');
        }
        assert_eq!(recent_list(&state).cursor, 2);
        press(&mut state, KeyCode::Up);

        let effects = press(&mut state, KeyCode::Enter);

        assert_eq!(state.repo, Some("/r/a".into()));
        assert_eq!(state.popups, []);
        assert_eq!(
            effects[0],
            Effect::SetRepo {
                root: "/r/a".into(),
                nested: vec![]
            }
        );
    }

    #[test]
    fn a_shorter_list_pulls_the_cursor_in() {
        let mut state = outside("/w");
        state.update(AppIntent::Start);
        state.update(AppIntent::RecentLoaded(vec!["/r/a".into(), "/r/b".into()]));
        press_char(&mut state, 'j');
        press_char(&mut state, 'j');
        state.update(AppIntent::RecentLoaded(vec![]));
        assert_eq!(recent_list(&state).cursor, 0);
    }

    fn init_tag(effects: &[Effect]) -> Tag {
        match effects {
            [Effect::InitRepo { tag }] => *tag,
            other => panic!("expected one InitRepo, got {other:?}"),
        }
    }

    #[test]
    fn i_or_enter_on_the_init_row_runs_git_init_and_opens_the_result() {
        let mut state = outside("/w");
        state.update(AppIntent::Start);
        let tag = init_tag(&press_char(&mut state, 'i'));
        assert!(tag.user);
        assert_eq!(state.busy, 1);
        // Enter on the init row asks the same, once git is done.
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice, Some("Loading…"));

        let effects = state.update(AppIntent::RepoInitialised {
            tag,
            result: Ok("/w".into()),
        });

        assert_eq!(state.busy, 1, "only the status load is left");
        assert_eq!(state.repo, Some("/w".into()));
        assert_eq!(state.popups, []);
        match effects.as_slice() {
            [
                Effect::SetRepo { root, nested },
                Effect::RecordRecent { path },
                Effect::LoadStatus { .. },
                Effect::LoadConfig { repo },
            ] => {
                assert_eq!((root.as_path(), nested.len()), (Path::new("/w"), 0));
                assert_eq!(path, Path::new("/w"));
                assert_eq!(repo.as_deref(), Some(Path::new("/w")));
            }
            other => {
                panic!("expected SetRepo, RecordRecent, LoadStatus, LoadConfig, got {other:?}")
            }
        }
    }

    #[test]
    fn enter_on_the_init_row_inits_too() {
        let mut state = outside("/w");
        state.update(AppIntent::Start);
        init_tag(&press(&mut state, KeyCode::Enter));
    }

    #[test]
    fn a_failed_init_shows_gits_words_and_stays_outside() {
        let mut state = outside("/w");
        state.update(AppIntent::Start);
        let tag = init_tag(&press_char(&mut state, 'i'));
        let effects = state.update(AppIntent::RepoInitialised {
            tag,
            result: Err("fatal: cannot mkdir".into()),
        });
        assert_eq!(effects, vec![]);
        assert_eq!(state.busy, 0);
        assert_eq!(state.repo, None);
        match state.popups.last() {
            Some(Popup::Error(error)) => assert_eq!(error.message, "fatal: cannot mkdir"),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn i_is_refused_inside_a_repository() {
        let (state, _) = with_files(three_files(), 24);
        let mut state = state.with_location(Some("/r/a".into()), "/r/a".into());
        state.update(AppIntent::OpenRecent);
        assert_eq!(press_char(&mut state, 'i'), vec![]);
        assert_eq!(state.notice, Some("Already in a repository"));
    }

    /// Repository `/r/a` with three files loaded, `b.rs` selected.
    fn in_repo_a() -> AppState {
        let (state, _) = with_files(three_files(), 24);
        let mut state = state.with_location(Some("/r/a".into()), "/r/a".into());
        state.update(AppIntent::SelectNext);
        state
    }

    #[test]
    fn open_repo_switches_to_a_fresh_repository() {
        let mut state = in_repo_a();
        let generation = state.generation;
        let effects = state.update(AppIntent::OpenRepo("/r/b".into()));

        assert_eq!(state.generation, generation + 1);
        assert_eq!(state.repo, Some("/r/b".into()));
        assert_eq!(state.files, []);
        assert!(!state.files_loaded);
        assert_eq!(state.selected, None);
        assert_eq!(state.focus, Focus::Files);
        match effects.as_slice() {
            [
                Effect::SetRepo { root, nested },
                Effect::RecordRecent { path },
                Effect::LoadStatus { tag },
                Effect::LoadConfig { repo },
            ] => {
                assert_eq!(repo.as_deref(), Some(Path::new("/r/b")), "its own keys");
                assert_eq!(root, Path::new("/r/b"));
                assert_eq!(nested, &[] as &[RepoPath]);
                assert_eq!(path, Path::new("/r/b"));
                assert_eq!(tag.generation, state.generation);
            }
            other => {
                panic!("expected SetRepo, RecordRecent, LoadStatus, LoadConfig, got {other:?}")
            }
        }
        assert!(state.parked.contains_key(Path::new("/r/a")));
    }

    #[test]
    fn switching_back_restores_the_files_selection_and_diff_focus() {
        let mut state = in_repo_a();
        let files = state.files.clone();
        let branch = state.branch.clone();
        press(&mut state, KeyCode::Enter);
        assert!(matches!(state.focus, Focus::Diff(_)));

        state.update(AppIntent::OpenRepo("/r/b".into()));
        let tag = refresh_tag(&mut state);
        state.update(loaded(tag, &["b.txt"]));
        let effects = state.update(AppIntent::OpenRepo("/r/a".into()));

        // Back before the status lands.
        assert_eq!(state.files, files);
        assert!(state.files_loaded);
        assert_eq!(state.branch, branch);
        assert_eq!(state.selected, Some(1));
        match &state.focus {
            Focus::Diff(view) => assert_eq!(view.path, "b.rs".into()),
            other => panic!("expected the Diff pane, got {other:?}"),
        }
        assert!(matches!(effects[0], Effect::SetRepo { .. }));
        assert!(matches!(effects[1], Effect::RecordRecent { .. }));
        assert!(
            effects.iter().any(|e| matches!(
                e,
                Effect::LoadPatch { request, .. } if request.path == "b.rs".into()
            )),
            "{effects:?}"
        );
        assert!(matches!(
            effects[effects.len() - 2..],
            [Effect::LoadStatus { .. }, Effect::LoadConfig { .. }]
        ));
        assert!(!state.parked.contains_key(Path::new("/r/a")));
        assert!(state.parked.contains_key(Path::new("/r/b")));
    }

    #[test]
    fn switching_back_with_files_focused_keeps_the_files_panel() {
        let mut state = in_repo_a();
        state.update(AppIntent::OpenRepo("/r/b".into()));
        let effects = state.update(AppIntent::OpenRepo("/r/a".into()));
        assert_eq!(state.focus, Focus::Files);
        assert_eq!(state.selected, Some(1));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::LoadPatch { .. })),
            "{effects:?}"
        );
    }

    #[test]
    fn switching_back_into_a_submodule_restores_its_path() {
        let mut state = in_repo_a();
        state.repo_stack = vec![ParentRepo {
            path: "sub".into(),
            files: vec![],
            files_loaded: true,
            branch: None,
            selected: None,
        }];
        let stack = state.repo_stack.clone();

        state.update(AppIntent::OpenRepo("/r/b".into()));
        assert_eq!(state.repo_stack, []);
        let effects = state.update(AppIntent::OpenRepo("/r/a".into()));

        assert_eq!(state.repo_stack, stack);
        assert_eq!(
            effects[0],
            Effect::SetRepo {
                root: "/r/a".into(),
                nested: vec!["sub".into()],
            }
        );
    }

    #[test]
    fn opening_the_open_repository_only_closes_the_list() {
        let mut state = in_repo_a();
        state.update(AppIntent::OpenRecent);
        let before = AppState {
            popups: vec![],
            ..state.clone()
        };
        assert_eq!(state.update(AppIntent::OpenRepo("/r/a".into())), vec![]);
        assert_eq!(state, before);
    }

    #[test]
    fn opening_the_open_repository_from_a_submodule_goes_back_to_its_top() {
        let mut state = in_repo_a();
        let files = state.files.clone();
        state.repo_stack = vec![ParentRepo {
            path: "sub".into(),
            files: files.clone(),
            files_loaded: true,
            branch: None,
            selected: Some(2),
        }];
        state.files = vec![];
        let effects = state.update(AppIntent::OpenRepo("/r/a".into()));
        assert_eq!(state.repo_stack, []);
        assert_eq!(state.files, files);
        assert_eq!(state.selected, Some(2));
        assert_eq!(
            effects[0],
            Effect::SetRepo {
                root: "/r/a".into(),
                nested: vec![],
            }
        );
    }

    #[test]
    fn open_repo_waits_for_a_running_op() {
        let mut state = in_repo_a();
        state.op_seq = Some(9);
        assert_eq!(state.update(AppIntent::OpenRepo("/r/b".into())), vec![]);
        assert_eq!(state.notice, Some("Wait for the operation to finish"));
        assert_eq!(state.repo, Some("/r/a".into()));
    }

    #[test]
    fn a_late_init_result_for_another_generation_only_lowers_busy() {
        let mut state = outside("/w");
        state.update(AppIntent::Start);
        let tag = init_tag(&press_char(&mut state, 'i'));
        let stale = Tag {
            generation: tag.generation + 1,
            ..tag
        };
        let effects = state.update(AppIntent::RepoInitialised {
            tag: stale,
            result: Ok("/w".into()),
        });
        assert_eq!(effects, vec![]);
        assert_eq!(state.busy, 0);
        assert_eq!(state.repo, None);
    }

    // --- Watching for outside changes ---

    use crate::git::refs::RefsSnapshot;
    use crate::watch::WatchEvent;

    fn watch(state: &mut AppState, event: WatchEvent) -> Vec<Effect> {
        state.update(AppIntent::Watch(event))
    }

    /// The tag of the one background status load in `effects`.
    fn background_status_tag(effects: &[Effect]) -> Tag {
        match effects {
            [Effect::LoadStatus { tag }] => {
                assert!(!tag.user, "background loads stay out of the busy count");
                *tag
            }
            other => panic!("expected one background LoadStatus, got {other:?}"),
        }
    }

    fn snapshot_tag(effects: &[Effect]) -> Tag {
        match effects {
            [Effect::SnapshotRefs { tag }] => {
                assert!(!tag.user);
                *tag
            }
            other => panic!("expected one SnapshotRefs, got {other:?}"),
        }
    }

    fn refs_loaded(tag: Tag, bytes: &[u8]) -> AppIntent {
        AppIntent::RefsLoaded {
            tag,
            result: Ok(RefsSnapshot::of(bytes)),
        }
    }

    /// An idle workspace showing `three_files` with its diff settled.
    fn idle_with_files() -> AppState {
        let (state, _) = with_files(three_files(), 24);
        let state = state.with_location(Some("/r/a".into()), "/r/a".into());
        assert_eq!(state.busy, 0);
        state
    }

    #[test]
    fn poll_refs_asks_for_one_snapshot_until_it_reports() {
        let mut state = idle_with_files();
        let tag = snapshot_tag(&watch(&mut state, WatchEvent::PollRefs));
        assert_eq!(tag.generation, state.generation);
        assert!(watch(&mut state, WatchEvent::PollRefs).is_empty());

        state.update(refs_loaded(tag, b"a"));
        snapshot_tag(&watch(&mut state, WatchEvent::PollRefs));
    }

    #[test]
    fn the_first_or_an_equal_snapshot_starts_nothing_and_a_changed_one_a_background_status() {
        let mut state = idle_with_files();
        let first = snapshot_tag(&watch(&mut state, WatchEvent::PollRefs));
        assert!(state.update(refs_loaded(first, b"a")).is_empty());

        let equal = snapshot_tag(&watch(&mut state, WatchEvent::PollRefs));
        assert!(state.update(refs_loaded(equal, b"a")).is_empty());

        let moved = snapshot_tag(&watch(&mut state, WatchEvent::PollRefs));
        let tag = background_status_tag(&state.update(refs_loaded(moved, b"b")));
        assert_eq!(state.status_seq, Some(tag.seq));
        assert_eq!(state.busy, 0);
    }

    #[test]
    fn stale_or_failed_snapshots_are_dropped_but_free_the_slot() {
        let mut state = idle_with_files();
        let tag = snapshot_tag(&watch(&mut state, WatchEvent::PollRefs));
        state.update(refs_loaded(tag, b"a"));

        let stale = snapshot_tag(&watch(&mut state, WatchEvent::PollRefs));
        state.generation += 1;
        assert!(state.update(refs_loaded(stale, b"b")).is_empty());
        assert_eq!(state.watch.refs, Some(RefsSnapshot::of(b"a")));

        let failed = snapshot_tag(&watch(&mut state, WatchEvent::PollRefs));
        let effects = state.update(AppIntent::RefsLoaded {
            tag: failed,
            result: Err("boom".into()),
        });
        assert!(effects.is_empty());
        assert!(state.popups.is_empty());
        assert!(!state.watch.refs_pending);
    }

    #[test]
    fn poll_files_starts_a_background_status_that_never_touches_busy() {
        let mut state = idle_with_files();
        let tag = background_status_tag(&watch(&mut state, WatchEvent::PollFiles));
        assert_eq!(state.busy, 0);
        assert_eq!(state.status_seq, Some(tag.seq));
        assert!(state.watch.status_pending);

        state.update(AppIntent::StatusLoaded {
            tag,
            result: Ok(repo(three_files())),
        });
        assert_eq!(state.busy, 0);
        assert!(!state.watch.status_pending);
    }

    #[test]
    fn focus_gained_reloads_the_config_and_starts_a_background_status_and_a_snapshot() {
        let mut state = idle_with_files();
        let effects = watch(&mut state, WatchEvent::FocusGained);
        match effects.as_slice() {
            [
                Effect::LoadConfig { repo },
                Effect::LoadStatus { tag: status },
                Effect::SnapshotRefs { tag: refs },
            ] => {
                assert_eq!(repo.as_deref(), Some(Path::new("/r/a")));
                assert!(!status.user && !refs.user);
            }
            other => panic!("expected a config, a status and a snapshot, got {other:?}"),
        }
        assert_eq!(state.busy, 0);
    }

    #[test]
    fn background_status_is_skipped_while_busy_an_op_runs_or_one_is_pending() {
        let mut state = idle_with_files();
        state.busy = 1;
        assert!(watch(&mut state, WatchEvent::PollFiles).is_empty());
        assert_eq!(state.busy, 1);

        let mut state = idle_with_files();
        state.op_seq = Some(42);
        assert!(watch(&mut state, WatchEvent::PollFiles).is_empty());

        let mut state = idle_with_files();
        background_status_tag(&watch(&mut state, WatchEvent::PollFiles));
        let seq = state.status_seq;
        for _ in 0..3 {
            assert!(watch(&mut state, WatchEvent::PollFiles).is_empty());
        }
        assert_eq!(state.status_seq, seq);
    }

    #[test]
    fn focus_gained_while_busy_still_takes_a_snapshot() {
        let mut state = idle_with_files();
        state.busy = 1;
        let effects = watch(&mut state, WatchEvent::FocusGained);
        assert_eq!(
            effects[0],
            Effect::LoadConfig {
                repo: Some("/r/a".into())
            }
        );
        snapshot_tag(&effects[1..]);
    }

    #[test]
    fn an_equal_background_status_returns_no_effects() {
        let mut state = idle_with_files();
        let before = state.clone();
        let tag = background_status_tag(&watch(&mut state, WatchEvent::PollFiles));

        let effects = state.update(AppIntent::StatusLoaded {
            tag,
            result: Ok(repo(three_files())),
        });

        assert!(
            effects.is_empty(),
            "no diff reload and no CancelDiff: {effects:?}"
        );
        assert_eq!(state.diff, before.diff);
        assert_eq!(state.selected, before.selected);
    }

    #[test]
    fn a_changed_background_status_applies_like_a_refresh_keeping_the_path() {
        let mut state = idle_with_files();
        select_path(&mut state, "b.rs");
        let tag = background_status_tag(&watch(&mut state, WatchEvent::PollFiles));

        let effects = state.update(AppIntent::StatusLoaded {
            tag,
            result: Ok(repo(vec![
                changed("b.rs", 'M', '.'),
                untracked("c.txt"),
                untracked("d.txt"),
            ])),
        });

        assert_eq!(state.files.len(), 3);
        assert_eq!(state.selected, Some(0));
        diff_tag(&effects);
    }

    #[test]
    fn a_background_status_that_empties_the_list_cancels_the_diff() {
        let mut state = idle_with_files();
        let tag = background_status_tag(&watch(&mut state, WatchEvent::PollFiles));
        let effects = state.update(AppIntent::StatusLoaded {
            tag,
            result: Ok(repo(vec![])),
        });
        assert_eq!(effects, [Effect::CancelDiff]);
        assert!(state.files.is_empty());
    }

    #[test]
    fn a_failed_background_status_is_dropped_without_a_popup() {
        let mut state = idle_with_files();
        let files = state.files.clone();
        let tag = background_status_tag(&watch(&mut state, WatchEvent::PollFiles));

        let effects = state.update(AppIntent::StatusLoaded {
            tag,
            result: Err("fatal: index file corrupt".into()),
        });

        assert!(effects.is_empty());
        assert!(state.popups.is_empty());
        assert_eq!(state.status_error, None);
        assert_eq!(state.files, files);
        assert!(!state.watch.status_pending);
    }

    #[test]
    fn a_user_refresh_supersedes_a_background_status() {
        let mut state = idle_with_files();
        let background = background_status_tag(&watch(&mut state, WatchEvent::PollFiles));
        let user = refresh_tag(&mut state);

        let effects = state.update(AppIntent::StatusLoaded {
            tag: background,
            result: Ok(repo(vec![untracked("other.txt")])),
        });

        assert!(effects.is_empty());
        assert_eq!(state.files, three_files());
        assert!(!state.watch.status_pending);
        assert_eq!(state.busy, 1);
        state.update(loaded(user, &["x.txt"]));
        assert_eq!(state.busy, 0);
    }

    #[test]
    fn a_changed_background_status_keeps_an_open_blame_like_a_refresh_does() {
        let mut state = blamed(3, 24).with_location(Some("/r/a".into()), "/r/a".into());
        let tag = background_status_tag(&watch(&mut state, WatchEvent::PollFiles));
        state.update(AppIntent::StatusLoaded {
            tag,
            result: Ok(repo(vec![changed("a.rs", '.', 'M'), untracked("new.txt")])),
        });
        assert_eq!(blame(&state).path, RepoPath::from("a.rs"));
        assert_eq!(state.files.len(), 2);
    }

    #[test]
    fn entering_a_submodule_makes_its_first_snapshot_a_baseline() {
        let mut state = with_submodules(vec![submodule("sub", SubmoduleState::UpToDate)]);
        state.watch.refs = Some(RefsSnapshot::of(b"parent"));
        let tag = match press(&mut state, KeyCode::Enter).as_slice() {
            [_, Effect::LoadStatus { tag }] => *tag,
            other => panic!("expected SetRepo and LoadStatus, got {other:?}"),
        };
        state.update(loaded(tag, &[]));
        assert_eq!(state.busy, 0);

        let refs = snapshot_tag(&watch(&mut state, WatchEvent::PollRefs));
        assert!(state.update(refs_loaded(refs, b"sub")).is_empty());
        assert_eq!(state.watch.refs, Some(RefsSnapshot::of(b"sub")));
    }

    #[test]
    fn watch_events_only_reload_the_config_in_review() {
        let mut state = AppState::new(Context::Review { number: 1 });
        for event in [
            WatchEvent::PollRefs,
            WatchEvent::PollFiles,
            WatchEvent::Fetch,
        ] {
            assert!(watch(&mut state, event).is_empty(), "{event:?}");
        }
        assert_eq!(
            watch(&mut state, WatchEvent::FocusGained),
            [Effect::LoadConfig { repo: None }]
        );
        assert!(!state.watch.is_pending());
    }

    // --- Config ---

    use crate::config::ConfigError;
    use crate::keymap::{Keymap, remap as keymap};

    /// A config result for `idle_with_files`'s repository, `/r/a`.
    fn config_loaded(keymap: Keymap, errors: Vec<ConfigError>) -> AppIntent {
        config_for(Some("/r/a"), keymap, errors)
    }

    fn config_for(repo: Option<&str>, keymap: Keymap, errors: Vec<ConfigError>) -> AppIntent {
        AppIntent::ConfigLoaded {
            repo: repo.map(PathBuf::from),
            keymap,
            errors,
        }
    }

    fn config_error(message: &str) -> ConfigError {
        ConfigError {
            path: "/home/me/.config/kika/config.toml".into(),
            message: message.into(),
        }
    }

    fn is_stage(effects: &[Effect]) -> bool {
        matches!(
            effects,
            [Effect::RunOp {
                op: GitOp::Stage(_),
                ..
            }]
        )
    }

    #[test]
    fn a_remapped_key_runs_its_binding_and_the_old_one_does_nothing() {
        let mut state = idle_with_files();
        state.update(config_loaded(keymap(&[("files.stage", &["x"])]), vec![]));

        assert_eq!(press_char(&mut state.clone(), 's'), []);
        let mut space = state.clone();
        assert_eq!(press_char(&mut space, ' '), []);
        assert_eq!(space.op_seq, None);
        assert_eq!(space.notice, None);
        assert!(is_stage(&press_char(&mut state, 'x')), "x stages");
    }

    #[test]
    fn a_disabled_binding_leaves_its_key_unbound() {
        let mut state = idle_with_files();
        let mut before = state.clone();
        press_char(&mut before, 'd');
        assert_eq!(before.popups.len(), 1, "d asks to discard by default");
        state.update(config_loaded(keymap(&[("files.discard", &[])]), vec![]));
        assert_eq!(press_char(&mut state, 'd'), []);
        assert!(state.popups.is_empty(), "{:?}", state.popups);
        assert_eq!(state.notice, None);
    }

    #[test]
    fn config_errors_are_shown_once_until_they_change() {
        let mut state = idle_with_files();
        let errors = vec![config_error("unknown binding \"files.stag\"")];
        state.update(config_loaded(Keymap::default(), errors.clone()));
        match state.popups.as_slice() {
            [Popup::Error(error)] => {
                assert_eq!(error.title, "Config error");
                assert!(error.message.contains("config.toml: unknown binding"));
                assert!(error.next.contains("ignored"), "{}", error.next);
            }
            other => panic!("expected one config error, got {other:?}"),
        }

        // The user closes it; refocusing finds the same broken file.
        state.popups.clear();
        state.update(config_loaded(Keymap::default(), errors.clone()));
        assert!(state.popups.is_empty(), "{:?}", state.popups);

        // A different error is news.
        state.update(config_loaded(
            Keymap::default(),
            vec![config_error("other")],
        ));
        assert_eq!(state.popups.len(), 1);

        // Once fixed, the same error later shows again.
        state.popups.clear();
        state.update(config_loaded(Keymap::default(), vec![]));
        assert_eq!(state.config_error, None);
        state.update(config_loaded(Keymap::default(), errors));
        assert_eq!(state.popups.len(), 1);
    }

    #[test]
    fn a_config_load_replaces_the_keymap_even_with_errors() {
        let mut state = idle_with_files();
        let remapped = keymap(&[("files.stage", &["x"])]);
        state.update(config_loaded(remapped.clone(), vec![]));
        assert_eq!(state.keymap, remapped);
        state.update(config_loaded(Keymap::default(), vec![config_error("bad")]));
        assert_eq!(state.keymap, Keymap::default());
    }

    #[test]
    fn a_config_result_for_another_repository_changes_nothing() {
        let mut state = idle_with_files();
        let before = state.clone();
        state.update(config_for(
            Some("/r/b"),
            keymap(&[("files.stage", &["x"])]),
            vec![config_error("bad")],
        ));
        assert_eq!(state.keymap, before.keymap);
        assert_eq!(state.config_error, before.config_error);
        assert_eq!(state.popups, before.popups);
    }

    #[test]
    fn a_late_config_result_from_before_a_switch_is_dropped() {
        let mut state = idle_with_files();
        assert_eq!(
            watch(&mut state, WatchEvent::FocusGained)[0],
            Effect::LoadConfig {
                repo: Some("/r/a".into())
            }
        );
        let effects = state.update(AppIntent::OpenRepo("/r/b".into()));
        assert_eq!(
            effects.last(),
            Some(&Effect::LoadConfig {
                repo: Some("/r/b".into())
            })
        );

        // A's reload lands after the switch.
        state.update(config_for(
            Some("/r/a"),
            keymap(&[("files.stage", &["x"])]),
            vec![config_error("a is broken")],
        ));
        assert_eq!(state.popups, []);
        assert_eq!(state.config_error, None);
        assert_eq!(state.keymap, Keymap::default());

        // B's own result applies.
        let remapped = keymap(&[("files.stage", &["y"])]);
        state.update(config_for(
            Some("/r/b"),
            remapped.clone(),
            vec![config_error("b is broken")],
        ));
        assert_eq!(state.keymap, remapped);
        match state.popups.as_slice() {
            [Popup::Error(error)] => assert!(error.message.contains("b is broken")),
            other => panic!("expected B's config error, got {other:?}"),
        }
    }

    #[test]
    fn a_config_result_without_a_repository_applies_while_none_is_open() {
        let mut state = outside("/w");
        state.update(AppIntent::Start);
        let remapped = keymap(&[("files.stage", &["x"])]);
        state.update(config_for(None, remapped.clone(), vec![]));
        assert_eq!(state.keymap, remapped);
        // Nor does a repository's result leak outside one.
        state.update(config_for(Some("/w"), Keymap::default(), vec![]));
        assert_eq!(state.keymap, remapped);
    }

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
        assert_eq!(state.busy, 0);
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
        assert_eq!(state.busy, 0);
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
        state.op_seq = Some(9);
        let effects = state.update(AppIntent::FetchFinished {
            tag,
            result: Ok(()),
        });
        assert!(effects.is_empty());
    }

    fn renderer(label: &str) -> ExternalRenderer {
        ExternalRenderer {
            label: label.into(),
            argv: vec![label.into()],
        }
    }

    /// `files` loaded on an 80x24 screen, with renderers `labels` found.
    fn with_renderers(files: Vec<FileEntry>, labels: &[&str]) -> AppState {
        let (mut state, _) = with_files(files, 24);
        state.update(AppIntent::RenderersFound(
            labels.iter().map(|l| renderer(l)).collect(),
        ));
        state
    }

    /// The one `RenderDiff` among `effects`: its tag, request and cols.
    fn render_of(effects: &[Effect]) -> (Tag, DiffRequest, u16) {
        let renders: Vec<_> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::RenderDiff {
                    tag, request, cols, ..
                } => Some((*tag, request.clone(), *cols)),
                _ => None,
            })
            .collect();
        match renders.as_slice() {
            [render] => render.clone(),
            other => panic!("expected one RenderDiff in {effects:?}, got {other:?}"),
        }
    }

    fn rendered(tag: Tag, text: &str) -> AppIntent {
        AppIntent::DiffRendered {
            tag,
            result: Ok(crate::ansi::parse(text.as_bytes())),
        }
    }

    #[test]
    fn cycling_renders_the_shown_diff_at_the_pane_width() {
        let mut state = with_renderers(three_files(), &["delta"]);
        let busy = state.busy;
        let (tag, request, cols) = render_of(&state.update(AppIntent::CycleRenderer));
        assert_eq!(state.renderer, Some(0));
        assert_eq!(cols, state.pane_cols());
        // 80 columns: the Diff pane is 53 wide, less borders and gutter.
        assert_eq!(cols, 50);
        assert!(tag.user);
        assert_eq!(state.busy, busy + 1);
        assert_eq!(
            request,
            DiffRequest::for_entry(&state.files[0], state.diff_options)
        );
        assert!(state.rendered.as_ref().unwrap().rendering());
    }

    #[test]
    fn a_wider_screen_renders_again_and_a_taller_one_does_not() {
        let mut state = with_renderers(three_files(), &["delta"]);
        state.update(AppIntent::CycleRenderer);
        let effects = state.update(AppIntent::Resize {
            width: 120,
            height: 24,
        });
        let (_, _, cols) = render_of(&effects);
        assert_eq!(cols, state.pane_cols());
        assert_eq!(cols, 77);
        let effects = state.update(AppIntent::Resize {
            width: 120,
            height: 40,
        });
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::RenderDiff { .. }))
        );
    }

    #[test]
    fn zooming_renders_again_at_the_diffs_new_width() {
        let mut state = with_renderers(three_files(), &["delta"]);
        let (tag, _, _) = render_of(&state.update(AppIntent::CycleRenderer));
        state.update(rendered(tag, "row\n"));
        // Focus the diff, then zoom it: half, then full.
        // The diff's own view renders on focus; answer it.
        let (tag, _, _) = render_of(&press(&mut state, KeyCode::Char('l')));
        state.update(rendered(tag, "row\n"));
        let mut widths = vec![state.pane_cols()];
        for _ in 0..2 {
            let (tag, _, cols) = render_of(&press(&mut state, KeyCode::Char('z')));
            assert_eq!(cols, state.pane_cols());
            widths.push(cols);
            state.update(rendered(tag, "row\n"));
        }
        // 80 columns: 2/3, then 3/4, then all of it, less borders and gutter.
        assert_eq!(widths, [50, 57, 77]);
    }

    #[test]
    fn r_works_in_the_diff_reached_from_any_side_panel() {
        let mut state = with_renderers(three_files(), &["delta"]);
        press(&mut state, KeyCode::Char('1'));
        press(&mut state, KeyCode::Char('l'));
        assert!(matches!(state.focus, Focus::Diff(_)));
        render_of(&state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('R'),
            KeyModifiers::SHIFT,
        ))));
        assert_eq!(state.renderer, Some(0));
    }

    #[test]
    fn r_steps_through_each_renderer_and_back_to_the_built_in_view() {
        let mut state = with_renderers(three_files(), &["delta", "riff"]);
        // R is a Diff pane key.
        press(&mut state, KeyCode::Enter);
        let shift_r = |state: &mut AppState| {
            state.update(AppIntent::Key(KeyPress::new(
                KeyCode::Char('R'),
                KeyModifiers::SHIFT,
            )))
        };
        let (_, _, _) = render_of(&shift_r(&mut state));
        assert_eq!(state.renderer, Some(0));
        let (_, _, _) = render_of(&shift_r(&mut state));
        assert_eq!(state.renderer, Some(1));
        assert_eq!(shift_r(&mut state), vec![]);
        assert_eq!(state.renderer, None);
        assert_eq!(state.rendered, None);
    }

    #[test]
    fn without_renderers_r_says_so_and_does_nothing() {
        let (mut state, _) = with_files(three_files(), 24);
        assert_eq!(state.update(AppIntent::CycleRenderer), vec![]);
        assert_eq!(state.notice, Some(external_diff::NO_RENDERER));
        assert_eq!(state.renderer, None);
        state.notice = None;
        press(&mut state, KeyCode::Enter);
        assert_eq!(press(&mut state, KeyCode::Char('R')), vec![]);
        assert_eq!(state.notice, Some(external_diff::NO_RENDERER));
    }

    #[test]
    fn a_stale_render_is_dropped_but_busy_still_falls() {
        let mut state = with_renderers(three_files(), &["delta"]);
        let (old, _, _) = render_of(&state.update(AppIntent::CycleRenderer));
        let (new, _, _) = render_of(&state.update(AppIntent::SelectNext));
        let busy = state.busy;
        state.update(rendered(old, "old\n"));
        assert_eq!(state.busy, busy - 1);
        assert!(state.rendered.as_ref().unwrap().rows.is_empty());
        state.update(rendered(new, "new\n"));
        let shown = state.rendered_shown().expect("rendered rows shown");
        assert_eq!(shown.path, "b.rs".into());
        assert_eq!(shown.rows.len(), 1);
    }

    #[test]
    fn scrolling_moves_the_rendered_rows_while_they_are_shown() {
        let mut state = with_renderers(three_files(), &["delta"]);
        let (tag, _, _) = render_of(&state.update(AppIntent::CycleRenderer));
        state.update(rendered(tag, &"row\n".repeat(50)));
        state.update(AppIntent::ScrollDiff(Scroll::PageDown));
        assert_eq!(state.rendered.as_ref().unwrap().scroll, state.pane_rows());
        assert_eq!(state.diff.scroll, 0);
    }

    #[test]
    fn staging_options_and_side_changes_render_again() {
        let mut state = with_renderers(three_files(), &["delta"]);
        state.update(AppIntent::CycleRenderer);
        render_of(&press(&mut state, KeyCode::Char('w')));
        let (_, request, _) = render_of(&press(&mut state, KeyCode::Enter));
        assert_eq!(
            (request.side, request.orig_path),
            (DiffSide::Worktree, None)
        );
        let (_, request, _) = render_of(&press(&mut state, KeyCode::Char('t')));
        assert_eq!(request.side, DiffSide::Index);
        // Back to the Files panel on the entry's side again.
        let (_, request, _) = render_of(&press(&mut state, KeyCode::Esc));
        assert_eq!(request.side, DiffSide::Worktree);
        let (tag, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        let status = refresh_tag(&mut state);
        state.update(finished(tag, op, Ok(())));
        let effects = state.update(AppIntent::StatusLoaded {
            tag: Tag {
                seq: state.status_seq.unwrap(),
                ..status
            },
            result: Ok(repo(three_files())),
        });
        render_of(&effects);
    }

    /// `TWO_HUNKS` as a decorating renderer draws it: a file banner, a box
    /// per hunk, and old and new line numbers before each line.
    const DECORATED: &str = "───────\na.rs\n───────\n┌────┐\n│ @1 │\n└────┘\n\
        1 ⋮   │a\n  ⋮ 1 │b\n  ⋮ 2 │c\n2 ⋮ 3 │d\n┌────┐\n│ @9 │\n└────┘\n\
        9 ⋮   │e\n  ⋮10 │f\n";

    /// Focus the first of `three_files` with `TWO_HUNKS` loaded, through
    /// renderer `delta` when `render` is set.
    fn focused_through(render: bool) -> AppState {
        let mut state = with_renderers(three_files(), &["delta"]);
        if render {
            state.update(AppIntent::CycleRenderer);
        }
        let effects = press(&mut state, KeyCode::Enter);
        let (tag, _) = patch_load(&effects);
        state.update(AppIntent::PatchLoaded {
            tag,
            result: Ok(patch_of(TWO_HUNKS)),
        });
        if render {
            let (tag, _, _) = render_of(&effects);
            state.update(rendered(tag, DECORATED));
        }
        state
    }

    #[test]
    fn line_staging_through_a_renderer_stages_what_the_built_in_view_does() {
        let mut built_in = focused_through(false);
        let mut through = focused_through(true);
        for state in [&mut built_in, &mut through] {
            press(state, KeyCode::Char('v'));
            press(state, KeyCode::Char('j'));
        }
        let shown = through
            .rendered_shown()
            .expect("the rendered rows are shown");
        let picked = view(&through).selected_range();
        assert_eq!(shown.mapped_rows(0, Some(&picked)), [7], "the `b` row");
        let expected = op_of(&press(&mut built_in, KeyCode::Char(' '))).1;
        let (_, op) = op_of(&press(&mut through, KeyCode::Char(' ')));
        assert_eq!(op, expected);
    }

    #[test]
    fn hunk_moves_scroll_the_rendered_rows_to_the_hunk() {
        let (mut state, _) = with_files(three_files(), 6);
        state.update(AppIntent::RenderersFound(vec![renderer("delta")]));
        state.update(AppIntent::CycleRenderer);
        let effects = press(&mut state, KeyCode::Enter);
        let (tag, _) = patch_load(&effects);
        state.update(AppIntent::PatchLoaded {
            tag,
            result: Ok(patch_of(TWO_HUNKS)),
        });
        let (tag, _, _) = render_of(&effects);
        state.update(rendered(tag, DECORATED));
        // 3 rows: hunk 0's lines are rows 6 to 9, so its first is on top.
        assert_eq!(state.rendered.as_ref().unwrap().scroll, 6);
        // Hunk 1's are rows 13 and 14, so the last is at the bottom.
        press(&mut state, KeyCode::Char('j'));
        assert_eq!(state.rendered.as_ref().unwrap().scroll, 12);
    }
    #[test]
    fn outside_a_repository_the_watch_only_reloads_the_global_config() {
        let mut state = outside("/w");
        state.update(AppIntent::Start);
        for event in [
            WatchEvent::PollFiles,
            WatchEvent::PollRefs,
            WatchEvent::Fetch,
        ] {
            assert!(watch(&mut state, event).is_empty(), "{event:?}");
        }
        assert_eq!(
            watch(&mut state, WatchEvent::FocusGained),
            [Effect::LoadConfig { repo: None }]
        );
    }

    // --- Custom commands ---

    use crate::custom_command::{CommandFailure, CommandTable, Panel, test_table};

    /// `state` with these commands from the global file.
    fn with_commands(state: &mut AppState, tables: &[CommandTable]) {
        let keymap = Keymap::default().with_commands(tables).unwrap();
        let repo = state.repo.clone();
        state.update(AppIntent::ConfigLoaded {
            repo,
            keymap,
            errors: vec![],
        });
    }

    fn ctrl_t(state: &mut AppState) -> Vec<Effect> {
        state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('t'),
            KeyModifiers::CONTROL,
        )))
    }

    fn files_command(command: &[&str], prompts: &[&str], output: OutputMode) -> CommandTable {
        test_table(Panel::Files, &["ctrl-t"], command, prompts, output)
    }

    fn os(items: &[&str]) -> Vec<std::ffi::OsString> {
        items.iter().map(std::ffi::OsString::from).collect()
    }

    /// A workspace whose one file has a hostile name, selected.
    fn hostile_file() -> AppState {
        let (state, _) = with_files(vec![changed("a b;$(rm -rf x).rs", '.', 'M')], 24);
        state
    }

    #[test]
    fn a_captured_command_runs_on_the_selected_file_as_one_argument() {
        let mut state = hostile_file();
        with_commands(
            &mut state,
            &[files_command(
                &["wc", "--", "{path}"],
                &[],
                OutputMode::None,
            )],
        );
        let busy = state.busy;

        let effects = ctrl_t(&mut state);

        assert_eq!(
            effects,
            [Effect::RunCommand {
                title: "Run wc".into(),
                argv: os(&["wc", "--", "a b;$(rm -rf x).rs"]),
                output: OutputMode::None,
            }]
        );
        assert!(state.command_running);
        assert_eq!(state.busy, busy + 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_command_gets_the_raw_bytes_of_the_file_name() {
        use std::os::unix::ffi::OsStrExt;
        let raw = FileEntry {
            path: RepoPath::from_bytes(b"x\xff.rs"),
            ..changed("x", '.', 'M')
        };
        let (mut state, _) = with_files(vec![raw], 24);
        with_commands(
            &mut state,
            &[files_command(
                &["less", "{path}"],
                &[],
                OutputMode::Terminal,
            )],
        );

        match ctrl_t(&mut state).as_slice() {
            [Effect::HandOff(HandOff::RunCommand { title, argv })] => {
                assert_eq!(title, "Run less");
                assert_eq!(argv[1].as_bytes(), b"x\xff.rs");
            }
            other => panic!("expected a RunCommand hand-off, got {other:?}"),
        }
        // The terminal is handed over; nothing counts as running here.
        assert!(!state.command_running);
    }

    #[test]
    fn prompts_are_asked_in_order_then_the_command_runs_with_the_answers() {
        let mut state = hostile_file();
        with_commands(
            &mut state,
            &[files_command(
                &["cp", "{path}", "{prompt1}", "--mode={prompt2}"],
                &["Copy to", "Mode\x1b[2J"],
                OutputMode::Popup,
            )],
        );

        assert_eq!(ctrl_t(&mut state), []);
        assert!(matches!(
            state.popups.as_slice(),
            [Popup::Prompt {
                kind: PromptKind::Command,
                ..
            }]
        ));
        assert_eq!(state.prompt_title(PromptKind::Command), "Copy to");
        type_text(&mut state, "dir with space");
        assert_eq!(press(&mut state, KeyCode::Enter), []);
        assert_eq!(state.popups.len(), 1);
        let title = state.prompt_title(PromptKind::Command);
        assert_eq!(title, "Mode\u{FFFD}[2J");
        // A reload while the user types does not change what runs.
        with_commands(&mut state, &[]);

        let effects = state.update(AppIntent::SubmitPrompt);

        assert_eq!(
            effects,
            [Effect::RunCommand {
                title: "Run cp".into(),
                argv: os(&["cp", "a b;$(rm -rf x).rs", "dir with space", "--mode="]),
                output: OutputMode::Popup,
            }]
        );
        assert!(state.popups.is_empty());
        assert_eq!(state.pending_command, None);
    }

    #[test]
    fn answers_arrive_as_intents_in_order() {
        let mut state = hostile_file();
        with_commands(
            &mut state,
            &[files_command(
                &["echo", "{prompt2}", "{prompt1}"],
                &["One", "Two"],
                OutputMode::None,
            )],
        );
        ctrl_t(&mut state);
        state.popups.pop();
        assert_eq!(
            state.update(AppIntent::CommandPromptAnswered("1".into())),
            []
        );
        state.popups.pop();
        let effects = state.update(AppIntent::CommandPromptAnswered("2".into()));
        match effects.as_slice() {
            [Effect::RunCommand { argv, .. }] => assert_eq!(argv, &os(&["echo", "2", "1"])),
            other => panic!("expected RunCommand, got {other:?}"),
        }
    }

    #[test]
    fn cancelling_a_prompt_cancels_the_command() {
        let mut state = hostile_file();
        with_commands(
            &mut state,
            &[files_command(
                &["echo", "{prompt1}"],
                &["Q"],
                OutputMode::None,
            )],
        );
        ctrl_t(&mut state);
        assert!(state.pending_command.is_some());

        assert_eq!(state.update(AppIntent::PopPopup), []);

        assert_eq!(state.pending_command, None);
        assert!(state.popups.is_empty());
        assert!(!state.command_running);
        // A late answer has nothing to run.
        assert_eq!(
            state.update(AppIntent::CommandPromptAnswered("x".into())),
            []
        );
        // Esc does the same.
        ctrl_t(&mut state);
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.pending_command, None);
    }

    #[test]
    fn a_command_needs_a_selection_and_what_its_template_uses() {
        let mut state = AppState::new(Context::Workspace);
        with_commands(
            &mut state,
            &[files_command(&["echo", "{path}"], &["Q"], OutputMode::None)],
        );
        assert_eq!(ctrl_t(&mut state), []);
        assert_eq!(state.notice, Some("No file selected"));
        assert!(state.popups.is_empty());
    }

    #[test]
    fn a_blame_command_gets_the_line_and_commit_or_says_why_not() {
        let mut state = blamed(3, 24);
        let table = test_table(
            Panel::Blame,
            &["ctrl-t"],
            &["git", "show", "{commit}", "{path}:{line}"],
            &["Q"],
            OutputMode::Terminal,
        );
        with_commands(&mut state, &[table]);
        state.update(AppIntent::BlameNext);
        state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('t'),
            KeyModifiers::CONTROL,
        )));
        type_text(&mut state, "x");
        let effects = press(&mut state, KeyCode::Enter);
        assert_eq!(
            effects,
            [Effect::HandOff(HandOff::RunCommand {
                title: "Run git".into(),
                argv: os(&["git", "show", OID, "a.rs:2"]),
            })]
        );

        // The last line is not committed: refused before any prompt.
        state.update(AppIntent::BlameNext);
        assert_eq!(ctrl_t(&mut state), []);
        assert_eq!(state.notice, Some("Not committed yet"));
        assert!(state.popups.is_empty());
    }

    #[test]
    fn only_one_captured_command_runs_at_a_time() {
        let mut state = hostile_file();
        with_commands(
            &mut state,
            &[files_command(&["sleep", "9"], &[], OutputMode::None)],
        );
        assert_eq!(ctrl_t(&mut state).len(), 1);
        let busy = state.busy;

        assert_eq!(ctrl_t(&mut state), []);

        assert_eq!(state.notice, Some("A command is running"));
        assert_eq!(state.busy, busy);
        assert!(state.popups.is_empty());
    }

    fn command_finished(output: OutputMode, result: Result<&str, CommandFailure>) -> AppIntent {
        AppIntent::CommandFinished {
            title: "Show it".into(),
            output,
            result: result.map(String::from),
        }
    }

    fn exited(text: &str) -> CommandFailure {
        CommandFailure::Exited(text.into())
    }

    fn is_status_load(effects: &[Effect]) -> bool {
        matches!(effects, [Effect::LoadStatus { .. }])
    }

    #[test]
    fn captured_output_is_shown_safely_and_the_status_reloaded() {
        let mut state = hostile_file();
        with_commands(
            &mut state,
            &[files_command(&["sh", "x"], &[], OutputMode::Popup)],
        );
        ctrl_t(&mut state);
        let busy = state.busy;

        let effects = state.update(command_finished(
            OutputMode::Popup,
            Ok("red \x1b[31mtext\nline 2"),
        ));

        assert!(is_status_load(&effects), "{effects:?}");
        assert!(!state.command_running);
        // One down for the command, one up for the reload.
        assert_eq!(state.busy, busy);
        match state.popups.as_slice() {
            [Popup::Error(popup)] => {
                assert_eq!(popup.title, "Show it");
                assert!(!popup.message.contains('\x1b'), "{popup:?}");
                assert!(popup.message.starts_with("red "), "{popup:?}");
                assert!(popup.message.ends_with("text\nline 2"), "{popup:?}");
            }
            other => panic!("expected the output popup, got {other:?}"),
        }
        // And another command may start.
        state.popups.clear();
        assert_eq!(ctrl_t(&mut state).len(), 1);
    }

    #[test]
    fn a_command_that_exited_non_zero_says_so_and_still_reloads() {
        let mut state = hostile_file();
        for output in [OutputMode::None, OutputMode::Popup, OutputMode::Terminal] {
            state.popups.clear();
            let effects = state.update(command_finished(
                output,
                Err(exited("exited with exit status: 3\nno")),
            ));
            assert!(is_status_load(&effects), "{effects:?}");
            match state.popups.as_slice() {
                [Popup::Error(popup)] => {
                    assert_eq!(popup.title, "Show it failed");
                    assert_eq!(popup.message, "exited with exit status: 3\nno");
                    // It ran as configured: the config is not to blame.
                    assert_eq!(popup.next, "Press Esc to close");
                }
                other => panic!("expected the failure popup, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_command_that_could_not_launch_points_at_the_config() {
        let mut state = hostile_file();
        let effects = state.update(command_finished(
            OutputMode::None,
            Err(CommandFailure::Launch(
                "Could not start nope: not found".into(),
            )),
        ));
        assert!(is_status_load(&effects), "{effects:?}");
        match state.popups.as_slice() {
            [Popup::Error(popup)] => {
                assert_eq!(popup.title, "Show it failed");
                assert_eq!(popup.message, "Could not start nope: not found");
                assert_eq!(
                    popup.next,
                    "Check the command in your config file, then try again."
                );
            }
            other => panic!("expected the failure popup, got {other:?}"),
        }
    }

    #[test]
    fn the_cancel_key_stops_only_a_running_command() {
        let mut state = hostile_file();
        assert_eq!(state.update(AppIntent::CancelCommand), []);

        with_commands(
            &mut state,
            &[files_command(&["sleep", "9"], &[], OutputMode::Popup)],
        );
        ctrl_t(&mut state);
        assert_eq!(
            state.update(AppIntent::CancelCommand),
            [Effect::CancelCommand]
        );
        // Still running until the command answers.
        assert!(state.command_running);
    }

    #[test]
    fn a_cancelled_command_says_so_without_a_popup_and_reloads() {
        let mut state = hostile_file();
        with_commands(
            &mut state,
            &[files_command(&["sleep", "9"], &[], OutputMode::Popup)],
        );
        ctrl_t(&mut state);
        let busy = state.busy;

        let effects = state.update(command_finished(
            OutputMode::Popup,
            Err(CommandFailure::Cancelled),
        ));

        assert!(is_status_load(&effects), "{effects:?}");
        assert!(!state.command_running);
        // One down for the command, one up for the reload.
        assert_eq!(state.busy, busy);
        assert!(state.popups.is_empty(), "{:?}", state.popups);
        assert_eq!(state.notice, Some("Command cancelled"));
    }

    #[test]
    fn a_quiet_success_shows_nothing_but_reloads() {
        let mut state = hostile_file();
        for output in [OutputMode::None, OutputMode::Terminal] {
            let effects = state.update(command_finished(output, Ok("")));
            assert!(is_status_load(&effects), "{effects:?}");
            assert!(state.popups.is_empty());
        }
    }
}
