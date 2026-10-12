use crate::ansi::StyledRow;
use crate::bisect;
use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::branches::{BranchesPanel, BranchesTab};
use crate::clipboard::{self, CopyWhat};
use crate::commit_editor::CommitEditor;
use crate::commit_files::CommitFilesList;
use crate::commits::{CommitsPanel, CommitsTab};
use crate::config::ConfigError;
use crate::custom_patch::CustomPatch;
use crate::diff_mark::{DiffEnd, DiffMark};
use crate::diff_pane::DiffPane;
use crate::diff_search::DiffSearch;
use crate::effect::{Effect, Tag};
use crate::external_diff::{self, ExternalRenderer, RendererKind, map_rows};
use crate::features::blame::BlameView;
use crate::features::branches::remotes::keys::change_remote_disabled;
use crate::features::branches::remotes::{RemotePending, RemoteRow};
use crate::features::branches::tags::PendingTag;
use crate::features::branches::tags::keys::delete_tag_disabled;
use crate::features::command_log::CommandLog;
use crate::features::commits::compare::{MARK_CLEARED, PendingDiff};
use crate::features::commits::copy::CopiedCommits;
use crate::features::conflicts::ConflictView;
use crate::features::custom_commands::PendingCommand;
use crate::features::pull_requests::{self, PrCreating, PrDraft};
use crate::features::repos::ParkedRepo;
use crate::features::review::{self, PrMerging};
use crate::forge::{ForgeHost, ForgeState};
use crate::git::FileEntry;
use crate::git::askpass::Secret;
use crate::git::bisect::BisectMark;
use crate::git::branches::{Branch, BranchName, LogRow};
use crate::git::commit_files::{CommitFile, CommitId, CommitRange};
use crate::git::diff::{
    DiffChunk, DiffOptions, DiffRequest, DiffSide, EmojiZwj, MAX_CONTEXT, MIN_CONTEXT, sanitize,
};
use crate::git::log::LogQuery;
use crate::git::patch::{self, Direction, FileKind, FilePatch, PatchSelection};
use crate::git::path::RepoPath;
use crate::git::rebase_plan;
use crate::git::reflog::Reflog;
use crate::git::refs::RefsSnapshot;
use crate::git::remote::RemoteOp;
use crate::git::repo_state::Operation;
use crate::git::status::{BranchStatus, EntryKind, RepoStatus};
use crate::git::undo;
use crate::git::version::GitVersion;
use crate::gitignore::{self, IgnoreAdded, IgnoreDraft};
use crate::handoff::HandOff;
use crate::hunk_view::{HunkView, NO_CHANGES};
use crate::intent::{AppIntent, Scroll};
use crate::keymap::{self, KeyPattern, KeyScope, Keymap, Reason, Resolution};
use crate::layout::{self, Panel, ScreenMode, SidePanel};
use crate::list_filter::{self, InputTarget, ListInput};
use crate::menu;
use crate::op::{GitOp, MergeStrategy, ResetMode, StashMode};
use crate::pick_list::{PickList, PickPurpose};
use crate::popup::{
    ConfirmKind, CredentialPrompt, DeleteBranchConfirm, DiscardConfirm, DiscardWhat, ErrorPopup,
    LineInput, Popup, PromptKind, RangeKind, TravelConfirm,
};
use crate::prompts::{PromptFacts, needs_terminal, runs_attached};
use crate::rebase_todo::{self, RebaseTodo, Rewrite, TODO_CHANGED, TodoFile};
use crate::refresh::{self, RefreshPlan, Scope, Selection};
use crate::remote_run::RemoteRun;
use crate::rendered_view::RenderedDiff;
use crate::stash::{StashDraft, StashList};
use crate::watch::{WatchEvent, WatchState};
use crate::worktree_file::CHANGED_ON_DISK;

/// What Kika is looking at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Context {
    /// The local working tree and index.
    Workspace,
    /// GitHub pull requests to review; `number` is the one the session
    /// was started on, if any.
    Review { number: Option<u64> },
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
    /// The selected conflicted file's blocks, in the Diff pane's place.
    Conflict(ConflictView),
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

    /// Where it is drawn. The blame and the conflict view take the
    /// diff's place.
    pub fn panel(&self) -> Panel {
        match self {
            Focus::Status => Panel::Side(SidePanel::Status),
            Focus::Files => Panel::Side(SidePanel::Files),
            Focus::Branches => Panel::Side(SidePanel::Branches),
            Focus::Commits => Panel::Side(SidePanel::Commits),
            Focus::Stash => Panel::Side(SidePanel::Stash),
            Focus::Diff(_) | Focus::Blame(_) | Focus::Conflict(_) => Panel::Diff,
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
    /// The left-behind check in flight; one at a time, so a second press
    /// cannot stack a second question.
    pub left_behind_check: Option<leave_commits::LeftBehindCheck>,
    /// The running commit has the terminal, so Ctrl+C goes to git, not Kika.
    pub commit_on_terminal: bool,
    /// From the global `[commit] hooks_detached`: the user's hooks never ask
    /// anything, so a commit that only runs hooks stays detached.
    pub commit_hooks_detached: bool,
    /// The last operation's failure, or why one was refused.
    pub op_error: Option<String>,
    /// How the selection is kept when the pending status load lands.
    pub status_selection: Selection,
    pub help_open: bool,
    /// The first help row shown, for a list taller than the screen.
    pub help_scroll: usize,
    pub commit: CommitFlow,
    /// Why the last key did nothing, shown in the bottom bar until the next key.
    pub notice: Option<Reason>,
    /// Menus, questions, prompts and errors, drawn bottom to top. The top
    /// one takes the keys; closing it shows the one below again.
    pub popups: Vec<Popup>,
    /// How diffs are drawn. Kept in memory for the session: it outlives file
    /// selection, focus changes and reloads.
    pub diff_options: DiffOptions,
    /// How diff body rows draw emoji ZWJ sequences: resolved once at
    /// startup for the running terminal, and fixed for the session.
    pub emoji_zwj: EmojiZwj,
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
    /// The last recent-list request sent. Requests run in parallel and can
    /// answer out of order, so each carries its number.
    pub recent_seq: u64,
    /// The request `recent` answers; an answer older than it is dropped.
    pub recent_shown: u64,
    /// Repositories switched away from, by top level, to restore on return.
    pub parked: HashMap<PathBuf, ParkedRepo>,
    /// The custom command whose prompts are open.
    pub pending_command: Option<PendingCommand>,
    /// A custom command with captured output is running. One at a time,
    /// so their results cannot pile up.
    pub command_running: bool,
    /// The Branches panel and the selected branch's log.
    pub branches: BranchesPanel,
    /// The fetch, pull or push running now. It also holds `op_seq`.
    pub remote: Option<RemoteRun>,
    /// The history of `HEAD`, loaded at startup whether shown or not.
    pub commits: CommitsPanel,
    /// The installed git, read at startup; `None` when it could not be
    /// read, which counts as older than any feature needs.
    pub git_version: Option<GitVersion>,
    /// The stash list, loaded at startup whether shown or not.
    pub stash: StashList,
    /// What the stash being made takes, fixed when its menu item was
    /// chosen; taken when its message is submitted.
    pub stash_draft: Option<StashDraft>,
    /// Every git command run this session, recorded whether shown or not.
    pub command_log: CommandLog,
    /// The commits a paste cherry-picks. Kept across checkouts, so they
    /// can be copied on one branch and pasted on another.
    pub copied: CopiedCommits,
    /// A notice whose words are made at run time, such as a count. Shown
    /// and cleared like `notice`, which wins when both are set.
    pub notice_text: Option<String>,
    /// The filter or search being typed on the bottom bar, if one is.
    pub input: Option<ListInput>,
    /// Narrows the Files panel as typed; empty shows every file.
    pub files_filter: String,
    /// The file a range selection started on. By path, so a refresh that
    /// moves the rows keeps the range on the same files.
    pub files_anchor: Option<RepoPath>,
    /// Pull requests per branch; off unless the global config names hosts.
    pub forge: ForgeState,
    /// The global `[forge] gitlab` hosts: only URLs are built for them.
    pub forge_gitlab_hosts: Vec<ForgeHost>,
    /// The global `[forge] browser` argv; `None` leaves Open off.
    pub browser: Option<Vec<OsString>>,
    /// The pull request being put together, from its first picker to the
    /// question.
    pub pr_draft: Option<PrDraft>,
    /// The pull request being created on GitHub; one at a time.
    pub pr_create: Option<PrCreating>,
    /// The pull request being merged on GitHub; one at a time.
    pub pr_merge: Option<PrMerging>,
    /// The message read to copy it; any other answer is stale.
    pub pending_copy: Option<(Tag, CommitId)>,
    /// The pattern the open `.gitignore` prompt was filled with.
    pub ignore_draft: Option<IgnoreDraft>,
    /// The commit or local branch a comparison starts from.
    pub diff_mark: Option<DiffMark>,
    /// The comparison whose ends are being read; any other answer is stale.
    pub pending_diff: Option<PendingDiff>,
    /// Changes picked from one commit, to rewrite history with. Kept in
    /// memory only; never empty while set.
    pub custom_patch: Option<CustomPatch>,
    /// Review mode's queues and pull request.
    pub review: review::ReviewState,
    /// Seconds since the epoch, set by the loop before each update, so
    /// rate limits can be judged without a clock in the model.
    pub now: i64,
    /// The workspace has had its first load; a session started in review
    /// mode starts it on the way out.
    pub workspace_started: bool,
}

impl AppState {
    pub fn new(context: Context) -> Self {
        Self {
            review: review::ReviewState::for_context(&context),
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
            left_behind_check: None,
            commit_on_terminal: false,
            commit_hooks_detached: false,
            op_error: None,
            status_selection: Selection::KeepPath,
            help_open: false,
            help_scroll: 0,
            commit: CommitFlow::Closed,
            notice: None,
            popups: Vec::new(),
            diff_options: DiffOptions::default(),
            emoji_zwj: EmojiZwj::Escape,
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
            recent_seq: 0,
            recent_shown: 0,
            parked: HashMap::new(),
            pending_command: None,
            command_running: false,
            branches: BranchesPanel::default(),
            remote: None,
            commits: CommitsPanel::default(),
            git_version: None,
            stash: StashList::default(),
            stash_draft: None,
            command_log: CommandLog::default(),
            copied: CopiedCommits::default(),
            notice_text: None,
            input: None,
            diff_mark: None,
            pending_diff: None,
            custom_patch: None,
            files_filter: String::new(),
            files_anchor: None,
            forge: ForgeState::default(),
            forge_gitlab_hosts: Vec::new(),
            browser: None,
            pr_draft: None,
            pr_create: None,
            pr_merge: None,
            pending_copy: None,
            ignore_draft: None,
            now: 0,
            workspace_started: false,
        }
    }

    /// The git `main` found installed.
    pub fn with_git_version(self, git_version: Option<GitVersion>) -> Self {
        Self {
            git_version,
            ..self
        }
    }

    /// Whether git has `version` or newer. An unknown git has nothing.
    pub fn git_has(&self, version: GitVersion) -> bool {
        self.git_version.is_some_and(|found| found >= version)
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
                self.notice_text = None;
                let focus = self.key_focus();
                match keymap::resolve(keymap::registry(), &self.keymap, focus, self, key) {
                    Resolution::Run(binding) => return self.update(binding.intent.clone()),
                    Resolution::Command(index) => return self.start_command(index),
                    Resolution::Disabled(reason) => self.notice = Some(reason),
                    Resolution::Unbound if focus == KeyScope::Input => {
                        if let Some(input) = &mut self.input {
                            // A long paste would only slow every frame down.
                            let full = input.line.text.chars().count() >= list_filter::MAX_INPUT;
                            if full && matches!(key.code, crossterm::event::KeyCode::Char(_)) {
                                self.notice = Some(list_filter::input_full_notice().into());
                                return vec![];
                            }
                            input.line.edit(key);
                        }
                        return self.apply_input();
                    }
                    // Only a prompt or the editor takes text; elsewhere
                    // unbound keys are dropped. A popup over the editor
                    // keeps the keys from it.
                    Resolution::Unbound => match self.popups.last_mut() {
                        Some(Popup::Prompt { input, .. }) => input.edit(key),
                        Some(Popup::Credential(prompt)) if !prompt.confirm => {
                            prompt.input.edit(key)
                        }
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
                let mut effects = Vec::new();
                // Git is told no before it is stopped, so nothing waits on the user.
                if self.close_credential() {
                    effects.push(Effect::AnswerCredential(None));
                }
                // A hook or signing step may wait forever; quitting must not.
                if self.op_seq.is_some() {
                    effects.push(Effect::InterruptGit);
                }
                return effects;
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
                self.render_review_body();
                let mut effects = self.diff.demand(self.diff_rows());
                if self.pane_cols() != cols {
                    effects.extend(self.render_shown());
                }
                return effects;
            }
            AppIntent::RefreshStatus => {
                self.status_selection = Selection::KeepPath;
                let mut effects = self.refresh_status();
                effects.extend(self.reload_branches());
                effects.extend(self.reload_commits());
                effects.extend(self.reload_stashes());
                return effects;
            }
            AppIntent::StatusLoaded { tag, result } => return self.status_loaded(tag, result),
            AppIntent::SelectNext => return self.move_selection(1),
            AppIntent::SelectPrev => return self.move_selection(-1),
            AppIntent::ScrollDiff(by) if self.review_files_shown() => {
                return self.scroll_review_diff(by);
            }
            AppIntent::ScrollDiff(by) => {
                let rows = self.pane_rows();
                if self.focus == Focus::Branches {
                    self.branches.log.scroll(by, rows);
                } else if self.rendered_shown().is_some()
                    && let Some(rendered) = &mut self.rendered
                {
                    rendered.scroll(by, rows);
                } else if let Focus::Diff(view) = &mut self.focus {
                    view.scroll(by, rows);
                } else if let Focus::Conflict(view) = &mut self.focus {
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
            AppIntent::EnterLineMode => {
                if self.display_only() && matches!(self.focus, Focus::Diff(_)) {
                    self.notice = Some(keymap::DISPLAY_ONLY.into());
                } else {
                    self.with_lines(HunkView::enter_lines);
                }
            }
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
            // Only where the range shows; elsewhere Stage is the one file's.
            AppIntent::ToggleStage if self.focus == Focus::Files && self.files_anchor.is_some() => {
                return self.stage_range();
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
                Some(Popup::Prompt {
                    kind: PromptKind::NewBranch | PromptKind::RenameBranch | PromptKind::SetUpstream,
                    ..
                }) => self.branches.pending = None,
                Some(Popup::Prompt {
                    kind: PromptKind::StashMessage,
                    ..
                }) => self.stash_draft = None,
                Some(Popup::Prompt {
                    kind: PromptKind::RenameStash,
                    ..
                }) => self.stash.pending = None,
                Some(Popup::Prompt {
                    kind:
                        PromptKind::TrackBranch
                        | PromptKind::RemoteName
                        | PromptKind::RemoteUrl
                        | PromptKind::RenameRemote,
                    ..
                }) => self.branches.remotes.pending = None,
                Some(Popup::Prompt {
                    kind: PromptKind::WorktreePath,
                    ..
                }) => self.forget_worktree_branch(),
                Some(Popup::Prompt {
                    kind: PromptKind::TagName | PromptKind::TagMessage,
                    ..
                }) => self.branches.pending_tag = None,
                Some(Popup::Prompt {
                    kind: PromptKind::IgnorePattern,
                    ..
                }) => self.ignore_draft = None,
                // Closing a step of a pull request stops it.
                Some(Popup::PickList(PickList {
                    purpose: PickPurpose::CheckDetails,
                    ..
                })) => self.review.check_urls.clear(),
                Some(
                    Popup::PickList(_)
                    | Popup::Prompt {
                        kind: PromptKind::PullRequestTitle,
                        ..
                    },
                ) => self.pr_draft = None,
                _ => {}
            },
            AppIntent::CommandPromptAnswered(text) => return self.command_answered(text),
            AppIntent::CommandFinished {
                title,
                output,
                result,
            } => return self.command_finished(&title, output, result),
            AppIntent::CancelCommand => return self.cancel_command(),
            AppIntent::OpenInEditor => return self.open_in_editor(),
            AppIntent::EditHunk => return self.edit_hunk(),
            AppIntent::EditorClosed { result } => return self.editor_closed(result),
            AppIntent::HunkEdited { result } => return self.hunk_edited(result),
            AppIntent::Watch(event) => return self.watch(event),
            AppIntent::RefsLoaded { tag, result } => return self.refs_loaded(tag, result),
            AppIntent::FetchFinished { tag, result } => return self.fetch_finished(tag, result),
            AppIntent::CommitsLoaded { tag, skip, result } => {
                if tag.generation == self.generation {
                    self.commits.page_loaded(tag.seq, skip, result);
                    self.commits.reconcile();
                    return self.more_commits();
                }
            }
            AppIntent::MarkersLoaded { tag, result } => {
                if tag.generation == self.generation {
                    self.commits.markers_loaded(tag.seq, result);
                }
            }
            AppIntent::CommitNext => {
                self.commits.step(1);
                return self.more_commits();
            }
            AppIntent::CommitPrev => self.commits.step(-1),
            AppIntent::OpenCommitFiles => return self.open_commit_files(),
            AppIntent::CloseCommitFiles => return self.close_commit_files(),
            AppIntent::CommitFileNext => return self.move_commit_file(1),
            AppIntent::CommitFilePrev => return self.move_commit_file(-1),
            AppIntent::CommitFilesLoaded { tag, result } => {
                return self.commit_files_loaded(tag, result);
            }
            AppIntent::StashesLoaded { tag, result } => {
                if tag.generation == self.generation {
                    self.stash.loaded(tag.seq, result);
                }
            }
            AppIntent::StashNext => self.stash.move_by(1),
            AppIntent::StashPrev => self.stash.move_by(-1),
            AppIntent::OpenStashFiles => return self.open_stash_files(),
            AppIntent::CloseStashFiles => return self.close_stash_files(),
            AppIntent::StashFileNext => return self.move_stash_file(1),
            AppIntent::StashFilePrev => return self.move_stash_file(-1),
            AppIntent::StashFilesLoaded { tag, result } => {
                return self.stash_files_loaded(tag, result);
            }
            AppIntent::ApplyStash => return self.stash_op(GitOp::StashApply),
            AppIntent::PopStash => return self.stash_op(GitOp::StashPop),
            AppIntent::DropStash if self.stash.range_anchor.is_some() => {
                self.ask_delete_range(RangeKind::Stashes)
            }
            AppIntent::DropStash => self.ask_drop_stash(),
            AppIntent::ConfirmDropStash => return self.confirm_drop_stash(),
            AppIntent::RenameStash => self.ask_rename_stash(),
            AppIntent::RenameStashTo(text) => return self.rename_stash_to(&text),
            AppIntent::StashAll => self.ask_stash_message(StashMode::All),
            AppIntent::StashIncludingUntracked => {
                self.ask_stash_message(StashMode::IncludeUntracked)
            }
            AppIntent::StashStaged => self.ask_stash_message(StashMode::Staged),
            AppIntent::StashFile => self.ask_stash_file_message(),
            AppIntent::StashMessageAnswered(text) => return self.stash_message_answered(&text),
            AppIntent::OpenSubmodules => return self.open_submodules(),
            AppIntent::SubmodulesLoaded { tag, result } => self.submodules_loaded(tag, result),
            AppIntent::SubmoduleNext => self.move_submodule_cursor(1),
            AppIntent::SubmodulePrev => self.move_submodule_cursor(-1),
            AppIntent::InitSubmodule => return self.init_submodule(),
            AppIntent::UpdateSubmodule => return self.update_submodule(),
            AppIntent::EnterSubmodule => return self.enter_submodule(),
            AppIntent::LeaveSubmodule => return self.leave_submodule(),
            AppIntent::OpenWorktrees => return self.open_worktrees(),
            AppIntent::WorktreesLoaded { tag, result } => self.worktrees_loaded(tag, result),
            AppIntent::WorktreeNext => self.move_worktree_cursor(1),
            AppIntent::WorktreePrev => self.move_worktree_cursor(-1),
            AppIntent::OpenWorktree => return self.open_worktree(),
            AppIntent::AddWorktree => self.ask_worktree_path(),
            AppIntent::AddWorktreeAt(text) => return self.add_worktree_at(text),
            AppIntent::WorktreeAdded { tag, result } => return self.worktree_added(tag, result),
            AppIntent::RemoveWorktree => self.ask_remove_worktree(),
            AppIntent::ConfirmRemoveWorktree => return self.confirm_remove_worktree(),
            AppIntent::WorktreeRemoved { tag, result } => {
                return self.worktree_removed(tag, result);
            }
            AppIntent::CommandLogged(event) => self.command_logged(event),
            AppIntent::OpenCommandLog => self.open_command_log(),
            AppIntent::CommandLogNext => self.move_command_log(1),
            AppIntent::CommandLogPrev => self.move_command_log(-1),
            AppIntent::CommandLogPageDown => self.move_command_log(self.command_log_page()),
            AppIntent::CommandLogPageUp => self.move_command_log(-self.command_log_page()),
            AppIntent::OpenBlame => return self.open_blame(),
            AppIntent::BlameLoaded { tag, result } => self.blame_loaded(tag, result),
            AppIntent::BlameNext => self.with_blame(|view, rows| view.move_by(1, rows)),
            AppIntent::BlamePrev => self.with_blame(|view, rows| view.move_by(-1, rows)),
            AppIntent::BlamePageDown => self.with_blame(|view, rows| view.page(true, rows)),
            AppIntent::BlamePageUp => self.with_blame(|view, rows| view.page(false, rows)),
            AppIntent::OpenBlameCommit => return self.open_blame_commit(),
            AppIntent::CommitLoaded { tag, result } => self.commit_loaded(tag, result),
            AppIntent::CloseBlameCommit => self.close_blame_commit(),
            AppIntent::ScrollCommit(by) => self.scroll_commit(by),
            AppIntent::CloseBlame => return self.close_blame(),
            AppIntent::ConflictLoaded { tag, path, result } => {
                self.conflict_loaded(tag, path, result)
            }
            AppIntent::ConflictNext => self.with_conflict(|view, rows| view.move_by(1, rows)),
            AppIntent::ConflictPrev => self.with_conflict(|view, rows| view.move_by(-1, rows)),
            AppIntent::PickConflict(choice) => return self.pick_conflict(choice),
            AppIntent::ResolvedWritten { tag, path, result } => {
                return self.resolved_written(tag, path, result);
            }
            AppIntent::StageResolved => return self.stage_resolved(),
            AppIntent::OpenMergeTool => return self.open_merge_tool(),
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
            AppIntent::RecentLoaded { seq, list } => self.recent_loaded(seq, list),
            AppIntent::RecentNext => self.move_recent_cursor(1),
            AppIntent::RecentPrev => self.move_recent_cursor(-1),
            AppIntent::RecentSelect => return self.recent_select(),
            AppIntent::OpenRepo(path) => return self.open_repo(path),
            AppIntent::InitRepo => return self.init_repo(),
            AppIntent::RepoInitialised { tag, result } => {
                return self.repo_initialised(tag, result);
            }
            AppIntent::BranchesLoaded { tag, result } => return self.branches_loaded(tag, result),
            AppIntent::ForgeRepoNone { tag } => {
                if tag.generation == self.generation {
                    self.forge.repo_none(tag);
                }
            }
            AppIntent::PullRequestsCached { tag, repo, badges } => {
                if tag.generation == self.generation {
                    self.forge.cached(tag, repo, badges);
                }
            }
            AppIntent::PullRequestsFetched {
                tag,
                result,
                limited_until,
            } => {
                return self.pull_requests_fetched(tag, result, limited_until);
            }
            AppIntent::OpenForgeUrl => return self.open_forge_url(),
            AppIntent::CopyForgeUrl => return self.copy_forge_url(),
            AppIntent::RefreshPullRequests => return self.refresh_pull_requests(),
            AppIntent::OpenUrlFailed(message) => self.push_error(ErrorPopup::new(
                "Could not open the browser",
                &message,
                "Check forge.browser in the global config.",
            )),
            AppIntent::CreatePrStart => self.create_pr_start(),
            AppIntent::PickNext => self.move_pick_cursor(1),
            AppIntent::PickPrev => self.move_pick_cursor(-1),
            AppIntent::PickSelect => return self.pick_select(),
            AppIntent::PullRequestTitled(text) => self.pull_request_titled(&text),
            AppIntent::ConfirmCreatePullRequest => return self.confirm_create_pull_request(),
            AppIntent::PullRequestCreated { tag, result } => {
                return self.pull_request_created(tag, result);
            }
            AppIntent::EnterReview => return self.enter_review(),
            AppIntent::NextQueue => self.step_review_queue(false),
            AppIntent::PrevQueue => self.step_review_queue(true),
            AppIntent::OpenPr => return self.open_pr(),
            AppIntent::CloseReviewDetail => return self.close_review(),
            AppIntent::RefreshReview => return self.refresh_review(),
            AppIntent::OpenCheckDetails => return self.open_check_details(),
            AppIntent::OpenMergePr => self.open_merge_pr(),
            AppIntent::ConfirmMergePr => return self.confirm_merge_pr(),
            AppIntent::PullRequestMerged {
                tag,
                number,
                result,
            } => return self.pull_request_merged(tag, number, result),
            AppIntent::ReviewNext => return self.review_move(1),
            AppIntent::ReviewPrev => return self.review_move(-1),
            AppIntent::OpenReviewFiles => return self.open_review_files(),
            AppIntent::ToggleReviewFilesFocus => self.toggle_review_files_focus(),
            AppIntent::ReviewObjectsChecked { tag, result } => {
                return self.review_objects_checked(tag, result);
            }
            AppIntent::ReviewHeadFetched { tag, result } => {
                return self.review_head_fetched(tag, result);
            }
            AppIntent::ReviewFilesLoaded { tag, result } => {
                return self.review_files_loaded(tag, result);
            }
            AppIntent::ReviewQueuesLoaded { tag, repo, result } => {
                self.review_queues_loaded(tag, repo, result)
            }
            AppIntent::PrDetailLoaded {
                tag,
                number,
                repo,
                result,
            } => self.pr_detail_loaded(tag, number, repo, result),
            AppIntent::BranchLogLoaded {
                tag,
                refname,
                result,
            } => self.branch_log_loaded(tag, refname, result),
            AppIntent::RemotesLoaded { tag, result } => return self.remotes_loaded(tag, result),
            AppIntent::SwitchBranchesTab => return self.switch_branches_tab(BranchesTab::next),
            AppIntent::PrevBranchesTab => return self.switch_branches_tab(BranchesTab::prev),
            AppIntent::TagsLoaded { tag, result } => return self.tags_loaded(tag, result),
            AppIntent::TagNext => return self.move_tag(1),
            AppIntent::TagPrev => return self.move_tag(-1),
            AppIntent::NewTag => self.ask_tag_name(false),
            AppIntent::NewAnnotatedTag => self.ask_tag_name(true),
            AppIntent::TagNamed(text) => return self.tag_named(text),
            AppIntent::TagMessaged(text) => return self.tag_messaged(&text),
            AppIntent::DeleteTag if self.branches.tags.range_anchor.is_some() => {
                self.ask_delete_range(RangeKind::Tags)
            }
            AppIntent::DeleteTag => self.ask_delete_tag(),
            AppIntent::ConfirmDeleteTag => return self.confirm_delete_tag(),
            AppIntent::PushTag => return self.push_tag(),
            AppIntent::RemoteNext => return self.move_remote(1),
            AppIntent::RemotePrev => return self.move_remote(-1),
            AppIntent::TrackRemoteBranch => self.ask_track_name(),
            AppIntent::TrackRemoteBranchNamed(text) => return self.track_named(&text),
            AppIntent::AddRemote => self.ask_remote_name(),
            AppIntent::AddRemoteNamed(text) => self.remote_named(&text),
            AppIntent::AddRemoteUrl(text) => return self.remote_url(&text),
            AppIntent::RenameRemote => self.ask_remote_rename(),
            AppIntent::RenameRemoteTo(text) => return self.rename_remote_to(&text),
            AppIntent::RemoveRemote if self.branches.remotes.range_anchor.is_some() => {
                self.ask_delete_range(RangeKind::Remotes)
            }
            AppIntent::RemoveRemote => self.ask_remove_remote(),
            AppIntent::ConfirmRemoveRemote => return self.confirm_remove_remote(),
            AppIntent::ConfirmDeleteRange => return self.confirm_delete_range(),
            AppIntent::SetUpstream => self.ask_upstream(),
            AppIntent::SetUpstreamTo(text) => return self.set_upstream_to(&text),
            AppIntent::UnsetUpstream => {
                let op = self
                    .branch_for_action(keymap::unset_upstream_disabled)
                    .map(GitOp::UnsetUpstream);
                return self.start_op(Ok(op));
            }
            AppIntent::BranchNext => return self.move_branch(1),
            AppIntent::BranchPrev => return self.move_branch(-1),
            AppIntent::CheckoutBranch => return self.checkout_branch(),
            AppIntent::NewBranch => self.ask_branch_name(PromptKind::NewBranch),
            AppIntent::RenameBranch => self.ask_branch_name(PromptKind::RenameBranch),
            AppIntent::DeleteBranch if self.branches.range_anchor.is_some() => {
                self.ask_delete_range(RangeKind::Branches)
            }
            AppIntent::DeleteBranch => self.ask_delete_branch(),
            AppIntent::CreateBranchNamed(text) => {
                // A bisect may have begun while the prompt was open.
                if self.bisect_refuses() {
                    self.branches.pending = None;
                    return vec![];
                }
                return self
                    .named_branch_op(&text, |from, name| GitOp::CreateBranch { name, from });
            }
            AppIntent::RenameBranchTo(text) => {
                return self.named_branch_op(&text, |from, to| GitOp::RenameBranch { from, to });
            }
            AppIntent::ConfirmDeleteBranch { force } => return self.confirm_delete_branch(force),
            AppIntent::Fetch => return self.start_remote(RemoteOp::Fetch),
            AppIntent::Pull => return self.start_remote(RemoteOp::Pull),
            AppIntent::Push => {
                return self.start_remote(RemoteOp::Push {
                    force_with_lease: false,
                });
            }
            AppIntent::ForcePush => {
                return self.start_remote(RemoteOp::Push {
                    force_with_lease: true,
                });
            }
            AppIntent::CredentialAsked {
                tag,
                prompt,
                confirm,
            } => return self.credential_asked(tag, prompt, confirm),
            AppIntent::SubmitCredential => {
                if let Some(Popup::Credential(prompt)) = self.popups.last() {
                    let answer = prompt.input.secret();
                    return self.update(AppIntent::CredentialAnswered(Some(answer)));
                }
            }
            AppIntent::CredentialAnswered(answer) => return self.credential_answered(answer),
            AppIntent::RemoteNotice { tag, text } => self.remote_notice(tag, text),
            AppIntent::RemoteFinished { tag, op, result } => {
                return self.remote_finished(tag, op, result);
            }
            AppIntent::SpinnerTick => self.spinner_tick(),
            AppIntent::Merge(strategy) => {
                return self.branch_op(|branch| GitOp::Merge { branch, strategy });
            }
            AppIntent::RebaseOnto => return self.branch_op(|onto| GitOp::Rebase { onto }),
            AppIntent::ContinueOperation => {
                return self.resume(bisect::continue_disabled, GitOp::Continue);
            }
            AppIntent::SkipOperation if bisect::bisect_only(self) => {
                return self.update(AppIntent::BisectHead(BisectMark::Skip));
            }
            AppIntent::SkipOperation => return self.resume(keymap::skip_disabled, GitOp::Skip),
            AppIntent::AbortOperation if bisect::bisect_only(self) => self.ask_bisect_reset(),
            AppIntent::AbortOperation => self.ask_abort(),
            AppIntent::ConfirmAbort(operation) => {
                return self.start_op(Ok(Some(GitOp::Abort(operation))));
            }
            AppIntent::CheckoutCommit => return self.ask_checkout_commit(),
            AppIntent::ConfirmCheckoutCommit => {
                let op = self.commits.pending.take().map(GitOp::CheckoutCommit);
                if op.as_ref().is_some_and(|op| self.confirm_refused(op)) {
                    return vec![];
                }
                return self.start_op(Ok(op));
            }
            AppIntent::RevertCommit => return self.revert_commit(),
            AppIntent::CopyCommit if self.commits.range_anchor.is_some() => self.copy_range(),
            AppIntent::CopyCommit => self.copy_commit(),
            AppIntent::ClearCopied => {
                self.copied.clear();
                self.notice = Some("Cleared copied commits".into());
            }
            AppIntent::PasteCommits => return self.paste_commits(),
            AppIntent::CopyCommitText(what) => return self.copy_commit_text(what),
            AppIntent::CommitMessageLoaded { tag, oid, result } => {
                return self.commit_message_loaded(tag, oid, result);
            }
            AppIntent::CopyFailed(why) => {
                self.notice_text = None;
                self.push_error(ErrorPopup::new(
                    "Could not copy",
                    &why,
                    "Close this; nothing was copied.",
                ));
            }
            AppIntent::OpenIgnore => self.open_ignore(),
            AppIntent::IgnorePatternAnswered(text) => return self.ignore_answered(text),
            AppIntent::IgnoreWritten { tag, result } => return self.ignore_written(tag, result),
            AppIntent::ResetTo(mode) => return self.reset_to(mode),
            AppIntent::ConfirmResetHard => return self.confirm_reset_hard(),
            AppIntent::LeftBehindChecked {
                tag,
                op,
                head,
                result,
            } => return self.left_behind_checked(tag, op, head, result),
            AppIntent::ConfirmLeaveCommits => return self.confirm_leave_commits(),
            AppIntent::RewriteCommit(
                edit @ (rebase_plan::Edit::Squash
                | rebase_plan::Edit::Fixup
                | rebase_plan::Edit::Drop),
            ) if self.commits.range_anchor.is_some() => return self.rewrite_range(edit),
            AppIntent::RewriteCommit(edit) => return self.rewrite(Rewrite::Edit(edit)),
            AppIntent::ConfirmDropRange(ids) => return self.confirm_drop_range(ids),
            AppIntent::MoveCommit(direction) => return self.rewrite(Rewrite::Move(direction)),
            AppIntent::CreateFixup => return self.create_fixup(),
            AppIntent::Autosquash => self.ask_autosquash(),
            AppIntent::ConfirmAutosquash => return self.autosquash(),
            AppIntent::FindFixupBase => return self.find_fixup_base(),
            AppIntent::FixupBaseFound { tag, result } => self.fixup_base_found(tag, result),
            AppIntent::PatchToggleFile => self.patch_toggle_file(),
            AppIntent::OpenPatchMenu => {
                if self.context == Context::Workspace {
                    self.popups.push(Popup::Menu {
                        id: menu::MenuId::CustomPatch,
                        cursor: 0,
                    });
                }
            }
            AppIntent::ChoosePatchAction(choice) => self.choose_patch_action(choice),
            AppIntent::PatchCommitMessage(message) => self.patch_commit_message(message),
            AppIntent::ConfirmPatchAction(action) => return self.run_patch_action(action),
            AppIntent::ResetPatch => {
                if self.custom_patch.take().is_none() {
                    self.notice = Some(NO_PATCH.into());
                }
            }
            AppIntent::InsertBreak => return self.rewrite(Rewrite::Break),
            AppIntent::BranchTipsLoaded { tag, result } => {
                if tag.generation == self.generation {
                    self.commits.tips_loaded(tag.seq, result);
                }
            }
            AppIntent::RebaseTodoLoaded { tag, result } => self.rebase_todo_loaded(tag, result),
            AppIntent::RebaseTodoWritten { tag, result } => {
                return self.rebase_todo_written(tag, result);
            }
            AppIntent::BisectSelected(mark) => return self.bisect_selected(mark),
            AppIntent::BisectHead(mark) => return self.bisect_head(mark),
            AppIntent::ConfirmBisectReset => return self.bisect_reset(),
            AppIntent::SwitchCommitsTab => return self.switch_commits_tab(),
            AppIntent::ReflogNext => self.commits.reflog.move_by(1),
            AppIntent::ReflogPrev => self.commits.reflog.move_by(-1),
            AppIntent::ReflogLoaded { tag, result } => return self.reflog_loaded(tag, result),
            AppIntent::Undo => return self.travel(undo::Direction::Undo),
            AppIntent::Redo => return self.travel(undo::Direction::Redo),
            AppIntent::ConfirmTravel => return self.confirm_travel(),
            AppIntent::OpenFilter => self.open_filter(),
            AppIntent::OpenSearch if self.review_files_shown() => {
                self.input = Some(search_input(InputTarget::PreviewSearch, &self.diff.search));
            }
            AppIntent::OpenSearch => self.open_search(),
            AppIntent::OpenPreviewSearch => {
                self.input = Some(search_input(InputTarget::PreviewSearch, &self.diff.search));
            }
            AppIntent::InputAccept => return self.input_accept(),
            AppIntent::InputCancel => {
                if let Some(input) = &mut self.input {
                    input.line = LineInput::default();
                    let effects = self.apply_input();
                    self.input = None;
                    return effects;
                }
            }
            AppIntent::ToggleRange => self.toggle_range(),
            AppIntent::ListEscape => return self.list_escape(),
            AppIntent::DiffEscape if self.review_files_shown() => self.review_diff_escape(),
            AppIntent::DiffEscape => match &mut self.focus {
                Focus::Diff(view) if view.search.is_some() => view.search = None,
                _ => return self.update(AppIntent::FocusLeft),
            },
            AppIntent::PaneEscape => return self.pane_escape(),
            AppIntent::SearchNext => return self.step_search(false),
            AppIntent::SearchPrev => return self.step_search(true),
            AppIntent::OpenLogFilter(field) => self.open_log_filter(field),
            AppIntent::LogOfFile => return self.log_of_file(),
            AppIntent::ToggleDiffMark => self.toggle_diff_mark(),
            AppIntent::OpenDiff => return self.open_branch_diff(),
            AppIntent::DiffResolved { tag, result } => return self.diff_resolved(tag, result),
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
            let hint = if errors.iter().all(|e| e.dropped_binding) {
                "Kika dropped these entries and used the rest of the file. \
                 Fix it, then come back to Kika to reload it."
            } else {
                "Kika ignored this file. Fix it, then come back to Kika to reload it."
            };
            self.push_error(ErrorPopup::new("Config error", &text, hint));
            self.config_error = Some(text);
        }
    }

    /// Move the help list by a line or a page, never past its last screenful.
    /// A page is the popup's inside at its tallest, counted by the same
    /// function the view clamps with, so the last rows stay reachable.
    fn scroll_help(&mut self, by: Scroll) {
        let rows = layout::help_rows(ratatui::layout::Rect::new(0, 0, self.size.0, self.size.1));
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
            (Context::Workspace, Focus::Branches) => match self.branches.tab {
                BranchesTab::Local => KeyScope::Branches,
                BranchesTab::Remotes => KeyScope::Remotes,
                BranchesTab::Tags => KeyScope::Tags,
            },
            (Context::Workspace, Focus::Commits) if self.commits.tab == CommitsTab::Reflog => {
                KeyScope::Reflog
            }
            (Context::Workspace, Focus::Commits) if self.commits.open.is_some() => {
                KeyScope::CommitFiles
            }
            (Context::Workspace, Focus::Commits) => KeyScope::Commits,
            (Context::Workspace, Focus::Status) => KeyScope::Status,
            (Context::Workspace, Focus::Stash) if self.stash.open.is_some() => KeyScope::StashFiles,
            (Context::Workspace, Focus::Stash) => KeyScope::Stash,
            (Context::Workspace, Focus::Diff(view)) if view.lines.is_some() => KeyScope::Lines,
            (Context::Workspace, Focus::Diff(_)) => KeyScope::Diff,
            (Context::Workspace, Focus::Blame(view)) if view.commit.is_some() => {
                KeyScope::BlameCommit
            }
            (Context::Workspace, Focus::Blame(_)) => KeyScope::Blame,
            (Context::Workspace, Focus::Conflict(_)) => KeyScope::Conflict,
            (Context::Review { .. }, _) => match &self.review.files {
                Some(_) if review::review_diff_focused(self) => KeyScope::ReviewDiff,
                Some(_) if matches!(self.review.screen, review::ReviewScreen::Files(_)) => {
                    KeyScope::ReviewFiles
                }
                _ => KeyScope::Review,
            },
        }
    }

    /// Where key lookup starts: the top popup, else the commit popup, else
    /// the help menu while one is open.
    pub fn key_focus(&self) -> KeyScope {
        // The Worktrees popup's filter types on the bar below it.
        if matches!(self.popups.last(), Some(Popup::Worktrees(_)))
            && self
                .input
                .as_ref()
                .is_some_and(|i| i.target == InputTarget::WorktreesFilter)
        {
            return KeyScope::Input;
        }
        // So does the Command log's.
        if matches!(self.popups.last(), Some(Popup::CommandLog(_)))
            && self
                .input
                .as_ref()
                .is_some_and(|i| i.target == InputTarget::CommandLogFilter)
        {
            return KeyScope::Input;
        }
        match self.popups.last() {
            None => {}
            Some(Popup::Menu { .. }) => return KeyScope::Menu,
            // It reaches GitHub, so only `y` answers it.
            Some(Popup::Confirm(ConfirmKind::CreatePullRequest)) => {
                return KeyScope::CreatePullRequest;
            }
            Some(Popup::Confirm(ConfirmKind::Abort(_))) => return KeyScope::AbortConfirm,
            Some(Popup::Confirm(ConfirmKind::Autosquash)) => return KeyScope::AutosquashConfirm,
            Some(Popup::Confirm(ConfirmKind::CustomPatch(_))) => return KeyScope::PatchConfirm,
            Some(Popup::Confirm(ConfirmKind::DropRange(_))) => return KeyScope::DropRangeConfirm,
            Some(Popup::Confirm(_)) => return KeyScope::Confirm,
            Some(Popup::Prompt { .. }) => return KeyScope::Prompt,
            Some(Popup::Error(_)) => return KeyScope::Error,
            Some(Popup::Discard(_)) => return KeyScope::DiscardConfirm,
            Some(Popup::DeleteBranch(_)) => return KeyScope::DeleteBranch,
            Some(Popup::RemoveRemote(_)) => return KeyScope::RemoveRemote,
            Some(Popup::DeleteTag(_)) => return KeyScope::DeleteTag,
            Some(Popup::DeleteRange(_)) => return KeyScope::DeleteRange,
            Some(Popup::ResetHard(_)) => return KeyScope::ResetHard,
            Some(Popup::LeaveCommits(_)) => return KeyScope::LeaveCommits,
            Some(Popup::DropStash(_)) => return KeyScope::DropStash,
            Some(Popup::Submodules(_)) => return KeyScope::Submodules,
            Some(Popup::Worktrees(_)) => return KeyScope::Worktrees,
            Some(Popup::RemoveWorktree(_)) => return KeyScope::RemoveWorktree,
            Some(Popup::Recent(_)) => return KeyScope::Recent,
            Some(Popup::PickList(_)) => return KeyScope::PickList,
            // Ssh's yes/no kind is answered like any other question.
            Some(Popup::Credential(prompt)) if prompt.confirm => return KeyScope::Confirm,
            Some(Popup::Credential(_)) => return KeyScope::Credential,
            Some(Popup::Travel(_)) => return KeyScope::Travel,
            Some(Popup::CommandLog(_)) => return KeyScope::CommandLog,
            Some(Popup::MergePullRequest(_)) => return KeyScope::MergePullRequest,
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
        } else if self.input.is_some() {
            KeyScope::Input
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

    /// Whether the active renderer is a tool, whose rows show no patch
    /// lines: the Diff pane can be read but not staged from.
    pub fn display_only(&self) -> bool {
        self.active_renderer()
            .is_some_and(|r| r.kind == RendererKind::Tool)
    }

    /// The diff the Diff pane shows: the selected file's from the Files
    /// panel, the focused view's side with the Diff pane focused (renames
    /// off, as its patch has them), nothing behind a blame or a conflict.
    /// A commit's or a stash's file is never rendered, so it is not one of
    /// these.
    fn shown_request(&self) -> Option<DiffRequest> {
        match &self.focus {
            Focus::Commits if self.commits.open.is_some() => None,
            Focus::Stash if self.stash.open.is_some() => None,
            Focus::Status | Focus::Files | Focus::Branches | Focus::Commits | Focus::Stash => self
                .selected
                .and_then(|i| self.files.get(i))
                .map(|entry| DiffRequest::for_entry(entry, self.diff_options)),
            // Like the commit's file in the Commits panel, never rendered.
            Focus::Diff(view) if view.commit.is_some() => None,
            Focus::Diff(view) => Some(DiffRequest {
                path: view.path.clone(),
                orig_path: None,
                side: view.side,
                options: self.diff_options,
                commit: None,
            }),
            Focus::Blame(_) | Focus::Conflict(_) => None,
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
    /// renderer did not draw would lie, so the built-in view is drawn. A
    /// tool's rows have no cursor to lie with, so they are always drawn.
    pub fn rendered_shown(&self) -> Option<&RenderedDiff> {
        let rendered = self.rendered_for_shown()?;
        if self.display_only() {
            return Some(rendered);
        }
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
            self.notice = Some(external_diff::NO_RENDERER.into());
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
    pub(crate) fn render_if_stale(&mut self) -> Vec<Effect> {
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
    /// A tool's rows are never mapped: they show the files, not the patch.
    fn remap(&mut self) {
        let display_only = self.display_only();
        let Some(rendered) = &mut self.rendered else {
            return;
        };
        if display_only {
            rendered.map.clear();
            return;
        }
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
            // Only `ConfirmCreatePullRequest` creates; yes here keeps the
            // question, no closes it and drops the draft.
            Some(popup @ Popup::Confirm(ConfirmKind::CreatePullRequest)) => {
                if yes {
                    self.popups.push(popup);
                } else {
                    self.pr_draft = None;
                }
                vec![]
            }
            Some(Popup::Confirm(kind)) => {
                if kind == ConfirmKind::DetachedCheckout && !yes {
                    self.commits.pending = None;
                }
                let answer = if yes { Some(kind.yes()) } else { kind.no() };
                answer.map_or_else(Vec::new, |intent| self.update(intent))
            }
            Some(Popup::Credential(prompt)) if prompt.confirm => {
                // Ssh reads an exit of 0 with "yes" as allowed.
                vec![Effect::AnswerCredential(yes.then(|| Secret::new("yes")))]
            }
            Some(Popup::Discard(confirm)) if yes => {
                // The range was what was asked about; it is done with.
                if matches!(confirm.what, DiscardWhat::Range { .. }) {
                    self.files_anchor = None;
                }
                self.start_op(Ok(Some(confirm.op)))
            }
            Some(Popup::Discard(_)) => vec![],
            // Only `ConfirmDeleteBranch` deletes, only `ConfirmResetHard`
            // resets, only `ConfirmLeaveCommits` leaves commits behind, only
            // `ConfirmRemoveRemote` removes, only `ConfirmDeleteTag` deletes
            // a tag, only `ConfirmDeleteRange` deletes a range, only
            // `ConfirmTravel` undoes or redoes and only `ConfirmDropStash`
            // drops a stash; yes here keeps the question, no closes it.
            Some(
                Popup::DeleteBranch(_)
                | Popup::ResetHard(_)
                | Popup::LeaveCommits(_)
                | Popup::RemoveRemote(_)
                | Popup::DeleteTag(_)
                | Popup::DeleteRange(_)
                | Popup::Travel(_)
                | Popup::DropStash(_),
            ) if !yes => vec![],
            // Only `ConfirmRemoveWorktree` removes a worktree.
            Some(Popup::RemoveWorktree(_)) if !yes => vec![],
            // Only `ConfirmMergePr` merges a pull request.
            Some(Popup::MergePullRequest(_)) if !yes => vec![],
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
        if matches!(&self.focus, Focus::Diff(view) if view.commit.is_some()) {
            self.notice = Some(keymap::NOT_IN_COMMIT.into());
            return;
        }
        if self.focus == Focus::Files && self.files_anchor.is_some() {
            return self.ask_discard_range();
        }
        match self.discard_confirm() {
            Ok(Some(confirm)) => self.popups.push(Popup::Discard(confirm)),
            Ok(None) => {}
            Err(reason) => self.notice = Some(reason.into()),
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
        if self.display_only() {
            return Err(keymap::DISPLAY_ONLY);
        }
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
            self.notice = Some("The file is deleted".into());
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
        if view.commit.is_some() {
            self.notice = Some(keymap::NOT_IN_COMMIT.into());
            return vec![];
        }
        if self.display_only() {
            self.notice = Some(keymap::DISPLAY_ONLY.into());
            return vec![];
        }
        if view.lines.is_some() {
            return vec![];
        }
        if self.diff_options.ignore_whitespace {
            self.notice = Some(keymap::WHITESPACE_HIDDEN.into());
            return vec![];
        }
        if view.side == DiffSide::Index {
            self.notice = Some("Edit hunks from the unstaged view".into());
            return vec![];
        }
        let Some(file_patch) = &view.patch else {
            return vec![];
        };
        if file_patch.kind != FileKind::Modified {
            self.notice = Some("Only hunks of changed files can be edited".into());
            return vec![];
        }
        if file_patch.hunks.is_empty() {
            self.notice = Some(NO_CHANGES.into());
            return vec![];
        }
        match patch::build(
            file_patch,
            &PatchSelection::Hunk(view.hunk),
            Direction::Stage,
        ) {
            Ok(built) => vec![Effect::HandOff(HandOff::edit_hunk(&built))],
            Err(error) => {
                self.notice = Some(error.message().into());
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
                self.notice = Some("Edit cancelled".into());
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
                self.notice = Some("No file matches".into());
                vec![]
            }
        }
    }

    /// The title of a prompt: a command's comes from the user's config,
    /// made one line and safe to draw.
    pub fn prompt_title(&self, kind: PromptKind) -> String {
        if let (PromptKind::RenameStash, Some(stash)) = (kind, &self.stash.pending) {
            return format!("Rename {}", stash.arg());
        }
        if kind == PromptKind::WorktreePath
            && let Some(branch) = self.worktree_list().and_then(|l| l.pending_branch.as_ref())
        {
            return format!("New worktree for {}", branch.display());
        }
        if let Some(name) = &self.branches.pending {
            match kind {
                PromptKind::NewBranch => return format!("New branch from {}", name.display()),
                PromptKind::RenameBranch => return format!("Rename {}", name.display()),
                PromptKind::SetUpstream => return format!("Upstream of {}", name.display()),
                _ => {}
            }
        }
        match (kind, &self.branches.pending_tag) {
            (PromptKind::TagName, Some(pending)) => {
                let short = self.commits.short_oid(&pending.target);
                return format!("New tag on {short}");
            }
            (
                PromptKind::TagMessage,
                Some(PendingTag {
                    name: Some(name), ..
                }),
            ) => {
                return format!("Message of tag {}", sanitize(name.as_bytes()));
            }
            _ => {}
        }
        match (kind, &self.branches.remotes.pending) {
            (PromptKind::TrackBranch, Some(RemotePending::Track(from))) => {
                return format!("Check out {} as", from.display());
            }
            (PromptKind::RenameRemote, Some(RemotePending::Rename(name))) => {
                return format!("Rename remote {}", name.display());
            }
            (PromptKind::RemoteUrl, Some(RemotePending::AddUrl { name })) => {
                return format!("URL of {}", sanitize(name.as_bytes()));
            }
            _ => {}
        }
        let pending = self
            .pending_command
            .as_ref()
            .filter(|_| kind == PromptKind::Command);
        pending
            .and_then(|p| p.command.prompts.get(p.answers.len()))
            .map_or_else(|| kind.title().to_string(), |t| sanitize(t.as_bytes()))
    }

    /// A yes/no question's text. The detached checkout names the commit
    /// it was asked about.
    pub fn confirm_question(&self, kind: &ConfirmKind) -> Cow<'static, str> {
        match (kind, &self.commits.pending) {
            (ConfirmKind::DetachedCheckout, Some(id)) => {
                let short = self.commits.short_oid(id);
                Cow::Owned(format!(
                    "Check out {short} as a detached HEAD? You will be on no branch; \
                     new commits there belong to no branch until you create one. (y/n)"
                ))
            }
            (ConfirmKind::Abort(Operation::Bisect), _) => {
                Cow::Owned(bisect::reset_question(bisect::bisect_state(self)))
            }
            (ConfirmKind::CreatePullRequest, _) => match pull_requests::question(self) {
                Some(question) => Cow::Owned(question),
                None => Cow::Borrowed(kind.question()),
            },
            (ConfirmKind::CustomPatch(action), _) => match self.patch_question(action) {
                Some(question) => Cow::Owned(question),
                None => Cow::Borrowed(kind.question()),
            },
            // Only the count: subjects and the filter are not drawn here.
            (ConfirmKind::DropRange(ids), _) => Cow::Owned(format!(
                "Drop {} commits and their changes from the history? (y/n)",
                ids.len()
            )),
            _ => Cow::Borrowed(kind.question()),
        }
    }

    /// Show an error, unless the same one is already on top: a repeated
    /// failure should not need dismissing twice.
    pub(crate) fn push_error(&mut self, error: ErrorPopup) {
        if self.popups.last() != Some(&Popup::Error(error.clone())) {
            self.popups.push(Popup::Error(error));
        }
    }

    pub(crate) fn take_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    /// Move by `delta` rows among those the filter shows.
    fn move_selection(&mut self, delta: isize) -> Vec<Effect> {
        let Some(current) = self.selected else {
            return vec![];
        };
        let Some(next) = list_filter::step(&self.files_visible(), current, delta) else {
            return vec![];
        };
        if next == current {
            return vec![];
        }
        self.select(next)
    }

    /// Select file `index` and start loading its diff. With the Diff pane
    /// focused, its patch reloads too, on the same side while the file is
    /// the same one.
    ///
    /// While a commit's or a stash's files are open the pane is theirs:
    /// the selection moves, but nothing loads.
    pub(crate) fn select(&mut self, index: usize) -> Vec<Effect> {
        self.selected = Some(index);
        // The pane shows another diff; leaving it streams this one.
        if self.files_open() || self.review_files_shown() {
            return vec![];
        }
        let request = DiffRequest::for_entry(&self.files[index], self.diff_options);
        let mut effects = self.stream_diff(request.clone());
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

    /// Start streaming `request` into the Diff pane.
    pub(crate) fn stream_diff(&mut self, request: DiffRequest) -> Vec<Effect> {
        let seq = self.take_seq();
        let want = self.diff.begin(seq, &request.path, self.diff_rows());
        // Diff streams report many times, so they stay out of the busy count.
        let tag = Tag {
            generation: self.generation,
            seq,
            user: false,
        };
        vec![
            Effect::LoadDiff { tag, request, want },
            Effect::DiffLoadingDelay { tag },
        ]
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
        if self.commits.open.is_some() {
            return self.commit_file_diff();
        }
        if self.stash.open.is_some() {
            return self.stash_file_diff();
        }
        match self.selected {
            Some(index) if index < self.files.len() => self.select(index),
            _ => vec![],
        }
    }

    /// Focus `panel`. A side panel expands, and closes a blame on the way.
    /// The diff keeps its hunk view when it already has focus. Leaving the
    /// Commits or Stash panel closes a commit's or a stash's files, so the
    /// pane never shows their diff under another panel.
    pub(crate) fn focus_panel(&mut self, panel: Panel) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        match panel {
            Panel::Side(side) => {
                let mut effects = if side == SidePanel::Commits {
                    vec![]
                } else {
                    self.close_commit_files()
                };
                if side != SidePanel::Stash {
                    effects.extend(self.close_stash_files());
                }
                self.focus = Focus::side(side);
                // Another panel has no keys to step or clear the preview
                // search, so it would stay marked with no way out.
                if side != self.expanded {
                    self.diff.search = None;
                }
                self.expanded = side;
                // Leaving the diff or a blame, the pane shows the selected file.
                effects.extend(self.render_if_stale());
                effects
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

    /// Focus the Diff pane on the selected file, on the side its diff
    /// shows; a conflicted file opens the conflict view instead. Not while
    /// a commit's or a stash's file is shown: it has no hunks to stage.
    fn focus_diff(&mut self) -> Vec<Effect> {
        if self.focus == Focus::Commits && self.commits.open.is_some() {
            return self.focus_commit_diff();
        }
        if self.files_open() {
            return vec![];
        }
        let Some(entry) = self.selected.and_then(|i| self.files.get(i)) else {
            return vec![];
        };
        if entry.kind == EntryKind::Conflicted {
            return self.open_conflict();
        }
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
            commit: view.commit.clone(),
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
        if let Focus::Diff(view) = &self.focus
            && view.commit.is_some()
        {
            let lines = view.hunk_lines();
            self.patch_toggle_lines(lines);
            return vec![];
        }
        match &self.focus {
            Focus::Diff(view) => self.toggle_selection_stage(PatchSelection::Hunk(view.hunk)),
            _ => vec![],
        }
    }

    fn toggle_lines_stage(&mut self) -> Vec<Effect> {
        if let Focus::Diff(view) = &self.focus
            && view.commit.is_some()
        {
            let lines = view.selected_lines();
            self.patch_toggle_lines(lines);
            return vec![];
        }
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
        if self.display_only() {
            self.notice = Some(keymap::DISPLAY_ONLY.into());
            return vec![];
        }
        if self.diff_options.ignore_whitespace {
            self.notice = Some(keymap::WHITESPACE_HIDDEN.into());
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
                self.notice = Some(error.message().into());
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
        if view.commit.is_some() {
            self.notice = Some(keymap::NOT_IN_COMMIT.into());
            return vec![];
        }
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
            if self.files_open() || self.review_files_shown() {
                return vec![];
            }
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
        // A row the filter hides gives way to the first one it shows.
        let visible = self.files_visible();
        let index = match visible.first() {
            Some(&first) if !visible.contains(&index) => first,
            _ => index,
        };
        self.select(index)
    }

    fn diff_loaded(&mut self, tag: Tag, result: Result<DiffChunk, String>) {
        if tag.generation == self.generation {
            self.diff.apply(tag.seq, result);
        }
    }

    /// Run the operation a stage key chose. Only one runs at a time, and
    /// only on the local workspace.
    pub(crate) fn start_op(&mut self, op: Result<Option<GitOp>, String>) -> Vec<Effect> {
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

    /// Whether a bisect in progress refuses an answer that would move
    /// `HEAD` or make a commit, saying why in the bar.
    pub(crate) fn bisect_refuses(&mut self) -> bool {
        let reason = crate::bisect::head_move_disabled(self);
        let refused = reason.is_some();
        if refused {
            self.notice = reason;
        }
        refused
    }

    /// Open the commit popup: straight to the editor when something is
    /// staged, else ask whether to stage everything first.
    fn open_commit(&mut self) {
        if self.context != Context::Workspace || self.commit != CommitFlow::Closed {
            return;
        }
        if let Some(reason) = keymap::commit_disabled(self) {
            self.notice = Some(reason);
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
        if let Some(reason) = keymap::amend_disabled(self) {
            self.notice = Some(reason);
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
            self.notice = Some("Write a subject first".into());
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
        // A bisect may have begun while the editor was open; the text stays.
        if self.bisect_refuses() {
            return vec![];
        }
        let op = GitOp::Commit {
            message,
            stage_all: editor.stage_all,
            amend: editor.amend.is_some(),
            no_verify: editor.no_verify,
            fixup: None,
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
        if needs_terminal(&op, facts.as_ref(), self.commit_hooks_detached) {
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
                self.notice = Some("Empty message; commit aborted".into());
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
        // A fixup is made from the Commits panel, not the commit popup, so
        // its failure shows as any other op's.
        let commit = matches!(op, GitOp::Commit { fixup: None, .. });
        if matches!(op, GitOp::Commit { fixup: Some(_), .. }) {
            self.commit_on_terminal = false;
        }
        if tag.generation != self.generation {
            // The result is for another repository; give the message back.
            if commit {
                self.end_commit(None);
            }
            return vec![];
        }
        // The patch is in history now; a failure keeps it to try again.
        let rewritten = matches!(op, GitOp::CustomPatch { .. }) && result.is_ok();
        // The resolved file is staged: nothing is left for the view.
        if let (Focus::Conflict(view), GitOp::Stage(paths), Ok(())) = (&self.focus, &op, &result)
            && paths == std::slice::from_ref(&view.path)
        {
            self.focus = Focus::Files;
            self.expanded = SidePanel::Files;
        }
        if commit {
            // A commit's failure shows in its own popup, not the status bar.
            self.end_commit(Some(result));
        } else {
            // A failed git command gets a popup; `op_error` is for refusals.
            self.op_error = None;
            match result {
                // Git stops before committing a squash, so say what is left.
                Ok(())
                    if matches!(
                        op,
                        GitOp::Merge {
                            strategy: MergeStrategy::Squash,
                            ..
                        }
                    ) =>
                {
                    self.notice = Some("Squashed changes are staged; commit them".into());
                }
                Ok(()) => {}
                Err(message) => {
                    let error = if matches!(op, GitOp::ApplyEdited { .. }) {
                        self.edited_hunk_error(&message)
                    } else {
                        ErrorPopup::new(failure_title(&op), &message, failure_next(&op))
                    };
                    self.push_error(error);
                }
            }
        }
        let mut effects = if rewritten {
            self.patch_rewritten()
        } else {
            vec![]
        };
        // Refresh even after a failure: a failed `add` of several paths can
        // still have changed the index.
        effects.extend(self.apply_plan(refresh::plan(&op)));
        effects
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
    pub(crate) fn apply_plan(&mut self, plan: RefreshPlan) -> Vec<Effect> {
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
        if plan.scopes.contains(&Scope::Branches) {
            effects.extend(self.reload_branches());
        }
        if plan.scopes.contains(&Scope::Commits) {
            effects.extend(self.reload_commits());
        }
        if plan.scopes.contains(&Scope::Stash) {
            effects.extend(self.reload_stashes());
        }
        effects
    }

    /// Load the log from the top, and the markers and branch tips with it. What is shown
    /// stays until the new page lands, which supersedes any page in flight.
    pub(crate) fn reload_commits(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        let page = self.first_commits_page();
        let markers = self.background_tag();
        let tips = self.background_tag();
        self.commits.markers_seq = Some(markers.seq);
        self.commits.tips_seq = Some(tips.seq);
        let mut effects = vec![
            page,
            Effect::LoadMarkers { tag: markers },
            Effect::LoadBranchTips { tag: tips },
        ];
        // The reflog moves with the log, once it has been read at all.
        let reflog = &self.commits.reflog;
        if reflog.data.is_some() || reflog.error.is_some() {
            effects.extend(self.load_reflog(false));
        }
        effects
    }

    /// Load page 0 of the log under the current query, superseding any
    /// page in flight.
    fn first_commits_page(&mut self) -> Effect {
        let tag = self.background_tag();
        self.commits.begin_page(tag.seq, 0);
        Effect::LoadCommits {
            tag,
            tip: None,
            skip: 0,
            query: self.commits.query.clone(),
        }
    }

    /// Show the Commits panel's other tab, reading the reflog the first
    /// time it is shown.
    fn switch_commits_tab(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        self.commits.tab = self.commits.tab.other();
        let reflog = &self.commits.reflog;
        let unread = reflog.data.is_none() && reflog.error.is_none() && reflog.seq.is_none();
        if self.commits.tab == CommitsTab::Reflog && unread {
            self.load_reflog(true)
        } else {
            vec![]
        }
    }

    /// `HEAD` is on a branch with no commits, so it has no reflog yet.
    fn head_unborn(&self) -> bool {
        self.branch
            .as_ref()
            .is_some_and(|b| matches!(b.head, crate::git::status::Head::Unborn(_)))
    }

    /// Start reading the reflog; `user` as for the branch list. With no
    /// commits there is nothing to read: the tab shows an empty list.
    fn load_reflog(&mut self, user: bool) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if self.head_unborn() {
            let reflog = &mut self.commits.reflog;
            reflog.data = Some(Reflog::default());
            reflog.error = None;
            reflog.seq = None;
            reflog.selected = 0;
            return vec![];
        }
        let seq = self.take_seq();
        self.commits.reflog.seq = Some(seq);
        if user {
            self.busy += 1;
        }
        vec![Effect::LoadReflog {
            tag: Tag {
                generation: self.generation,
                seq,
                user,
            },
        }]
    }

    /// Show the reflog read, and plan the undo or redo waiting for it. A
    /// failed read plans nothing: the list shown may be older than the
    /// key press.
    fn reflog_loaded(&mut self, tag: Tag, result: Result<Reflog, String>) -> Vec<Effect> {
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        if tag.generation != self.generation {
            return vec![];
        }
        let failed = result.is_err();
        if !self.commits.reflog.loaded(tag.seq, result) {
            return vec![];
        }
        let Some(direction) = self.commits.reflog.planning.take() else {
            return vec![];
        };
        if failed {
            self.op_error = self.commits.reflog.error.clone();
            return vec![];
        }
        self.plan_travel(direction);
        vec![]
    }

    /// Undo or redo: read the reflog afresh, then plan from it. Nothing
    /// moves until the question that follows is answered.
    fn travel(&mut self, direction: undo::Direction) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if let Some(reason) = keymap::undo_disabled(self) {
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
        self.op_error = None;
        self.commits.reflog.planning = Some(direction);
        self.load_reflog(true)
    }

    /// Ask about the plan for `direction` from the reflog just read, or
    /// say why there is none. The state may have changed while it was
    /// read, so the refusals are checked again.
    fn plan_travel(&mut self, direction: undo::Direction) {
        if let Some(reason) = keymap::undo_disabled(self) {
            self.notice = Some(reason);
            return;
        }
        let Some(reflog) = &self.commits.reflog.data else {
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
            branches: &self.branches.branches,
            dirty: self.files.iter().any(|f| f.kind != EntryKind::Untracked),
        };
        match undo::plan(direction, reflog, &ctx) {
            Ok(plan) => self.popups.push(Popup::Travel(TravelConfirm {
                title: plan.direction.title(),
                preview: plan.preview,
                op: GitOp::Travel {
                    direction: plan.direction,
                    step: plan.step,
                    expect: plan.expect,
                },
            })),
            Err(message) => self.op_error = Some(message),
        }
    }

    /// Undo or redo as the open question said, and only if one is open.
    fn confirm_travel(&mut self) -> Vec<Effect> {
        if !matches!(self.popups.last(), Some(Popup::Travel(_))) {
            return vec![];
        }
        let Some(Popup::Travel(confirm)) = self.popups.pop() else {
            return vec![];
        };
        self.start_op(Ok(Some(confirm.op)))
    }

    /// Load the next page when the cursor nears the end of the log.
    fn more_commits(&mut self) -> Vec<Effect> {
        let Some(skip) = self.commits.wants_more() else {
            return vec![];
        };
        let tag = self.background_tag();
        self.commits.begin_page(tag.seq, skip);
        vec![Effect::LoadCommits {
            tag,
            tip: self.commits.tip.clone(),
            skip,
            query: self.commits.query.clone(),
        }]
    }

    /// The Branches panel's selected branch, when its Local tab shows.
    pub(crate) fn local_branch_selected(&self) -> Option<&Branch> {
        (self.branches.tab == BranchesTab::Local)
            .then(|| self.branches.selected_branch())
            .flatten()
    }

    /// A tag counted as busy until its answer lands.
    pub(crate) fn user_tag(&mut self, seq: u64) -> Tag {
        self.busy += 1;
        Tag {
            generation: self.generation,
            seq,
            user: true,
        }
    }

    /// Point git at `root` and the submodules entered from it.
    pub(crate) fn set_repo(&self, root: &Path) -> Effect {
        Effect::SetRepo {
            root: root.to_path_buf(),
            nested: self.nested(),
        }
    }

    /// Record the repository Kika started in and load it, or welcome the
    /// user with the recent list and the offer to make one here.
    pub(crate) fn start(&mut self) -> Vec<Effect> {
        if let Context::Review { number } = self.context {
            return self.start_review(number);
        }
        self.workspace_started = true;
        match self.repo.clone() {
            Some(path) => {
                let mut effects = vec![self.record_recent(path)];
                effects.extend(self.refresh_status());
                effects.extend(self.reload_branches());
                effects.extend(self.reload_commits());
                effects.extend(self.reload_stashes());
                effects
            }
            None => self.open_recent(),
        }
    }

    /// Forget what belonged to the repository being left: results still in
    /// flight for it are dropped by the new generation.
    pub(crate) fn switch_repo(&mut self) {
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
        self.branches = BranchesPanel::default();
        self.commits = CommitsPanel::default();
        self.stash = StashList::default();
        self.stash_draft = None;
        // The ids name commits of the repository being left.
        self.copied.clear();
        // A filter and a range were made over the old repository's rows.
        self.input = None;
        self.files_filter.clear();
        self.files_anchor = None;
        // A mark names a commit or branch of the repository being left.
        self.diff_mark = None;
        self.pending_diff = None;
        // The picks name a commit of the repository being left.
        self.custom_patch = None;
        // The refs seen belong to the repository being left; the next
        // snapshot is the new one's baseline, not a change.
        self.watch.refs = None;
        self.forge.reset();
        // A pull request in the making names the old repository's branches.
        self.pr_draft = None;
        self.pr_create = None;
    }

    pub(crate) fn refresh_status(&mut self) -> Vec<Effect> {
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
                let old_range = self.files_range_paths();
                let had_range = !old_range.is_empty();
                self.files = status.files;
                let rebasing = status.branch.in_progress.contains(&Operation::Rebase);
                if let Some(anchor) = &self.files_anchor
                    && !self.files.iter().any(|f| &f.path == anchor)
                {
                    self.files_anchor = None;
                }
                let before = self.branch.replace(status.branch);
                self.announce_bisect(before.and_then(|b| b.bisect));
                self.status_error = None;
                let mut effects = match self.status_selection {
                    Selection::KeepPath => self.reselect(old_path),
                };
                // A range whose files changed is not the set the user
                // marked, so nothing may act on it.
                // That includes a range whose first file went, cleared above.
                if had_range && self.files_range_paths() != old_range {
                    self.files_anchor = None;
                    self.notice = Some(RANGE_FILES_CHANGED.into());
                }
                // The todo list shows while a rebase is stopped, and only then.
                if rebasing && self.context == Context::Workspace {
                    effects.extend(self.reload_rebase_todo());
                } else {
                    self.commits.todo_seq = None;
                    self.commits.set_todo(None);
                }
                effects
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

/// A commit opened to its files from the Commits panel. The Diff pane
/// shows the selected file's change in that commit until it closes.
impl AppState {
    /// Open the selected commit's files in place of the log. A user
    /// effect, so the busy indicator shows while git lists them.
    fn open_commit_files(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace
            || self.focus != Focus::Commits
            || self.commits.open.is_some()
        {
            return vec![];
        }
        let Some(row) = self.commits.rows.get(self.commits.selected) else {
            return vec![];
        };
        if let Some(mark) = &self.diff_mark
            && let Some(id) = CommitId::parse(&row.oid)
            && !matches!(mark, DiffMark::Commit { id: marked, .. } if *marked == id)
        {
            let target = row.short_oid.clone();
            return self.open_comparison(DiffEnd::Commit(id), target);
        }
        let range = match CommitRange::of(row) {
            Ok(range) => range,
            Err(message) => {
                self.commits.error = Some(message);
                return vec![];
            }
        };
        let short_oid = row.short_oid.clone();
        let seq = self.take_seq();
        self.commits.open = Some(CommitFilesList::begin(range.clone(), short_oid, seq));
        self.busy += 1;
        let tag = Tag {
            generation: self.generation,
            seq,
            user: true,
        };
        vec![Effect::LoadCommitFiles { tag, range }]
    }

    /// Show the first file's diff, or nothing for a commit with no files.
    fn commit_files_loaded(
        &mut self,
        tag: Tag,
        result: Result<Vec<CommitFile>, String>,
    ) -> Vec<Effect> {
        // Every user effect reports back once, stale or not.
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        if tag.generation != self.generation {
            return vec![];
        }
        let Some(open) = &mut self.commits.open else {
            return vec![];
        };
        if !open.loaded(tag.seq, result) {
            return vec![];
        }
        if open.selected_file().is_none() {
            self.diff = DiffPane::default();
            return vec![Effect::CancelDiff];
        }
        // Another commit's file: start at its top, whatever was shown.
        self.diff.path = None;
        self.commit_file_diff()
    }

    fn move_commit_file(&mut self, delta: isize) -> Vec<Effect> {
        let moved = self
            .commits
            .open
            .as_mut()
            .is_some_and(|open| open.move_by(delta));
        if moved {
            self.commit_file_diff()
        } else {
            vec![]
        }
    }

    /// Stream the selected commit file's diff, if one is selected.
    fn commit_file_diff(&mut self) -> Vec<Effect> {
        let request = self.commits.open.as_ref().and_then(|open| {
            let file = open.selected_file()?;
            Some(DiffRequest::for_commit_file(
                &open.range,
                file,
                self.diff_options,
            ))
        });
        match request {
            Some(request) => self.stream_diff(request),
            None => vec![],
        }
    }

    /// Stream the open commit's or stash's file again, as when it was
    /// picked; nothing when neither is open.
    pub(super) fn reload_open_file_diff(&mut self) -> Vec<Effect> {
        if self.commits.open.is_some() {
            self.diff.path = None;
            self.commit_file_diff()
        } else if self.stash.open.is_some() {
            self.diff.path = None;
            self.stash_file_diff()
        } else {
            vec![]
        }
    }

    /// Back to the log, still on the same commit, with the pane showing the
    /// selected working-tree file again.
    pub(crate) fn close_commit_files(&mut self) -> Vec<Effect> {
        if self.commits.open.take().is_none() {
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
                if self.watch.start_fetch(self.remote.is_some()) {
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
        let mut effects = self.background_status();
        effects.extend(self.background_branches());
        effects.extend(self.reload_commits());
        effects.extend(self.reload_stashes());
        effects
    }

    /// Reload `git status` quietly. Skipped while the user is waiting on
    /// git, whose own reload is coming, or while one is already running.
    /// It takes `status_seq`, so a user refresh started later wins.
    pub(crate) fn background_status(&mut self) -> Vec<Effect> {
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

/// The Branches panel: loading the list and the selected branch's log,
/// and the actions on the selected branch.
impl AppState {
    /// Load the branch list again, keeping what it shows until it lands.
    pub(crate) fn reload_branches(&mut self) -> Vec<Effect> {
        self.load_branches(true)
    }

    /// [`Self::reload_branches`], for a change noticed outside Kika: out
    /// of the busy count, and only one at a time.
    fn background_branches(&mut self) -> Vec<Effect> {
        if self.watch.branches_pending {
            return vec![];
        }
        self.load_branches(false)
    }

    /// Load the local branches and the remotes together: whatever moves
    /// one can move the other. A background remotes load is not counted:
    /// the branch load beside it keeps a second pair from starting.
    fn load_branches(&mut self, user: bool) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        let seq = self.take_seq();
        self.branches.list_seq = Some(seq);
        self.start_branch_load(user);
        let remotes = self.take_seq();
        self.branches.remotes.list_seq = Some(remotes);
        if user {
            self.busy += 1;
        }
        let tags = self.take_seq();
        self.branches.tags.list_seq = Some(tags);
        if user {
            self.busy += 1;
        }
        let tag = |seq| Tag {
            generation: self.generation,
            seq,
            user,
        };
        vec![
            Effect::LoadBranches { tag: tag(seq) },
            Effect::LoadRemotes { tag: tag(remotes) },
            Effect::LoadTags { tag: tag(tags) },
        ]
    }

    /// A user load raises the busy count; a background one the watch's flag.
    fn start_branch_load(&mut self, user: bool) {
        if user {
            self.busy += 1;
        } else {
            self.watch.branches_pending = true;
        }
    }

    /// Every branch load reports back once, stale or not.
    fn end_branch_load(&mut self, user: bool) {
        if user {
            self.busy = self.busy.saturating_sub(1);
        } else {
            self.watch.branches_pending = false;
        }
    }

    /// Show the list, and load the selected branch's log afresh: a
    /// reload means its commits may have moved.
    fn branches_loaded(&mut self, tag: Tag, result: Result<Vec<Branch>, String>) -> Vec<Effect> {
        self.end_branch_load(tag.user);
        if tag.generation != self.generation || self.branches.list_seq != Some(tag.seq) {
            return vec![];
        }
        self.branches.list_seq = None;
        if let Ok(branches) = &result {
            self.forge.set_branches(branches.iter().map(|b| &b.name));
        }
        let listed = result.is_ok();
        self.branches.loaded(result);
        let mut effects = if self.branches.tab == BranchesTab::Local {
            self.load_branch_log(tag.user)
        } else {
            vec![]
        };
        if listed {
            effects.extend(self.load_pull_requests());
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
                .selected_tag()
                .map(|t| t.name.ref_bytes()),
        };
        let Some(refname) = refname else {
            self.branches.log = Default::default();
            return vec![];
        };
        let seq = self.take_seq();
        self.branches.log.begin(seq, refname.clone());
        self.start_branch_load(user);
        let tag = Tag {
            generation: self.generation,
            seq,
            user,
        };
        vec![Effect::LoadBranchLog { tag, refname }]
    }

    fn branch_log_loaded(
        &mut self,
        tag: Tag,
        refname: Vec<u8>,
        result: Result<Vec<LogRow>, String>,
    ) {
        self.end_branch_load(tag.user);
        let log = &mut self.branches.log;
        if tag.generation != self.generation || log.for_ref.as_ref() != Some(&refname) {
            return;
        }
        log.apply(tag.seq, result);
    }

    /// Show the tab `step` gives, and the log of what is selected there.
    fn switch_branches_tab(&mut self, step: fn(BranchesTab) -> BranchesTab) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        self.branches.clear_range(self.branches.tab);
        self.branches.tab = step(self.branches.tab);
        self.load_branch_log(true)
    }

    fn move_branch(&mut self, delta: isize) -> Vec<Effect> {
        if self.branches.move_by(delta) {
            self.load_branch_log(true)
        } else {
            vec![]
        }
    }

    /// The selected branch's name, or why no action may run on it now,
    /// said in the bar.
    pub(crate) fn branch_for_action(
        &mut self,
        disabled: fn(&AppState) -> Option<Reason>,
    ) -> Option<BranchName> {
        if self.context != Context::Workspace {
            return None;
        }
        if let Some(reason) = disabled(self) {
            self.notice = Some(reason);
            return None;
        }
        self.branches.selected_branch().map(|b| b.name.clone())
    }

    fn checkout_branch(&mut self) -> Vec<Effect> {
        match self.branch_for_action(keymap::checkout_branch_disabled) {
            Some(name) => self.start_op(Ok(Some(GitOp::Checkout(name)))),
            None => vec![],
        }
    }

    /// Start `op` attached to the terminal or detached, as
    /// [`runs_attached`] says.
    pub(crate) fn run_op_where_it_belongs(&mut self, op: GitOp) -> Vec<Effect> {
        if runs_attached(&op) {
            self.start_op_as(Ok(Some(op)), |tag, op| {
                Effect::HandOff(HandOff::Attached { tag, op })
            })
        } else {
            self.start_op(Ok(Some(op)))
        }
    }

    /// The selected commit's id and short id, or why no action may run on
    /// it now, said in the bar. Only an id of hex digits reaches argv.
    pub(crate) fn commit_for_action(
        &mut self,
        disabled: fn(&AppState) -> Option<Reason>,
    ) -> Option<(CommitId, String)> {
        if self.context != Context::Workspace {
            return None;
        }
        if let Some(reason) = disabled(self) {
            self.notice = Some(reason);
            return None;
        }
        let row = self.commits.rows.get(self.commits.selected)?;
        match CommitId::parse(&row.oid) {
            Some(id) => Some((id, row.short_oid.clone())),
            None => {
                self.notice = Some("Not a commit id git can use".into());
                None
            }
        }
    }

    /// Ask before leaving the branch: commits made on a detached `HEAD`
    /// belong to no branch. From a detached `HEAD`, look first for the
    /// commits the move would leave behind.
    fn ask_checkout_commit(&mut self) -> Vec<Effect> {
        match self.commit_for_action(keymap::checkout_commit_disabled) {
            Some((id, _)) => self.move_head(GitOp::CheckoutCommit(id.clone()), id),
            None => vec![],
        }
    }

    /// Revert the selected commit on the terminal, so git's editor and the
    /// commit hooks work as on the command line.
    fn revert_commit(&mut self) -> Vec<Effect> {
        match self.commit_for_action(keymap::revert_disabled) {
            Some((id, _)) => self.run_op_where_it_belongs(GitOp::Revert(id)),
            None => vec![],
        }
    }

    /// Copy the selected commit's hash, short hash or subject now, or
    /// read its full message first. Pure, so it works while an operation
    /// runs.
    fn copy_commit_text(&mut self, what: CopyWhat) -> Vec<Effect> {
        let Some((id, short)) = self.commit_for_action(keymap::no_commit_selected) else {
            return vec![];
        };
        let text = match what {
            CopyWhat::Hash => id.as_str().to_string(),
            CopyWhat::ShortHash => match CommitId::parse(&short) {
                Some(short) => short.as_str().to_string(),
                None => {
                    self.notice = Some("Not a commit id git can use".into());
                    return vec![];
                }
            },
            CopyWhat::Subject => self.commits.rows[self.commits.selected].subject.clone(),
            CopyWhat::Message => {
                let seq = self.take_seq();
                let tag = self.user_tag(seq);
                self.pending_copy = Some((tag, id.clone()));
                return vec![Effect::LoadCommitMessage { tag, oid: id }];
            }
            CopyWhat::ForgeUrl => match pull_requests::commit_page(self, &id) {
                Ok(url) => url,
                Err(reason) => {
                    self.notice = Some(reason);
                    return vec![];
                }
            },
        };
        self.copy_text(what, text)
    }

    /// Send `text` to the clipboard and say so; a failure replaces the
    /// notice with an error.
    pub(crate) fn copy_text(&mut self, what: CopyWhat, text: String) -> Vec<Effect> {
        self.notice_text = Some(format!(
            "Copied {}: {}",
            what.label(),
            clipboard::preview(&text)
        ));
        vec![Effect::Clipboard(text)]
    }

    /// Copy the message read for the last request; an older one is dropped.
    fn commit_message_loaded(
        &mut self,
        tag: Tag,
        oid: CommitId,
        result: Result<String, String>,
    ) -> Vec<Effect> {
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        if self.pending_copy.as_ref() != Some(&(tag, oid)) {
            return vec![];
        }
        self.pending_copy = None;
        match result {
            Ok(message) => self.copy_text(CopyWhat::Message, message),
            Err(message) => {
                self.push_error(ErrorPopup::new(
                    "Could not read the commit message",
                    &message,
                    "Close this, then try again.",
                ));
                vec![]
            }
        }
    }

    /// Ask for the pattern to ignore the selected untracked file by,
    /// filled with the one that matches exactly that file.
    fn open_ignore(&mut self) {
        if self.context != Context::Workspace {
            return;
        }
        if let Some(reason) = keymap::ignore_disabled(self) {
            self.notice = Some(reason);
            return;
        }
        let Some(file) = self.selected.and_then(|i| self.files.get(i)) else {
            return;
        };
        let bytes = match gitignore::escape(&file.path) {
            Ok(bytes) => bytes,
            Err(why) => {
                self.notice = Some(why.into());
                return;
            }
        };
        let shown = sanitize(&bytes);
        self.popups.push(Popup::Prompt {
            kind: PromptKind::IgnorePattern,
            input: LineInput::with_text(&shown),
        });
        self.ignore_draft = Some(IgnoreDraft { shown, bytes });
    }

    /// Add the accepted pattern: the offered one's exact bytes when it is
    /// unchanged, else the typed text as the user's own pattern. Empty
    /// text or a control character keeps the prompt open with a notice.
    fn ignore_answered(&mut self, text: String) -> Vec<Effect> {
        let Some(draft) = self.ignore_draft.take() else {
            return vec![];
        };
        let line = if text == draft.shown {
            draft.bytes.clone()
        } else if text.trim().is_empty() || text.contains(char::is_control) {
            self.notice = Some(if text.trim().is_empty() {
                "Type a pattern to ignore".into()
            } else {
                "The pattern has a control character".into()
            });
            self.popups.push(Popup::Prompt {
                kind: PromptKind::IgnorePattern,
                input: LineInput::with_text(&text),
            });
            self.ignore_draft = Some(draft);
            return vec![];
        } else {
            text.into_bytes()
        };
        let seq = self.take_seq();
        vec![Effect::AppendIgnore {
            tag: self.user_tag(seq),
            line,
        }]
    }

    /// Say what the add did and reload the status, so an ignored file
    /// leaves the list.
    fn ignore_written(&mut self, tag: Tag, result: Result<IgnoreAdded, String>) -> Vec<Effect> {
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        if tag.generation != self.generation {
            return vec![];
        }
        match result {
            Ok(IgnoreAdded { pattern, added }) => {
                self.notice_text = Some(if added {
                    format!("Added {pattern} to .gitignore")
                } else {
                    format!("{pattern} is already in .gitignore")
                });
                self.refresh_status()
            }
            Err(message) => {
                self.push_error(ErrorPopup::new(
                    "Could not add to .gitignore",
                    &message,
                    "Close this, fix the cause, then try again.",
                ));
                vec![]
            }
        }
    }

    /// Reset to the selected commit. Soft and mixed keep the work; hard
    /// throws it away, so it asks first. From a detached `HEAD`, look
    /// first for the commits the reset would leave behind.
    fn reset_to(&mut self, mode: ResetMode) -> Vec<Effect> {
        let Some((to, _)) = self.commit_for_action(keymap::reset_disabled) else {
            return vec![];
        };
        self.move_head(
            GitOp::Reset {
                to: to.clone(),
                mode,
            },
            to,
        )
    }

    /// Rewrite the selected commit on the terminal: squash and reword open
    /// git's editor, and hooks, signing and conflict stops work as on the
    /// command line. On a todo row, edit the stopped rebase's todo list
    /// instead.
    fn rewrite(&mut self, rewrite: Rewrite) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if let Some(reason) = keymap::rewrite_disabled(self, rewrite) {
            self.notice = Some(reason);
            return vec![];
        }
        if self.commits.todo_selected.is_some() {
            return self.edit_todo(rewrite);
        }
        match keymap::log_plan(self, rewrite) {
            Ok(plan) => {
                // The cursor follows the moved commit.
                match rewrite {
                    Rewrite::Move(rebase_plan::Direction::Up) => self.commits.selected -= 1,
                    Rewrite::Move(rebase_plan::Direction::Down) => self.commits.selected += 1,
                    _ => {}
                }
                self.run_op_where_it_belongs(GitOp::InteractiveRebase(plan))
            }
            Err(reason) => {
                self.notice = Some(reason);
                vec![]
            }
        }
    }

    /// Write the todo list with `rewrite` made on the selected row. The
    /// list shows the edit at once; the reload after the write shows what
    /// git has.
    fn edit_todo(&mut self, rewrite: Rewrite) -> Vec<Effect> {
        let (entries, cursor) = match keymap::todo_edit(self, rewrite) {
            Ok(edited) => edited,
            Err(reason) => {
                self.notice = Some(reason);
                return vec![];
            }
        };
        let Some(todo) = self.commits.todo.as_mut() else {
            return vec![];
        };
        let content = rebase_todo::serialize(&entries);
        let (path, expected) = (todo.path.clone(), todo.original.clone());
        let comment = todo.comment.clone();
        todo.entries = entries;
        self.commits.todo_selected = Some(cursor);
        let seq = self.take_seq();
        self.commits.todo_write_seq = Some(seq);
        self.busy += 1;
        let tag = Tag {
            generation: self.generation,
            seq,
            user: true,
        };
        vec![Effect::WriteRebaseTodo {
            tag,
            path,
            expected,
            content,
            comment,
        }]
    }

    /// An edited todo list was written, or not; either way, reload it so
    /// the panel shows what git will run.
    fn rebase_todo_written(&mut self, tag: Tag, result: Result<(), String>) -> Vec<Effect> {
        self.busy = self.busy.saturating_sub(1);
        if tag.generation != self.generation || self.commits.todo_write_seq != Some(tag.seq) {
            return vec![];
        }
        self.commits.todo_write_seq = None;
        match result {
            Ok(()) => {}
            Err(message) if message == CHANGED_ON_DISK => self.notice = Some(TODO_CHANGED.into()),
            Err(message) => self.op_error = Some(message),
        }
        self.reload_rebase_todo()
    }

    /// Read the stopped rebase's todo list.
    fn reload_rebase_todo(&mut self) -> Vec<Effect> {
        let tag = self.background_tag();
        self.commits.todo_seq = Some(tag.seq);
        // An older git ignores `core.commentString`, so its value does not
        // count; an unknown git counts as older.
        let string_too = self.git_has(crate::git::version::COMMENT_STRING);
        vec![Effect::LoadRebaseTodo { tag, string_too }]
    }

    /// Show the todo list read, if it is the load in flight.
    fn rebase_todo_loaded(&mut self, tag: Tag, result: Result<Option<TodoFile>, String>) {
        if tag.generation != self.generation || self.commits.todo_seq != Some(tag.seq) {
            return;
        }
        self.commits.todo_seq = None;
        // Unreadable counts as none: nothing is edited that was not read.
        let todo = result.ok().flatten().map(RebaseTodo::load);
        self.commits.set_todo(todo);
    }

    /// Reset hard as the open question asked, and only if one is open, its
    /// guard still allows it and `HEAD` has not moved since.
    fn confirm_reset_hard(&mut self) -> Vec<Effect> {
        if !matches!(self.popups.last(), Some(Popup::ResetHard(_))) {
            return vec![];
        }
        let Some(Popup::ResetHard(confirm)) = self.popups.pop() else {
            return vec![];
        };
        if self.confirm_refused(&confirm.op) {
            return vec![];
        }
        if !self.head_still(confirm.head.as_ref()) {
            return vec![];
        }
        self.start_op(Ok(Some(confirm.op)))
    }

    /// Ask for a branch name, keeping the selected branch for the answer.
    /// Renaming starts from the old name.
    fn ask_branch_name(&mut self, kind: PromptKind) {
        let disabled = match kind {
            // Creating a branch checks it out, which moves `HEAD`.
            PromptKind::NewBranch => keymap::new_branch_disabled,
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
        let name = text.trim();
        if name.is_empty() {
            self.notice = Some("A branch needs a name".into());
            return vec![];
        }
        if self.op_seq.is_some() {
            self.notice = Some(keymap::OP_RUNNING.into());
            return vec![];
        }
        self.start_op(Ok(Some(op(branch, name.to_string()))))
    }

    /// Ask before deleting the selected branch: plainly when `git branch
    /// -d` would take it, else warning that its commits may be lost.
    fn ask_delete_branch(&mut self) {
        if self
            .branch_for_action(keymap::delete_branch_disabled)
            .is_none()
        {
            return;
        }
        let Some(branch) = self.branches.selected_branch() else {
            return;
        };
        let confirm = DeleteBranchConfirm {
            name: branch.name.display(),
            op: GitOp::DeleteBranch {
                name: branch.name.clone(),
                force: !branch.merged,
            },
        };
        self.popups.push(Popup::DeleteBranch(confirm));
    }

    /// Delete as the open question asked: `force` must match it, so `y`
    /// never deletes unmerged commits.
    fn confirm_delete_branch(&mut self, force: bool) -> Vec<Effect> {
        match self.popups.last() {
            Some(Popup::DeleteBranch(confirm)) if confirm.force() == force => {}
            _ => return vec![],
        }
        let Some(Popup::DeleteBranch(confirm)) = self.popups.pop() else {
            return vec![];
        };
        self.start_op(Ok(Some(confirm.op)))
    }

    /// Ask which remote branch the selected branch should track, starting
    /// from its upstream, or else the same name on `origin` or the first
    /// remote.
    fn ask_upstream(&mut self) {
        let Some(name) = self.branch_for_action(keymap::branch_action_disabled) else {
            return;
        };
        let upstream = self
            .branches
            .selected_branch()
            .and_then(|b| b.upstream.clone());
        let text = upstream.unwrap_or_else(|| {
            let configured = || {
                self.branches
                    .remotes
                    .remotes
                    .iter()
                    .filter(|r| r.configured)
            };
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
    fn set_upstream_to(&mut self, text: &str) -> Vec<Effect> {
        let Some(branch) = self.branches.pending.take() else {
            return vec![];
        };
        let Some(upstream) = self.branches.remotes.branch_named(text.trim()).cloned() else {
            self.notice = Some("No such remote branch; fetch, or check the name".into());
            return vec![];
        };
        if self.op_seq.is_some() {
            self.notice = Some(keymap::OP_RUNNING.into());
            return vec![];
        }
        self.start_op(Ok(Some(GitOp::SetUpstream { branch, upstream })))
    }
}

/// Fetch, pull and push, and the questions git and ssh ask while they run.
impl AppState {
    /// Show the question, or say no at once to one nobody is waiting for.
    fn credential_asked(&mut self, tag: Tag, prompt: String, confirm: bool) -> Vec<Effect> {
        if self.current_remote(tag).is_none() && !self.review_fetch_running(tag) {
            return vec![Effect::AnswerCredential(None)];
        }
        self.popups
            .push(Popup::Credential(CredentialPrompt::new(prompt, confirm)));
        vec![]
    }

    /// Close the question on top and send its answer.
    fn credential_answered(&mut self, answer: Option<Secret>) -> Vec<Effect> {
        if !matches!(self.popups.last(), Some(Popup::Credential(_))) {
            return vec![];
        }
        self.popups.pop();
        vec![Effect::AnswerCredential(answer)]
    }

    /// Take every credential question off the stack; whether there was one.
    pub(crate) fn close_credential(&mut self) -> bool {
        let before = self.popups.len();
        self.popups
            .retain(|popup| !matches!(popup, Popup::Credential(_)));
        self.popups.len() != before
    }
}

/// Said when a changed filter drops the range made over the old rows.
const SELECTION_CLEARED: &str = "Selection cleared";
/// Said when a range's stash is refused because its files changed while
/// the message was typed.
const STASH_SET_CHANGED: &str = "Nothing stashed: the selected files changed";
/// Said when a reload changed which files the Files range covers.
const RANGE_FILES_CHANGED: &str = "Selection cleared: the file list changed";

/// The bottom-bar input for a search, starting from its query.
fn search_input(target: InputTarget, search: &Option<DiffSearch>) -> ListInput {
    let text = search.as_ref().map_or("", |s| s.query.as_str());
    ListInput {
        target,
        line: LineInput::with_text(text),
    }
}

/// A search is open and found nothing, so there is no match to go to.
fn no_matches(search: &Option<DiffSearch>) -> bool {
    search.as_ref().is_some_and(|s| s.current.is_none())
}

/// List filters, range selections and the diff search.
impl AppState {
    /// The indexes of the files the filter shows, in order.
    pub fn files_visible(&self) -> Vec<usize> {
        if self.files_filter.is_empty() {
            return (0..self.files.len()).collect();
        }
        let paths = self.files.iter().map(|f| f.path.display());
        list_filter::visible(&self.files_filter, paths)
    }

    /// The filter hides the selected file, which happens only when it
    /// hides every file.
    pub fn files_selected_hidden(&self) -> bool {
        let entry = self.selected.and_then(|i| self.files.get(i));
        entry.is_some_and(|f| !list_filter::matches(&self.files_filter, f.path.display()))
    }

    /// The files the range covers, in order; empty without one.
    pub fn files_range(&self) -> Vec<usize> {
        let anchor = self
            .files_anchor
            .as_ref()
            .and_then(|anchor| self.files.iter().position(|f| &f.path == anchor));
        match (anchor, self.selected) {
            (Some(anchor), Some(cursor)) => {
                list_filter::range(&self.files_visible(), anchor, cursor)
            }
            _ => Vec::new(),
        }
    }

    /// The files a Files action on "the selected files" takes: the range
    /// when one is set there, else the file under the cursor. Says which.
    pub(crate) fn files_marked(&self) -> (Vec<&FileEntry>, bool) {
        let from_range = self.files_anchor.is_some() && self.focus == Focus::Files;
        let marked = if from_range {
            self.files_range()
        } else {
            self.selected.into_iter().collect()
        };
        let entries = marked.iter().filter_map(|&i| self.files.get(i)).collect();
        (entries, from_range)
    }

    /// The paths of the files the range covers, in order.
    fn files_range_paths(&self) -> Vec<RepoPath> {
        self.files_range()
            .into_iter()
            .map(|i| self.files[i].path.clone())
            .collect()
    }

    /// Type into the focused list's filter, starting from its text.
    fn open_filter(&mut self) {
        if self.key_focus() == KeyScope::Worktrees {
            if let Some(list) = self.worktree_list() {
                self.input = Some(ListInput {
                    target: InputTarget::WorktreesFilter,
                    line: LineInput::with_text(&list.filter),
                });
            }
            return;
        }
        if self.key_focus() == KeyScope::CommandLog {
            if let Some(Popup::CommandLog(view)) = self.popups.last() {
                self.input = Some(ListInput {
                    target: InputTarget::CommandLogFilter,
                    line: LineInput::with_text(&view.filter),
                });
            }
            return;
        }
        let (target, text) = match self.panel_focus() {
            KeyScope::Files => (InputTarget::FilesFilter, &self.files_filter),
            KeyScope::Commits => (InputTarget::CommitsFilter, &self.commits.filter),
            KeyScope::Branches => (InputTarget::BranchesFilter, &self.branches.filter),
            KeyScope::Remotes => (InputTarget::RemotesFilter, &self.branches.remotes.filter),
            KeyScope::Tags => (InputTarget::TagsFilter, &self.branches.tags.filter),
            KeyScope::Reflog => (InputTarget::ReflogFilter, &self.commits.reflog.filter),
            KeyScope::CommitFiles => match &self.commits.open {
                Some(open) => (InputTarget::CommitFilesFilter, &open.filter),
                None => return,
            },
            KeyScope::Stash => (InputTarget::StashFilter, &self.stash.filter),
            KeyScope::Review if review::queue_keys_disabled(self).is_none() => {
                (InputTarget::ReviewFilter, &self.review.filter)
            }
            _ => return,
        };
        self.input = Some(ListInput {
            target,
            line: LineInput::with_text(text),
        });
    }

    /// Filter or search with the input's text as it is now. A changed
    /// filter clears the range, which was made over the rows shown before,
    /// and moves a cursor it hides to the first row it shows.
    fn apply_input(&mut self) -> Vec<Effect> {
        let Some(input) = &self.input else {
            return vec![];
        };
        let text = input.line.text.clone();
        match input.target {
            InputTarget::FilesFilter => {
                if text != self.files_filter {
                    self.files_filter = text;
                    if self.files_anchor.take().is_some() {
                        self.notice = Some(SELECTION_CLEARED.into());
                    }
                }
                if self.files_selected_hidden()
                    && let Some(&first) = self.files_visible().first()
                {
                    return self.select(first);
                }
            }
            InputTarget::CommitsFilter => {
                if text != self.commits.filter {
                    self.commits.filter = text;
                    if self.commits.range_anchor.take().is_some() {
                        self.notice = Some(SELECTION_CLEARED.into());
                    }
                }
                self.commits.reconcile();
            }
            InputTarget::BranchesFilter | InputTarget::RemotesFilter | InputTarget::TagsFilter => {
                return self.apply_branches_filter(input.target, text);
            }
            // A reflog row drives nothing, so a moved cursor loads nothing.
            InputTarget::ReflogFilter => self.commits.reflog.set_filter(text),
            // Only matched; the file the cursor moved to is loaded by its
            // own path, as a step would.
            InputTarget::CommitFilesFilter => {
                let moved = self
                    .commits
                    .open
                    .as_mut()
                    .is_some_and(|open| open.set_filter(text));
                if moved {
                    return self.commit_file_diff();
                }
            }
            // A stash row drives nothing, so a moved cursor loads nothing.
            InputTarget::StashFilter => {
                if self.stash.set_filter(text) {
                    self.notice = Some(SELECTION_CLEARED.into());
                }
            }
            InputTarget::DiffSearch => {
                if let Focus::Diff(view) = &mut self.focus {
                    view.set_search(&text, self.emoji_zwj);
                }
            }
            InputTarget::PreviewSearch => self.diff.set_search(&text),
            InputTarget::BlameSearch => {
                if let Focus::Blame(view) = &mut self.focus {
                    view.set_search(&text);
                }
            }
            InputTarget::BlameCommitSearch => {
                if let Focus::Blame(BlameView {
                    commit: Some(commit),
                    ..
                }) = &mut self.focus
                {
                    commit.set_search(&text);
                }
            }
            InputTarget::ReviewFilter => {
                self.review.filter = text;
                self.review.reconcile();
            }
            InputTarget::WorktreesFilter => {
                if let Some(list) = self.worktree_list_mut() {
                    list.filter = text;
                    list.reconcile();
                }
            }
            InputTarget::CommandLogFilter => {
                if let Some(Popup::CommandLog(view)) = self.popups.last_mut() {
                    view.filter = text;
                    view.reconcile(&self.command_log);
                }
            }
            // Git runs the query on Enter, not on every key.
            InputTarget::LogPath | InputTarget::LogAuthor => {}
        }
        vec![]
    }

    /// Close the input. The filter stays as typed; a search goes to its
    /// current match; a log query is read and run, or stays open saying
    /// why it cannot be.
    fn input_accept(&mut self) -> Vec<Effect> {
        let Some(input) = self.input.take() else {
            return vec![];
        };
        if input.target.is_log_query() {
            return self.accept_log_query(input);
        }
        let rows = self.diff_rows();
        let mut effects = vec![];
        let found = match (input.target, &mut self.focus) {
            (InputTarget::DiffSearch, Focus::Diff(view)) => {
                let found = !no_matches(&view.search);
                if found {
                    view.jump_to_match(rows);
                }
                found
            }
            (InputTarget::BlameSearch, Focus::Blame(view)) => {
                let found = !no_matches(&view.search);
                if found {
                    view.jump_to_match(rows);
                }
                found
            }
            (
                InputTarget::BlameCommitSearch,
                Focus::Blame(BlameView {
                    commit: Some(commit),
                    ..
                }),
            ) => {
                let found = !no_matches(&commit.search);
                if found {
                    commit.jump_to_match(rows);
                }
                found
            }
            (InputTarget::PreviewSearch, _) => {
                let found = !no_matches(&self.diff.search);
                if found {
                    effects = self.diff.jump_to_match(rows);
                }
                found
            }
            _ => true,
        };
        if !found {
            self.notice = Some("No matches".into());
        }
        effects
    }

    /// Type a search for the focused pane, starting from its query.
    fn open_search(&mut self) {
        let (target, search) = match &self.focus {
            Focus::Diff(view) => (InputTarget::DiffSearch, &view.search),
            Focus::Blame(view) => match &view.commit {
                Some(commit) => (InputTarget::BlameCommitSearch, &commit.search),
                None => (InputTarget::BlameSearch, &view.search),
            },
            _ => return,
        };
        self.input = Some(search_input(target, search));
    }

    /// `n` or `N`: the next or previous match of the search the key focus
    /// owns, saying so when it wrapped. A side panel steps its preview's.
    fn step_search(&mut self, back: bool) -> Vec<Effect> {
        let rows = self.diff_rows();
        let mut effects = vec![];
        // `None` when the search found nothing to step to.
        let wrapped = match (self.panel_focus(), &mut self.focus) {
            (KeyScope::Diff, Focus::Diff(view)) if no_matches(&view.search) => None,
            (KeyScope::Diff, Focus::Diff(view)) => Some(view.step_search(back, rows)),
            (KeyScope::Blame, Focus::Blame(view)) if no_matches(&view.search) => None,
            (KeyScope::Blame, Focus::Blame(view)) => Some(view.step_search(back, rows)),
            (
                KeyScope::BlameCommit,
                Focus::Blame(BlameView {
                    commit: Some(commit),
                    ..
                }),
            ) => (!no_matches(&commit.search)).then(|| commit.step_search(back, rows)),
            (
                KeyScope::Files
                | KeyScope::CommitFiles
                | KeyScope::StashFiles
                | KeyScope::ReviewDiff,
                _,
            ) => (!no_matches(&self.diff.search)).then(|| {
                let (wrapped, demand) = self.diff.step_search(back, rows);
                effects = demand;
                wrapped
            }),
            _ => Some(false),
        };
        match wrapped {
            None => self.notice = Some("No matches".into()),
            Some(true) => self.notice = Some("Search wrapped".into()),
            Some(false) => {}
        }
        effects
    }

    /// Esc in the blame, its commit, or a commit's or a stash's files:
    /// clear that pane's search, else close the pane as Esc did before. A
    /// commit's files clear their filter before the search.
    fn pane_escape(&mut self) -> Vec<Effect> {
        let scope = self.panel_focus();
        // The cursor stays: the filter showed it, or there were no rows.
        if scope == KeyScope::CommitFiles
            && let Some(open) = &mut self.commits.open
            && !open.filter.is_empty()
        {
            open.filter.clear();
            return vec![];
        }
        let cleared = match (scope, &mut self.focus) {
            (
                KeyScope::BlameCommit,
                Focus::Blame(BlameView {
                    commit: Some(commit),
                    ..
                }),
            ) => commit.search.take().is_some(),
            (KeyScope::Blame, Focus::Blame(view)) => view.search.take().is_some(),
            (KeyScope::CommitFiles | KeyScope::StashFiles, _) => self.diff.search.take().is_some(),
            _ => false,
        };
        if cleared {
            return vec![];
        }
        let close = match scope {
            KeyScope::Blame => AppIntent::CloseBlame,
            KeyScope::BlameCommit => AppIntent::CloseBlameCommit,
            KeyScope::CommitFiles => AppIntent::CloseCommitFiles,
            KeyScope::StashFiles => AppIntent::CloseStashFiles,
            _ => return vec![],
        };
        self.update(close)
    }

    /// Start a range at the cursor, or clear the one there is.
    fn toggle_range(&mut self) {
        match self.panel_focus() {
            KeyScope::Files if self.files_anchor.is_some() => self.files_anchor = None,
            KeyScope::Files if !self.files_selected_hidden() => {
                self.files_anchor = self
                    .selected
                    .and_then(|i| self.files.get(i))
                    .map(|f| f.path.clone());
            }
            KeyScope::Commits if self.commits.range_anchor.is_some() => {
                self.commits.range_anchor = None;
            }
            KeyScope::Commits if !self.commits.selected_hidden() => {
                let row = self.commits.rows.get(self.commits.selected);
                self.commits.range_anchor = row.map(|r| r.oid.clone());
            }
            KeyScope::Branches => self.branches.toggle_range(),
            KeyScope::Remotes => self.branches.remotes.toggle_range(),
            KeyScope::Tags => self.branches.tags.toggle_range(),
            KeyScope::Stash => self.stash.toggle_range(),
            _ => {}
        }
    }

    /// Esc in a list: clear the range, else the filter, else (in Files)
    /// the preview's search, else (in Commits) the diff mark, else the log
    /// query, else quit as Esc always has. In the Worktrees and Command log
    /// popups: clear the filter, else close the popup. The Reflog tab has
    /// no range: clear the filter, else quit.
    fn list_escape(&mut self) -> Vec<Effect> {
        if self.key_focus() == KeyScope::CommandLog {
            if let Some(Popup::CommandLog(view)) = self.popups.last_mut()
                && !view.filter.is_empty()
            {
                view.filter.clear();
                view.reconcile(&self.command_log);
                return vec![];
            }
            return self.update(AppIntent::PopPopup);
        }
        if self.key_focus() == KeyScope::Worktrees {
            if let Some(list) = self.worktree_list_mut()
                && !list.filter.is_empty()
            {
                list.filter.clear();
                return vec![];
            }
            return self.update(AppIntent::PopPopup);
        }
        let focus = self.panel_focus();
        let branches = &mut self.branches;
        let (cleared, filter) = match focus {
            KeyScope::Files => (self.files_anchor.take().is_some(), &mut self.files_filter),
            KeyScope::Commits => (
                self.commits.range_anchor.take().is_some(),
                &mut self.commits.filter,
            ),
            KeyScope::Branches => (branches.range_anchor.take().is_some(), &mut branches.filter),
            KeyScope::Remotes => (
                branches.remotes.range_anchor.take().is_some(),
                &mut branches.remotes.filter,
            ),
            KeyScope::Tags => (
                branches.tags.range_anchor.take().is_some(),
                &mut branches.tags.filter,
            ),
            KeyScope::Reflog => (false, &mut self.commits.reflog.filter),
            KeyScope::Stash => (
                self.stash.range_anchor.take().is_some(),
                &mut self.stash.filter,
            ),
            _ => return self.update(AppIntent::Quit),
        };
        if cleared {
            return vec![];
        }
        if !filter.is_empty() {
            filter.clear();
            return vec![];
        }
        if focus == KeyScope::Files && self.diff.search.take().is_some() {
            return vec![];
        }
        if self.panel_focus() == KeyScope::Commits {
            if self.diff_mark.take().is_some() {
                self.notice = Some(MARK_CLEARED.into());
                return vec![];
            }
            if self.commits.query.is_active() {
                self.notice = Some("Log filter cleared".into());
                return self.set_log_query(LogQuery::default());
            }
        }
        self.update(AppIntent::Quit)
    }

    /// Stage every file in the range, or unstage them all when none has
    /// anything left to stage. One op, so one git call.
    fn stage_range(&mut self) -> Vec<Effect> {
        let entries: Vec<&FileEntry> = self.files_range().iter().map(|&i| &self.files[i]).collect();
        let op = GitOp::toggle_many(&entries);
        if op.is_some() {
            self.files_anchor = None;
        }
        self.start_op(Ok(op))
    }

    /// Ask before discarding the range: the files it can restore and the
    /// untracked ones it would delete, counted in the question.
    fn ask_discard_range(&mut self) {
        let entries: Vec<&FileEntry> = self.files_range().iter().map(|&i| &self.files[i]).collect();
        let Some(op) = GitOp::discard_many(&entries) else {
            self.notice = Some("No unstaged changes to discard".into());
            return;
        };
        let (restore, delete) = match &op {
            GitOp::DiscardWorktree(paths) => (paths.len(), 0),
            GitOp::DeleteUntracked(paths) => (0, paths.len()),
            GitOp::DiscardFiles { restore, delete } => (restore.len(), delete.len()),
            _ => (0, 0),
        };
        self.popups.push(Popup::Discard(DiscardConfirm {
            what: DiscardWhat::Range { restore, delete },
            path: String::new(),
            op,
        }));
    }
}

/// The first line of a rejected edited hunk's error, above git's message.
pub(crate) const EDITED_HUNK_INVALID: &str = "The edited hunk is not a patch git can apply. \
Keep the @@ line as it was and start each line with a space, + or -.";

/// What the error popup says to do after a failed operation: what was
/// reloaded, then try again.
fn failure_next(op: &GitOp) -> &'static str {
    match op {
        // The run aborts itself, so there is nothing to resolve.
        GitOp::CustomPatch { .. } => {
            "Nothing was changed and the patch is kept. Fix the cause above, then try again."
        }
        // A pick that the branch already has stops as empty, not on a
        // conflict; skip drops it.
        GitOp::CherryPick(_) => {
            "If git stopped on conflicts or an empty pick, resolve what it says, then continue, skip or abort from the Status panel."
        }
        GitOp::Merge { .. }
        | GitOp::Rebase { .. }
        | GitOp::InteractiveRebase(_)
        | GitOp::Revert(_)
        | GitOp::Continue(_)
        | GitOp::Skip(_) => {
            "If git stopped on conflicts, resolve them, then continue or abort from the Status panel."
        }
        GitOp::RenameBranch { .. }
        | GitOp::DeleteBranch { .. }
        | GitOp::DeleteBranches(_)
        | GitOp::SetUpstream { .. }
        | GitOp::UnsetUpstream(_)
        | GitOp::AddRemote { .. }
        | GitOp::RenameRemote { .. }
        | GitOp::RemoveRemote(_)
        | GitOp::RemoveRemotes(_)
        | GitOp::CreateTag { .. }
        | GitOp::DeleteTag { .. }
        | GitOp::DeleteTags(_) => {
            "The branch list was reloaded. Fix the cause above, then try again."
        }
        GitOp::MergeTool(_) => {
            "The file list was reloaded. Check merge.tool, or resolve the file in your editor."
        }
        // A conflicted apply or pop leaves the entry in the list.
        GitOp::StashApply(_) | GitOp::StashPop(_) => {
            "The file list was reloaded. If git stopped on conflicts, resolve them in the Files panel; the stash is kept."
        }
        GitOp::StashDrop(_) | GitOp::StashDrops(_) | GitOp::StashRename { .. } => {
            "The stash list was reloaded. Fix the cause above, then try again."
        }
        _ => "The file list was reloaded. Fix the cause above, then try again.",
    }
}

/// What the error popup calls a failed operation.
fn failure_title(op: &GitOp) -> &'static str {
    match op {
        GitOp::Stage(_) => "Could not stage",
        GitOp::Unstage(_) => "Could not unstage",
        GitOp::StageAll => "Could not stage everything",
        GitOp::UnstageAll => "Could not unstage everything",
        GitOp::Commit { fixup: Some(_), .. } => "Could not create the fixup",
        GitOp::Commit { amend: true, .. } => "Could not amend",
        GitOp::Commit { .. } => "Could not commit",
        GitOp::ApplyCached { reverse: false, .. } => "Could not stage the change",
        GitOp::ApplyCached { reverse: true, .. } => "Could not unstage the change",
        GitOp::DiscardWorktree(_) => "Could not discard the changes",
        GitOp::DeleteUntracked(_) => "Could not delete the file",
        GitOp::DiscardFiles { .. } => "Could not discard the files",
        GitOp::ApplyWorktree { .. } => "Could not discard the change",
        GitOp::ApplyEdited { .. } => "Could not stage the edited hunk",
        GitOp::SubmoduleInit(_) => "Could not initialize the submodule",
        GitOp::SubmoduleUpdate(_) => "Could not update the submodule",
        GitOp::Checkout(_) => "Could not check out the branch",
        GitOp::CreateBranch { .. } => "Could not create the branch",
        GitOp::RenameBranch { .. } => "Could not rename the branch",
        GitOp::DeleteBranch { .. } => "Could not delete the branch",
        GitOp::DeleteBranches(_) => "Could not delete every branch",
        GitOp::Merge { .. } => "Could not merge",
        GitOp::Rebase { .. } | GitOp::InteractiveRebase(_) => "Could not rebase",
        GitOp::CustomPatch { .. } => "Could not apply the custom patch",
        GitOp::CheckoutCommit(_) => "Could not check out the commit",
        GitOp::Revert(_) => "Could not revert",
        GitOp::CherryPick(_) => "Could not cherry-pick",
        GitOp::Reset { .. } => "Could not reset",
        GitOp::Continue(Operation::Merge) => "Could not continue the merge",
        GitOp::Continue(Operation::Rebase) => "Could not continue the rebase",
        GitOp::Continue(Operation::CherryPick) => "Could not continue the cherry-pick",
        GitOp::Continue(Operation::Revert) => "Could not continue the revert",
        GitOp::Continue(Operation::Bisect) => "Could not continue the bisect",
        GitOp::Skip(_) => "Could not skip",
        GitOp::Abort(Operation::Merge) => "Could not abort the merge",
        GitOp::Abort(Operation::Rebase) => "Could not abort the rebase",
        GitOp::Abort(Operation::CherryPick) => "Could not abort the cherry-pick",
        GitOp::Abort(Operation::Revert) => "Could not abort the revert",
        GitOp::Abort(Operation::Bisect) => "Could not abort the bisect",
        GitOp::MergeTool(_) => "The merge tool did not resolve the file",
        GitOp::StashPush { .. } => "Could not stash",
        GitOp::StashApply(_) => "Could not apply the stash",
        GitOp::StashPop(_) => "Could not pop the stash",
        GitOp::StashDrop(_) => "Could not drop the stash",
        GitOp::StashDrops(_) => "Could not drop the stashes",
        GitOp::StashRename { .. } => "Could not rename the stash",
        GitOp::CheckoutTracking { .. } => "Could not check out the remote branch",
        GitOp::SetUpstream { .. } => "Could not set the upstream",
        GitOp::UnsetUpstream(_) => "Could not unset the upstream",
        GitOp::AddRemote { .. } => "Could not add the remote",
        GitOp::RenameRemote { .. } => "Could not rename the remote",
        GitOp::RemoveRemote(_) => "Could not remove the remote",
        GitOp::RemoveRemotes(_) => "Could not remove every remote",
        GitOp::CreateTag { .. } => "Could not create the tag",
        GitOp::DeleteTag { .. } => "Could not delete the tag",
        GitOp::DeleteTags(_) => "Could not delete every tag",
        GitOp::BisectMark { .. } => "Could not mark the commit",
        GitOp::BisectReset => "Could not reset the bisect",
        GitOp::Travel {
            direction: undo::Direction::Undo,
            ..
        } => "Could not undo",
        GitOp::Travel {
            direction: undo::Direction::Redo,
            ..
        } => "Could not redo",
    }
}

mod branch_lists;
mod confirm_guard;
mod custom_patch;
mod fixup;
mod leave_commits;
mod log_query;
mod rewrite_range;
mod stash_actions;

use custom_patch::NO_PATCH;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit_editor::Field;
    use crate::diff_pane::DIFF_PREFETCH;
    use crate::git::diff::DiffSide;
    use crate::git::status::{BranchStatus, EntryKind, Head, RepoStatus};
    use crate::git::submodule::SubmoduleState;
    use crate::intent::Scroll;
    use crate::keymap::KeyPress;
    use crate::menu::MenuId;
    use crate::popup::{
        DiscardConfirm, DiscardWhat, LeaveCommitsConfirm, PromptKind, ResetHardConfirm,
    };
    use crate::test_support::*;
    use crossterm::event::{KeyCode, KeyModifiers};

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
        // Seq 1 went to the branch list `r` reloads too, 2 to the remotes,
        // 3 to the tags, 4 to 6 to the commits, 7 to the stashes.
        assert_eq!(
            second,
            Tag {
                generation: 7,
                seq: 8,
                user: true
            }
        );
    }

    #[test]
    fn refresh_in_review_does_nothing() {
        let mut state = AppState::new(Context::Review { number: Some(3) });
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
                    commit: None,
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

    #[test]
    fn a_bound_key_runs_its_intent() {
        let mut state = AppState::new(Context::Workspace);
        // The status, the branch list, the remotes, the tags, the commits,
        // their markers and the branch tips, then the stashes.
        assert_eq!(press(&mut state, KeyCode::Char('r')).len(), 8);
        assert_eq!(
            state.busy, 4,
            "the status, the branch list, the remotes and the tags"
        );
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
        assert_eq!(state.notice, Some("An operation is running".into()));
        assert_eq!(state.busy, busy);
    }

    #[test]
    fn space_without_a_selection_says_so() {
        let (mut state, _) = with_files(vec![], 24);
        assert_eq!(press(&mut state, KeyCode::Char(' ')), vec![]);
        assert_eq!(state.notice, Some("No file selected".into()));
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
            Some("Resolve conflicts before staging everything".into())
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
        let mut review = AppState::new(Context::Review { number: Some(1) });
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
                    commit: None,
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
                fixup: None,
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
        assert_eq!(state.notice, Some("Write a subject first".into()));
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
                fixup: None,
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
        assert_eq!(
            state.notice,
            Some("Resolve conflicts before committing".into())
        );
        assert_eq!(state.commit, CommitFlow::Closed);

        let (mut state, _) = with_files(vec![], 24);
        press(&mut state, KeyCode::Char('c'));
        assert_eq!(state.notice, Some("Nothing to commit".into()));

        let (mut state, _) = with_files(three_files(), 24);
        state.op_seq = Some(5);
        press(&mut state, KeyCode::Char('c'));
        assert_eq!(state.notice, Some("An operation is running".into()));
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
        assert_eq!(state.notice, Some("An operation is running".into()));
        assert!(!state.should_quit);
    }

    #[test]
    fn a_successful_commit_closes_the_popup_and_refreshes() {
        let (mut state, tag, op) = submitted();
        let effects = state.update(finished(tag, op, Ok(())));
        assert_eq!(state.commit, CommitFlow::Closed);
        assert_eq!(state.op_seq, None);
        assert!(
            matches!(
                effects.as_slice(),
                [
                    Effect::LoadStatus { .. },
                    Effect::LoadBranches { .. },
                    Effect::LoadRemotes { .. },
                    Effect::LoadTags { .. },
                    Effect::LoadCommits { skip: 0, .. },
                    Effect::LoadMarkers { .. },
                    Effect::LoadBranchTips { .. },
                ]
            ),
            "{effects:?}"
        );
    }

    #[test]
    fn a_failed_commit_shows_its_output_and_esc_returns_to_the_kept_message() {
        let (mut state, tag, op) = submitted();
        let effects = state.update(finished(tag, op, Err("lint failed".into())));

        assert_eq!(state.op_error, None);
        assert!(
            matches!(
                effects.as_slice(),
                [
                    Effect::LoadStatus { .. },
                    Effect::LoadBranches { .. },
                    Effect::LoadRemotes { .. },
                    Effect::LoadTags { .. },
                    Effect::LoadCommits { skip: 0, .. },
                    Effect::LoadMarkers { .. },
                    Effect::LoadBranchTips { .. },
                ]
            ),
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

    fn hooks_only() -> Option<PromptFacts> {
        Some(PromptFacts {
            signs: false,
            hooks: vec!["pre-commit", "commit-msg", "post-commit"],
        })
    }

    #[test]
    fn with_hooks_detached_a_commit_that_only_runs_hooks_stays_detached() {
        for (mut state, tag, op) in submissions() {
            state.commit_hooks_detached = true;
            let effects = state.update(probed(tag, &op, hooks_only()));
            assert_eq!(effects, vec![Effect::RunOp { tag, op }]);
            assert!(!state.commit_on_terminal);
        }
    }

    #[test]
    fn without_hooks_detached_a_commit_with_hooks_is_handed_the_terminal() {
        let (mut state, tag, op) = submitted();
        assert!(!state.commit_hooks_detached, "off by default");
        assert_eq!(
            state.update(probed(tag, &op, hooks_only())),
            vec![Effect::HandOff(HandOff::Commit { tag, op })]
        );
        assert!(state.commit_on_terminal);
    }

    #[test]
    fn with_hooks_detached_a_commit_that_signs_is_still_handed_the_terminal() {
        for (mut state, tag, op) in submissions() {
            state.commit_hooks_detached = true;
            let effects = state.update(probed(tag, &op, signing()));
            assert_eq!(effects, vec![Effect::HandOff(HandOff::Commit { tag, op })]);
            assert!(state.commit_on_terminal);
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
                fixup: None,
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
                fixup: None,
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
        assert_eq!(state.notice, Some("No commit to amend".into()));
        assert_eq!(state.commit, CommitFlow::Closed);

        let (mut state, _) = with_files(three_files(), 24);
        state.op_seq = Some(7);
        assert_eq!(press(&mut state, KeyCode::Char('A')), vec![]);
        assert_eq!(state.notice, Some("An operation is running".into()));
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
                fixup: None,
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
        assert_eq!(state.notice, Some("Empty message; commit aborted".into()));
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
        let last = menu::items(MenuId::Actions).len() - 1;
        for _ in 0..last + 2 {
            press(&mut state, KeyCode::Char('j'));
        }
        assert_eq!(state.popups, [menu(MenuId::Actions, last)]);
        press(&mut state, KeyCode::Up);
        assert_eq!(state.popups, [menu(MenuId::Actions, last - 1)]);
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

        assert_eq!(state.notice, Some("No file selected".into()));
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
        let mut state = AppState::new(Context::Review { number: Some(1) });
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
        assert_eq!(state.notice, Some("No commit to amend".into()));
    }

    #[test]
    fn the_quit_item_asks_first_and_y_quits() {
        let (mut state, _) = with_files(three_files(), 24);
        state.update(AppIntent::OpenMenu(MenuId::Actions));
        // Quit is the last item.
        for _ in 1..menu::items(MenuId::Actions).len() {
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
        assert_eq!(state.notice, Some("No file matches".into()));
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

    const ONE_HUNK: &[u8] = b"diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n\
        @@ -9 +9 @@\n-e\n+f\n";
    const BINARY: &[u8] = b"diff --git a/a.rs b/a.rs\nindex 1..2 100644\n\
        Binary files a/a.rs and b/a.rs differ\n";

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
                commit: None,
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
        assert_eq!(state.notice, Some("No file selected".into()));
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
            Some("Binary file: stage it from the Files panel".into())
        );
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn space_is_refused_in_order_of_what_matters_most() {
        let mut state = focused(three_files(), TWO_HUNKS);
        state.op_seq = Some(1);
        press(&mut state, KeyCode::Char(' '));
        assert_eq!(state.notice, Some("An operation is running".into()));
        state.op_seq = None;

        let mut loading = state.clone();
        press(&mut loading, KeyCode::Char('t'));
        press(&mut loading, KeyCode::Char(' '));
        assert_eq!(loading.notice, Some("Loading…".into()));

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
            assert_eq!(odd.notice, Some(reason.into()), "{kind:?}");
        }

        let mut empty = focused(three_files(), b"");
        press(&mut empty, KeyCode::Char(' '));
        assert_eq!(empty.notice, Some("No changes in this view".into()));
    }

    #[test]
    fn a_submodule_is_refused_by_the_patch_engine() {
        let submodule = b"diff --git a/s b/s\nindex 1..2 160000\n--- a/s\n+++ b/s\n\
            @@ -1 +1 @@\n-Subproject commit 1\n+Subproject commit 2\n";
        let mut state = focused(three_files(), submodule);
        assert_eq!(press(&mut state, KeyCode::Char(' ')), vec![]);
        assert_eq!(
            state.notice,
            Some("This change can only be staged from the Files panel".into())
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
        assert_eq!(
            state.notice,
            Some("Untracked file: nothing is staged".into())
        );
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
            Some("New file: press Esc, then Space for the whole file".into())
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
        assert_eq!(empty.notice, Some("No changes in this view".into()));
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
            assert_eq!(state.notice, Some(reason.into()));
            assert_eq!(state.popups, []);
        }
    }

    #[test]
    fn discard_does_nothing_in_review_or_while_an_op_runs() {
        let mut review = AppState::new(Context::Review { number: Some(1) });
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
        assert_eq!(state.notice, Some("Unstage first to discard".into()));
        assert_eq!(state.popups, []);

        let mut binary = focused(three_files(), BINARY);
        assert_eq!(press(&mut binary, KeyCode::Char('d')), vec![]);
        assert_eq!(
            binary.notice,
            Some(patch::PatchError::Binary.message().into())
        );
        assert_eq!(binary.popups, []);

        // Still loading: no patch yet.
        let (mut loading, _) = with_files(three_files(), 24);
        press(&mut loading, KeyCode::Enter);
        assert_eq!(press(&mut loading, KeyCode::Char('d')), vec![]);
        assert_eq!(loading.notice, Some("Loading…".into()));
        assert_eq!(loading.popups, []);

        // The patch failed to load: nothing to discard from.
        let (tag, _) = patch_load(&press(&mut state, KeyCode::Char('t')));
        state.update(AppIntent::PatchLoaded {
            tag,
            result: Err("fatal: bad".into()),
        });
        state.popups.clear();
        assert_eq!(state.update(AppIntent::Discard), vec![]);
        assert_eq!(state.notice, Some(NO_CHANGES.into()));
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
            assert_eq!(state.notice, Some("The file is deleted".into()));
        }
        let mut state = focused(vec![changed("gone.rs", '.', 'D')], ONE_HUNK);
        assert_eq!(press(&mut state, KeyCode::Char('o')), vec![]);
        assert_eq!(state.notice, Some("The file is deleted".into()));
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
        assert_eq!(
            staged.notice,
            Some("Edit hunks from the unstaged view".into())
        );

        let new_file = b"diff --git a/n.txt b/n.txt\nnew file mode 100644\n\
            --- /dev/null\n+++ b/n.txt\n@@ -0,0 +1,2 @@\n+x\n+y\n";
        let mut added = focused(vec![untracked("n.txt")], new_file);
        assert_eq!(press(&mut added, KeyCode::Char('e')), vec![]);
        assert_eq!(
            added.notice,
            Some("Only hunks of changed files can be edited".into())
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
        assert_eq!(state.notice, Some("Edit cancelled".into()));
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
        let mut review = AppState::new(Context::Review { number: Some(1) });
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
            assert_eq!(state.notice, Some("Already at the most context".into()));
        }
        assert_eq!(state.update(AppIntent::MoreContext), vec![]);
        assert_eq!(state.diff_options.context, MAX_CONTEXT);

        state.diff_options.context = MIN_CONTEXT;
        assert_eq!(press_char(&mut state, '-'), vec![]);
        assert_eq!(state.notice, Some("Already at the least context".into()));
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
            assert_eq!(state.notice, Some(REASON.into()), "{c:?}");
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
            assert_eq!(state.notice, Some(REASON.into()), "{c:?}");
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
        let screen = ratatui::layout::Rect::new(0, 0, 80, 24);
        assert_eq!(state.help_scroll, total - layout::help_rows(screen));
        press(&mut state, KeyCode::Esc);
        press(&mut state, KeyCode::Char('?'));
        assert_eq!(state.help_scroll, 0);
        assert!(!state.should_quit);
    }

    #[test]
    fn help_scrolls_to_its_last_rows_at_a_short_height() {
        let (mut state, _) = with_files(three_files(), 8);
        press(&mut state, KeyCode::Char('?'));
        let focus = state.panel_focus();
        let commands = keymap::available_commands(&state.keymap, focus).len();
        let total = keymap::available(keymap::registry(), &state.keymap, focus, &state).len()
            + usize::from(commands > 0)
            + commands;
        let mut last = usize::MAX;
        while state.help_scroll != last {
            last = state.help_scroll;
            press(&mut state, KeyCode::PageDown);
        }
        let screen = ratatui::layout::Rect::new(0, 0, 80, 8);
        assert_eq!(state.help_scroll, total - layout::help_rows(screen));
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
        // Commits and Stash have keys of their own.
        state.update(AppIntent::FocusPanel(crate::layout::Panel::Side(
            SidePanel::Commits,
        )));
        assert_eq!(state.key_focus(), KeyScope::Commits);
        state.update(AppIntent::FocusPanel(crate::layout::Panel::Side(
            SidePanel::Stash,
        )));
        assert_eq!(state.key_focus(), KeyScope::Stash);
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
        assert_eq!(state.notice, Some("No file selected".into()));
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

    // --- The Commits panel ---

    use crate::commits::{PAGE, PREFETCH};
    use crate::git::log::Markers;

    fn markers_tag(effects: &[Effect]) -> Tag {
        match effects
            .iter()
            .find(|e| matches!(e, Effect::LoadMarkers { .. }))
        {
            Some(Effect::LoadMarkers { tag }) => *tag,
            _ => panic!("expected LoadMarkers in {effects:?}"),
        }
    }

    #[test]
    fn startup_loads_the_first_page_and_the_markers_without_waiting_for_focus() {
        let mut state =
            AppState::new(Context::Workspace).with_location(Some(TOP.into()), TOP.into());
        let effects = state.update(AppIntent::Start);
        let (tag, tip, skip) = commits_load(&effects);
        assert_eq!((tip, skip), (None, 0));
        assert!(!tag.user, "the panel loads quietly");
        markers_tag(&effects);
        assert_eq!(state.focus, Focus::Files);
    }

    #[test]
    fn a_loaded_page_fills_the_rows_and_the_graph() {
        let state = with_commits(3, true);
        let commits = &state.commits;
        assert!(commits.loaded && commits.done);
        assert_eq!(commits.rows.len(), 3);
        assert_eq!(commits.graph.len(), 3);
        assert_eq!(commits.tip.as_deref(), Some("c0"));
        assert_eq!(commits.loaded_at, 1_000);
        assert_eq!(commits.page_seq, None);
    }

    #[test]
    fn j_and_k_move_the_commit_cursor_within_the_list() {
        let mut state = with_commits(3, true);
        assert_eq!(state.key_focus(), KeyScope::Commits);
        for _ in 0..5 {
            press(&mut state, KeyCode::Char('j'));
        }
        assert_eq!(state.commits.selected, 2);
        press(&mut state, KeyCode::Up);
        assert_eq!(state.commits.selected, 1);
    }

    #[test]
    fn nearing_the_end_loads_the_next_page_once_from_the_same_tip() {
        let mut state = with_commits(PAGE, false);
        state.commits.selected = PAGE - PREFETCH - 2;
        assert_eq!(state.update(AppIntent::CommitNext), vec![]);

        let effects = state.update(AppIntent::CommitNext);
        let (tag, tip, skip) = commits_load(&effects);
        assert_eq!((tip.as_deref(), skip), (Some("c0"), PAGE));
        assert_eq!(effects.len(), 1);
        // Not again while that page is on its way.
        assert_eq!(state.update(AppIntent::CommitNext), vec![]);
        assert!(state.commits.more_in_flight);

        state.update(commits_loaded(tag, PAGE, page(PAGE, 10, true)));
        assert_eq!(state.commits.rows.len(), PAGE + 10);
        assert_eq!(state.commits.graph.len(), PAGE + 10);
        assert_eq!(state.commits.rows[PAGE].oid, format!("c{PAGE}"));
        assert!(!state.commits.more_in_flight);
    }

    #[test]
    fn a_done_page_stops_further_loads() {
        let mut state = with_commits(10, true);
        for _ in 0..20 {
            assert_eq!(state.update(AppIntent::CommitNext), vec![]);
        }
        assert_eq!(state.commits.selected, 9);
    }

    #[test]
    fn stale_pages_and_markers_are_dropped() {
        let mut state = with_commits(2, true);
        let effects = state.update(AppIntent::RefreshStatus);
        let (old, _, _) = commits_load(&effects);
        let old_markers = markers_tag(&effects);
        let effects = state.update(AppIntent::RefreshStatus);
        let (new, _, _) = commits_load(&effects);
        let new_markers = markers_tag(&effects);

        state.update(commits_loaded(old, 0, page(50, 3, true)));
        let unpushed = Markers {
            unpushed: Some(["c0".to_string()].into()),
            ..Markers::default()
        };
        state.update(AppIntent::MarkersLoaded {
            tag: old_markers,
            result: Ok(unpushed.clone()),
        });
        assert_eq!(
            state.commits.rows[0].oid, "c0",
            "the superseded page is dropped"
        );
        assert_eq!(state.commits.markers, Markers::default());

        // Right seq, but for a repository left since.
        let mut other = state.clone();
        other.generation += 1;
        other.update(commits_loaded(new, 0, page(50, 3, true)));
        other.update(AppIntent::MarkersLoaded {
            tag: new_markers,
            result: Ok(unpushed.clone()),
        });
        assert_eq!(other.commits.rows[0].oid, "c0");
        assert_eq!(other.commits.markers, Markers::default());

        state.update(commits_loaded(new, 0, page(50, 3, true)));
        state.update(AppIntent::MarkersLoaded {
            tag: new_markers,
            result: Ok(unpushed.clone()),
        });
        assert_eq!(state.commits.rows[0].oid, "c50");
        assert_eq!(state.commits.markers, unpushed);
    }

    #[test]
    fn a_reload_keeps_the_old_rows_until_it_lands_then_the_cursor_on_the_same_commit() {
        let mut state = with_commits(5, true);
        state.commits.selected = 2;
        let (tag, _, _) = commits_load(&state.update(AppIntent::RefreshStatus));
        assert_eq!(state.commits.rows.len(), 5, "shown while the reload runs");

        // A new commit on top moves c2 one row down.
        let mut newer = page(1, 4, true);
        newer.rows.insert(0, page(99, 1, true).rows.remove(0));
        newer.rows.splice(1..1, page(0, 1, true).rows);
        state.update(commits_loaded(tag, 0, newer));
        assert_eq!(state.commits.rows[state.commits.selected].oid, "c2");
        assert_eq!(state.commits.tip.as_deref(), Some("c99"));

        // Gone: the cursor stays near where it was, inside the list.
        state.commits.selected = 5;
        let (tag, _, _) = commits_load(&state.update(AppIntent::RefreshStatus));
        state.update(commits_loaded(tag, 0, page(10, 2, true)));
        assert_eq!(state.commits.selected, 1);
    }

    #[test]
    fn a_finished_commit_reloads_the_commits_from_the_top() {
        let mut state = with_commits(3, true);
        let effects = state.apply_plan(refresh::plan(&GitOp::Commit {
            message: Some("m".into()),
            stage_all: false,
            amend: false,
            no_verify: false,
            fixup: None,
        }));
        let (_, tip, skip) = commits_load(&effects);
        assert_eq!((tip, skip), (None, 0));
        markers_tag(&effects);
    }

    #[test]
    fn a_checkout_or_new_branch_reloads_the_commits_and_rename_or_delete_do_not() {
        let moves = [
            GitOp::Checkout("side".into()),
            GitOp::CreateBranch {
                name: "new".into(),
                from: "main".into(),
            },
        ];
        for op in moves {
            let mut state = with_commits(3, true);
            let effects = state.apply_plan(refresh::plan(&op));
            let (_, tip, skip) = commits_load(&effects);
            assert_eq!((tip, skip), (None, 0), "{op:?}");
            markers_tag(&effects);
        }
        let stays = [
            GitOp::RenameBranch {
                from: "a".into(),
                to: "b".into(),
            },
            GitOp::DeleteBranch {
                name: "a".into(),
                force: false,
            },
        ];
        for op in stays {
            let mut state = with_commits(3, true);
            let effects = state.apply_plan(refresh::plan(&op));
            assert!(
                !effects
                    .iter()
                    .any(|e| matches!(e, Effect::LoadCommits { .. } | Effect::LoadMarkers { .. })),
                "{op:?}: {effects:?}"
            );
        }
    }

    #[test]
    fn switching_repositories_resets_the_panel_and_loads_the_new_one() {
        let mut state = with_commits(3, true);
        let effects = state.update(AppIntent::OpenRepo("/r/other".into()));
        assert_eq!(state.commits.rows, []);
        assert!(!state.commits.loaded);
        let (tag, _, skip) = commits_load(&effects);
        assert_eq!((tag.generation, skip), (state.generation, 0));
    }

    // --- A commit's files ---

    use crate::git::commit_files::{CommitFile, CommitId};

    #[test]
    fn enter_opens_the_commit_against_its_first_parent_as_a_user_load() {
        let mut state = on_commit_five();
        let busy = state.busy;

        let (tag, range) = files_load(&press(&mut state, KeyCode::Enter));

        assert_eq!(range, range_of("c5", "c6"));
        assert!(tag.user);
        assert_eq!(state.busy, busy + 1);
        let open = state.commits.open.as_ref().unwrap();
        assert_eq!((open.short_oid.as_str(), open.loaded), ("c5", false));
        assert_eq!(state.key_focus(), KeyScope::CommitFiles);
        assert_eq!(state.commits.selected, 5);
    }

    #[test]
    fn a_commit_whose_id_or_parent_is_not_hex_opens_nothing_and_says_why() {
        for (oid, parent) in [("--output=x", "c6"), ("c5", "-p")] {
            let mut state = on_commit_five();
            state.commits.rows[5].oid = oid.into();
            state.commits.rows[5].parents = vec![parent.into()];

            assert_eq!(state.update(AppIntent::OpenCommitFiles), vec![]);

            assert_eq!(state.commits.open, None);
            let error = state.commits.error.as_deref().unwrap();
            assert!(error.starts_with("not a commit id"), "{error}");
        }
    }

    #[test]
    fn enter_with_no_commits_is_disabled() {
        let mut state = on_commit_five();
        state.commits.rows.clear();
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice, Some("No commits".into()));
    }

    #[test]
    fn the_loaded_files_show_the_first_ones_diff_in_that_commit_and_j_the_next() {
        let mut state = on_commit_five();
        let busy = state.busy;
        let range = range_of("c5", "c6");

        let effects = opened(&mut state, two_commit_files());

        assert_eq!(state.busy, busy);
        let request = diff_request(&effects);
        assert_eq!(request.commit, Some(range.clone()));
        assert_eq!(request.path, "src/lib.rs".into());
        assert_eq!(state.diff.path, Some("src/lib.rs".into()));

        let request = diff_request(&press(&mut state, KeyCode::Char('j')));
        assert_eq!(request.commit, Some(range));
        assert_eq!(
            (request.path, request.orig_path),
            ("new.rs".into(), Some("old.rs".into()))
        );
        // The ends of the list load nothing more.
        assert_eq!(press(&mut state, KeyCode::Down), vec![]);
        let request = diff_request(&state.update(AppIntent::CommitFilePrev));
        assert_eq!(request.path, "src/lib.rs".into());
    }

    #[test]
    fn a_stale_or_other_repositorys_file_list_is_dropped_but_lowers_busy() {
        let stale: [fn(Tag) -> Tag; 2] = [
            |tag| Tag {
                seq: tag.seq + 1,
                ..tag
            },
            |tag| Tag {
                generation: tag.generation + 1,
                ..tag
            },
        ];
        for stale in stale {
            let mut state = on_commit_five();
            let (tag, _) = files_load(&press(&mut state, KeyCode::Enter));
            let busy = state.busy;
            let effects = state.update(AppIntent::CommitFilesLoaded {
                tag: stale(tag),
                result: Ok(two_commit_files()),
            });
            assert_eq!(effects, vec![]);
            assert_eq!(state.busy, busy - 1);
            assert!(!state.commits.open.as_ref().unwrap().loaded);
        }
    }

    #[test]
    fn a_commit_with_no_files_or_a_failed_list_empties_the_diff() {
        for result in [Ok(vec![]), Err("bad object".to_string())] {
            let mut state = on_commit_five();
            let (tag, _) = files_load(&press(&mut state, KeyCode::Enter));
            let effects = state.update(AppIntent::CommitFilesLoaded { tag, result });
            assert_eq!(effects, vec![Effect::CancelDiff]);
            assert_eq!(state.diff, DiffPane::default());
        }
    }

    #[test]
    fn esc_goes_back_to_the_same_commit_and_reloads_the_worktree_diff() {
        let mut state = on_commit_five();
        opened(&mut state, two_commit_files());
        press(&mut state, KeyCode::Char('j'));

        let effects = press(&mut state, KeyCode::Esc);

        assert!(!state.should_quit);
        assert_eq!(state.commits.open, None);
        assert_eq!(state.commits.selected, 5);
        assert_eq!(state.key_focus(), KeyScope::Commits);
        let request = diff_request(&effects);
        assert_eq!((request.commit, request.path), (None, "a.rs".into()));
    }

    #[test]
    fn closing_with_no_worktree_file_empties_the_diff() {
        let mut state = on_commit_five();
        opened(&mut state, two_commit_files());
        state.files.clear();
        state.selected = None;
        assert_eq!(
            state.update(AppIntent::CloseCommitFiles),
            vec![Effect::CancelDiff]
        );
        assert_eq!(state.diff, DiffPane::default());
    }

    #[test]
    fn a_status_refresh_while_open_keeps_the_commit_diff() {
        let mut state = on_commit_five();
        opened(&mut state, two_commit_files());
        let diff = state.diff.clone();

        for files in [vec![changed("b.rs", 'M', '.')], vec![]] {
            let tag = refresh_tag(&mut state);
            let effects = state.update(AppIntent::StatusLoaded {
                tag,
                result: Ok(repo(files.clone())),
            });
            assert!(
                !effects
                    .iter()
                    .any(|e| matches!(e, Effect::LoadDiff { .. } | Effect::CancelDiff)),
                "{effects:?}"
            );
            assert_eq!(state.diff, diff);
            assert_eq!(state.selected, (!files.is_empty()).then_some(0));
        }
    }

    #[test]
    fn new_diff_options_reload_the_open_commits_file() {
        let mut state = on_commit_five();
        opened(&mut state, two_commit_files());
        let request = diff_request(&state.update(AppIntent::ToggleWhitespace));
        assert!(request.options.ignore_whitespace);
        assert_eq!(request.commit, Some(range_of("c5", "c6")));
        assert_eq!(request.path, "src/lib.rs".into());
    }

    #[test]
    fn the_diff_takes_focus_on_an_open_commits_file_to_pick_its_lines() {
        let mut state = on_commit_five();
        opened(&mut state, two_commit_files());
        let (_, request) = patch_load(&press(&mut state, KeyCode::Char('l')));
        assert_eq!(request.commit, Some(range_of("c5", "c6")));
        assert_eq!(request.path, "src/lib.rs".into());
        let view = view(&state);
        assert_eq!(view.commit, Some(range_of("c5", "c6")));
        assert_eq!(view.path, "src/lib.rs".into());
    }

    #[test]
    fn the_diff_cannot_take_focus_from_a_comparison() {
        let mut state = on_commit_five();
        opened(&mut state, two_commit_files());
        state.commits.open.as_mut().unwrap().against = Some("c2".into());
        assert_eq!(
            state.update(AppIntent::FocusPanel(crate::layout::Panel::Diff)),
            vec![]
        );
        assert_eq!(state.focus, Focus::Commits);
        assert!(matches!(
            custom_patch_tests::key_reason(&state, KeyScope::CommitFiles, 'l'),
            Some(r) if r.contains("comparison")
        ));
    }

    #[test]
    fn focusing_another_panel_closes_the_commit_and_shows_the_worktree_diff() {
        let mut state = on_commit_five();
        opened(&mut state, two_commit_files());

        let effects = press(&mut state, KeyCode::Char('2'));

        assert_eq!(state.commits.open, None);
        assert_eq!(state.focus, Focus::Files);
        let request = diff_request(&effects);
        assert_eq!((request.commit, request.path), (None, "a.rs".into()));

        // Focusing the Commits panel again from itself keeps a commit open.
        let mut state = on_commit_five();
        opened(&mut state, two_commit_files());
        assert_eq!(press(&mut state, KeyCode::Char('4')), vec![]);
        assert!(state.commits.open.is_some());
    }

    // --- Watching for outside changes ---

    use crate::git::refs::RefsSnapshot;
    use crate::watch::WatchEvent;

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
        let effects = state.update(refs_loaded(moved, b"b"));
        let tag = background_status_tag(&effects[..1]);
        assert_eq!(state.status_seq, Some(tag.seq));
        // Branches or their upstreams may have moved too, out of the busy count.
        // The log moved too.
        match &effects[1..] {
            [
                Effect::LoadBranches { tag },
                Effect::LoadRemotes { tag: remotes },
                Effect::LoadTags { tag: tags },
                Effect::LoadCommits { skip: 0, .. },
                Effect::LoadMarkers { .. },
                Effect::LoadBranchTips { .. },
                Effect::LoadStashes { .. },
            ] => assert!(!tag.user && !remotes.user && !tags.user),
            other => panic!("expected background LoadBranches and the commits, got {other:?}"),
        }
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
        let (tag, list, remotes, tags) = match press(&mut state, KeyCode::Enter).as_slice() {
            [
                _,
                Effect::LoadStatus { tag },
                Effect::LoadBranches { tag: list },
                Effect::LoadRemotes { tag: remotes },
                Effect::LoadTags { tag: tags },
                ..,
            ] => (*tag, *list, *remotes, *tags),
            other => panic!("expected SetRepo, LoadStatus and LoadBranches, got {other:?}"),
        };
        state.update(loaded(tag, &[]));
        state.update(AppIntent::BranchesLoaded {
            tag: list,
            result: Ok(vec![]),
        });
        state.update(AppIntent::RemotesLoaded {
            tag: remotes,
            result: Ok(vec![]),
        });
        state.update(AppIntent::TagsLoaded {
            tag: tags,
            result: Ok(vec![]),
        });
        assert_eq!(state.busy, 0);

        let refs = snapshot_tag(&watch(&mut state, WatchEvent::PollRefs));
        assert!(state.update(refs_loaded(refs, b"sub")).is_empty());
        assert_eq!(state.watch.refs, Some(RefsSnapshot::of(b"sub")));
    }

    #[test]
    fn watch_events_only_reload_the_config_in_review() {
        let mut state = AppState::new(Context::Review { number: Some(1) });
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
            dropped_binding: false,
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
    fn dropped_key_changes_say_the_rest_of_the_file_was_used() {
        let dropped = |message: &str| ConfigError {
            dropped_binding: true,
            ..config_error(message)
        };
        let hint = |errors: Vec<ConfigError>| {
            let mut state = idle_with_files();
            state.update(config_loaded(Keymap::default(), errors));
            match state.popups.as_slice() {
                [Popup::Error(error)] => {
                    assert_eq!(error.title, "Config error");
                    error.next.clone()
                }
                other => panic!("expected one config error, got {other:?}"),
            }
        };
        let only_dropped = hint(vec![dropped("a"), dropped("b")]);
        assert!(
            only_dropped.starts_with("Kika dropped these entries and used the rest of the file."),
            "{only_dropped}"
        );
        assert!(!only_dropped.contains("ignored"), "{only_dropped}");
        let mixed = hint(vec![dropped("a"), config_error("b")]);
        assert!(mixed.contains("ignored this file"), "{mixed}");
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

    fn renderer(label: &str) -> ExternalRenderer {
        ExternalRenderer {
            label: label.into(),
            argv: vec![label.into()],
            kind: crate::external_diff::RendererKind::Pager,
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
        assert_eq!(state.notice, Some(external_diff::NO_RENDERER.into()));
        assert_eq!(state.renderer, None);
        state.notice = None;
        press(&mut state, KeyCode::Enter);
        assert_eq!(press(&mut state, KeyCode::Char('R')), vec![]);
        assert_eq!(state.notice, Some(external_diff::NO_RENDERER.into()));
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

    fn tool(label: &str) -> ExternalRenderer {
        ExternalRenderer {
            kind: crate::external_diff::RendererKind::Tool,
            ..renderer(label)
        }
    }

    #[test]
    fn r_cycles_through_the_pagers_then_the_tool_then_built_in() {
        let mut state = with_renderers(three_files(), &[]);
        state.update(AppIntent::RenderersFound(vec![
            renderer("delta"),
            tool("difft"),
        ]));
        let mut seen = Vec::new();
        for _ in 0..3 {
            state.update(AppIntent::CycleRenderer);
            seen.push(state.active_renderer().map(|r| r.label.clone()));
        }
        assert_eq!(seen, [Some("delta".into()), Some("difft".into()), None]);

        state.update(AppIntent::RenderersFound(vec![tool("difft")]));
        let mut seen = Vec::new();
        for _ in 0..2 {
            state.update(AppIntent::CycleRenderer);
            seen.push((
                state.active_renderer().map(|r| r.label.clone()),
                state.display_only(),
            ));
        }
        assert_eq!(seen, [(Some("difft".into()), true), (None, false)]);
    }

    /// The first of `three_files` focused with `TWO_HUNKS` loaded, drawn
    /// by tool `difft` as rows that match no patch line.
    fn focused_through_tool() -> AppState {
        let mut state = with_renderers(three_files(), &[]);
        state.update(AppIntent::RenderersFound(vec![tool("difft")]));
        state.update(AppIntent::CycleRenderer);
        let effects = press(&mut state, KeyCode::Enter);
        let (tag, _) = patch_load(&effects);
        state.update(AppIntent::PatchLoaded {
            tag,
            result: Ok(patch_of(TWO_HUNKS)),
        });
        let (tag, _, _) = render_of(&effects);
        state.update(rendered(tag, "1 old one   1 new one\n2 same      2 same\n"));
        state
    }

    #[test]
    fn the_tool_view_shows_its_rows_unmapped() {
        let state = focused_through_tool();
        assert!(state.display_only());
        let shown = state.rendered_shown().expect("the tool rows are shown");
        assert_eq!(shown.rows.len(), 2);
        assert!(shown.map.is_empty());
    }

    #[test]
    fn in_the_tool_view_the_hunk_keys_say_why_and_keep_the_selection() {
        for code in [KeyCode::Char('j'), KeyCode::Down, KeyCode::Char('k')] {
            let mut state = focused_through_tool();
            assert_eq!(view(&state).hunk, 0);
            assert_eq!(press(&mut state, code), vec![], "{code:?}");
            assert_eq!(state.notice, Some(keymap::DISPLAY_ONLY.into()), "{code:?}");
            assert_eq!(view(&state).hunk, 0, "{code:?}");
        }
    }

    #[test]
    fn the_tool_view_refuses_staging_discard_and_hunk_edits() {
        for intent in [
            AppIntent::ToggleHunkStage,
            AppIntent::EnterLineMode,
            AppIntent::Discard,
            AppIntent::EditHunk,
        ] {
            let mut state = focused_through_tool();
            assert_eq!(state.update(intent.clone()), vec![], "{intent:?}");
            assert_eq!(
                state.notice,
                Some(keymap::DISPLAY_ONLY.into()),
                "{intent:?}"
            );
            assert_eq!(state.popups, [], "{intent:?}");
            assert_eq!(view(&state).lines, None, "{intent:?}");
        }
        // Line mode entered before the tool, then staged by intent.
        let mut state = focused_through(false);
        state.update(AppIntent::EnterLineMode);
        state.update(AppIntent::RenderersFound(vec![tool("difft")]));
        state.update(AppIntent::CycleRenderer);
        state.notice = None;
        assert_eq!(state.update(AppIntent::ToggleLinesStage), vec![]);
        assert_eq!(state.notice, Some(keymap::DISPLAY_ONLY.into()));
    }

    #[test]
    fn back_on_the_built_in_view_a_hunk_stages_again() {
        let mut state = focused_through_tool();
        assert_eq!(state.update(AppIntent::ToggleHunkStage), vec![]);
        state.update(AppIntent::CycleRenderer);
        assert!(!state.display_only());
        op_of(&state.update(AppIntent::ToggleHunkStage));
    }

    #[test]
    fn the_tool_view_leaves_whole_file_staging_alone() {
        let mut state = with_renderers(three_files(), &[]);
        state.update(AppIntent::RenderersFound(vec![tool("difft")]));
        state.update(AppIntent::CycleRenderer);
        assert!(state.display_only());
        assert_eq!(state.focus, Focus::Files);
        op_of(&press_char(&mut state, ' '));
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

    // --- The Branches panel ---

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
    fn a_reload_keeps_the_selected_branch_by_name() {
        let mut state = with_branches();
        state.update(AppIntent::BranchNext);
        assert_eq!(selected_name(&state), "topic");

        let tag = branches_tag(&state.update(AppIntent::RefreshStatus));
        let mut moved = three_branches();
        moved.swap(1, 2);
        state.update(branches_loaded(tag, moved));

        assert_eq!(selected_name(&state), "topic");
        assert_eq!(state.branches.selected, Some(2));
    }

    #[test]
    fn a_stale_branch_list_or_log_is_dropped() {
        let mut state = with_branches();
        let old = branches_tag(&state.update(AppIntent::RefreshStatus));
        let new = branches_tag(&state.update(AppIntent::RefreshStatus));
        state.update(branches_loaded(old, vec![git_branch("stale", true, true)]));
        assert_eq!(state.branches.branches, three_branches());

        let mut other_repo = new;
        other_repo.generation += 1;
        state.update(branches_loaded(other_repo, vec![]));
        assert_eq!(state.branches.branches, three_branches());

        let (first, refname) = log_load(&state.update(AppIntent::BranchNext)).unwrap();
        let (second, _) = log_load(&state.update(AppIntent::BranchNext)).unwrap();
        state.update(AppIntent::BranchLogLoaded {
            tag: first,
            refname,
            result: Ok(vec![log_row("topic's")]),
        });
        assert!(
            state.branches.log.rows.is_empty(),
            "the log of wip is awaited"
        );
        state.update(AppIntent::BranchLogLoaded {
            tag: second,
            refname: b"refs/heads/wip".to_vec(),
            result: Ok(vec![log_row("wip's")]),
        });
        assert_eq!(state.branches.log.rows, [log_row("wip's")]);
    }

    #[test]
    fn moving_the_selection_loads_the_new_branch_log() {
        let mut state = with_branches();
        let (tag, refname) = log_load(&state.update(AppIntent::BranchNext)).unwrap();
        assert_eq!(refname, b"refs/heads/topic");
        assert!(tag.user);
        assert_eq!(
            state.branches.log.for_ref.as_deref(),
            Some(&b"refs/heads/topic"[..])
        );
        // At the end there is nowhere to go, so nothing loads.
        state.update(AppIntent::BranchNext);
        assert_eq!(state.update(AppIntent::BranchNext), vec![]);
    }

    #[test]
    fn j_and_k_move_the_branch_selection_and_shift_j_scrolls_its_log() {
        let mut state = with_branches();
        press_char(&mut state, 'j');
        assert_eq!(selected_name(&state), "topic");
        let (tag, refname) = log_load(&press_char(&mut state, 'j')).unwrap();
        state.update(AppIntent::BranchLogLoaded {
            tag,
            refname,
            result: Ok(vec![log_row("1"), log_row("2")]),
        });
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
        state.update(AppIntent::BranchNext);
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
        assert_eq!(state.update(AppIntent::CheckoutBranch), vec![]);
        assert_eq!(state.notice, Some("Already on this branch".into()));
        assert_eq!(state.update(AppIntent::DeleteBranch), vec![]);
        assert_eq!(
            state.notice,
            Some("Cannot delete the current branch".into())
        );
        assert_eq!(state.popups, []);
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn branch_actions_are_refused_while_an_op_runs_or_with_nothing_selected() {
        let mut state = with_branches();
        state.update(AppIntent::BranchNext);
        state.op_seq = Some(99);
        for intent in [
            AppIntent::CheckoutBranch,
            AppIntent::NewBranch,
            AppIntent::RenameBranch,
            AppIntent::DeleteBranch,
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
        empty.update(AppIntent::CheckoutBranch);
        assert_eq!(empty.notice, Some("No branch selected".into()));
    }

    #[test]
    fn deleting_a_merged_branch_asks_and_y_deletes_without_force() {
        let mut state = with_branches();
        state.update(AppIntent::BranchNext);
        press_char(&mut state, 'd');
        assert_eq!(state.key_focus(), KeyScope::DeleteBranch);
        // `D` answers only the question about an unmerged branch.
        state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('D'),
            KeyModifiers::SHIFT,
        )));
        assert_eq!(state.op_seq, None);
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
        state.update(AppIntent::BranchNext);
        state.update(AppIntent::BranchNext);
        press_char(&mut state, 'd');
        match state.popups.as_slice() {
            [Popup::DeleteBranch(confirm)] => assert!(confirm.force()),
            other => panic!("expected the delete question, got {other:?}"),
        }

        assert_eq!(press_char(&mut state, 'y'), vec![]);
        assert_eq!(state.popups.len(), 1, "y leaves the question open");
        assert_eq!(state.op_seq, None);

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
            state.update(AppIntent::BranchNext);
            press_char(&mut state, 'd');
            assert_eq!(press(&mut state, key), vec![]);
            assert_eq!(state.popups, []);
            assert!(!state.should_quit);
            assert_eq!(state.op_seq, None);
        }
    }

    #[test]
    fn the_create_prompt_uses_the_branch_selected_when_it_opened() {
        let mut state = with_branches();
        state.update(AppIntent::BranchNext);
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
        state.update(AppIntent::BranchNext);
        state.update(AppIntent::BranchNext);
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
        assert_eq!(state.op_seq, None);

        press_char(&mut state, 'n');
        assert!(state.branches.pending.is_some());
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.branches.pending, None);
        assert_eq!(
            state.update(AppIntent::CreateBranchNamed("x".into())),
            vec![]
        );
    }

    #[test]
    fn switching_repository_resets_the_panel() {
        let mut state = with_branches().with_location(Some("/r/a".into()), "/r/a".into());
        state.branches.tab = BranchesTab::Remotes;
        let effects = state.update(AppIntent::OpenRepo("/r/b".into()));
        assert!(!state.branches.loaded);
        assert_eq!(state.branches.branches, []);
        assert_eq!(state.branches.log, Default::default());
        assert_eq!(state.branches.tab, BranchesTab::Local);
        assert!(!state.branches.remotes.loaded);
        assert!(
            effects.iter().any(
                |e| matches!(e, Effect::LoadRemotes { tag } if tag.generation == state.generation)
            ),
            "{effects:?}"
        );
    }

    // --- The Remotes tab, and upstreams ---

    #[test]
    fn set_upstream_starts_from_the_upstream_or_origin_and_needs_a_listed_branch() {
        let mut state = with_remotes();
        state.update(AppIntent::BranchNext);
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
        state.branches.branches[0].upstream = Some("fork/x".into());
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
        state.branches.branches[0].upstream = Some("origin/main".into());
        let (_, op) = op_of(&shift(&mut state, 'U'));
        assert_eq!(op, GitOp::UnsetUpstream("main".into()));
    }

    #[test]
    fn a_failed_delete_says_the_branch_list_was_reloaded() {
        let mut state = with_branches();
        state.update(AppIntent::BranchNext);
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
        state.update(AppIntent::BranchNext);
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

    // --- Merge, rebase and the operation in progress ---

    #[test]
    fn a_bisect_alone_stops_a_merge_until_it_is_reset() {
        let mut state = in_progress(&[Operation::Bisect]);
        state.update(AppIntent::BranchNext);
        assert_eq!(
            state.update(AppIntent::Merge(MergeStrategy::FastForwardOnly)),
            vec![]
        );
        assert_eq!(state.notice, Some(bisect::RESET_FIRST.into()));
    }

    #[test]
    fn enter_still_answers_the_other_questions() {
        let mut state = AppState::new(Context::Workspace);
        state.update(AppIntent::Ask(ConfirmKind::Quit));
        assert_eq!(state.key_focus(), KeyScope::Confirm);
        press(&mut state, KeyCode::Enter);
        assert!(state.should_quit);
        assert_eq!(state.popups, []);
    }

    #[test]
    fn a_finished_squash_says_the_changes_wait_to_be_committed() {
        let mut state = in_progress(&[]);
        state.update(AppIntent::BranchNext);
        let (tag, op) = op_of(&state.update(AppIntent::Merge(MergeStrategy::Squash)));
        let effects = state.update(finished(tag, op, Ok(())));
        assert_eq!(
            state.notice,
            Some("Squashed changes are staged; commit them".into())
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::LoadStatus { .. })),
            "{effects:?}"
        );
        branches_tag(&effects);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::LoadCommits { .. })),
            "{effects:?}"
        );
    }

    #[test]
    fn failures_name_the_step_and_point_at_the_status_panel() {
        let conflicts = "If git stopped on conflicts, resolve them, then continue or abort from the Status panel.";
        let cases = [
            (
                GitOp::Merge {
                    branch: "b".into(),
                    strategy: MergeStrategy::NoFastForward,
                },
                "Could not merge",
                conflicts,
            ),
            (
                GitOp::Rebase { onto: "b".into() },
                "Could not rebase",
                conflicts,
            ),
            (
                GitOp::Continue(Operation::CherryPick),
                "Could not continue the cherry-pick",
                conflicts,
            ),
            (GitOp::Skip(Operation::Rebase), "Could not skip", conflicts),
            (
                GitOp::Abort(Operation::Merge),
                "Could not abort the merge",
                "The file list was reloaded. Fix the cause above, then try again.",
            ),
        ];
        for (op, title, next) in cases {
            assert_eq!(failure_title(&op), title, "{op:?}");
            assert_eq!(failure_next(&op), next, "{op:?}");
        }
    }

    // --- Checkout, revert and reset of a commit ---

    #[test]
    fn space_asks_before_a_detached_checkout_and_y_checks_out_the_commit() {
        let mut state = on_commit();
        assert_eq!(press_char(&mut state, ' '), vec![]);
        assert_eq!(
            state.popups,
            [Popup::Confirm(ConfirmKind::DetachedCheckout)]
        );
        assert_eq!(state.commits.pending, Some(cid("c1")));
        assert!(
            state
                .confirm_question(&ConfirmKind::DetachedCheckout)
                .starts_with("Check out c1 as a detached HEAD? You will be on no branch;"),
        );
        assert_eq!(state.op_seq, None);

        let (tag, op) = op_of(&press_char(&mut state, 'y'));
        assert_eq!(op, GitOp::CheckoutCommit(cid("c1")));
        assert_eq!(state.op_seq, Some(tag.seq));
        assert_eq!(state.popups, []);
        assert_eq!(state.commits.pending, None);
    }

    #[test]
    fn enter_also_answers_the_detached_checkout() {
        let mut state = on_commit();
        press_char(&mut state, ' ');
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
        assert_eq!(op, GitOp::CheckoutCommit(cid("c1")));
    }

    #[test]
    fn n_or_esc_keeps_the_branch_and_forgets_the_commit() {
        for code in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut state = on_commit();
            press_char(&mut state, ' ');
            assert_eq!(press(&mut state, code), vec![], "{code:?}");
            assert_eq!(state.popups, [], "{code:?}");
            assert_eq!(state.commits.pending, None, "{code:?}");
            assert_eq!(state.op_seq, None, "{code:?}");
        }
    }

    #[test]
    fn capital_r_reverts_the_selected_commit_on_the_terminal() {
        let mut state = on_commit();
        let (tag, op) = attached_of(&press_char(&mut state, 'R'));
        assert_eq!(op, GitOp::Revert(cid("c1")));
        assert_eq!(state.op_seq, Some(tag.seq));
    }

    #[test]
    fn a_revert_is_refused_on_a_merge_while_an_op_runs_or_during_an_operation() {
        let mut merge = on_commit();
        merge.commits.rows[1].parents = vec!["c2".into(), "d0".into()];
        assert_eq!(press_char(&mut merge, 'R'), vec![]);
        assert_eq!(merge.notice, Some("Cannot revert a merge commit".into()));
        assert_eq!(merge.update(AppIntent::RevertCommit), vec![]);
        assert_eq!(merge.op_seq, None);

        let mut running = on_commit();
        running.op_seq = Some(9);
        assert_eq!(press_char(&mut running, 'R'), vec![]);
        assert_eq!(running.notice, Some(keymap::OP_RUNNING.into()));
        assert_eq!(running.update(AppIntent::RevertCommit), vec![]);

        let mut reverting = on_commit();
        let mut branch = on_branch("main");
        branch.in_progress = vec![Operation::Revert];
        reverting.branch = Some(branch);
        assert_eq!(press_char(&mut reverting, 'R'), vec![]);
        assert_eq!(
            reverting.notice,
            Some("Finish or abort the operation in progress".into())
        );
        assert_eq!(reverting.update(AppIntent::RevertCommit), vec![]);
        assert_eq!(reverting.op_seq, None);
    }

    #[test]
    fn capital_x_opens_the_reset_menu_and_soft_and_mixed_reset_at_once() {
        for (moves, mode) in [(0, ResetMode::Soft), (1, ResetMode::Mixed)] {
            let mut state = on_commit();
            assert_eq!(press_char(&mut state, 'X'), vec![]);
            assert_eq!(
                state.popups,
                [Popup::Menu {
                    id: MenuId::Reset,
                    cursor: 0
                }]
            );
            for _ in 0..moves {
                press_char(&mut state, 'j');
            }
            let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
            assert_eq!(
                op,
                GitOp::Reset {
                    to: cid("c1"),
                    mode
                }
            );
            assert_eq!(state.popups, []);
        }
    }

    #[test]
    fn a_hard_reset_asks_first_and_only_y_resets() {
        let mut state = on_commit();
        press_char(&mut state, 'X');
        press_char(&mut state, 'j');
        press_char(&mut state, 'j');
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        let hard = GitOp::Reset {
            to: cid("c1"),
            mode: ResetMode::Hard,
        };
        assert_eq!(
            state.popups,
            [Popup::ResetHard(ResetHardConfirm {
                short_oid: "c1".into(),
                op: hard.clone(),
                head: None,
            })]
        );
        assert_eq!(state.key_focus(), KeyScope::ResetHard);
        assert_eq!(state.op_seq, None);

        // Enter is not an answer: the question stays and nothing runs.
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.popups.len(), 1);
        assert_eq!(state.op_seq, None);

        let (_, op) = op_of(&press_char(&mut state, 'y'));
        assert_eq!(op, hard);
        assert_eq!(state.popups, []);
    }

    #[test]
    fn n_or_esc_closes_the_hard_reset_question() {
        for code in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut state = on_commit();
            state.update(AppIntent::ResetTo(ResetMode::Hard));
            assert_eq!(state.popups.len(), 1, "{code:?}");
            assert_eq!(press(&mut state, code), vec![], "{code:?}");
            assert_eq!(state.popups, [], "{code:?}");
            assert_eq!(state.op_seq, None, "{code:?}");
        }
    }

    #[test]
    fn confirm_reset_hard_does_nothing_without_the_question() {
        let mut state = on_commit();
        assert_eq!(state.update(AppIntent::ConfirmResetHard), vec![]);
        state.popups.push(Popup::Confirm(ConfirmKind::Quit));
        assert_eq!(state.update(AppIntent::ConfirmResetHard), vec![]);
        assert_eq!(state.popups, [Popup::Confirm(ConfirmKind::Quit)]);
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn a_reset_waits_for_the_operation_in_progress() {
        let mut state = on_commit();
        let mut branch = on_branch("main");
        branch.in_progress = vec![Operation::Merge];
        state.branch = Some(branch);
        assert_eq!(press_char(&mut state, 'X'), vec![]);
        assert_eq!(state.popups, []);
        assert_eq!(
            state.notice,
            Some("Finish or abort the operation in progress".into())
        );
        for mode in [ResetMode::Soft, ResetMode::Hard] {
            assert_eq!(state.update(AppIntent::ResetTo(mode)), vec![]);
        }
        assert_eq!(state.popups, []);
        assert_eq!(state.op_seq, None);
    }

    // --- Leaving commits behind from a detached HEAD ---

    #[test]
    fn a_detached_head_checks_for_commits_left_behind_before_anything_else() {
        for (intent, planned) in moves() {
            let mut state = detached_on_commit();
            let busy = state.busy;
            let (tag, op) = check_of(&state.update(intent.clone()));
            assert_eq!(op, planned, "{intent:?}");
            assert!(tag.user, "{intent:?}");
            assert_eq!(state.busy, busy + 1, "{intent:?}");
            assert_eq!(state.popups, [], "{intent:?}");
            assert_eq!(state.commits.pending, None, "{intent:?}");
            assert_eq!(state.op_seq, None, "{intent:?}");
        }
    }

    #[test]
    fn on_a_branch_nothing_is_checked_first() {
        let mut state = on_commit();
        state.branch = Some(on_branch("main"));
        assert_eq!(state.update(AppIntent::CheckoutCommit), vec![]);
        assert_eq!(
            state.popups,
            [Popup::Confirm(ConfirmKind::DetachedCheckout)]
        );

        let mut state = on_commit();
        state.branch = Some(on_branch("main"));
        let (_, op) = op_of(&state.update(AppIntent::ResetTo(ResetMode::Mixed)));
        assert_eq!(op, reset_op(ResetMode::Mixed));
    }

    #[test]
    fn with_nothing_left_behind_each_move_goes_on_as_before() {
        let (state, effects) = checked(AppIntent::CheckoutCommit, Ok(vec![]));
        assert_eq!(effects, vec![]);
        assert_eq!(
            state.popups,
            [Popup::Confirm(ConfirmKind::DetachedCheckout)]
        );
        assert_eq!(state.commits.pending, Some(cid("c1")));

        for mode in [ResetMode::Soft, ResetMode::Mixed] {
            let (state, effects) = checked(AppIntent::ResetTo(mode), Ok(vec![]));
            let (tag, op) = op_of(&effects);
            assert_eq!(op, reset_op(mode));
            assert_eq!(state.op_seq, Some(tag.seq));
        }

        let (state, effects) = checked(AppIntent::ResetTo(ResetMode::Hard), Ok(vec![]));
        assert_eq!(effects, vec![]);
        assert_eq!(
            state.popups,
            [Popup::ResetHard(ResetHardConfirm {
                short_oid: "c1".into(),
                op: reset_op(ResetMode::Hard),
                head: Some(cid("c0")),
            })]
        );
    }

    #[test]
    fn commits_left_behind_open_the_question_and_run_nothing() {
        for (intent, planned) in moves() {
            let (state, effects) = checked(intent.clone(), Ok(ids(3)));
            assert_eq!(effects, vec![], "{intent:?}");
            assert_eq!(state.busy, detached_on_commit().busy, "{intent:?}");
            assert_eq!(state.op_seq, None, "{intent:?}");
            assert_eq!(
                state.popups,
                [Popup::LeaveCommits(LeaveCommitsConfirm {
                    op: planned,
                    head: cid("c0"),
                    head_short_oid: "c0".into(),
                    short_oid: "c1".into(),
                    left: vec!["a0".into(), "a1".into(), "a2".into()],
                })],
                "{intent:?}"
            );
            assert_eq!(state.key_focus(), KeyScope::LeaveCommits);
            let Some(Popup::LeaveCommits(ask)) = state.popups.last() else {
                unreachable!()
            };
            let question = ask.question();
            assert!(question.contains("3 commits"), "{question}");
            assert!(question.contains("(a0, a1, a2)"), "{question}");
        }

        let (state, _) = checked(AppIntent::CheckoutCommit, Ok(ids(7)));
        let Some(Popup::LeaveCommits(ask)) = state.popups.last() else {
            panic!("{:?}", state.popups)
        };
        let question = ask.question();
        assert!(
            question.contains("(a0, a1, a2, a3, a4 and 2 more)"),
            "{question}"
        );
        assert!(!question.contains("a5"), "{question}");
    }

    #[test]
    fn only_y_leaves_the_commits_behind_and_enter_is_no_answer() {
        for (intent, planned) in moves() {
            let (mut state, _) = checked(intent.clone(), Ok(ids(2)));
            assert_eq!(press(&mut state, KeyCode::Enter), vec![], "{intent:?}");
            assert_eq!(state.update(AppIntent::ConfirmYes), vec![], "{intent:?}");
            assert_eq!(state.popups.len(), 1, "{intent:?}");
            assert_eq!(state.op_seq, None, "{intent:?}");

            let (tag, op) = op_of(&press_char(&mut state, 'y'));
            assert_eq!(op, planned, "{intent:?}");
            assert_eq!(state.op_seq, Some(tag.seq), "{intent:?}");
            assert_eq!(state.popups, [], "{intent:?}");
        }

        for code in [KeyCode::Char('n'), KeyCode::Esc] {
            let (mut state, _) = checked(AppIntent::CheckoutCommit, Ok(ids(2)));
            assert_eq!(press(&mut state, code), vec![], "{code:?}");
            assert_eq!(state.popups, [], "{code:?}");
            assert_eq!(state.op_seq, None, "{code:?}");
        }
    }

    #[test]
    fn confirm_leave_commits_does_nothing_without_the_question() {
        let mut state = detached_on_commit();
        assert_eq!(state.update(AppIntent::ConfirmLeaveCommits), vec![]);
        state.popups.push(Popup::Confirm(ConfirmKind::Quit));
        assert_eq!(state.update(AppIntent::ConfirmLeaveCommits), vec![]);
        assert_eq!(state.popups, [Popup::Confirm(ConfirmKind::Quit)]);
        assert_eq!(state.op_seq, None);
    }

    // --- Re-checking the guards when a move question is answered ---

    const IN_PROGRESS: &str = "Finish or abort the operation in progress";

    /// Each question that moves `HEAD` on `y`, open, with the op it
    /// carries: built through the flows that open it, never by hand.
    fn open_move_questions() -> Vec<(String, AppState, GitOp)> {
        let checkout = GitOp::CheckoutCommit(cid("c1"));
        let hard = reset_op(ResetMode::Hard);
        let on_main = |intent: AppIntent| {
            let mut state = on_commit();
            state.branch = Some(on_branch("main"));
            assert_eq!(state.update(intent.clone()), vec![], "{intent:?}");
            state
        };
        let mut questions = vec![
            (
                "checkout on a branch".to_string(),
                on_main(AppIntent::CheckoutCommit),
                checkout.clone(),
            ),
            (
                "checkout from a detached HEAD".to_string(),
                checked(AppIntent::CheckoutCommit, Ok(vec![])).0,
                checkout,
            ),
            (
                "hard reset on a branch".to_string(),
                on_main(AppIntent::ResetTo(ResetMode::Hard)),
                hard.clone(),
            ),
            (
                "hard reset from a detached HEAD".to_string(),
                checked(AppIntent::ResetTo(ResetMode::Hard), Ok(vec![])).0,
                hard,
            ),
        ];
        for (intent, op) in moves() {
            let (state, _) = checked(intent.clone(), Ok(ids(2)));
            questions.push((format!("leave commits for {intent:?}"), state, op));
        }
        for (name, state, _) in &questions {
            assert_eq!(state.popups.len(), 1, "{name}");
            assert_eq!(state.op_seq, None, "{name}");
        }
        questions
    }

    /// Answer the open question with `y`, as the key does.
    fn answer_yes(state: &mut AppState) -> Vec<Effect> {
        match state.popups.last() {
            Some(Popup::LeaveCommits(_)) => state.update(AppIntent::ConfirmLeaveCommits),
            _ => press_char(state, 'y'),
        }
    }

    fn start(state: &mut AppState, operations: &[Operation]) {
        state.notice = None;
        state.branch.as_mut().unwrap().in_progress = operations.to_vec();
    }

    fn assert_refused(state: &AppState, effects: &[Effect], reason: &str, name: &str) {
        assert_eq!(effects, [], "{name}");
        assert_eq!(state.notice, Some(reason.into()), "{name}");
        assert_eq!(state.popups, [], "{name}");
        assert_eq!(state.op_seq, None, "{name}");
        assert_eq!(state.commits.pending, None, "{name}");
    }

    #[test]
    fn y_runs_nothing_once_a_bisect_has_started_since_the_question_opened() {
        for (name, mut state, _) in open_move_questions() {
            start(&mut state, &[Operation::Bisect]);
            let effects = answer_yes(&mut state);
            assert_refused(&state, &effects, bisect::RESET_FIRST, &name);
        }
    }

    #[test]
    fn y_resets_nothing_once_an_operation_is_in_progress_but_still_checks_out() {
        for operation in [Operation::Rebase, Operation::Merge, Operation::CherryPick] {
            for (name, mut state, op) in open_move_questions() {
                let name = format!("{name} during {operation:?}");
                start(&mut state, &[operation]);
                let effects = answer_yes(&mut state);
                if matches!(op, GitOp::CheckoutCommit(_)) {
                    // The checkout key allows a checkout here, so its
                    // question does too.
                    let (tag, ran) = op_of(&effects);
                    assert_eq!(ran, op, "{name}");
                    assert_eq!(state.op_seq, Some(tag.seq), "{name}");
                } else {
                    assert_refused(&state, &effects, IN_PROGRESS, &name);
                }
            }
        }
    }

    #[test]
    fn a_refused_reset_names_the_operation_before_the_bisect() {
        for (name, mut state, op) in open_move_questions() {
            start(&mut state, &[Operation::Bisect, Operation::Merge]);
            let effects = answer_yes(&mut state);
            let reason = match op {
                GitOp::CheckoutCommit(_) => bisect::RESET_FIRST,
                _ => IN_PROGRESS,
            };
            assert_refused(&state, &effects, reason, &name);
        }
    }

    #[test]
    fn with_nothing_in_progress_y_runs_the_op_the_question_carries() {
        for (name, mut state, op) in open_move_questions() {
            let (tag, ran) = op_of(&answer_yes(&mut state));
            assert_eq!(ran, op, "{name}");
            assert_eq!(state.op_seq, Some(tag.seq), "{name}");
            assert_eq!(state.popups, [], "{name}");
            assert_eq!(state.notice, None, "{name}");
        }
    }

    #[test]
    fn a_check_for_a_head_that_moved_runs_nothing() {
        for result in [Ok(vec![]), Ok(ids(2))] {
            let mut state = detached_on_commit();
            let (tag, op) = check_of(&state.update(AppIntent::CheckoutCommit));
            state.branch.as_mut().unwrap().head = Head::Detached("c2".into());
            let effects = state.update(AppIntent::LeftBehindChecked {
                tag,
                op,
                head: cid("c0"),
                result,
            });
            assert_eq!(effects, vec![]);
            assert_eq!(state.popups, []);
            assert_eq!(state.notice, Some("HEAD moved; try again".into()));
            assert_eq!(state.op_seq, None);
            assert_eq!(state.commits.pending, None);
        }

        // Back on a branch is a move too.
        let mut state = detached_on_commit();
        let (tag, op) = check_of(&state.update(AppIntent::CheckoutCommit));
        state.branch = Some(on_branch("main"));
        state.update(AppIntent::LeftBehindChecked {
            tag,
            op,
            head: cid("c0"),
            result: Ok(vec![]),
        });
        assert_eq!(state.popups, []);
    }

    #[test]
    fn a_failed_check_shows_why_and_runs_nothing() {
        for (intent, _) in moves() {
            let (state, effects) = checked(intent.clone(), Err("fatal: bad".into()));
            assert_eq!(effects, vec![], "{intent:?}");
            assert_eq!(state.op_seq, None, "{intent:?}");
            let [Popup::Error(error)] = state.popups.as_slice() else {
                panic!("{intent:?}: {:?}", state.popups)
            };
            assert_eq!(error.title, "Could not check for commits left behind");
            assert!(error.message.contains("fatal: bad"));
            assert!(error.next.contains("retry"), "{}", error.next);
        }
    }

    #[test]
    fn a_second_press_while_a_check_runs_asks_only_once() {
        for (intent, _) in moves() {
            let mut state = detached_on_commit();
            let busy = state.busy;
            let (tag, op) = check_of(&state.update(intent.clone()));
            assert_eq!(state.update(intent.clone()), vec![], "{intent:?}");
            assert_eq!(state.busy, busy + 1, "{intent:?}");
            assert_eq!(
                state.notice,
                Some("Still checking for commits left behind".into()),
                "{intent:?}"
            );
            state.update(AppIntent::LeftBehindChecked {
                tag,
                op,
                head: cid("c0"),
                result: Ok(ids(2)),
            });
            assert!(
                matches!(state.popups.as_slice(), [Popup::LeaveCommits(_)]),
                "{intent:?}: {:?}",
                state.popups
            );
            assert_eq!(state.busy, busy, "{intent:?}");
        }
    }

    #[test]
    fn once_a_check_is_answered_the_next_press_checks_again() {
        // After the question, and after the error.
        for result in [Ok(ids(2)), Err("fatal: bad".to_string())] {
            let (mut state, _) = checked(AppIntent::CheckoutCommit, result.clone());
            state.popups.clear();
            check_of(&state.update(AppIntent::CheckoutCommit));
        }

        // An answer for an older repository clears it too.
        let mut state = detached_on_commit();
        let (tag, op) = check_of(&state.update(AppIntent::CheckoutCommit));
        state.generation += 1;
        state.update(AppIntent::LeftBehindChecked {
            tag,
            op: op.clone(),
            head: cid("c0"),
            result: Ok(ids(2)),
        });
        assert_eq!(state.popups, []);
        check_of(&state.update(AppIntent::CheckoutCommit));

        // An answer to some other check does not.
        let stale = Tag {
            seq: tag.seq + 1000,
            ..tag
        };
        state.update(AppIntent::LeftBehindChecked {
            tag: stale,
            op,
            head: cid("c0"),
            result: Ok(ids(2)),
        });
        assert_eq!(state.popups, []);
        assert_eq!(state.update(AppIntent::CheckoutCommit), vec![]);
    }

    #[test]
    fn an_answer_after_focus_moved_asks_nothing() {
        for name in ["error", "filter"] {
            for (intent, _) in moves() {
                for result in [Ok(ids(2)), Ok(vec![]), Err("fatal: bad".to_string())] {
                    let mut state = detached_on_commit();
                    let (tag, op) = check_of(&state.update(intent.clone()));
                    if name == "error" {
                        state.push_error(ErrorPopup::new("Elsewhere", "x", "y"));
                    } else {
                        press_char(&mut state, '/');
                        assert_eq!(state.key_focus(), KeyScope::Input);
                    }
                    let popups = state.popups.clone();
                    let effects = state.update(AppIntent::LeftBehindChecked {
                        tag,
                        op,
                        head: cid("c0"),
                        result,
                    });
                    let case = format!("{name} {intent:?}");
                    assert_eq!(effects, vec![], "{case}");
                    assert_eq!(state.popups, popups, "{case}");
                    assert_eq!(state.commits.pending, None, "{case}");
                    assert_eq!(state.op_seq, None, "{case}");
                    assert_eq!(
                        state.notice,
                        Some("Not asked: focus moved; try again".into()),
                        "{case}"
                    );
                    assert_eq!(state.update(AppIntent::ConfirmLeaveCommits), vec![]);
                    assert_eq!(state.update(AppIntent::ConfirmResetHard), vec![]);
                    assert_eq!(state.op_seq, None, "{case}");
                }
            }
        }
    }

    #[test]
    fn y_runs_nothing_once_head_has_moved() {
        for (intent, planned) in moves() {
            let (mut state, _) = checked(intent.clone(), Ok(ids(2)));
            let mut moved = state.clone();
            moved.branch.as_mut().unwrap().head = Head::Detached("c2".into());
            assert_eq!(
                moved.update(AppIntent::ConfirmLeaveCommits),
                vec![],
                "{intent:?}"
            );
            assert_eq!(moved.popups, [], "{intent:?}");
            assert_eq!(moved.op_seq, None, "{intent:?}");
            assert_eq!(moved.notice, Some("HEAD moved; try again".into()));

            let (_, op) = op_of(&state.update(AppIntent::ConfirmLeaveCommits));
            assert_eq!(op, planned, "{intent:?}");
        }
    }

    #[test]
    fn the_hard_reset_question_runs_nothing_once_head_has_moved() {
        let (mut state, _) = checked(AppIntent::ResetTo(ResetMode::Hard), Ok(vec![]));
        let mut moved = state.clone();
        moved.branch.as_mut().unwrap().head = Head::Detached("c2".into());
        assert_eq!(moved.update(AppIntent::ConfirmResetHard), vec![]);
        assert_eq!(moved.popups, []);
        assert_eq!(moved.op_seq, None);
        assert_eq!(moved.notice, Some("HEAD moved; try again".into()));

        // Back on a branch is a move too.
        let mut on_main = state.clone();
        on_main.branch = Some(on_branch("main"));
        assert_eq!(on_main.update(AppIntent::ConfirmResetHard), vec![]);
        assert_eq!(on_main.op_seq, None);

        let (_, op) = op_of(&state.update(AppIntent::ConfirmResetHard));
        assert_eq!(op, reset_op(ResetMode::Hard));
    }

    #[test]
    fn a_row_whose_id_is_not_hex_starts_nothing() {
        let mut state = on_commit();
        state.commits.rows[1].oid = "--upload-pack=x".into();
        for intent in [
            AppIntent::CheckoutCommit,
            AppIntent::RevertCommit,
            AppIntent::ResetTo(ResetMode::Soft),
            AppIntent::ResetTo(ResetMode::Hard),
        ] {
            state.notice = None;
            assert_eq!(state.update(intent.clone()), vec![], "{intent:?}");
            assert_eq!(
                state.notice,
                Some("Not a commit id git can use".into()),
                "{intent:?}"
            );
        }
        assert_eq!(state.popups, []);
        assert_eq!(state.commits.pending, None);
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn with_no_commits_the_actions_say_so() {
        let mut state = with_commits(0, true);
        for c in [' ', 'R', 'X'] {
            state.notice = None;
            assert_eq!(press_char(&mut state, c), vec![], "{c}");
            assert_eq!(state.notice, Some("No commits".into()), "{c}");
        }
        assert_eq!(state.popups, []);
    }

    #[test]
    fn commit_action_failures_have_their_own_titles() {
        let conflicts = "If git stopped on conflicts, resolve them, then continue or abort from the Status panel.";
        let files = "The file list was reloaded. Fix the cause above, then try again.";
        let cases = [
            (
                GitOp::CheckoutCommit(cid("c1")),
                "Could not check out the commit",
                files,
            ),
            (GitOp::Revert(cid("c1")), "Could not revert", conflicts),
            (
                GitOp::Reset {
                    to: cid("c1"),
                    mode: ResetMode::Mixed,
                },
                "Could not reset",
                files,
            ),
            (
                GitOp::Continue(Operation::Revert),
                "Could not continue the revert",
                conflicts,
            ),
            (
                GitOp::Abort(Operation::Revert),
                "Could not abort the revert",
                files,
            ),
        ];
        for (op, title, next) in cases {
            assert_eq!(failure_title(&op), title, "{op:?}");
            assert_eq!(failure_next(&op), next, "{op:?}");
        }
    }

    #[test]
    fn a_revert_in_progress_can_be_continued_skipped_and_aborted() {
        let mut state = status_focused(&[Operation::Revert]);
        let (tag, op) = attached_of(&press_char(&mut state, 'c'));
        assert_eq!(op, GitOp::Continue(Operation::Revert));
        state.update(finished(tag, op, Ok(())));
        let (tag, op) = attached_of(&press_char(&mut state, 's'));
        assert_eq!(op, GitOp::Skip(Operation::Revert));
        state.update(finished(tag, op, Ok(())));
        press_char(&mut state, 'a');
        assert_eq!(
            state.popups,
            [Popup::Confirm(ConfirmKind::Abort(Operation::Revert))]
        );
        let (_, op) = op_of(&press_char(&mut state, 'y'));
        assert_eq!(op, GitOp::Abort(Operation::Revert));
    }

    #[test]
    fn a_credential_answer_never_reaches_the_command_log() {
        use crate::git::command_line::display_line;
        use crate::git::recording::{CommandEvent, CommandOutcome};
        use crate::git::remote::{AskpassEnv, remote_command};

        let (mut state, _) = typing_password("hunter2");
        let askpass = AskpassEnv {
            program: "/usr/bin/kika".into(),
            socket: "/tmp/kika-askpass-1/socket".into(),
            git_askpass: true,
        };
        let line = display_line(&remote_command(&PUSH, Some(&askpass)));
        state.update(AppIntent::CommandLogged(CommandEvent::Started {
            id: 0,
            line,
            attached: false,
        }));
        state.update(AppIntent::CredentialAnswered(Some(Secret::new("hunter2"))));
        state.update(AppIntent::CommandLogged(CommandEvent::Finished {
            id: 0,
            outcome: CommandOutcome::Ok,
            elapsed: std::time::Duration::from_millis(5),
        }));

        let entries = state.command_log.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].line, "git push");
        for entry in entries {
            for hidden in ["hunter2", "kika-askpass", "/usr/bin/kika", "KIKA_"] {
                assert!(!entry.line.contains(hidden), "{hidden} in {entry:?}");
            }
        }
        assert!(!format!("{:?}", state.command_log).contains("hunter2"));
    }

    // --- Squash, fixup, drop and reword ---

    #[test]
    fn s_f_d_and_e_rewrite_the_selected_commit_on_the_terminal() {
        use rebase_plan::{RebasePlan, TodoAction as A};
        let fold = |action| RebasePlan {
            base: cid(&"d".repeat(40)),
            steps: vec![
                rebase_step(A::Pick, 'c'),
                rebase_step(action, 'b'),
                rebase_step(A::Pick, 'a'),
            ],
            update_refs: false,
        };
        let alone = |action| RebasePlan {
            base: cid(&"c".repeat(40)),
            steps: vec![rebase_step(action, 'b'), rebase_step(A::Pick, 'a')],
            update_refs: false,
        };
        let cases = [
            ('s', fold(A::Squash)),
            ('F', fold(A::Fixup)),
            ('d', alone(A::Drop)),
            ('e', alone(A::Reword)),
        ];
        for (key, plan) in cases {
            let mut state = on_linear_log();
            let (tag, op) = attached_of(&press_char(&mut state, key));
            assert_eq!(op, GitOp::InteractiveRebase(plan), "{key}");
            assert_eq!(state.op_seq, Some(tag.seq), "{key}");
            assert_eq!(state.notice, None, "{key}");
        }
    }

    /// Press each rewrite key on `state` and expect nothing to start, with
    /// `notice` said.
    fn no_rewrite(state: &AppState, notice: &'static str) {
        for key in ['s', 'F', 'd', 'e'] {
            let mut state = state.clone();
            assert_eq!(press_char(&mut state, key), vec![], "{key}");
            assert_eq!(state.notice, Some(notice.into()), "{key}");
            assert_eq!(state.op_seq, None, "{key}");
            assert_eq!(state.popups, [], "{key}");
        }
    }

    #[test]
    fn no_rewrite_starts_across_a_merge() {
        let mut state = on_linear_log();
        state.commits.rows[0].parents.push("e".repeat(40));
        no_rewrite(&state, "Cannot rewrite across a merge");
    }

    #[test]
    fn the_root_commit_is_not_rewritten() {
        let mut state = on_linear_log();
        state.commits.rows[1].parents.clear();
        no_rewrite(&state, "Cannot rewrite the first commit");
    }

    #[test]
    fn no_rewrite_starts_while_an_op_runs_or_an_operation_is_in_progress() {
        let mut running = on_linear_log();
        running.op_seq = Some(9);
        for key in ['s', 'F', 'd', 'e'] {
            let mut state = running.clone();
            assert_eq!(press_char(&mut state, key), vec![], "{key}");
            assert_eq!(state.notice, Some(keymap::OP_RUNNING.into()), "{key}");
            assert_eq!(state.op_seq, Some(9), "{key}");
        }

        let mut rebasing = on_linear_log();
        let mut branch = on_branch("main");
        branch.in_progress = vec![Operation::Rebase];
        rebasing.branch = Some(branch);
        no_rewrite(&rebasing, "Finish or abort the operation in progress");
    }

    #[test]
    fn the_intent_outside_the_workspace_starts_nothing() {
        let mut state = on_linear_log();
        state.context = Context::Review { number: Some(1) };
        assert_eq!(
            state.update(AppIntent::RewriteCommit(rebase_plan::Edit::Drop)),
            vec![]
        );
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn a_failed_interactive_rebase_points_at_the_status_panel() {
        let op = GitOp::InteractiveRebase(rebase_plan::sample());
        assert_eq!(failure_title(&op), "Could not rebase");
        assert_eq!(
            failure_next(&op),
            "If git stopped on conflicts, resolve them, then continue or abort from the Status panel."
        );
    }

    /// `with_commits` on branch `main`, which is loaded as a local branch.
    fn undoable() -> AppState {
        let mut state = with_commits(3, true);
        state.branch = Some(on_branch("main"));
        state.branches.branches = vec![Branch {
            name: "main".into(),
            head: true,
            upstream: None,
            track: crate::git::branches::Track::None,
            merged: true,
        }];
        state
    }

    fn reflog_tag(effects: &[Effect]) -> Tag {
        match effects {
            [Effect::LoadReflog { tag }] => *tag,
            other => panic!("expected one LoadReflog, got {other:?}"),
        }
    }

    /// Two commits on `main` after the first, `HEAD` at `ccc…`.
    fn commit_reflog(head: char) -> Reflog {
        Reflog {
            head: CommitId::parse(&head.to_string().repeat(40)),
            head_ref: Some(b"refs/heads/main".to_vec()),
            branch_entries: vec![],
            entries: crate::git::reflog::parse_records(include_bytes!(
                "git/fixtures/reflog/commit.bin"
            )),
        }
    }

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

    /// `state` after an undo whose fresh reflog read answered with
    /// `reflog`.
    fn undo_with(state: &mut AppState, reflog: Reflog) -> Vec<Effect> {
        let tag = reflog_tag(&state.update(AppIntent::Undo));
        state.update(AppIntent::ReflogLoaded {
            tag,
            result: Ok(reflog),
        })
    }

    #[test]
    fn undo_reads_the_reflog_afresh_then_asks_with_the_plan_and_emits_nothing_else() {
        let mut state = undoable();
        let busy = state.busy;
        let effects = press_with(&mut state, 'z', KeyModifiers::CONTROL);
        let tag = reflog_tag(&effects);
        assert!(tag.user);
        assert_eq!(state.busy, busy + 1);
        assert_eq!(state.commits.reflog.planning, Some(undo::Direction::Undo));
        assert_eq!(state.popups, []);

        let effects = state.update(AppIntent::ReflogLoaded {
            tag,
            result: Ok(commit_reflog('c')),
        });

        assert_eq!(effects, vec![]);
        assert_eq!(state.busy, busy);
        assert_eq!(state.commits.reflog.planning, None);
        assert_eq!(
            state.popups,
            [Popup::Travel(TravelConfirm {
                title: "Undo",
                preview: vec![
                    "Undo: commit: second".into(),
                    "Branch main moves from ccccccc to bbbbbbb.".into(),
                    "Files change to match bbbbbbb; ignored files in the way are replaced.".into(),
                    "Commits after bbbbbbb leave main; redo brings them back.".into(),
                ],
                op: undo_commit_op(),
            })]
        );
        assert_eq!(state.key_focus(), KeyScope::Travel);
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn redo_plans_from_its_own_fresh_read() {
        let mut state = undoable();
        let tag = reflog_tag(&press_with(&mut state, 'r', KeyModifiers::CONTROL));
        assert_eq!(state.commits.reflog.planning, Some(undo::Direction::Redo));
        state.update(AppIntent::ReflogLoaded {
            tag,
            result: Ok(commit_reflog('c')),
        });
        assert_eq!(state.popups, []);
        assert_eq!(state.op_error.as_deref(), Some("Nothing to redo"));
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
            assert_eq!(state.op_seq, None, "{code:?}");
        }
    }

    #[test]
    fn confirm_travel_does_nothing_without_the_question() {
        let mut state = undoable();
        assert_eq!(state.update(AppIntent::ConfirmTravel), vec![]);
        state.popups.push(Popup::Confirm(ConfirmKind::Quit));
        assert_eq!(state.update(AppIntent::ConfirmTravel), vec![]);
        assert_eq!(state.popups, [Popup::Confirm(ConfirmKind::Quit)]);
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn undo_and_redo_wait_for_a_running_op_or_an_operation_in_progress() {
        for intent in [AppIntent::Undo, AppIntent::Redo] {
            let mut state = undoable();
            state.op_seq = Some(7);
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
            state.op_error.as_deref(),
            Some("HEAD has moved since commit; nothing is undone")
        );

        let mut state = undoable();
        state.files = vec![changed("a.rs", '.', 'M')];
        undo_with(&mut state, commit_reflog('c'));
        assert_eq!(
            state.op_error.as_deref(),
            Some("Commit or stash your changes first")
        );

        // Untracked files are left to `reset --keep`.
        let mut state = undoable();
        state.files = vec![untracked("new.rs")];
        undo_with(&mut state, commit_reflog('c'));
        assert_eq!(state.popups.len(), 1);
    }

    #[test]
    fn a_stale_or_failed_read_plans_nothing() {
        let mut state = undoable();
        let tag = reflog_tag(&state.update(AppIntent::Undo));
        let stale = Tag {
            seq: tag.seq + 100,
            ..tag
        };
        state.update(AppIntent::ReflogLoaded {
            tag: stale,
            result: Ok(commit_reflog('c')),
        });
        assert_eq!(state.popups, []);
        assert_eq!(state.commits.reflog.planning, Some(undo::Direction::Undo));

        state.update(AppIntent::ReflogLoaded {
            tag,
            result: Err("fatal: broken".into()),
        });
        assert_eq!(state.popups, []);
        assert_eq!(state.commits.reflog.planning, None);
        assert_eq!(
            state.op_error.as_deref(),
            Some("Could not read the reflog: fatal: broken")
        );
    }

    #[test]
    fn brackets_switch_to_the_reflog_tab_reading_it_once() {
        let mut state = undoable();
        let tag = reflog_tag(&press_char(&mut state, ']'));
        assert_eq!(state.commits.tab, CommitsTab::Reflog);
        assert_eq!(state.key_focus(), KeyScope::Reflog);
        state.update(AppIntent::ReflogLoaded {
            tag,
            result: Ok(commit_reflog('c')),
        });
        press_char(&mut state, 'j');
        press(&mut state, KeyCode::Down);
        press(&mut state, KeyCode::Down);
        assert_eq!(state.commits.reflog.selected, 2);
        press_char(&mut state, 'k');
        assert_eq!(state.commits.reflog.selected, 1);
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
        state.update(AppIntent::ReflogLoaded {
            tag,
            result: Ok(commit_reflog('c')),
        });
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
        assert_eq!(state.commits.reflog.visible(), [1], "by short id");
        assert_eq!(state.commits.reflog.selected, 1, "the cursor follows");
        for _ in 0..3 {
            press(&mut state, KeyCode::Backspace);
        }
        type_text(&mut state, "root");
        assert_eq!(state.commits.reflog.visible(), [2], "by message");
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.input, None);
        assert_eq!(state.commits.reflog.filter, "root");
        assert_eq!(state.key_focus(), KeyScope::Reflog);
        // `/` again starts from the kept text.
        press_char(&mut state, '/');
        assert_eq!(state.input.as_ref().unwrap().line.text, "root");
    }

    #[test]
    fn esc_in_the_reflog_tab_clears_the_filter_then_quits_and_v_sets_no_range() {
        let mut state = on_reflog_tab();
        assert_eq!(press_char(&mut state, 'V'), vec![]);
        assert_eq!(state.commits.range_anchor, None);
        assert_eq!(state.popups, []);
        press_char(&mut state, '/');
        type_text(&mut state, "first\n");
        assert_eq!(press(&mut state, KeyCode::Esc), vec![]);
        assert_eq!(state.commits.reflog.filter, "");
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
            state.commits.reflog.filter.chars().count(),
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
        assert_eq!(state.commits.filter, "c1");
        assert_eq!(state.commits.reflog.filter, "root");
        press_char(&mut state, ']');
        assert_eq!(state.commits.reflog.filter, "root");
        assert_eq!(state.commits.filter, "c1");
        // Esc in the Reflog tab clears only its own.
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.commits.reflog.filter, "");
        assert_eq!(state.commits.filter, "c1");
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
        assert!(state.commits.reflog.filter.starts_with("--force -D x"));
        assert!(state.commits.reflog.visible().is_empty());
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
        state.update(AppIntent::ReflogLoaded {
            tag,
            result: Ok(commit_reflog('c')),
        });

        // Undo plans from every entry, though the filter hides them all.
        let undo = ctrl(&mut state, 'z');
        assert!(!leaks(&undo), "{undo:?}");
        let tag = reflog_tag(&undo);
        let loaded = state.update(AppIntent::ReflogLoaded {
            tag,
            result: Ok(commit_reflog('c')),
        });
        assert!(!leaks(&loaded), "{loaded:?}");
        match state.popups.as_slice() {
            [Popup::Travel(confirm)] => assert_eq!(confirm.op, undo_commit_op()),
            other => panic!("expected the undo question, got {other:?}"),
        }
        let ran = press_char(&mut state, 'y');
        assert!(!leaks(&ran), "{ran:?}");
        assert_eq!(op_of(&ran).1, undo_commit_op());

        state.op_seq = None;
        let redo = ctrl(&mut state, 'r');
        assert!(!leaks(&redo), "{redo:?}");
        let tag = reflog_tag(&redo);
        let loaded = state.update(AppIntent::ReflogLoaded {
            tag,
            result: Ok(commit_reflog('c')),
        });
        assert!(!leaks(&loaded), "{loaded:?}");
        assert!(state.commits.reflog.filter.starts_with("--force -D x"));
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
        let tag = reflog_tag(&state.update(AppIntent::SwitchCommitsTab));
        state.update(AppIntent::ReflogLoaded {
            tag,
            result: Ok(commit_reflog('c')),
        });
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
        assert_eq!(state.update(AppIntent::SwitchCommitsTab), vec![]);
        assert_eq!(state.commits.reflog.data, Some(Reflog::default()));
        assert_eq!(state.update(AppIntent::Undo), vec![]);
        assert_eq!(state.notice, Some("Nothing to undo".into()));
    }

    #[test]
    fn a_travel_failure_is_titled_by_its_direction() {
        let mut state = undoable();
        undo_with(&mut state, commit_reflog('c'));
        let (tag, op) = op_of(&press_char(&mut state, 'y'));
        state.update(AppIntent::OpFinished {
            tag,
            op,
            result: Err("error: Entry 'a.txt' not uptodate. Cannot merge.".into()),
        });
        match state.popups.last() {
            Some(Popup::Error(error)) => assert_eq!(error.title, "Could not undo"),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    // --- Tags ---

    #[test]
    fn brackets_cycle_local_remotes_tags_and_back() {
        let mut state = with_branches();
        reload_tags(&mut state, two_tags());
        let mut tabs = vec![];
        for key in [']', ']', ']', '[', '[', '['] {
            press_char(&mut state, key);
            tabs.push((state.branches.tab, state.key_focus()));
        }
        use BranchesTab::*;
        assert_eq!(
            tabs,
            [
                (Remotes, KeyScope::Remotes),
                (Tags, KeyScope::Tags),
                (Local, KeyScope::Branches),
                (Tags, KeyScope::Tags),
                (Remotes, KeyScope::Remotes),
                (Local, KeyScope::Branches),
            ]
        );
    }

    #[test]
    fn shift_p_pushes_the_selected_tag_to_the_upstreams_remote() {
        let mut state = remote_ready();
        state.branches.branches[0].upstream = Some("origin/main".into());
        reload_remotes(
            &mut state,
            vec![
                git_remote("fork", true, &[]),
                git_remote("origin", true, &["main"]),
            ],
        );
        reload_tags(&mut state, two_tags());
        state.busy = 0;
        press_char(&mut state, ']');
        press_char(&mut state, ']');
        let (tag, op) = remote_of(&shift(&mut state, 'P'));
        assert_eq!(
            op,
            RemoteOp::PushTag {
                remote: "origin".into(),
                tag: "v2".into(),
            }
        );
        assert_eq!(state.remote.as_ref().map(|r| &r.op), Some(&op));
        let effects = state.update(remote_finished(tag, op, Ok(())));
        tags_tag(&effects);

        // Two remotes and no upstream: nothing to choose from.
        state.branches.branches[0].upstream = None;
        assert_eq!(shift(&mut state, 'P'), vec![]);
        assert_eq!(
            state.notice,
            Some("Several remotes and no upstream to choose from".into())
        );
        assert_eq!(state.remote, None);
    }

    // --- Bisect ---

    #[test]
    fn c_says_to_mark_while_only_a_bisect_is_in_progress() {
        let mut state = testing_c1(&[Operation::Bisect]);
        assert_eq!(press_char(&mut state, 'c'), vec![]);
        assert_eq!(state.notice, Some(bisect::MARK_INSTEAD.into()));
        assert_eq!(state.update(AppIntent::ContinueOperation), vec![]);
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn a_while_only_bisecting_asks_then_y_resets_and_only_y_answers() {
        let mut state = testing_c1(&[Operation::Bisect]);
        assert_eq!(press_char(&mut state, 'a'), vec![]);
        let question = [Popup::Confirm(ConfirmKind::Abort(Operation::Bisect))];
        assert_eq!(state.popups, question);
        assert_eq!(state.key_focus(), KeyScope::AbortConfirm);
        assert!(
            state
                .confirm_question(&ConfirmKind::Abort(Operation::Bisect))
                .contains("HEAD goes back to main;")
        );

        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.popups, question);
        assert_eq!(state.op_seq, None);

        let (_, op) = op_of(&press_char(&mut state, 'y'));
        assert_eq!(op, GitOp::BisectReset);
        assert_eq!(state.popups, []);

        for no in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut state = testing_c1(&[Operation::Bisect]);
            press_char(&mut state, 'a');
            assert_eq!(press(&mut state, no), vec![], "{no:?}");
            assert_eq!(state.popups, []);
            assert_eq!(state.op_seq, None);
        }
    }

    #[test]
    fn a_with_a_rebase_and_a_bisect_still_aborts_the_rebase() {
        let mut state = testing_c1(&[Operation::Rebase, Operation::Bisect]);
        press_char(&mut state, 'a');
        assert_eq!(
            state.popups,
            [Popup::Confirm(ConfirmKind::Abort(Operation::Rebase))]
        );
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        let (_, op) = op_of(&press_char(&mut state, 'y'));
        assert_eq!(op, GitOp::Abort(Operation::Rebase));
    }

    fn key(c: char) -> AppIntent {
        AppIntent::Key(KeyPress::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    fn files_to_commit() -> AppState {
        with_files(three_files(), 24).0
    }

    fn on_remote_topic() -> AppState {
        on_remote_row(3)
    }

    /// An action that moves `HEAD` or makes a commit: a name, the state it
    /// runs from, the intents that lead up to it, and the action.
    type HeadMove = (&'static str, fn() -> AppState, Vec<AppIntent>, AppIntent);

    fn head_moves() -> Vec<HeadMove> {
        use rebase_plan::{Direction, Edit};
        let to_commit = files_to_commit as fn() -> AppState;
        let actions_menu = |moves: usize| {
            let mut pre = vec![AppIntent::OpenMenu(MenuId::Actions)];
            pre.extend(std::iter::repeat_n(AppIntent::MenuNext, moves));
            pre
        };
        // Rows 0 and 1, folded into row 2.
        let range = || vec![key('k'), key('V'), key('j')];
        let mut cases: Vec<HeadMove> = vec![
            ("commit key", to_commit, vec![], key('c')),
            ("amend key", to_commit, vec![], key('A')),
            ("open commit", to_commit, vec![], AppIntent::OpenCommit),
            ("open amend", to_commit, vec![], AppIntent::OpenAmend),
            (
                "commit item",
                to_commit,
                actions_menu(1),
                AppIntent::MenuSelect,
            ),
            (
                "amend item",
                to_commit,
                actions_menu(2),
                AppIntent::MenuSelect,
            ),
            (
                "checkout commit",
                on_commit,
                vec![],
                AppIntent::CheckoutCommit,
            ),
            ("revert", on_commit, vec![], AppIntent::RevertCommit),
            ("reset menu", on_commit, vec![], key('X')),
            ("paste", one_copied, vec![], AppIntent::PasteCommits),
            (
                "move up",
                on_linear_log,
                vec![],
                AppIntent::MoveCommit(Direction::Up),
            ),
            (
                "move down",
                on_linear_log,
                vec![],
                AppIntent::MoveCommit(Direction::Down),
            ),
            ("autosquash", with_a_fixup, vec![], AppIntent::Autosquash),
            ("create fixup", with_a_fixup, vec![], AppIntent::CreateFixup),
            (
                "checkout branch",
                with_branches,
                vec![AppIntent::BranchNext],
                AppIntent::CheckoutBranch,
            ),
            ("new branch", with_branches, vec![], AppIntent::NewBranch),
            (
                "new branch named",
                with_branches,
                vec![AppIntent::NewBranch],
                AppIntent::CreateBranchNamed("x".into()),
            ),
            (
                "track",
                on_remote_topic,
                vec![],
                AppIntent::TrackRemoteBranch,
            ),
            (
                "track named",
                on_remote_topic,
                vec![AppIntent::TrackRemoteBranch],
                AppIntent::TrackRemoteBranchNamed("x".into()),
            ),
            (
                "merge key",
                with_branches,
                vec![AppIntent::BranchNext],
                key('m'),
            ),
            (
                "rebase onto",
                with_branches,
                vec![AppIntent::BranchNext],
                AppIntent::RebaseOnto,
            ),
        ];
        for mode in [ResetMode::Soft, ResetMode::Mixed, ResetMode::Hard] {
            cases.push(("reset to", on_commit, vec![], AppIntent::ResetTo(mode)));
        }
        for edit in [Edit::Squash, Edit::Fixup, Edit::Drop, Edit::Reword] {
            cases.push((
                "rewrite",
                on_linear_log,
                vec![],
                AppIntent::RewriteCommit(edit),
            ));
        }
        for edit in [Edit::Squash, Edit::Fixup, Edit::Drop] {
            cases.push((
                "range rewrite",
                on_linear_log,
                range(),
                AppIntent::RewriteCommit(edit),
            ));
        }
        for strategy in [
            MergeStrategy::FastForwardOnly,
            MergeStrategy::NoFastForward,
            MergeStrategy::Squash,
        ] {
            cases.push((
                "merge",
                with_branches,
                vec![AppIntent::BranchNext],
                AppIntent::Merge(strategy),
            ));
        }
        cases
    }

    #[test]
    fn during_a_bisect_nothing_moves_head_or_makes_a_commit() {
        for (name, make, pre, action) in head_moves() {
            let ready = || {
                let mut state = make();
                state.branch.get_or_insert_with(|| on_branch("main"));
                for intent in &pre {
                    state.update(intent.clone());
                }
                state.notice = None;
                state
            };
            // Without a bisect the action runs or asks, so the bisect is
            // what stops it below.
            let mut idle = ready();
            let popups = idle.popups.len();
            let effects = idle.update(action.clone());
            assert_ne!(idle.notice, Some(bisect::RESET_FIRST.into()), "{name}");
            assert!(
                !effects.is_empty()
                    || idle.popups.len() > popups
                    || idle.commit != CommitFlow::Closed,
                "{name} {action:?} did nothing: {:?}",
                idle.notice
            );

            let mut state = ready();
            start_bisecting(&mut state);
            let popups = state.popups.clone();
            assert_eq!(state.update(action.clone()), vec![], "{name} {action:?}");
            assert_eq!(
                state.notice,
                Some(bisect::RESET_FIRST.into()),
                "{name} {action:?}"
            );
            assert_eq!(state.popups, popups, "{name} {action:?}");
            assert_eq!(state.commit, CommitFlow::Closed, "{name} {action:?}");
            assert_eq!(state.op_seq, None, "{name} {action:?}");
        }
    }

    #[test]
    fn an_editor_open_when_a_bisect_began_does_not_commit() {
        let mut state = editing();
        type_text(&mut state, "fix");
        let before = state.commit.clone();
        assert!(matches!(before, CommitFlow::Editing(_)));
        start_bisecting(&mut state);
        let effects = state.update(AppIntent::SubmitCommit);
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::ProbeCommit { .. })),
            "{effects:?}"
        );
        assert_eq!(effects, vec![]);
        assert_eq!(state.notice, Some(bisect::RESET_FIRST.into()));
        assert_eq!(state.commit, before);
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn marks_and_reset_still_run_during_a_bisect() {
        let mut state = testing_c1(&[Operation::Bisect]);
        let (_, op) = op_of(&state.update(AppIntent::BisectHead(BisectMark::Good)));
        assert_eq!(op, bisect_mark(BisectMark::Good, "c1", false));
        state.op_seq = None;
        let (_, op) = op_of(&state.update(AppIntent::BisectSelected(BisectMark::Bad)));
        assert_eq!(op, bisect_mark(BisectMark::Bad, "c0", false));
        state.op_seq = None;
        state.update(AppIntent::Ask(ConfirmKind::Abort(Operation::Bisect)));
        let (_, op) = op_of(&state.update(AppIntent::ConfirmBisectReset));
        assert_eq!(op, GitOp::BisectReset);
        assert_eq!(state.notice, None);
    }

    #[test]
    fn without_a_bisect_head_moves_are_not_refused_for_one() {
        let state = ready_to_bisect();
        let guards: [fn(&AppState) -> Option<Reason>; 6] = [
            keymap::commit_disabled,
            keymap::amend_disabled,
            keymap::checkout_commit_disabled,
            keymap::reset_disabled,
            crate::features::operation::keys::merge_disabled,
            keymap::new_branch_disabled,
        ];
        for guard in guards {
            assert_ne!(guard(&state), Some(bisect::RESET_FIRST.into()));
        }
        let mut state = ready_to_bisect();
        state.commits.selected = 1;
        state.update(AppIntent::CheckoutCommit);
        assert_eq!(state.notice, None);
        assert_ne!(state.popups, []);
        let mut state = ready_to_bisect();
        state.commits.selected = 1;
        assert_ne!(state.update(AppIntent::ResetTo(ResetMode::Soft)), vec![]);
        assert_eq!(state.notice, None);
    }

    #[test]
    fn with_a_rebase_and_a_bisect_the_rebase_is_named_first() {
        let mut state = testing_c1(&[Operation::Rebase, Operation::Bisect]);
        state.update(AppIntent::FocusPanel(layout::Panel::Side(
            SidePanel::Commits,
        )));
        for action in [
            AppIntent::RevertCommit,
            AppIntent::ResetTo(ResetMode::Soft),
            AppIntent::RewriteCommit(rebase_plan::Edit::Drop),
        ] {
            state.notice = None;
            assert_eq!(state.update(action.clone()), vec![], "{action:?}");
            assert_eq!(
                state.notice,
                Some("Finish or abort the operation in progress".into()),
                "{action:?}"
            );
        }
    }

    // --- List filters, ranges and the diff search ---

    fn has_run_op(effects: &[Effect]) -> bool {
        effects.iter().any(|e| matches!(e, Effect::RunOp { .. }))
    }

    #[test]
    fn typing_a_files_filter_narrows_the_list_and_esc_clears_it_then_quits() {
        let files = vec![
            changed("src/model.rs", '.', 'M'),
            changed("src/view.rs", '.', 'M'),
            changed("docs/mode.md", '.', 'M'),
            changed("src/main.rs", '.', 'M'),
        ];
        let (mut state, _) = with_files(files, 24);
        press_char(&mut state, 'j');
        assert_eq!(state.selected, Some(1));
        type_text(&mut state, "/mod");
        assert_eq!(state.key_focus(), KeyScope::Input);
        assert_eq!(state.files_filter, "mod");
        assert_eq!(state.files_visible(), [0, 2]);
        assert_eq!(
            state.selected,
            Some(0),
            "the hidden cursor moved to a shown row"
        );

        press(&mut state, KeyCode::Enter);
        assert_eq!(state.input, None);
        assert_eq!(state.files_filter, "mod", "Enter keeps the filter");
        press_char(&mut state, 'j');
        assert_eq!(state.selected, Some(2), "j skips the hidden row");
        press_char(&mut state, 'j');
        assert_eq!(state.selected, Some(2));
        press_char(&mut state, 'k');
        assert_eq!(state.selected, Some(0));

        press(&mut state, KeyCode::Esc);
        assert_eq!(state.files_filter, "");
        assert!(!state.should_quit);
        press(&mut state, KeyCode::Esc);
        assert!(
            state.should_quit,
            "with nothing to clear, Esc quits as before"
        );
    }

    #[test]
    fn esc_in_the_filter_input_clears_the_filter_and_closes_it() {
        let (mut state, _) = with_files(three_files(), 24);
        type_text(&mut state, "/b");
        assert_eq!(state.files_visible(), [1]);
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.input, None);
        assert_eq!(state.files_filter, "");
        assert_eq!(state.files_visible(), [0, 1, 2]);
        assert!(!state.should_quit);
    }

    #[test]
    fn typing_a_commits_filter_narrows_the_loaded_rows() {
        let mut state = with_commits(5, true);
        assert_eq!(state.key_focus(), KeyScope::Commits);
        type_text(&mut state, "/commit 3");
        assert_eq!(state.commits.filter, "commit 3");
        assert_eq!(state.commits.visible(), [3]);
        assert_eq!(state.commits.selected, 3);
        press(&mut state, KeyCode::Enter);
        press_char(&mut state, 'k');
        assert_eq!(
            state.commits.selected, 3,
            "nothing else is shown to move to"
        );
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.commits.filter, "");
        assert_eq!(state.commits.visible().len(), 5);
    }

    #[test]
    fn q_question_mark_and_d_are_typed_into_the_filter() {
        let (mut state, _) = with_files(three_files(), 24);
        press_char(&mut state, '/');
        let mut effects = Vec::new();
        for c in ['q', '?', 'd'] {
            effects.extend(press_char(&mut state, c));
        }
        assert_eq!(state.files_filter, "q?d");
        assert!(!state.should_quit);
        assert!(!state.help_open);
        assert!(state.popups.is_empty());
        assert!(!has_run_op(&effects), "{effects:?}");
    }

    #[test]
    fn space_stages_a_range_in_one_op_skipping_conflicts() {
        let files = vec![
            changed("a.rs", '.', 'M'),
            conflicted("c.rs"),
            untracked("d.rs"),
        ];
        let (mut state, _) = with_files(files, 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        press_char(&mut state, 'j');
        assert_eq!(state.files_range(), [0, 1, 2]);
        let (_, op) = op_of(&press_char(&mut state, ' '));
        assert_eq!(op, GitOp::Stage(vec!["a.rs".into(), "d.rs".into()]));
        assert_eq!(state.files_anchor, None, "the range is done with");
    }

    #[test]
    fn space_unstages_a_staged_range_with_each_renames_old_path() {
        let renamed = FileEntry {
            orig_path: Some("old.rs".into()),
            kind: EntryKind::Renamed,
            ..changed("new.rs", 'R', '.')
        };
        let files = vec![renamed, changed("b.rs", 'M', '.'), conflicted("c.rs")];
        let (mut state, _) = with_files(files, 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        press_char(&mut state, 'j');
        let (_, op) = op_of(&press_char(&mut state, ' '));
        assert_eq!(
            op,
            GitOp::Unstage(vec!["old.rs".into(), "new.rs".into(), "b.rs".into()])
        );
    }

    #[test]
    fn discarding_a_mixed_range_asks_with_the_counts_and_only_y_answers() {
        let files = vec![
            changed("a.rs", '.', 'M'),
            changed("b.rs", '.', 'M'),
            untracked("n.txt"),
        ];
        let (mut state, _) = with_files(files, 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        press_char(&mut state, 'j');
        assert_eq!(press_char(&mut state, 'd'), vec![]);
        let Some(Popup::Discard(confirm)) = state.popups.last() else {
            panic!("expected the discard question, got {:?}", state.popups);
        };
        assert_eq!(
            confirm.what.question(),
            "Discard unstaged changes to 2 files and delete 1 untracked file? (y/n)"
        );
        assert_eq!(confirm.what.title(), "Discard 3 files");
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.popups.len(), 1, "Enter does not answer");
        assert_eq!(press_char(&mut state, 'n'), vec![]);
        assert!(state.popups.is_empty());
        assert!(state.files_anchor.is_some(), "no keeps the range");

        press_char(&mut state, 'd');
        let (_, op) = op_of(&press_char(&mut state, 'y'));
        assert_eq!(
            op,
            GitOp::DiscardFiles {
                restore: vec!["a.rs".into(), "b.rs".into()],
                delete: vec!["n.txt".into()],
            }
        );
        assert_eq!(state.files_anchor, None);
    }

    #[test]
    fn a_range_with_nothing_to_discard_says_so() {
        let files = vec![changed("a.rs", 'M', '.'), changed("b.rs", 'M', '.')];
        let (mut state, _) = with_files(files, 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        assert_eq!(press_char(&mut state, 'd'), vec![]);
        assert!(state.popups.is_empty());
        assert_eq!(state.notice, Some("No unstaged changes to discard".into()));
    }

    #[test]
    fn a_files_range_refuses_single_file_keys() {
        let (mut state, _) = with_files(three_files(), 24);
        press_char(&mut state, 'V');
        for c in ['o', 'b', 'M', 'i'] {
            press_char(&mut state, c);
            assert_eq!(state.notice, Some(keymap::CLEAR_RANGE_FIRST.into()), "{c}");
        }
        press(&mut state, KeyCode::Enter);
        assert_eq!(state.notice, Some(keymap::CLEAR_RANGE_FIRST.into()));
        assert_eq!(state.focus, Focus::Files);
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.files_anchor, None);
        assert!(!state.should_quit, "Esc cleared the range first");
    }

    #[test]
    fn a_filter_hiding_every_row_refuses_row_keys() {
        let (mut state, _) = with_files(three_files(), 24);
        type_text(&mut state, "/zzz\n");
        assert!(state.files_visible().is_empty());
        assert_eq!(state.selected, Some(0), "nothing to move to");
        press_char(&mut state, ' ');
        assert_eq!(state.notice, Some(keymap::FILTER_HIDES_ROW.into()));
    }

    #[test]
    fn a_refresh_that_drops_the_anchor_clears_the_range() {
        let (mut state, _) = with_files(three_files(), 24);
        press_char(&mut state, 'V');
        let tag = refresh_tag(&mut state);
        let rest = three_files().split_off(1);
        state.update(AppIntent::StatusLoaded {
            tag,
            result: Ok(repo(rest)),
        });
        assert_eq!(state.files_anchor, None);
    }

    #[test]
    fn c_copies_a_range_skipping_merges_and_again_takes_them_out() {
        let mut state = with_merge();
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        press_char(&mut state, 'j');
        assert_eq!(state.commits.range(), [0, 1, 2]);
        press_char(&mut state, 'R');
        assert_eq!(state.notice, Some("Clear the selection first (esc)".into()));
        press_char(&mut state, 'B');
        assert_eq!(state.notice, Some("Clear the selection first (esc)".into()));
        assert!(state.popups.is_empty(), "no bisect menu over a range");
        press_char(&mut state, 'y');
        assert_eq!(state.notice, Some("Clear the selection first (esc)".into()));
        assert!(state.popups.is_empty(), "no copy menu over a range");

        assert_eq!(press_char(&mut state, 'c'), vec![]);
        assert_eq!(
            state.copied.ids(),
            [cid(&"0".repeat(40)), cid(&"2".repeat(40))]
        );
        assert_eq!(
            state.notice_text.as_deref(),
            Some("Copied 2 (1 merge skipped, 2 copied)")
        );
        assert_eq!(state.commits.range_anchor, None);

        press_char(&mut state, 'V');
        press_char(&mut state, 'k');
        press_char(&mut state, 'k');
        press_char(&mut state, 'c');
        assert!(state.copied.is_empty());
        assert_eq!(
            state.notice_text.as_deref(),
            Some("Removed 2 (1 merge skipped, 0 copied)")
        );
    }

    /// Two hunks; "needle" once in the first changed line of the second
    /// and twice in the next. Rows 8 and 9 hold the matches.
    const NEEDLES: &[u8] = b"diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n\
        @@ -1,2 +1,2 @@\n-one\n+two\n ctx\n\
        @@ -20,2 +20,2 @@\n-needle\n+needle needle\n end\n";

    fn current(state: &AppState) -> Option<usize> {
        view(state).search.as_ref().and_then(|s| s.current)
    }

    #[test]
    fn a_diff_search_jumps_cycles_wraps_and_clears() {
        let mut state = focused(three_files(), NEEDLES);
        type_text(&mut state, "/needle");
        assert_eq!(view(&state).search.as_ref().unwrap().matches.len(), 3);
        assert_eq!(view(&state).hunk, 0, "typing does not jump");
        press(&mut state, KeyCode::Enter);
        assert_eq!(state.input, None);
        assert_eq!(view(&state).hunk, 1, "Enter goes to the first match's hunk");
        assert_eq!(current(&state), Some(0));

        press_char(&mut state, 'n');
        press_char(&mut state, 'n');
        assert_eq!(current(&state), Some(2));
        assert_eq!(state.notice, None);
        press_char(&mut state, 'n');
        assert_eq!(current(&state), Some(0));
        assert_eq!(state.notice, Some("Search wrapped".into()));
        press_char(&mut state, 'N');
        assert_eq!(current(&state), Some(2));
        assert_eq!(state.notice, Some("Search wrapped".into()));

        press_char(&mut state, 'v');
        press_char(&mut state, '/');
        assert_eq!(state.notice, Some("Leave line mode first".into()));
        assert_eq!(state.input, None);
        press(&mut state, KeyCode::Esc);

        press(&mut state, KeyCode::Esc);
        assert!(
            matches!(state.focus, Focus::Diff(_)),
            "Esc only cleared the search"
        );
        assert_eq!(view(&state).search, None);
        press_char(&mut state, 'n');
        assert_eq!(state.notice, Some("Nothing searched yet (/)".into()));
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.focus, Focus::Files);
        assert!(!state.should_quit);
    }

    // --- Search in the diff preview, the blame and its commit ---

    /// The tag of the diff load among `effects`.
    fn load_diff_tag(effects: &[Effect]) -> Tag {
        effects
            .iter()
            .find_map(|e| match e {
                Effect::LoadDiff { tag, .. } => Some(*tag),
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected a LoadDiff in {effects:?}"))
    }

    /// 60 preview lines, "foo" on rows 30 and 50, more still to come.
    fn preview_loaded(state: &mut AppState, tag: Tag) {
        let lines = (0..60)
            .map(|i| line(if i == 30 || i == 50 { "+foo" } else { " x" }))
            .collect();
        state.update(AppIntent::DiffLoaded {
            tag,
            result: Ok(DiffChunk { lines, done: false }),
        });
    }

    fn preview_current(state: &AppState) -> Option<usize> {
        state.diff.search.as_ref().and_then(|s| s.current)
    }

    /// ctrl-f, `foo`, Enter: the preview scrolls to the first match, and
    /// `n` and `N` cycle round both ways.
    fn search_the_preview_for_foo(state: &mut AppState) {
        ctrl(state, 'f');
        assert_eq!(state.key_focus(), KeyScope::Input);
        type_text(state, "foo");
        assert_eq!(state.diff.scroll, 0, "typing does not jump");
        press(state, KeyCode::Enter);
        assert_eq!(state.input, None);
        // 21 rows: row 30 is the bottom one.
        assert_eq!(state.diff.scroll, 10);
        assert_eq!(preview_current(state), Some(0));

        press_char(state, 'n');
        assert_eq!((preview_current(state), state.diff.scroll), (Some(1), 30));
        assert_eq!(state.notice, None);
        press_char(state, 'n');
        assert_eq!((preview_current(state), state.diff.scroll), (Some(0), 30));
        assert_eq!(state.notice, Some("Search wrapped".into()));
        press_char(state, 'N');
        assert_eq!(preview_current(state), Some(1));
        assert_eq!(state.notice, Some("Search wrapped".into()));
        press_char(state, 'N');
        assert_eq!(preview_current(state), Some(0));
    }

    #[test]
    fn ctrl_f_searches_the_files_preview_and_esc_clears_range_filter_search_then_quits() {
        let (mut state, effects) = with_files(three_files(), 24);
        preview_loaded(&mut state, diff_tag(&effects));
        type_text(&mut state, "/a\n");
        press_char(&mut state, 'V');
        search_the_preview_for_foo(&mut state);
        assert_eq!(state.diff.path, Some("a.rs".into()));

        press(&mut state, KeyCode::Esc);
        assert_eq!(state.files_anchor, None, "the range goes first");
        assert!(state.diff.search.is_some());
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.files_filter, "", "then the filter");
        assert!(state.diff.search.is_some());
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.diff.search, None, "then the search");
        assert!(!state.should_quit);
        press_char(&mut state, 'n');
        assert_eq!(state.notice, Some("Nothing searched yet (ctrl-f)".into()));
        press(&mut state, KeyCode::Esc);
        assert!(state.should_quit, "then Esc quits as before");
    }

    /// Files with a found preview search for "foo".
    fn files_searching_foo() -> AppState {
        let (mut state, effects) = with_files(three_files(), 24);
        preview_loaded(&mut state, diff_tag(&effects));
        ctrl(&mut state, 'f');
        type_text(&mut state, "foo\n");
        assert!(state.diff.search.is_some());
        assert_eq!(state.focus, Focus::Files);
        state
    }

    #[test]
    fn switching_the_side_panel_clears_the_preview_search() {
        for (key, side) in [('1', SidePanel::Status), ('4', SidePanel::Commits)] {
            let mut state = files_searching_foo();
            press_char(&mut state, key);
            assert_eq!(state.expanded, side, "{key}");
            assert_eq!(state.diff.search, None, "{key}");
        }
        for back in [false, true] {
            let mut state = files_searching_foo();
            state.update(AppIntent::CycleFocus { back });
            assert_ne!(state.expanded, SidePanel::Files, "back: {back}");
            assert_eq!(state.diff.search, None, "back: {back}");
        }
    }

    #[test]
    fn the_diff_pane_and_back_to_files_keeps_the_preview_search() {
        let mut state = files_searching_foo();
        state.update(AppIntent::FocusPanel(crate::layout::Panel::Diff));
        assert!(matches!(state.focus, Focus::Diff(_)), "{:?}", state.focus);
        state.update(AppIntent::FocusLeft);
        assert_eq!(state.focus, Focus::Files);
        assert!(state.diff.search.is_some());
        // Files again by its number: the side panel did not change.
        press_char(&mut state, '2');
        assert_eq!(state.focus, Focus::Files);
        assert!(state.diff.search.is_some());
    }

    #[test]
    fn esc_in_the_preview_search_input_clears_it_and_stays_in_files() {
        let mut state = files_searching_foo();
        ctrl(&mut state, 'f');
        assert!(state.input.is_some());
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.input, None);
        assert_eq!(state.diff.search, None);
        assert_eq!(state.focus, Focus::Files);
        assert!(!state.should_quit);
    }

    #[test]
    fn esc_in_the_blame_search_input_clears_it_and_keeps_the_blame() {
        let mut state = blamed_foo();
        type_text(&mut state, "/foo\n");
        assert!(blame(&state).search.is_some());
        press_char(&mut state, '/');
        assert_eq!(
            state.input.as_ref().map(|i| i.target),
            Some(InputTarget::BlameSearch)
        );
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.input, None);
        assert_eq!(blame(&state).search, None);
    }

    #[test]
    fn esc_in_the_blame_commit_search_input_clears_it_and_keeps_the_commit() {
        let mut state = blamed_foo();
        let tag = match press(&mut state, KeyCode::Enter).as_slice() {
            [Effect::LoadCommit { tag, .. }] => *tag,
            other => panic!("expected one LoadCommit, got {other:?}"),
        };
        state.update(AppIntent::CommitLoaded {
            tag,
            result: Ok(vec![line("+foo")]),
        });
        type_text(&mut state, "/foo\n");
        assert!(blame(&state).commit.as_ref().unwrap().search.is_some());
        press_char(&mut state, '/');
        assert_eq!(
            state.input.as_ref().map(|i| i.target),
            Some(InputTarget::BlameCommitSearch)
        );
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.input, None);
        let commit = blame(&state).commit.as_ref().expect("still open");
        assert_eq!(commit.search, None);
    }

    #[test]
    fn a_preview_search_with_no_match_says_so_on_enter_and_n() {
        let (mut state, effects) = with_files(three_files(), 24);
        preview_loaded(&mut state, diff_tag(&effects));
        ctrl(&mut state, 'f');
        type_text(&mut state, "zzz\n");
        assert_eq!(state.notice, Some("No matches".into()));
        state.notice = None;
        press_char(&mut state, 'n');
        assert_eq!(state.notice, Some("No matches".into()));
    }

    #[test]
    fn the_preview_search_waits_for_a_file() {
        let (mut state, _) = with_files(vec![], 24);
        state.focus = Focus::Files;
        assert_eq!(state.diff.path, None);
        ctrl(&mut state, 'f');
        assert_eq!(state.input, None);
        assert_eq!(state.notice, Some("No file selected".into()));
    }

    #[test]
    fn a_commits_files_preview_is_searched_and_esc_clears_it_before_going_back() {
        let mut state = on_commit_five();
        let tag = load_diff_tag(&opened(&mut state, two_commit_files()));
        preview_loaded(&mut state, tag);
        assert_eq!(state.key_focus(), KeyScope::CommitFiles);
        search_the_preview_for_foo(&mut state);

        press(&mut state, KeyCode::Esc);
        assert_eq!(state.diff.search, None);
        assert_eq!(state.key_focus(), KeyScope::CommitFiles, "still open");
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.commits.open, None);
        assert_eq!(state.key_focus(), KeyScope::Commits);
        assert!(!state.should_quit);
    }

    /// A file, a rename and another file.
    fn three_commit_files() -> Vec<CommitFile> {
        vec![
            commit_file('M', "src/lib.rs", None),
            commit_file('R', "src/new.rs", Some("legacy/old.rs")),
            commit_file('A', "docs/guide.md", None),
        ]
    }

    /// `c5` open with `three_commit_files`, the first one's diff streaming.
    fn commit_files_open() -> AppState {
        let mut state = on_commit_five();
        opened(&mut state, three_commit_files());
        state
    }

    fn open_files(state: &AppState) -> &CommitFilesList {
        state.commits.open.as_ref().expect("a commit is open")
    }

    /// Each char pressed in turn; every effect in order.
    fn typed(state: &mut AppState, text: &str) -> Vec<Effect> {
        text.chars().flat_map(|c| press_char(state, c)).collect()
    }

    #[test]
    fn slash_in_a_commits_files_narrows_them_by_drawn_path_and_enter_keeps_it() {
        let mut state = commit_files_open();
        assert_eq!(press_char(&mut state, '/'), vec![]);
        let input = state.input.as_ref().expect("the input opens");
        assert_eq!(input.target, InputTarget::CommitFilesFilter);
        assert_eq!(input.target.prompt(), "/");
        assert_eq!(state.key_focus(), KeyScope::Input);
        type_text(&mut state, "src");
        assert_eq!(open_files(&state).visible(), [0, 1]);
        for _ in 0..3 {
            press(&mut state, KeyCode::Backspace);
        }
        type_text(&mut state, "legacy");
        assert_eq!(open_files(&state).visible(), [1], "a rename's old name");
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.input, None);
        assert_eq!(open_files(&state).filter, "legacy");
        assert_eq!(state.key_focus(), KeyScope::CommitFiles);
        // `/` again starts from the kept text.
        press_char(&mut state, '/');
        assert_eq!(state.input.as_ref().unwrap().line.text, "legacy");
    }

    #[test]
    fn a_filter_that_hides_the_cursor_loads_the_first_shown_files_diff() {
        let mut state = commit_files_open();
        press_char(&mut state, '/');
        // No other path has an m.
        let effects = typed(&mut state, "md");
        assert_eq!(open_files(&state).selected, 2);
        let request = diff_request(&effects);
        assert_eq!(request.path, "docs/guide.md".into());
        assert_eq!(request.commit, Some(range_of("c5", "c6")));
        // Typing on with the cursor still shown loads nothing more.
        assert_eq!(typed(&mut state, " guide"), vec![]);
        // Hiding every row leaves the cursor and loads nothing.
        assert_eq!(typed(&mut state, "zzz"), vec![]);
        assert_eq!(open_files(&state).selected, 2);
        assert!(open_files(&state).visible().is_empty());
    }

    #[test]
    fn j_and_k_in_a_commits_files_step_over_hidden_rows() {
        let mut state = commit_files_open();
        type_text(&mut state, "/rs\n"); // lib.rs and the rename
        assert_eq!(open_files(&state).visible(), [0, 1]);
        let request = diff_request(&press_char(&mut state, 'j'));
        assert_eq!(request.path, "src/new.rs".into());
        assert_eq!(
            press_char(&mut state, 'j'),
            vec![],
            "stops at the last shown"
        );
        assert_eq!(open_files(&state).selected, 1);
        press_char(&mut state, 'k');
        assert_eq!(open_files(&state).selected, 0);

        // `/` starts from `rs`: `d` in its place shows the rename and the doc.
        press_char(&mut state, '/');
        press(&mut state, KeyCode::Backspace);
        press(&mut state, KeyCode::Backspace);
        type_text(&mut state, "d\n");
        assert_eq!(open_files(&state).visible(), [1, 2]);
        assert_eq!(open_files(&state).selected, 1);
        press_char(&mut state, 'j');
        assert_eq!(open_files(&state).selected, 2);
        press_char(&mut state, 'k');
        press_char(&mut state, 'k');
        assert_eq!(open_files(&state).selected, 1, "row 0 is hidden");
    }

    #[test]
    fn esc_in_a_commits_files_clears_the_filter_then_the_search_then_goes_back() {
        let mut state = on_commit_five();
        let tag = load_diff_tag(&opened(&mut state, two_commit_files()));
        preview_loaded(&mut state, tag);
        search_the_preview_for_foo(&mut state);
        type_text(&mut state, "/lib\n");
        assert_eq!(open_files(&state).filter, "lib");

        assert_eq!(press(&mut state, KeyCode::Esc), vec![]);
        assert_eq!(open_files(&state).filter, "", "the filter goes first");
        assert!(state.diff.search.is_some(), "the search stays");
        assert_eq!(state.key_focus(), KeyScope::CommitFiles);
        assert!(!state.should_quit);
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.diff.search, None, "then the search");
        assert_eq!(state.key_focus(), KeyScope::CommitFiles);
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.commits.open, None, "then back to the log");
        assert_eq!(state.key_focus(), KeyScope::Commits);
        assert_eq!(state.commits.selected, 5, "on the same commit");
        assert!(!state.should_quit);

        // With neither set, one Esc goes back.
        let mut state = commit_files_open();
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.commits.open, None);
        assert_eq!(state.commits.selected, 5);
        assert!(!state.should_quit);
    }

    #[test]
    fn the_commit_files_filter_stops_at_its_limit() {
        let mut state = commit_files_open();
        press_char(&mut state, '/');
        for _ in 0..300 {
            press_char(&mut state, 'z');
        }
        assert_eq!(
            open_files(&state).filter.chars().count(),
            list_filter::MAX_INPUT
        );
        assert_eq!(state.notice, Some(list_filter::input_full_notice().into()));
    }

    #[test]
    fn each_open_of_a_commits_files_starts_with_no_filter() {
        let mut state = commit_files_open();
        type_text(&mut state, "/lib\n");
        press_char(&mut state, 'h');
        assert_eq!(state.commits.open, None);
        press_char(&mut state, 'j');
        let (_, range) = files_load(&press(&mut state, KeyCode::Enter));
        assert_eq!(range, range_of("c6", "c7"));
        assert_eq!(open_files(&state).filter, "");
    }

    #[test]
    fn a_hidden_commit_file_is_neither_picked_nor_focused() {
        let mut state = commit_files_open();
        type_text(&mut state, "/zzz\n");
        assert!(open_files(&state).selected_hidden());
        assert_eq!(
            crate::keymap::patch_file_disabled(&state).as_deref(),
            Some(crate::keymap::FILTER_HIDES_ROW)
        );
        assert_eq!(state.update(AppIntent::PatchToggleFile), vec![]);
        assert_eq!(state.custom_patch, None);
        assert_eq!(state.notice, Some(crate::keymap::FILTER_HIDES_ROW.into()));
        state.notice = None;
        assert_eq!(press_char(&mut state, 'l'), vec![]);
        assert_eq!(state.focus, Focus::Commits);
        assert_eq!(state.notice, Some(crate::keymap::FILTER_HIDES_ROW.into()));
    }

    #[test]
    fn the_commit_files_filter_never_reaches_git() {
        let hostile = "--output=x -- \x1b[31m\u{202e}";
        let leaks = |effects: &[Effect]| {
            let debug = format!("{effects:?}");
            debug.contains("--output=x") || debug.contains("[31m") || debug.contains('\u{202e}')
        };
        let mut state = commit_files_open();
        let twin = commit_files_open();
        let mut effects = press_char(&mut state, '/');
        for c in hostile.chars() {
            effects.extend(press_char(&mut state, c));
        }
        effects.extend(press(&mut state, KeyCode::Enter));
        let kept = open_files(&state).filter.clone();
        assert!(kept.starts_with("--output=x -- "), "{kept:?}");
        assert!(kept.ends_with('\u{202e}'), "{kept:?}");
        for key in ['j', 'k', 'l'] {
            effects.extend(press_char(&mut state, key));
        }
        effects.extend(state.update(AppIntent::ToggleWhitespace));
        let refresh = press_char(&mut state, 'r');
        assert!(!leaks(&effects), "{effects:?}");
        assert!(!leaks(&refresh), "{refresh:?}");
        assert!(!format!("{effects:?}{refresh:?}").contains(&kept));
        // The list git was asked for is the same as without a filter.
        assert_eq!(open_files(&state).range, open_files(&twin).range);
        let mut twin = twin;
        twin.update(AppIntent::ToggleWhitespace);
        assert_eq!(refresh, press_char(&mut twin, 'r'));
    }

    #[test]
    fn a_stashs_files_preview_is_searched_and_esc_clears_it_before_going_back() {
        let mut state = on_stash_one();
        let tag = load_diff_tag(&stash_opened(&mut state));
        preview_loaded(&mut state, tag);
        assert_eq!(state.key_focus(), KeyScope::StashFiles);
        search_the_preview_for_foo(&mut state);

        press(&mut state, KeyCode::Esc);
        assert_eq!(state.diff.search, None);
        assert_eq!(state.key_focus(), KeyScope::StashFiles, "still open");
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.stash.open, None);
        assert_eq!(state.key_focus(), KeyScope::Stash);
        assert!(!state.should_quit);
    }

    /// A loaded blame of five committed lines, "foo" on lines 2 and 4.
    fn blamed_foo() -> AppState {
        let (mut state, tag) = blaming(24);
        let lines = ["fn a", "foo", "x", "foo bar", "y"]
            .iter()
            .enumerate()
            .map(|(i, text)| crate::git::blame::BlameLine {
                text: text.to_string(),
                ..blame_line(OID, i + 1)
            })
            .collect();
        state.update(AppIntent::BlameLoaded {
            tag,
            result: Ok(lines),
        });
        state
    }

    #[test]
    fn the_blame_and_its_commit_each_search_on_their_own() {
        let mut state = blamed_foo();
        type_text(&mut state, "/foo");
        assert_eq!(blame(&state).cursor, 0, "typing does not move the cursor");
        press(&mut state, KeyCode::Enter);
        assert_eq!(blame(&state).cursor, 1);
        press_char(&mut state, 'n');
        assert_eq!(blame(&state).cursor, 3);
        press_char(&mut state, 'n');
        assert_eq!(blame(&state).cursor, 1);
        assert_eq!(state.notice, Some("Search wrapped".into()));
        press_char(&mut state, 'N');
        assert_eq!(blame(&state).cursor, 3);

        // Enter opens the match's line's commit.
        let tag = match press(&mut state, KeyCode::Enter).as_slice() {
            [Effect::LoadCommit { tag, oid }] => {
                assert_eq!(oid, OID);
                *tag
            }
            other => panic!("expected one LoadCommit, got {other:?}"),
        };
        press_char(&mut state, '/');
        assert_eq!(state.input, None, "nothing to search while it loads");
        assert_eq!(state.notice, Some("Loading…".into()));
        state.update(AppIntent::CommitLoaded {
            tag,
            result: Ok(vec![line("commit abc"), line("+foo"), line(" ctx")]),
        });
        press_char(&mut state, '/');
        assert_eq!(
            state
                .input
                .as_ref()
                .map(|i| (i.target, i.line.text.as_str())),
            Some((InputTarget::BlameCommitSearch, "")),
            "the commit's own search, not the blame's"
        );
        type_text(&mut state, "ctx\n");
        let commit = blame(&state).commit.as_ref().unwrap();
        let found = commit.search.as_ref().unwrap();
        assert_eq!((found.query.as_str(), found.current), ("ctx", Some(0)));
        assert_eq!(blame(&state).search.as_ref().unwrap().query, "foo");

        press(&mut state, KeyCode::Esc);
        let commit = blame(&state).commit.as_ref().expect("still open");
        assert_eq!(commit.search, None);
        press(&mut state, KeyCode::Esc);
        assert_eq!(blame(&state).commit, None);
        assert_eq!(state.panel_focus(), KeyScope::Blame);
        assert!(blame(&state).search.is_some(), "the blame kept its search");

        press(&mut state, KeyCode::Esc);
        assert_eq!(blame(&state).search, None);
        press_char(&mut state, 'n');
        assert_eq!(state.notice, Some("Nothing searched yet (/)".into()));
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.focus, Focus::Files);
        assert!(!state.should_quit);
    }

    #[test]
    fn keys_typed_into_each_new_search_are_only_text() {
        let typed = "q?d\u{202E}";
        let check = |mut state: AppState, open: &dyn Fn(&mut AppState), target| {
            open(&mut state);
            assert_eq!(state.input.as_ref().map(|i| i.target), Some(target));
            let mut effects = Vec::new();
            for c in typed.chars() {
                effects.extend(press_char(&mut state, c));
            }
            assert_eq!(state.input.as_ref().unwrap().line.text, typed);
            assert!(!state.should_quit && !state.help_open, "{target:?}");
            assert!(state.popups.is_empty(), "{target:?}");
            effects.extend(press(&mut state, KeyCode::Enter));
            for effect in &effects {
                assert!(
                    !matches!(
                        effect,
                        Effect::RunOp { .. }
                            | Effect::ProbeCommit { .. }
                            | Effect::CheckLeftBehind { .. }
                    ),
                    "{target:?}: {effect:?}"
                );
                let shown = format!("{effect:?}");
                assert!(!shown.contains("q?d"), "{target:?}: {shown}");
            }
        };
        let ctrl_f = |state: &mut AppState| {
            ctrl(state, 'f');
        };
        let slash = |state: &mut AppState| {
            press_char(state, '/');
        };

        let (mut files, effects) = with_files(three_files(), 24);
        preview_loaded(&mut files, diff_tag(&effects));
        check(files, &ctrl_f, InputTarget::PreviewSearch);

        let mut commit_files = on_commit_five();
        let tag = load_diff_tag(&opened(&mut commit_files, two_commit_files()));
        preview_loaded(&mut commit_files, tag);
        check(commit_files, &ctrl_f, InputTarget::PreviewSearch);

        let mut stash_files = on_stash_one();
        let tag = load_diff_tag(&stash_opened(&mut stash_files));
        preview_loaded(&mut stash_files, tag);
        check(stash_files, &ctrl_f, InputTarget::PreviewSearch);

        check(blamed_foo(), &slash, InputTarget::BlameSearch);

        let mut commit = blamed_foo();
        let tag = match press(&mut commit, KeyCode::Enter).as_slice() {
            [Effect::LoadCommit { tag, .. }] => *tag,
            other => panic!("expected one LoadCommit, got {other:?}"),
        };
        commit.update(AppIntent::CommitLoaded {
            tag,
            result: Ok(vec![line("+foo")]),
        });
        check(commit, &slash, InputTarget::BlameCommitSearch);
    }

    #[test]
    fn the_focused_commit_file_diff_is_still_searched_with_slash() {
        let mut state = on_commit_five();
        let tag = load_diff_tag(&opened(&mut state, two_commit_files()));
        preview_loaded(&mut state, tag);
        state.update(AppIntent::FocusPanel(crate::layout::Panel::Diff));
        let Focus::Diff(view) = &state.focus else {
            panic!("expected the commit file's hunks, got {:?}", state.focus);
        };
        assert!(view.commit.is_some());
        press_char(&mut state, '/');
        assert_eq!(
            state.input.as_ref().map(|i| i.target),
            Some(InputTarget::DiffSearch)
        );
    }

    #[test]
    fn the_input_stops_at_its_limit_but_still_edits() {
        let (mut state, _) = with_files(three_files(), 24);
        press_char(&mut state, '/');
        for _ in 0..300 {
            press_char(&mut state, 'z');
        }
        assert_eq!(state.files_filter.chars().count(), list_filter::MAX_INPUT);
        assert_eq!(state.notice, Some(list_filter::input_full_notice().into()));
        press(&mut state, KeyCode::Backspace);
        assert_eq!(
            state.files_filter.chars().count(),
            list_filter::MAX_INPUT - 1
        );
    }

    #[test]
    fn stage_from_another_panel_stages_the_one_file_not_the_range() {
        let (mut state, _) = with_files(three_files(), 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        state.update(AppIntent::FocusPanel(crate::layout::Panel::Side(
            SidePanel::Branches,
        )));
        let (_, op) = op_of(&state.update(AppIntent::ToggleStage));
        assert_eq!(op, GitOp::toggle_for(&state.files[1]).unwrap());
    }

    #[test]
    fn a_changed_filter_says_it_cleared_the_selection() {
        let (mut state, _) = with_files(three_files(), 24);
        press_char(&mut state, 'V');
        type_text(&mut state, "/a");
        assert_eq!(state.files_anchor, None);
        assert_eq!(state.notice, Some("Selection cleared".into()));
    }

    /// The one menu open over `state`, and its title.
    fn open_menu(state: &AppState) -> MenuId {
        match state.popups.as_slice() {
            [Popup::Menu { id, .. }] => *id,
            other => panic!("expected one menu, got {other:?}"),
        }
    }

    #[test]
    fn y_opens_the_copy_menu_and_each_item_copies_its_part_of_the_selected_commit() {
        let mut state = copy_ready();
        state.commits.rows[1].short_oid = "1111111".into();
        assert_eq!(press_char(&mut state, 'y'), vec![]);
        assert_eq!(open_menu(&state), MenuId::Copy);

        let cases = [
            (CopyWhat::Hash, COPIED_OID, "hash"),
            (CopyWhat::ShortHash, "1111111", "short hash"),
            (CopyWhat::Subject, "commit 1", "subject"),
        ];
        for (what, text, label) in cases {
            let effects = state.update(AppIntent::CopyCommitText(what));
            assert_eq!(effects, [Effect::Clipboard(text.into())], "{what:?}");
            assert_eq!(
                state.notice_text,
                Some(format!("Copied {label}: {text}")),
                "{what:?}"
            );
        }
        // The notice keeps the first 40 characters; the copy has them all.
        state.commits.rows[1].subject = "s".repeat(41);
        let effects = state.update(AppIntent::CopyCommitText(CopyWhat::Subject));
        assert_eq!(effects, [Effect::Clipboard("s".repeat(41))]);
        assert_eq!(
            state.notice_text,
            Some(format!("Copied subject: {}…", "s".repeat(40)))
        );
    }

    #[test]
    fn a_short_hash_that_is_not_hex_is_not_copied() {
        let mut state = copy_ready();
        state.commits.rows[1].short_oid = "-x".into();
        assert_eq!(
            state.update(AppIntent::CopyCommitText(CopyWhat::ShortHash)),
            vec![]
        );
        assert_eq!(state.notice, Some("Not a commit id git can use".into()));
    }

    #[test]
    fn with_no_commit_selected_nothing_is_copied() {
        let mut state = AppState::new(Context::Workspace);
        for what in [CopyWhat::Hash, CopyWhat::Message] {
            assert_eq!(state.update(AppIntent::CopyCommitText(what)), vec![]);
            assert_eq!(state.notice, Some("No commit selected".into()));
        }
        for item in menu::items(MenuId::Copy) {
            assert_eq!((item.disabled)(&state), Some("No commit selected".into()));
            // A forge URL needs a remote on a forge host as well.
            let ready = (item.disabled)(&copy_ready());
            if item.intent == AppIntent::CopyCommitText(CopyWhat::ForgeUrl) {
                assert!(
                    ready.is_some_and(|r| r.contains("forge host")),
                    "{}",
                    item.label
                );
            } else {
                assert_eq!(ready, None, "{}", item.label);
            }
        }
    }

    #[test]
    fn the_full_message_is_read_first_and_only_the_latest_read_is_copied() {
        let mut state = copy_ready();
        let busy = state.busy;
        let first = state.update(AppIntent::CopyCommitText(CopyWhat::Message));
        let [Effect::LoadCommitMessage { tag: old, oid }] = first.as_slice() else {
            panic!("expected a message load, got {first:?}");
        };
        assert_eq!(oid, &cid(COPIED_OID));
        assert!(old.user);
        assert_eq!(state.busy, busy + 1);
        let second = state.update(AppIntent::CopyCommitText(CopyWhat::Message));
        let [Effect::LoadCommitMessage { tag: new, .. }] = second.as_slice() else {
            panic!("expected a message load, got {second:?}");
        };
        assert_eq!(state.busy, busy + 2);

        let stale = state.update(AppIntent::CommitMessageLoaded {
            tag: *old,
            oid: cid(COPIED_OID),
            result: Ok("old".into()),
        });
        assert_eq!(stale, vec![]);
        assert_eq!(state.busy, busy + 1);
        let effects = state.update(AppIntent::CommitMessageLoaded {
            tag: *new,
            oid: cid(COPIED_OID),
            result: Ok("subj\n\nbody".into()),
        });
        assert_eq!(effects, [Effect::Clipboard("subj\n\nbody".into())]);
        assert_eq!(
            state.notice_text.as_deref(),
            Some("Copied message: subj  body")
        );
        assert_eq!((state.busy, state.pending_copy.clone()), (busy, None));
    }

    #[test]
    fn a_failed_read_or_copy_shows_an_error() {
        let mut state = copy_ready();
        let effects = state.update(AppIntent::CopyCommitText(CopyWhat::Message));
        let [Effect::LoadCommitMessage { tag, oid }] = effects.as_slice() else {
            panic!("expected a message load, got {effects:?}");
        };
        let failed = AppIntent::CommitMessageLoaded {
            tag: *tag,
            oid: oid.clone(),
            result: Err("bad object".into()),
        };
        assert_eq!(state.update(failed), vec![]);
        assert!(matches!(state.popups.last(), Some(Popup::Error(e)) if e.message == "bad object"));

        let mut state = copy_ready();
        state.update(AppIntent::CopyCommitText(CopyWhat::Hash));
        state.update(AppIntent::CopyFailed(clipboard::TOO_LONG.into()));
        assert_eq!(state.notice_text, None);
        assert!(
            matches!(state.popups.last(), Some(Popup::Error(e)) if e.message == clipboard::TOO_LONG)
        );

        // A clipboard program's failure arrives after "Copied …" is shown.
        let mut state = copy_ready();
        state.update(AppIntent::CopyCommitText(CopyWhat::Hash));
        assert!(state.notice_text.is_some());
        let late = "false failed: exit status: 1. Check clipboard.command in the global config.";
        state.update(AppIntent::CopyFailed(late.into()));
        assert_eq!(state.notice_text, None);
        assert!(matches!(
            state.popups.last(),
            Some(Popup::Error(e)) if e.title == "Could not copy" && e.message == late
        ));
    }

    /// The text and kind of the prompt on top.
    fn top_prompt(state: &AppState) -> (PromptKind, String) {
        match state.popups.last() {
            Some(Popup::Prompt { kind, input }) => (*kind, input.text.clone()),
            other => panic!("expected a prompt, got {other:?}"),
        }
    }

    /// Files listing a modified `a.rs` and an untracked file named by
    /// `name`'s bytes, with `a.rs` selected.
    fn ignorable(name: &[u8]) -> AppState {
        let mut file = untracked("x");
        file.path = RepoPath::from_bytes(name);
        let (state, _) = with_files(vec![changed("a.rs", '.', 'M'), file], 24);
        assert_eq!(state.selected, Some(0));
        state
    }

    #[test]
    fn i_on_a_tracked_file_says_why_it_cannot_be_ignored() {
        let mut state = ignorable(b"build.log");
        assert_eq!(press_char(&mut state, 'i'), vec![]);
        assert_eq!(
            state.notice,
            Some("Only untracked files can be ignored".into())
        );
        assert!(state.popups.is_empty());

        let (mut empty, _) = with_files(vec![], 24);
        press_char(&mut empty, 'i');
        assert_eq!(empty.notice, Some("No files".into()));
    }

    #[test]
    fn i_on_an_untracked_file_offers_its_exact_pattern_and_enter_adds_it() {
        let mut state = ignorable(b"build[1].log");
        press_char(&mut state, 'j');
        assert_eq!(press_char(&mut state, 'i'), vec![]);
        assert_eq!(
            top_prompt(&state),
            (PromptKind::IgnorePattern, "/build\\[1].log".into())
        );
        let effects = press(&mut state, KeyCode::Enter);
        let [Effect::AppendIgnore { tag, line }] = effects.as_slice() else {
            panic!("expected an append, got {effects:?}");
        };
        assert_eq!(line, b"/build\\[1].log");
        assert!(state.popups.is_empty());
        assert_eq!(state.ignore_draft, None);

        let effects = state.update(AppIntent::IgnoreWritten {
            tag: *tag,
            result: Ok(IgnoreAdded {
                pattern: "/build\\[1].log".into(),
                added: true,
            }),
        });
        assert_eq!(
            state.notice_text.as_deref(),
            Some("Added /build\\[1].log to .gitignore")
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::LoadStatus { .. })),
            "{effects:?}"
        );
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::RunOp { .. })),
            "no git command runs: {effects:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_utf8_is_added_by_its_exact_bytes() {
        let mut state = ignorable(b"x\xff");
        press_char(&mut state, 'j');
        press_char(&mut state, 'i');
        assert_eq!(
            top_prompt(&state),
            (PromptKind::IgnorePattern, "/x\u{FFFD}".into())
        );
        let effects = press(&mut state, KeyCode::Enter);
        assert!(
            matches!(effects.as_slice(), [Effect::AppendIgnore { line, .. }] if line == b"/x\xff"),
            "{effects:?}"
        );
    }

    #[test]
    fn an_edited_pattern_is_added_as_typed_and_a_blank_one_is_refused() {
        let mut state = ignorable(b"build.log");
        press_char(&mut state, 'j');
        press_char(&mut state, 'i');
        if let Some(Popup::Prompt { input, .. }) = state.popups.last_mut() {
            *input = LineInput::with_text("  ");
        }
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice, Some("Type a pattern to ignore".into()));
        assert_eq!(top_prompt(&state).0, PromptKind::IgnorePattern);

        if let Some(Popup::Prompt { input, .. }) = state.popups.last_mut() {
            *input = LineInput::with_text("*.log");
        }
        let effects = press(&mut state, KeyCode::Enter);
        assert!(
            matches!(effects.as_slice(), [Effect::AppendIgnore { line, .. }] if line == b"*.log"),
            "{effects:?}"
        );
    }

    #[test]
    fn esc_drops_the_draft_and_a_failed_write_shows_an_error() {
        let mut state = ignorable(b"build.log");
        press_char(&mut state, 'j');
        press_char(&mut state, 'i');
        press(&mut state, KeyCode::Esc);
        assert!(state.popups.is_empty());
        assert_eq!(state.ignore_draft, None);

        press_char(&mut state, 'i');
        let effects = press(&mut state, KeyCode::Enter);
        let [Effect::AppendIgnore { tag, .. }] = effects.as_slice() else {
            panic!("expected an append, got {effects:?}");
        };
        let effects = state.update(AppIntent::IgnoreWritten {
            tag: *tag,
            result: Err(crate::worktree_file::CHANGED_ON_DISK.into()),
        });
        assert_eq!(effects, vec![]);
        assert!(matches!(state.popups.last(), Some(Popup::Error(_))));
    }

    // --- Stash ---

    use crate::git::stash::StashRef;
    use crate::popup::DropStashConfirm;

    fn stash_ref(index: usize, oid: &str) -> StashRef {
        StashRef {
            index,
            oid: cid(oid),
        }
    }

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
        let busy = state.busy;
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
        assert_eq!(state.busy, busy);
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
        let busy = state.busy;

        let (tag, entry) = stash_files_load(&press(&mut state, KeyCode::Enter));

        assert_eq!(entry, three_stashes()[1]);
        assert!(tag.user);
        assert_eq!(state.busy, busy + 1);
        assert_eq!(state.key_focus(), KeyScope::StashFiles);
        let effects = state.update(AppIntent::StashFilesLoaded {
            tag,
            result: Ok(tracked_and_untracked()),
        });
        assert_eq!(state.busy, busy);
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
        let busy = state.busy;
        let stale = Tag {
            seq: tag.seq + 1,
            ..tag
        };
        let effects = state.update(AppIntent::StashFilesLoaded {
            tag: stale,
            result: Ok(tracked_and_untracked()),
        });
        assert_eq!(effects, vec![]);
        assert_eq!(state.busy, busy - 1);
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
        assert_eq!(state.op_seq, None);
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
        assert_eq!(state.op_seq, None);
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
            assert_eq!(state.op_seq, None, "{code:?}");
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
            assert_eq!((state.op_seq, state.popups.len()), (None, 0), "{code:?}");
            assert_eq!(state.stash.open, None, "{code:?}");
        }
        // Asked of the model directly, the guard still holds.
        state.update(AppIntent::ApplyStash);
        assert_eq!(state.op_seq, None);
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
            assert_eq!(state.op_seq, None, "{code:?}");
        }
        let mut state = on_stash_one();
        assert_eq!(state.update(AppIntent::ConfirmDropStash), vec![]);
        assert_eq!(state.op_seq, None);
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
        assert_eq!(state.op_seq, None);

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
    fn stash_failures_have_their_own_titles() {
        let kept = "The file list was reloaded. If git stopped on conflicts, resolve them in the Files panel; the stash is kept.";
        let list = "The stash list was reloaded. Fix the cause above, then try again.";
        let files = "The file list was reloaded. Fix the cause above, then try again.";
        let push = GitOp::StashPush {
            mode: StashMode::All,
            message: None,
            paths: vec![],
            restore: vec![],
            unstage: vec![],
        };
        let cases = [
            (push, "Could not stash", files),
            (
                GitOp::StashApply(stash_ref(0, "a0")),
                "Could not apply the stash",
                kept,
            ),
            (
                GitOp::StashPop(stash_ref(0, "a0")),
                "Could not pop the stash",
                kept,
            ),
            (
                GitOp::StashDrop(stash_ref(0, "a0")),
                "Could not drop the stash",
                list,
            ),
            (
                GitOp::StashRename {
                    stash: stash_ref(0, "a0"),
                    message: "m".into(),
                },
                "Could not rename the stash",
                list,
            ),
        ];
        for (op, title, next) in cases {
            assert_eq!(failure_title(&op), title, "{op:?}");
            assert_eq!(failure_next(&op), next, "{op:?}");
        }
    }

    #[test]
    fn switching_repository_forgets_the_stashes() {
        let mut state = on_stash_one();
        stash_opened(&mut state);
        state.stash_draft = Some(crate::stash::StashDraft {
            mode: StashMode::All,
            paths: vec![],
            restore: vec![],
            unstage: vec![],
            from_range: false,
        });
        state.switch_repo();
        assert_eq!(state.stash, crate::stash::StashList::default());
        assert_eq!(state.stash_draft, None);
    }

    #[test]
    fn stage_is_judged_on_the_one_file_outside_the_files_panel() {
        let (mut state, _) = with_files(vec![conflicted("c.rs"), changed("a.rs", '.', 'M')], 24);
        press_char(&mut state, 'V');
        assert_eq!(
            keymap::files_stage_disabled(&state),
            Some("Resolve the conflicts before staging these files".into())
        );
        state.update(AppIntent::FocusPanel(crate::layout::Panel::Side(
            SidePanel::Branches,
        )));
        assert!(state.files_anchor.is_some(), "the range stays for later");
        assert_eq!(
            keymap::files_stage_disabled(&state),
            Some("Resolve the conflict before staging this file".into())
        );
    }

    #[test]
    fn stashing_selected_files_is_offered_over_a_range_but_waits_for_ops_and_conflicts() {
        let (mut state, _) = with_files(three_files(), 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        assert_eq!(crate::menu::stash_file_disabled(&state), None);
        state.op_seq = Some(1);
        assert_eq!(
            crate::menu::stash_file_disabled(&state),
            Some(keymap::OP_RUNNING.into())
        );

        let files = vec![changed("a.rs", '.', 'M'), conflicted("c.rs")];
        let (mut state, _) = with_files(files, 24);
        press_char(&mut state, 'V');
        assert_eq!(
            crate::menu::stash_file_disabled(&state),
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
        assert_eq!(crate::menu::stash_file_disabled(&state), None);
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
        let reason = crate::menu::STASH_RENAME_TAKEN;
        assert_eq!(
            crate::menu::stash_file_disabled(&state),
            Some(reason.into())
        );
        // Marked too, it is refused for the same reason.
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        assert_eq!(
            crate::menu::stash_file_disabled(&state),
            Some(reason.into())
        );
        assert_eq!(stash_menu_item(&mut state, 3), vec![]);
        assert_eq!(state.notice, Some(reason.into()));
        assert_eq!(state.stash_draft, None);
        assert!(state.files_anchor.is_some());
    }

    #[test]
    fn a_rename_whose_old_directory_is_now_a_file_is_refused_with_a_reason() {
        let reason = Some(crate::menu::STASH_RENAME_TAKEN.into());
        for taken in ["d", "d/e"] {
            let files = vec![renamed("new.rs", "d/e/old.rs"), untracked(taken)];
            let (state, _) = with_files(files, 24);
            assert_eq!(crate::menu::stash_file_disabled(&state), reason, "{taken}");
        }
        // A case-insensitive filesystem would fold these onto the old path
        // or its directory, so they are refused too.
        for (old, taken) in [("Old.rs", "old.rs"), ("D/old.rs", "d")] {
            let files = vec![renamed("new.rs", old), untracked(taken)];
            let (state, _) = with_files(files, 24);
            assert_eq!(crate::menu::stash_file_disabled(&state), reason, "{taken}");
        }
        // A file beside the old path, inside its directory, is fine.
        let files = vec![renamed("new.rs", "d/e/old.rs"), untracked("d/e/other.rs")];
        let (state, _) = with_files(files, 24);
        assert_eq!(crate::menu::stash_file_disabled(&state), None);
    }

    #[test]
    fn a_staged_deletion_whose_path_is_taken_again_is_refused_with_a_reason() {
        let reason = Some(crate::menu::STASH_DELETED_TAKEN.into());
        // An untracked file at the path, or folded onto it or a directory
        // it needs by a case-insensitive filesystem.
        for taken in ["gone.rs", "Gone.rs"] {
            let files = vec![changed("gone.rs", 'D', '.'), untracked(taken)];
            let (state, _) = with_files(files, 24);
            assert_eq!(crate::menu::stash_file_disabled(&state), reason, "{taken}");
        }
        for taken in ["d", "D"] {
            let files = vec![changed("d/gone.rs", 'D', '.'), untracked(taken)];
            let (state, _) = with_files(files, 24);
            assert_eq!(crate::menu::stash_file_disabled(&state), reason, "{taken}");
        }
        // Marked as well, it is still refused, with the same reason.
        let files = vec![changed("gone.rs", 'D', '.'), untracked("gone.rs")];
        let (mut state, _) = with_files(files, 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        assert_eq!(crate::menu::stash_file_disabled(&state), reason);
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
        assert_eq!(crate::menu::stash_file_disabled(&state), None);
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
        assert_eq!(state.notice, Some(crate::menu::STASH_APART.into()));
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

    /// `files` loaded again, as a refresh would.
    fn reload(state: &mut AppState, files: Vec<FileEntry>) {
        let tag = refresh_tag(state);
        state.update(AppIntent::StatusLoaded {
            tag,
            result: Ok(repo(files)),
        });
    }

    #[test]
    fn a_reload_that_drops_a_file_inside_the_range_clears_it() {
        let files = vec![
            changed("a.rs", '.', 'M'),
            changed("b.rs", '.', 'M'),
            changed("c.rs", '.', 'M'),
        ];
        let (mut state, _) = with_files(files, 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        press_char(&mut state, 'j');
        assert_eq!(state.files_range(), [0, 1, 2]);
        reload(
            &mut state,
            vec![changed("a.rs", '.', 'M'), changed("c.rs", '.', 'M')],
        );
        assert_eq!(state.files_anchor, None);
        assert_eq!(state.notice, Some(RANGE_FILES_CHANGED.into()));
    }

    #[test]
    fn a_reload_that_drops_the_ranges_first_file_clears_it_and_says_so() {
        let files = vec![changed("a.rs", '.', 'M'), changed("b.rs", '.', 'M')];
        let (mut state, _) = with_files(files, 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        reload(&mut state, vec![changed("b.rs", '.', 'M')]);
        assert_eq!(state.files_anchor, None);
        assert_eq!(state.notice, Some(RANGE_FILES_CHANGED.into()));
    }

    #[test]
    fn a_reload_that_adds_a_file_inside_the_range_clears_it() {
        let files = vec![changed("a.rs", '.', 'M'), changed("c.rs", '.', 'M')];
        let (mut state, _) = with_files(files, 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        reload(
            &mut state,
            vec![
                changed("a.rs", '.', 'M'),
                untracked("b.rs"),
                changed("c.rs", '.', 'M'),
            ],
        );
        assert_eq!(state.files_anchor, None);
        assert_eq!(state.notice, Some(RANGE_FILES_CHANGED.into()));
    }

    #[test]
    fn a_reload_that_keeps_the_ranges_files_keeps_the_range() {
        let files = vec![
            changed("a.rs", '.', 'M'),
            changed("b.rs", '.', 'M'),
            changed("z.rs", '.', 'M'),
        ];
        let (mut state, _) = with_files(files, 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        // A file outside the range comes and goes; one inside is staged.
        reload(
            &mut state,
            vec![
                changed("a.rs", '.', 'M'),
                changed("b.rs", 'M', '.'),
                untracked("y.rs"),
            ],
        );
        assert_eq!(state.files_anchor, Some("a.rs".into()));
        assert_eq!(state.files_range(), [0, 1]);
        assert_eq!(state.notice, None);
    }

    // --- Moves, edit, break and the stopped rebase's todo list ---

    fn alt(state: &mut AppState, code: KeyCode) -> Vec<Effect> {
        state.update(AppIntent::Key(KeyPress::new(code, KeyModifiers::ALT)))
    }

    fn version(minor: u32) -> Option<GitVersion> {
        Some(GitVersion {
            major: 2,
            minor,
            patch: 0,
        })
    }

    fn picks(digits: &[char]) -> Vec<rebase_plan::Step> {
        digits
            .iter()
            .map(|&d| rebase_step(rebase_plan::TodoAction::Pick, d))
            .collect()
    }

    #[test]
    fn alt_k_and_alt_j_move_the_commit_and_the_cursor_follows_it() {
        let mut state = on_linear_log();
        let (_, op) = attached_of(&alt(&mut state, KeyCode::Char('k')));
        let GitOp::InteractiveRebase(plan) = op else {
            panic!("{op:?}")
        };
        assert_eq!(plan.base, cid(&"c".repeat(40)));
        assert_eq!(plan.steps, picks(&['a', 'b']));
        assert!(!plan.update_refs);
        assert_eq!(state.commits.selected, 0);

        let mut state = on_linear_log();
        state.git_version = version(38);
        let (_, op) = attached_of(&alt(&mut state, KeyCode::Down));
        let GitOp::InteractiveRebase(plan) = op else {
            panic!("{op:?}")
        };
        assert_eq!(plan.base, cid(&"d".repeat(40)));
        assert_eq!(plan.steps, picks(&['b', 'c', 'a']));
        assert!(plan.update_refs);
        assert_eq!(state.commits.selected, 2);
    }

    #[test]
    fn capital_e_stops_at_the_commit_and_b_after_it() {
        use rebase_plan::TodoAction as A;
        let mut state = on_linear_log();
        let (_, op) = attached_of(&press_char(&mut state, 'E'));
        let GitOp::InteractiveRebase(plan) = op else {
            panic!("{op:?}")
        };
        assert_eq!(
            plan.steps,
            [rebase_step(A::Edit, 'b'), rebase_step(A::Pick, 'a')]
        );

        let mut state = on_linear_log();
        assert_eq!(press_char(&mut state, 'b'), vec![]);
        assert_eq!(state.notice, Some(keymap::NEEDS_BREAK.into()));
        state.git_version = version(20);
        let (_, op) = attached_of(&press_char(&mut state, 'b'));
        let GitOp::InteractiveRebase(plan) = op else {
            panic!("{op:?}")
        };
        let mut stop = rebase_step(A::Pick, 'b');
        stop.break_after = true;
        assert_eq!(plan.steps, [stop, rebase_step(A::Pick, 'a')]);
    }

    /// `on_linear_log` with branch `stack` on `bbb…` and `main` checked out
    /// at `aaa…`.
    fn with_stacked_branch(minor: u32) -> AppState {
        let mut state = on_linear_log();
        state.git_version = version(minor);
        state.branch = Some(on_branch("main"));
        state.commits.tips = Some(vec![
            (cid(&"a".repeat(40)), b"refs/heads/main".to_vec()),
            (cid(&"b".repeat(40)), b"refs/heads/stack".to_vec()),
        ]);
        state
    }

    #[test]
    fn an_older_git_refuses_every_rewrite_that_would_leave_a_branch_behind() {
        let reason = "Branch stack sits on these commits; Git 2.38 or newer moves it along";
        let mut state = with_stacked_branch(37);
        for key in ['s', 'F', 'd', 'e', 'E', 'b'] {
            let mut state = state.clone();
            assert_eq!(press_char(&mut state, key), vec![], "{key}");
            assert_eq!(state.notice, Some(reason.into()), "{key}");
            assert_eq!(state.op_seq, None, "{key}");
        }
        for code in [KeyCode::Char('k'), KeyCode::Char('j')] {
            let mut state = state.clone();
            assert_eq!(alt(&mut state, code), vec![], "{code:?}");
            assert_eq!(state.notice, Some(reason.into()), "{code:?}");
        }
        // Rewriting only the head commit leaves `stack` where it is.
        state.commits.selected = 0;
        assert!(matches!(
            press_char(&mut state, 'e').as_slice(),
            [Effect::HandOff(_)]
        ));
        // Unknown tips count as a branch that may be there.
        let mut unknown = on_linear_log();
        unknown.commits.tips = None;
        assert_eq!(press_char(&mut unknown, 'd'), vec![]);
        assert_eq!(unknown.notice, Some(keymap::TIPS_UNKNOWN.into()));
        // A failed read says so, and how to try again.
        unknown.commits.tips_error = Some("fatal: bad ref\nmore".into());
        assert_eq!(press_char(&mut unknown, 'd'), vec![]);
        assert_eq!(
            unknown.notice.as_deref(),
            Some("Could not read the branches (fatal: bad ref); press r to retry")
        );
    }

    #[test]
    fn with_update_refs_only_a_move_that_changes_a_stacked_branch_is_refused() {
        let state = with_stacked_branch(38);
        for key in ['s', 'd', 'e', 'E', 'b'] {
            let mut state = state.clone();
            let (_, op) = attached_of(&press_char(&mut state, key));
            assert!(
                matches!(op, GitOp::InteractiveRebase(ref plan) if plan.update_refs),
                "{key}"
            );
        }
        // `bbb…` moving above `aaa…` would leave `stack` holding `aaa…`.
        let mut up = state.clone();
        assert_eq!(alt(&mut up, KeyCode::Char('k')), vec![]);
        assert_eq!(
            up.notice,
            Some("Moving bbbbbbb past branch stack would change what it holds".into())
        );
        let mut from_head = state.clone();
        from_head.commits.selected = 0;
        assert_eq!(alt(&mut from_head, KeyCode::Char('j')), vec![]);
        assert_eq!(
            from_head.notice,
            Some("Moving aaaaaaa past branch stack would change what it holds".into())
        );
        // Moving `bbb…` below `ccc…` is fine: `stack` holds both either way.
        let mut down = state.clone();
        assert!(matches!(
            alt(&mut down, KeyCode::Char('j')).as_slice(),
            [Effect::HandOff(_)]
        ));
    }

    /// A rebase stopped with `todo` left, read from `/r/.git/rebase-merge`,
    /// its row `selected` under the cursor.
    fn stopped(todo: &[u8], selected: usize) -> AppState {
        let mut state = on_linear_log();
        state.git_version = version(38);
        let mut branch = on_branch("main");
        branch.in_progress = vec![Operation::Rebase];
        state.branch = Some(branch);
        state.commits.set_todo(Some(RebaseTodo::load(TodoFile {
            path: PathBuf::from("/r/.git/rebase-merge/git-rebase-todo"),
            todo: todo.to_vec(),
            done: Some(b"pick 0000 x\n".to_vec()),
            comment: Ok(b"#".to_vec()),
        })));
        state.commits.todo_selected = Some(selected);
        state
    }

    const TODO: &[u8] = b"pick 1111 one\nupdate-ref refs/heads/x\npick 2222 two\npick 3333 three\n";

    /// The one todo write in `effects`: what it expects on disk and writes.
    fn todo_write(effects: &[Effect]) -> (Vec<u8>, String) {
        match effects {
            [
                Effect::WriteRebaseTodo {
                    path,
                    expected,
                    content,
                    ..
                },
            ] => {
                assert_eq!(path, Path::new("/r/.git/rebase-merge/git-rebase-todo"));
                (
                    expected.clone(),
                    String::from_utf8(content.clone()).unwrap(),
                )
            }
            other => panic!("expected one todo write, got {other:?}"),
        }
    }

    #[test]
    fn keys_on_a_todo_row_write_the_edited_list() {
        let head = "pick 1111 one\nupdate-ref refs/heads/x\n";
        let with_alt = |code| KeyPress::new(code, KeyModifiers::ALT);
        let plain = |c| KeyPress::new(KeyCode::Char(c), KeyModifiers::NONE);
        let cases: [(&str, KeyPress, String); 9] = [
            (
                "alt-k",
                with_alt(KeyCode::Char('k')),
                format!("{head}pick 3333 three\npick 2222 two\n"),
            ),
            (
                "alt-j",
                with_alt(KeyCode::Down),
                format!("{head}pick 3333 three\npick 2222 two\n"),
            ),
            (
                "E",
                plain('E'),
                format!("{head}edit 2222 two\npick 3333 three\n"),
            ),
            (
                "b",
                plain('b'),
                format!("{head}pick 2222 two\nbreak\npick 3333 three\n"),
            ),
            (
                "s",
                plain('s'),
                format!("{head}squash 2222 two\npick 3333 three\n"),
            ),
            (
                "F",
                plain('F'),
                format!("{head}fixup 2222 two\npick 3333 three\n"),
            ),
            (
                "d",
                plain('d'),
                format!("{head}drop 2222 two\npick 3333 three\n"),
            ),
            (
                "e",
                plain('e'),
                format!("{head}reword 2222 two\npick 3333 three\n"),
            ),
            ("u", plain('u'), String::new()),
        ];
        for (name, press, written) in cases {
            // `alt-j` from the row above, every other key on `two`.
            let mut state = stopped(TODO, if name == "alt-j" { 3 } else { 2 });
            let effects = state.update(AppIntent::Key(press));
            if written.is_empty() {
                assert_eq!(effects, vec![], "{name}");
                assert_eq!(state.notice, Some("Already a pick".into()), "{name}");
                continue;
            }
            let (expected, content) = todo_write(&effects);
            assert_eq!(expected, TODO, "{name}");
            assert_eq!(content, written, "{name}");
            assert!(state.commits.todo_write_seq.is_some(), "{name}");
            assert_eq!(state.op_seq, None, "{name}");
        }
        // Pick undoes another action.
        let mut state = stopped(b"drop 1111 one\n", 0);
        let (_, content) = todo_write(&press_char(&mut state, 'u'));
        assert_eq!(content, "pick 1111 one\n");
    }

    #[test]
    fn a_todo_move_follows_the_row_and_one_write_runs_at_a_time() {
        let mut state = stopped(TODO, 2);
        let tag = match alt(&mut state, KeyCode::Char('k')).as_slice() {
            [Effect::WriteRebaseTodo { tag, .. }] => *tag,
            other => panic!("{other:?}"),
        };
        assert_eq!(state.commits.todo_selected, Some(3));
        assert_eq!(alt(&mut state, KeyCode::Char('j')), vec![]);
        assert_eq!(state.notice, Some(keymap::TODO_SAVING.into()));
        // Until the written list is read back, too.
        state.update(AppIntent::RebaseTodoWritten {
            tag,
            result: Ok(()),
        });
        assert_eq!(press_char(&mut state, 'd'), vec![]);
        assert_eq!(state.notice, Some(keymap::TODO_SAVING.into()));
    }

    #[test]
    fn a_todo_move_across_a_branch_update_is_refused_with_a_notice() {
        let mut state = stopped(TODO, 2);
        assert_eq!(alt(&mut state, KeyCode::Char('j')), vec![]);
        assert_eq!(
            state.notice,
            Some("Moving 2222 past branch x would change what it holds".into())
        );
        assert_eq!(state.commits.todo_write_seq, None);
    }

    #[test]
    fn a_todo_kika_does_not_edit_writes_nothing() {
        let todo = b"pick 1111 one\nexec make\n";
        for key in ['s', 'F', 'd', 'e', 'E', 'b', 'u'] {
            let mut state = stopped(todo, 0);
            assert_eq!(press_char(&mut state, key), vec![], "{key}");
            assert_eq!(
                state.notice.as_deref(),
                Some(
                    "Read-only, fix with git rebase --edit-todo: line 2, unknown command \
                     \"exec make\""
                ),
                "{key}"
            );
        }
        let mut state = stopped(todo, 0);
        assert_eq!(alt(&mut state, KeyCode::Char('k')), vec![]);
    }

    #[test]
    fn b_on_a_todo_row_needs_git_2_20() {
        let mut state = stopped(TODO, 2);
        state.git_version = version(19);
        assert_eq!(press_char(&mut state, 'b'), vec![]);
        assert_eq!(state.notice, Some(keymap::NEEDS_BREAK.into()));
    }

    #[test]
    fn j_and_k_move_through_the_todo_rows_and_on_into_the_log() {
        let mut state = stopped(TODO, 0);
        state.commits.todo_selected = None;
        state.commits.selected = 1;
        press_char(&mut state, 'k');
        assert_eq!(state.commits.selected, 0);
        press_char(&mut state, 'k');
        assert_eq!(state.commits.todo_selected, Some(0));
        for _ in 0..5 {
            press_char(&mut state, 'k');
        }
        assert_eq!(state.commits.todo_selected, Some(3));
        for _ in 0..4 {
            press_char(&mut state, 'j');
        }
        assert_eq!(state.commits.todo_selected, None);
        assert_eq!(state.commits.selected, 0);
        // Log keys do not act on a todo row.
        state.commits.todo_selected = Some(0);
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(
            state.notice,
            Some("Select a commit in the log first".into())
        );
    }

    #[test]
    fn a_status_showing_a_rebase_loads_the_todo_and_one_without_clears_it() {
        let mut state = on_linear_log();
        let tag = refresh_tag(&mut state);
        let mut status = repo(vec![]);
        status.branch.in_progress = vec![Operation::Rebase];
        let effects = state.update(AppIntent::StatusLoaded {
            tag,
            result: Ok(status),
        });
        let load = match effects.as_slice() {
            [.., Effect::LoadRebaseTodo { tag, .. }] => *tag,
            other => panic!("expected LoadRebaseTodo, got {other:?}"),
        };
        state.update(AppIntent::RebaseTodoLoaded {
            tag: load,
            result: Ok(Some(TodoFile {
                path: PathBuf::from("/r/.git/rebase-merge/git-rebase-todo"),
                todo: TODO.to_vec(),
                done: None,
                comment: Ok(b"#".to_vec()),
            })),
        });
        assert_eq!(
            state.commits.todo.as_ref().map(|t| t.entries.len()),
            Some(4)
        );
        assert_eq!(state.commits.todo_seq, None);

        state.commits.todo_selected = Some(2);
        let tag = refresh_tag(&mut state);
        let effects = state.update(AppIntent::StatusLoaded {
            tag,
            result: Ok(repo(vec![])),
        });
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::LoadRebaseTodo { .. })),
            "{effects:?}"
        );
        assert_eq!(state.commits.todo, None);
        assert_eq!(state.commits.todo_selected, None);
    }

    #[test]
    fn the_todo_load_reads_core_comment_string_only_from_git_2_45() {
        for (git, string_too) in [(version(45), true), (version(44), false), (None, false)] {
            let mut state = on_linear_log();
            state.git_version = git;
            let tag = refresh_tag(&mut state);
            let mut status = repo(vec![]);
            status.branch.in_progress = vec![Operation::Rebase];
            let effects = state.update(AppIntent::StatusLoaded {
                tag,
                result: Ok(status),
            });
            assert!(
                matches!(
                    effects.as_slice(),
                    [.., Effect::LoadRebaseTodo { string_too: s, .. }] if *s == string_too
                ),
                "{git:?}: {effects:?}"
            );
        }
    }

    #[test]
    fn a_todo_write_carries_the_comment_prefix_the_list_was_loaded_with() {
        let mut state = stopped(TODO, 0);
        state.commits.set_todo(Some(RebaseTodo::load(TodoFile {
            path: PathBuf::from("/r/.git/rebase-merge/git-rebase-todo"),
            todo: b"; help\npick 1111 one\npick 2222 two\n".to_vec(),
            done: Some(b"pick 0000 x\n".to_vec()),
            comment: Ok(b";".to_vec()),
        })));
        state.commits.todo_selected = Some(0);
        match press_char(&mut state, 'd').as_slice() {
            [Effect::WriteRebaseTodo { comment, .. }] => assert_eq!(comment, b";"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_todo_read_with_a_letter_prefix_writes_nothing() {
        for prefix in [&b"b"[..], b"p"] {
            let mut state = stopped(TODO, 0);
            state.commits.set_todo(Some(RebaseTodo::load(TodoFile {
                path: PathBuf::from("/r/.git/rebase-merge/git-rebase-todo"),
                todo: b"pick 1111 one\npick 2222 two\n".to_vec(),
                done: Some(b"pick 0000 x\n".to_vec()),
                comment: Ok(prefix.to_vec()),
            })));
            state.commits.todo_selected = Some(0);
            assert_eq!(press_char(&mut state, 'd'), vec![], "{prefix:?}");
            assert!(
                state
                    .notice
                    .as_deref()
                    .is_some_and(|n| n.contains("hides commands")),
                "{:?}",
                state.notice
            );
        }
    }

    #[test]
    fn a_written_todo_is_reloaded_and_a_changed_one_says_so() {
        for (result, notice, error) in [
            (Ok(()), None, None),
            (Err(CHANGED_ON_DISK.to_string()), Some(TODO_CHANGED), None),
            (
                Err("disk full".to_string()),
                None,
                Some("disk full".to_string()),
            ),
        ] {
            let mut state = stopped(TODO, 2);
            let busy = state.busy;
            let tag = match press_char(&mut state, 'd').as_slice() {
                [Effect::WriteRebaseTodo { tag, .. }] => *tag,
                other => panic!("{other:?}"),
            };
            assert_eq!(state.busy, busy + 1);
            let effects = state.update(AppIntent::RebaseTodoWritten { tag, result });
            assert!(
                matches!(effects.as_slice(), [Effect::LoadRebaseTodo { .. }]),
                "{effects:?}"
            );
            assert_eq!(state.busy, busy);
            assert_eq!(state.commits.todo_write_seq, None);
            assert_eq!(state.notice.as_deref(), notice);
            assert_eq!(state.op_error, error);
        }
    }

    #[test]
    fn a_log_rewrite_still_waits_for_the_rebase_in_progress() {
        let mut state = stopped(TODO, 0);
        state.commits.todo_selected = None;
        for key in ['d', 'E', 'b'] {
            let mut state = state.clone();
            assert_eq!(press_char(&mut state, key), vec![], "{key}");
            assert_eq!(
                state.notice,
                Some("Finish or abort the operation in progress".into()),
                "{key}"
            );
        }
    }

    #[test]
    fn reorder_and_edit_keys_act_on_one_row_only() {
        let range = Some(keymap::CLEAR_RANGE_FIRST);
        // Over a range of log rows.
        let mut state = on_linear_log();
        state.commits.range_anchor = Some("a".repeat(40));
        for key in ['E', 'b', 'u'] {
            let mut state = state.clone();
            assert_eq!(press_char(&mut state, key), vec![], "{key}");
            assert_eq!(state.notice.as_deref(), range, "{key}");
        }
        assert_eq!(alt(&mut state, KeyCode::Char('k')), vec![]);
        assert_eq!(state.notice.as_deref(), range);
        // On a todo row while a range is kept.
        let mut todo = stopped(TODO, 2);
        todo.commits.range_anchor = Some("a".repeat(40));
        assert_eq!(press_char(&mut todo, 'e'), vec![]);
        assert_eq!(todo.notice.as_deref(), range);
        // Drop takes a range, but only one of log rows.
        assert_eq!(press_char(&mut todo, 'd'), vec![]);
        assert_eq!(
            todo.notice.as_deref(),
            Some("Select a commit in the log first")
        );
        // A range starts on log rows only.
        let mut todo = stopped(TODO, 2);
        press_char(&mut todo, 'V');
        assert_eq!(todo.commits.range_anchor, None);
        assert_eq!(
            todo.notice.as_deref(),
            Some("Select a commit in the log first")
        );
    }

    #[test]
    fn a_filter_stops_moves_and_any_rewrite_of_a_hidden_row() {
        let mut state = on_linear_log();
        state.git_version = version(38);
        // `commit 1` is the selected row, the only one it shows.
        state.commits.filter = "commit 1".into();
        assert_eq!(alt(&mut state, KeyCode::Char('k')), vec![]);
        assert_eq!(
            state.notice.as_deref(),
            Some(keymap::FILTER_HIDES_NEIGHBOUR)
        );
        assert!(matches!(
            press_char(&mut state.clone(), 'E').as_slice(),
            [Effect::HandOff(_)]
        ));
        state.commits.filter = "commit 2".into();
        for key in ['E', 'b'] {
            let mut state = state.clone();
            assert_eq!(press_char(&mut state, key), vec![], "{key}");
            assert_eq!(
                state.notice.as_deref(),
                Some(keymap::FILTER_HIDES_ROW),
                "{key}"
            );
        }
        // The filter hides log rows only: the todo above stays editable.
        let mut todo = stopped(TODO, 2);
        todo.commits.filter = "nothing matches".into();
        todo_write(&press_char(&mut todo, 'd'));
    }

    #[test]
    fn with_a_filter_k_enters_the_todo_from_the_top_row_it_shows() {
        let mut state = stopped(TODO, 0);
        state.commits.todo_selected = None;
        state.commits.filter = "commit 1".into();
        state.commits.selected = 1;
        press_char(&mut state, 'k');
        assert_eq!(state.commits.todo_selected, Some(0));
        press_char(&mut state, 'j');
        assert_eq!(
            (state.commits.todo_selected, state.commits.selected),
            (None, 1)
        );
    }

    #[test]
    fn with_git_version_records_what_main_found() {
        let state = AppState::new(Context::Workspace).with_git_version(version(38));
        assert!(state.git_has(crate::git::version::UPDATE_REFS));
        assert!(!AppState::new(Context::Workspace).git_has(crate::git::version::BREAK));
    }

    /// `on_linear_log` with `a` = `fixup! C` on `b` = `B` on `c` = `C`,
    /// short ids `a1`, `b1`, `c1`, `b` selected and `x.txt` staged.
    fn with_a_fixup() -> AppState {
        let mut state = on_linear_log();
        for (row, (subject, short)) in
            state
                .commits
                .rows
                .iter_mut()
                .zip([("fixup! C", "a1"), ("B", "b1"), ("C", "c1")])
        {
            row.subject = subject.into();
            row.short_oid = short.into();
        }
        state.files = vec![FileEntry {
            path: "x.txt".into(),
            orig_path: None,
            index: 'M',
            worktree: '.',
            kind: EntryKind::Changed,
        }];
        state
    }

    #[test]
    fn ctrl_f_commits_the_staged_change_as_a_fixup_of_the_selected_commit() {
        let mut state = with_a_fixup();
        let effects = state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        )));
        let (tag, op) = probe_of(&effects);
        assert_eq!(
            op,
            GitOp::Commit {
                message: None,
                stage_all: false,
                amend: false,
                no_verify: false,
                fixup: Some(cid(&"b".repeat(40))),
            }
        );
        assert_eq!(state.op_seq, Some(tag.seq));
        assert_eq!(state.commit, CommitFlow::Closed);

        // Its failure shows as any op's, not in the commit popup.
        state.update(AppIntent::OpFinished {
            tag,
            op,
            result: Err("hook said no".into()),
        });
        match state.popups.as_slice() {
            [Popup::Error(error)] => {
                assert_eq!(error.title, "Could not create the fixup");
                assert!(error.message.contains("hook said no"));
            }
            other => panic!("expected the error, got {other:?}"),
        }
        assert_eq!(state.op_seq, None);
        assert!(!state.commit_on_terminal);
    }

    #[test]
    fn a_fixup_needs_something_staged() {
        let mut state = with_a_fixup();
        state.files.clear();
        assert_eq!(state.update(AppIntent::CreateFixup), vec![]);
        assert_eq!(state.notice, Some(keymap::STAGE_THE_FIX.into()));
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn shift_s_asks_and_only_y_folds_every_fixup() {
        use rebase_plan::{RebasePlan, TodoAction as A};
        let mut state = with_a_fixup();
        let ask = |state: &mut AppState| {
            assert_eq!(press_char(state, 'S'), vec![]);
            assert_eq!(state.popups, [Popup::Confirm(ConfirmKind::Autosquash)]);
            assert_eq!(ConfirmKind::Autosquash.title(), "Autosquash");
            assert!(
                state
                    .confirm_question(&ConfirmKind::Autosquash)
                    .starts_with("Fold every fixup! commit into the commit it fixes?")
            );
        };

        ask(&mut state);
        assert_eq!(state.key_focus(), KeyScope::AutosquashConfirm);
        // Enter is too easy to press by habit: it answers nothing.
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.popups, [Popup::Confirm(ConfirmKind::Autosquash)]);
        assert_eq!(press_char(&mut state, 'q'), vec![]);
        assert_eq!(state.op_seq, None);
        assert_eq!(press_char(&mut state, 'n'), vec![]);
        assert_eq!(state.popups, []);
        assert_eq!(state.op_seq, None);

        ask(&mut state);
        let (tag, op) = attached_of(&press_char(&mut state, 'y'));
        assert_eq!(
            op,
            GitOp::InteractiveRebase(RebasePlan {
                base: cid(&"d".repeat(40)),
                steps: vec![
                    rebase_step(A::Pick, 'c'),
                    rebase_step(A::Fixup, 'a'),
                    rebase_step(A::Pick, 'b'),
                ],
                update_refs: false,
            })
        );
        assert_eq!(state.op_seq, Some(tag.seq));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn y_plans_again_from_the_log_as_it_is_now() {
        let mut state = with_a_fixup();
        press_char(&mut state, 'S');
        // A reload while asking found the fixup folded already.
        state.commits.rows[0].subject = "A".into();
        assert_eq!(press_char(&mut state, 'y'), vec![]);
        assert_eq!(state.notice, Some(rebase_plan::NO_FIXUPS.into()));
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn autosquash_says_why_not_and_asks_nothing() {
        let mut state = with_a_fixup();
        state.commits.rows[0].subject = "A".into();
        assert_eq!(state.update(AppIntent::Autosquash), vec![]);
        assert_eq!(state.popups, []);
        assert_eq!(state.notice, Some(rebase_plan::NO_FIXUPS.into()));
    }

    /// Ask for the fixup base, as `ctrl-b` does, and return its tag.
    fn find_base(state: &mut AppState) -> Tag {
        let effects = state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        )));
        match effects.as_slice() {
            [Effect::FindFixupBase { tag }] => *tag,
            other => panic!("expected FindFixupBase, got {other:?}"),
        }
    }

    fn base_found(tag: Tag, base: crate::git::fixup_base::FixupBase) -> AppIntent {
        AppIntent::FixupBaseFound {
            tag,
            result: Ok(base),
        }
    }

    #[test]
    fn one_fixup_base_moves_the_cursor_to_it() {
        use crate::git::fixup_base::FixupBase;
        let mut state = with_a_fixup();
        state.commits.range_anchor = Some("b".repeat(40));
        // From the Files panel a range in the log does not stop it.
        state.focus = Focus::Files;
        let before = state.busy;
        let tag = state.update(AppIntent::FindFixupBase);
        let tag = match tag.as_slice() {
            [Effect::FindFixupBase { tag }] => *tag,
            other => panic!("{other:?}"),
        };
        assert_eq!(state.busy, before + 1);
        state.update(base_found(tag, FixupBase::One(cid(&"c".repeat(40)))));
        assert_eq!(state.commits.selected, 2);
        assert_eq!(state.commits.range_anchor, None);
        assert_eq!(state.notice, Some("Fixup base: c1 C".into()));
        assert_eq!(state.busy, before);
        state.focus = Focus::Commits;

        let tag = find_base(&mut state);
        state.update(base_found(tag, FixupBase::One(cid(&"e".repeat(40)))));
        assert_eq!(state.commits.selected, 2);
        assert_eq!(
            state.notice,
            Some(
                "Fixup base: eeeeeee (not loaded; move down to load more, then look again)".into()
            )
        );
    }

    #[test]
    fn several_fixup_bases_are_listed_newest_first_and_the_cursor_stays() {
        use crate::git::fixup_base::FixupBase;
        let mut state = with_a_fixup();
        let tag = find_base(&mut state);
        let ids = ["e", "c", "a"].map(|d| cid(&d.repeat(40)));
        state.update(base_found(tag, FixupBase::Many(ids.to_vec())));
        assert_eq!(state.commits.selected, 1);
        assert_eq!(
            state.notice,
            Some("Staged lines come from 3 commits: a1, c1, eeeeeee".into())
        );

        let tag = find_base(&mut state);
        state.update(base_found(tag, FixupBase::None));
        assert_eq!(
            state.notice,
            Some("No commit touched the staged lines".into())
        );

        let tag = find_base(&mut state);
        state.update(AppIntent::FixupBaseFound {
            tag,
            result: Err("fatal: no".into()),
        });
        assert!(matches!(
            state.popups.as_slice(),
            [Popup::Error(error)] if error.title == "Could not find the fixup base"
        ));
    }

    #[test]
    fn a_stale_fixup_base_changes_nothing() {
        use crate::git::fixup_base::FixupBase;
        let mut state = with_a_fixup();
        let old = find_base(&mut state);
        let new = find_base(&mut state);
        let busy = state.busy;
        state.notice = None;
        state.update(base_found(old, FixupBase::One(cid(&"c".repeat(40)))));
        assert_eq!(state.commits.selected, 1);
        assert_eq!(state.notice, None);
        assert_eq!(state.busy, busy - 1, "the newer look is still on its way");
        assert_eq!(state.commits.fixup_base_seq, Some(new.seq));

        let other = Tag {
            generation: new.generation + 1,
            ..new
        };
        state.update(base_found(other, FixupBase::One(cid(&"c".repeat(40)))));
        assert_eq!(state.commits.selected, 1);
        assert_eq!(state.notice, None);
    }

    /// The custom patch: picking from a commit, and the menu of rewrites.
    mod custom_patch_tests {
        use super::*;
        use crate::custom_patch::{FilePick, PatchAction, PatchChoice};
        use crate::git::commit_files::{CommitFile, CommitId};
        use crate::git::log::{CommitRow, LogPage};
        use crate::git::patch::LineId;
        use crate::handoff::HandOff;
        use crate::keymap::{Resolution, registry, resolve};

        /// Why `c` cannot run in `scope`, or `None` when it runs.
        pub(crate) fn key_reason(state: &AppState, scope: KeyScope, c: char) -> Option<Reason> {
            let key = KeyPress::new(KeyCode::Char(c), KeyModifiers::NONE);
            match resolve(registry(), &Keymap::default(), scope, state, key) {
                Resolution::Disabled(reason) => Some(reason),
                Resolution::Run(_) => None,
                other => panic!("{c} in {scope:?}: {other:?}"),
            }
        }

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
            state
                .commits
                .page_loaded(900, 0, Ok((LogPage { rows, done: true }, 1_000)));
            state.commits.tips = Some(vec![]);
            state.focus = Focus::Commits;
            state.expanded = SidePanel::Commits;
            state.commits.selected = selected;
            state
        }

        /// Open row `row`'s files, listing `a.rs` and `b.rs`.
        fn open_files(state: &mut AppState, row: usize) {
            state.commits.selected = row;
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
            state.update(AppIntent::CommitFilesLoaded {
                tag,
                result: Ok(vec![file("a.rs"), file("b.rs")]),
            });
        }

        /// Row 2's `a.rs` in the Diff pane, its patch `TWO_HUNKS`.
        fn picking() -> AppState {
            let mut state = history(0);
            open_files(&mut state, 2);
            let (tag, request) = patch_load(&press(&mut state, KeyCode::Char('l')));
            assert_eq!(request.commit.as_ref().map(|r| r.oid.clone()), Some(id(2)));
            state.update(AppIntent::PatchLoaded {
                tag,
                result: Ok(patch_of(TWO_HUNKS)),
            });
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
            assert_eq!(state.op_seq, None);

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
            assert_eq!(state.op_seq, None);
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
                AppIntent::ToggleDiffSide,
                AppIntent::Discard,
                AppIntent::EditHunk,
            ] {
                state.notice = None;
                assert_eq!(state.update(intent.clone()), vec![], "{intent:?}");
                assert!(state.popups.is_empty(), "{intent:?}");
                assert_eq!(state.notice.as_deref(), Some(keymap::NOT_IN_COMMIT));
            }
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
                Some(
                    format!("The custom patch holds changes from {short}; reset it first").as_str()
                )
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
            state.commits.selected = row;
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
                state.update(AppIntent::ChoosePatchAction(choice));
                assert_eq!(
                    state.popups,
                    [Popup::Confirm(ConfirmKind::CustomPatch(action.clone()))]
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
                    .filter(|s| s.action == crate::git::rebase_plan::TodoAction::Edit)
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
                idle.update(AppIntent::ChoosePatchAction(choice));
                assert_eq!(idle.popups.len(), 1, "{choice:?} asks without a bisect");

                let mut state = with_patch(0);
                start_bisecting(&mut state);
                let effects = state.update(AppIntent::ChoosePatchAction(choice));
                assert_eq!(effects, vec![], "{choice:?}");
                assert_eq!(state.notice, Some(bisect::RESET_FIRST.into()), "{choice:?}");
                assert_eq!(state.popups, [], "{choice:?}");
                assert_eq!(state.op_seq, None, "{choice:?}");
            }
        }

        #[test]
        fn no_closes_the_question_and_keeps_the_patch() {
            let mut state = with_patch(0);
            state.update(AppIntent::ChoosePatchAction(PatchChoice::Remove));
            assert_eq!(press(&mut state, KeyCode::Char('n')), vec![]);
            assert!(state.popups.is_empty());
            assert!(state.custom_patch.is_some());
        }

        #[test]
        fn a_new_commit_asks_for_its_message_first_and_refuses_an_empty_one() {
            let mut state = with_patch(0);
            state.update(AppIntent::ChoosePatchAction(PatchChoice::NewCommit));
            assert!(matches!(
                state.popups.as_slice(),
                [Popup::Prompt {
                    kind: PromptKind::PatchCommitMessage,
                    ..
                }]
            ));
            press(&mut state, KeyCode::Char(' '));
            assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
            assert_eq!(
                state.notice.as_deref(),
                Some(super::super::custom_patch::MESSAGE_NEEDED)
            );
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
                [Popup::Confirm(ConfirmKind::CustomPatch(
                    PatchAction::NewCommit("Split".into())
                ))]
            );
            let Popup::Confirm(kind) = state.popups[0].clone() else {
                panic!("{:?}", state.popups);
            };
            let question = state.confirm_question(&kind);
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
            state.update(AppIntent::ChoosePatchAction(PatchChoice::Remove));
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
            let effects = state.update(AppIntent::ChoosePatchAction(PatchChoice::Remove));
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
            use crate::git::repo_state::Operation;
            let why = |state: &AppState, choice| {
                keymap::patch_rewrite_disabled(state, choice).map(|r| r.into_owned())
            };
            let mut state = with_patch(2);
            let picks = state.custom_patch.take();
            state.op_seq = Some(1);
            state.branch.as_mut().unwrap().in_progress = vec![Operation::Rebase];
            state.files.push(changed("t.rs", '.', 'M'));
            state.commits.filter = "x".into();
            state.commits.rows[2].parents = vec![oid(3), oid(9)];

            let mut reasons = vec![];
            reasons.push(why(&state, PatchChoice::MoveToSelected));
            state.custom_patch = picks;
            reasons.push(why(&state, PatchChoice::MoveToSelected));
            state.op_seq = None;
            reasons.push(why(&state, PatchChoice::MoveToSelected));
            state.branch.as_mut().unwrap().in_progress.clear();
            reasons.push(why(&state, PatchChoice::MoveToSelected));
            state.files.retain(|f| f.kind == EntryKind::Untracked);
            reasons.push(why(&state, PatchChoice::MoveToSelected));
            state.commits.filter.clear();
            reasons.push(why(&state, PatchChoice::MoveToSelected));
            state.commits.rows[2].parents.clear();
            reasons.push(why(&state, PatchChoice::MoveToSelected));
            state.commits.rows[2].parents = vec![oid(3)];
            reasons.push(why(&state, PatchChoice::MoveToSelected));
            reasons.push(why(&state, PatchChoice::Remove));

            let want: Vec<Option<String>> = [
                Some(keymap::PATCH_EMPTY),
                Some(keymap::OP_RUNNING),
                Some("Finish or abort the operation in progress"),
                Some("Commit or stash your changes first"),
                Some(keymap::CLEAR_LOG_FILTER),
                Some("Cannot rewrite a merge commit"),
                Some("Cannot rewrite the first commit"),
                Some(keymap::TARGET_IS_SOURCE),
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
}
