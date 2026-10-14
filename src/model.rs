use crate::bisect;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::clipboard::{self, CopyWhat};
use crate::config::ConfigError;
use crate::confirm_question::{Answer, ConfirmQuestion, Then, quit_question};
use crate::diff_mark::DiffMark;
use crate::diff_pane::{DiffPane, DiffStart, focused_hunks, focused_hunks_mut};
use crate::effect::{Effect, Tag};
use crate::features::blame::{self, BlameView};
use crate::features::branches::remotes::RemotePending;
use crate::features::branches::tags::PendingTag;
use crate::features::branches::{self, BranchesPanel, BranchesTab};
use crate::features::command_log::{self, CommandLog};
use crate::features::commit::{self, CommitEditor};
use crate::features::commits::bisect::BisectIntent;
use crate::features::commits::compare::PendingDiff;
use crate::features::commits::copy::CopiedCommits;
use crate::features::commits::custom_patch::CustomPatch;
use crate::features::commits::rewrite::LeftBehindCheck;
use crate::features::commits::{self, CommitsIntent, CommitsPanel, CommitsTab};
use crate::features::conflicts::{self, ConflictView};
use crate::features::custom_commands::{self, PendingCommand};
use crate::features::diff::renderers::external_diff::map_rows;
use crate::features::diff::renderers::{ExternalRenderer, RenderedDiff, RendererKind};
use crate::features::diff::{self, DiffIntent};
use crate::features::files::{self, IgnoreDraft};
use crate::features::operation;
use crate::features::operation::StopWorkCheck;
use crate::features::pull_requests::{self, PrCreating, PrDraft};
use crate::features::repos::{self, ParkedRepo};
use crate::features::review::{self, PrMerging};
use crate::features::stash::{self, StashDraft, StashList};
use crate::features::submodules;
use crate::features::sync;
use crate::features::worktrees;
use crate::filtered_list::FilteredList;
use crate::handoff::HandOff;
use crate::hunk_view::{HunkView, NO_CHANGES};
use crate::intent::{AppIntent, Scroll};
use crate::keymap::{self, KeyScope, Keymap, Reason, Resolution};
use crate::layout::{self, Panel, ScreenMode, SidePanel};
use crate::list_filter::{self, InputTarget, ListInput};
use crate::list_input::{RANGE_FILES_CHANGED, search_input};
use crate::menu;
use crate::op::GitOp;
use crate::op_slot::OpSlot;
use crate::popup::{DiscardWhat, ErrorPopup, LineInput, Popup, PromptKind};
use crate::prompts::runs_attached;
use crate::refresh::Selection;
use crate::remote_run::RemoteRun;
use crate::watch::{WatchEvent, WatchState};
use kika_forge::{ForgeHost, ForgeState};
use kika_git::FileEntry;
use kika_git::askpass::Secret;
use kika_git::bisect::BisectMark;
use kika_git::branches::{Branch, BranchName};
use kika_git::commit_files::CommitId;
use kika_git::diff::{
    DiffOptions, DiffRequest, DiffSide, EmojiZwj, MAX_CONTEXT, MIN_CONTEXT, sanitize,
};
use kika_git::patch::{self, Direction, PatchSelection};
use kika_git::path::RepoPath;
use kika_git::refs::RefsSnapshot;
use kika_git::repo_state::Operation;
use kika_git::status::{BranchStatus, EntryKind, RepoStatus};
use kika_git::version::GitVersion;
// Names the tests below reach through `super::*` since the op slot moved
// to `op_slot.rs`.
#[cfg(test)]
use crate::op::MergeStrategy;
#[cfg(test)]
use crate::refresh;
#[cfg(test)]
use kika_git::remote::RemoteOp;

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
    /// The Diff pane, working on the selected file's hunks. The hunk
    /// cursor is the pane's own (`DiffPane::hunks`).
    Diff,
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
            Focus::Diff | Focus::Blame(_) | Focus::Conflict(_) => Panel::Diff,
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
    /// The op slot: the running git op, the busy count and why the last op
    /// was refused. Only `op_slot.rs` sees inside it.
    pub(crate) op_slot: OpSlot,
    /// The Files panel: the working tree's files, with the cursor (the
    /// file whose diff is shown), the filter, the range (anchored by path,
    /// so a refresh that moves the rows keeps it on the same files) and
    /// the status load in flight. `loaded` tells "loading" from "clean";
    /// a failed status keeps the files shown.
    pub files: FilteredList<FileEntry>,
    /// Branch, upstream and operations in progress. `None` until the first
    /// status load succeeds; a failed load keeps the last one.
    pub branch: Option<BranchStatus>,
    pub diff: DiffPane,
    pub focus: Focus,
    /// The side panel last focused. It stays expanded while the diff has
    /// focus, and is where focus goes back to.
    pub expanded: SidePanel,
    /// How much of the screen the focused panel takes; kept across focus
    /// changes.
    pub screen_mode: ScreenMode,
    /// The left-behind check in flight; one at a time, so a second press
    /// cannot stack a second question.
    pub left_behind_check: Option<LeftBehindCheck>,
    /// The check for work since the stop that a Skip waits on; one at a
    /// time, so a second press cannot stack a second question.
    pub skip_check: Option<StopWorkCheck>,
    /// The running commit has the terminal, so Ctrl+C goes to git, not Kika.
    pub commit_on_terminal: bool,
    /// From the global `[commit] hooks_detached`: the user's hooks never ask
    /// anything, so a commit that only runs hooks stays detached.
    pub commit_hooks_detached: bool,
    /// How the selection is kept when the pending status load lands.
    pub status_selection: Selection,
    pub help_open: bool,
    /// The first help row shown, for a list taller than the screen.
    pub help_scroll: usize,
    pub commit: CommitFlow,
    /// One line on the bottom bar until the next key: why the last key did
    /// nothing, or what just happened ("Copied ...", "Bisect found ...").
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
    /// The fetch, pull or push running now. It also holds the op slot.
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
    /// The filter or search being typed on the bottom bar, if one is.
    pub input: Option<ListInput>,
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
            op_slot: OpSlot::default(),
            files: FilteredList::default(),
            branch: None,
            diff: DiffPane::default(),
            focus: Focus::Files,
            expanded: SidePanel::Files,
            screen_mode: ScreenMode::Normal,
            left_behind_check: None,
            skip_check: None,
            commit_on_terminal: false,
            commit_hooks_detached: false,
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
            input: None,
            diff_mark: None,
            pending_diff: None,
            custom_patch: None,
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
                return self.quit_interrupts();
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
            AppIntent::RefreshStatus => return self.reload_repo(),
            AppIntent::StatusLoaded { tag, result } => return self.status_loaded(tag, result),
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
                } else if let Some(view) = self.hunk_view_mut() {
                    view.scroll(by, rows);
                } else if let Focus::Conflict(view) = &mut self.focus {
                    view.scroll(by, rows);
                } else {
                    return self.diff.scroll(by, rows);
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
            AppIntent::Discard => self.ask_discard(),
            AppIntent::OpFinished { tag, op, result } => return self.op_finished(tag, op, result),
            AppIntent::OpenMenu(id) => {
                if self.context == Context::Workspace {
                    self.popups.push(Popup::Menu { id, cursor: 0 });
                }
            }
            AppIntent::MenuNext => self.move_menu_cursor(1),
            AppIntent::MenuPrev => self.move_menu_cursor(-1),
            AppIntent::MenuSelect => return self.menu_select(),
            AppIntent::AskQuit => self.popups.push(Popup::Question(quit_question())),
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
            AppIntent::PopPopup => match self.popups.pop() {
                // Closing the welcome shows what git says about the launch
                // directory, as Kika did before there was a welcome.
                Some(Popup::Recent(_)) if self.repo.is_none() => return self.refresh_status(),
                Some(closed) => self.popup_closed(&closed),
                None => {}
            },
            AppIntent::OpenInEditor => return self.open_in_editor(),
            AppIntent::Watch(event) => return self.watch(event),
            AppIntent::RefsLoaded { tag, result } => return self.refs_loaded(tag, result),
            AppIntent::Stash(i) => return stash::update(self, i),
            AppIntent::Submodules(i) => return submodules::update(self, i),
            AppIntent::Worktrees(i) => return worktrees::update(self, i),
            AppIntent::CommandLog(i) => return command_log::update(self, i),
            AppIntent::Commits(i) => return commits::update(self, i),
            AppIntent::Branches(i) => return branches::update(self, i),
            AppIntent::Commit(i) => return commit::update(self, i),
            AppIntent::Blame(i) => return blame::update(self, i),
            AppIntent::Conflicts(i) => return conflicts::update(self, i),
            AppIntent::CustomCommands(i) => return custom_commands::update(self, i),
            AppIntent::Sync(i) => return sync::update(self, i),
            AppIntent::Operation(i) => return operation::update(self, i),
            AppIntent::Repos(i) => return repos::update(self, i),
            AppIntent::Files(i) => return files::update(self, i),
            AppIntent::Diff(i) => {
                let landed = matches!(i, DiffIntent::Loaded { .. });
                let mut effects = diff::update(self, i);
                // A review thread `]`/`[` waits for may have streamed in.
                if landed {
                    effects.extend(self.review_jump_landed());
                }
                return effects;
            }
            AppIntent::ConfigLoaded {
                repo,
                keymap,
                errors,
            } => self.config_loaded(repo, keymap, errors),
            AppIntent::Start => return self.start(),
            AppIntent::OpenRepo(path) => return self.open_repo(path),
            AppIntent::PullRequests(i) => return pull_requests::update(self, i),
            AppIntent::Review(i) => return review::update(self, i),
            AppIntent::ConfirmDeleteRange => return self.confirm_delete_range(),
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
            AppIntent::ContinueOperation => {
                return self.resume(bisect::continue_disabled, GitOp::Continue);
            }
            AppIntent::SkipOperation if bisect::bisect_only(self) => {
                return self.update(AppIntent::Commits(CommitsIntent::Bisect(
                    BisectIntent::MarkHead(BisectMark::Skip),
                )));
            }
            AppIntent::SkipOperation => return self.skip(),
            AppIntent::AbortOperation if bisect::bisect_only(self) => self.ask_bisect_reset(),
            AppIntent::AbortOperation => self.ask_abort(),
            AppIntent::CopyFailed(why) => {
                self.notice = None;
                self.push_error(ErrorPopup::new(
                    "Could not copy",
                    &why,
                    "Close this; nothing was copied.",
                ));
            }
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
            AppIntent::DiffEscape => match self.hunk_view_mut() {
                Some(view) if view.search.is_some() => view.search = None,
                _ => return self.update(AppIntent::FocusLeft),
            },
            AppIntent::PaneEscape => return self.pane_escape(),
            AppIntent::SearchNext => return self.step_search(false),
            AppIntent::SearchPrev => return self.step_search(true),
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
            (Context::Workspace, Focus::Diff)
                if self.hunk_view().is_some_and(|view| view.lines.is_some()) =>
            {
                KeyScope::Lines
            }
            (Context::Workspace, Focus::Diff) => KeyScope::Diff,
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
            Some(Popup::Prompt { .. }) => return KeyScope::Prompt,
            Some(Popup::Error(_)) => return KeyScope::Error,
            Some(Popup::Question(q)) => return q.scope,
            Some(Popup::Submodules(_)) => return KeyScope::Submodules,
            Some(Popup::Worktrees(_)) => return KeyScope::Worktrees,
            Some(Popup::Recent(_)) => return KeyScope::Recent,
            Some(Popup::PickList(_)) => return KeyScope::PickList,
            // Ssh's yes/no kind is answered like any other question.
            Some(Popup::Credential(prompt)) if prompt.confirm => return KeyScope::Confirm,
            Some(Popup::Credential(_)) => return KeyScope::Credential,
            Some(Popup::CommandLog(_)) => return KeyScope::CommandLog,
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

    /// Whether the active renderer is a tool, whose rows show no patch
    /// lines: the Diff pane can be read but not staged from.
    pub fn display_only(&self) -> bool {
        self.active_renderer()
            .is_some_and(|r| r.kind == RendererKind::Tool)
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

    /// Map the rendered rows to the focused view's patch, when both are
    /// of the same file and side, then bring the selection into view.
    /// A tool's rows are never mapped: they show the files, not the patch.
    pub(crate) fn remap(&mut self) {
        let display_only = self.display_only();
        let Some(rendered) = &mut self.rendered else {
            return;
        };
        if display_only {
            rendered.map.clear();
            return;
        }
        rendered.map = match focused_hunks(&self.focus, &self.diff.hunks) {
            Some(view) if view.path == rendered.path && view.side == rendered.side => view
                .patch
                .as_ref()
                .map(|patch| map_rows(patch, &rendered.rows))
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        self.follow_cursor();
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

    /// A popup was closed without an answer: each feature forgets the
    /// draft it kept for that popup. Each one matches only its own
    /// popups, so core names none of them.
    fn popup_closed(&mut self, closed: &Popup) {
        self.custom_commands_popup_closed(closed);
        self.branches_popup_closed(closed);
        self.remotes_popup_closed(closed);
        self.tags_popup_closed(closed);
        self.stash_popup_closed(closed);
        self.worktrees_popup_closed(closed);
        self.files_popup_closed(closed);
        self.review_popup_closed(closed);
        self.pull_requests_popup_closed(closed);
    }

    /// Close the question on top and run its answer, if it has one.
    fn answer(&mut self, yes: bool) -> Vec<Effect> {
        if let Some(Popup::Question(q)) = self.popups.last() {
            return self.answer_confirm_question(q.scope, yes);
        }
        match self.popups.pop() {
            Some(Popup::Credential(prompt)) if prompt.confirm => {
                // Ssh reads an exit of 0 with "yes" as allowed.
                vec![Effect::AnswerCredential(yes.then(|| Secret::new("yes")))]
            }
            // Not a question: leave it where it was.
            other => {
                self.popups.extend(other);
                vec![]
            }
        }
    }

    /// Answer the question on top, asked in `scope`. No closes any
    /// question, and its feature forgets the draft it kept for it. Yes
    /// answers a discard, the `y`-or-Enter questions (quit, force push,
    /// stage resolved and the detached checkout) and the `y`-only
    /// questions whose binding sends `ConfirmYes` (abort, bisect reset,
    /// autosquash, custom patch and drop range). Every other question
    /// answers only to its own intent (such as
    /// `BranchesIntent::ConfirmDelete` or `PullRequestsIntent::ConfirmCreate`), so
    /// yes here keeps it open.
    fn answer_confirm_question(&mut self, scope: KeyScope, yes: bool) -> Vec<Effect> {
        if !yes {
            if let Some(closed) = self.popups.pop() {
                self.popup_closed(&closed);
            }
            return vec![];
        }
        match scope {
            KeyScope::DiscardConfirm => {
                let Some(q) = self.take_question(scope, Answer::YOnly) else {
                    return vec![];
                };
                // The range was what was asked about; it is done with.
                if q.range {
                    self.files.range_anchor = None;
                }
                match q.then {
                    Then::Op(op) => self.start_op(Ok(Some(op))),
                    _ => vec![],
                }
            }
            KeyScope::Confirm => self.answer_question(scope, Answer::YOrEnter),
            KeyScope::AbortConfirm
            | KeyScope::SkipConfirm
            | KeyScope::AutosquashConfirm
            | KeyScope::PatchConfirm
            | KeyScope::DropRangeConfirm => self.answer_question(scope, Answer::YOnly),
            _ => vec![],
        }
    }

    /// Ask before throwing away the selected file, hunk or lines. Nothing
    /// is discarded here; a refusal says why in the bar instead of asking.
    fn ask_discard(&mut self) {
        if self.context != Context::Workspace || self.op_is_running() {
            return;
        }
        if self.hunk_view().is_some_and(|view| view.commit.is_some()) {
            self.notice = Some(keymap::NOT_IN_COMMIT.into());
            return;
        }
        if self.focus == Focus::Files && self.files.range_anchor.is_some() {
            return self.ask_discard_range();
        }
        match self.discard_question() {
            Ok(Some(question)) => self.popups.push(Popup::Question(question)),
            Ok(None) => {}
            Err(reason) => self.notice = Some(reason.into()),
        }
    }

    /// What `d` would discard from the current focus, fixed now.
    fn discard_question(&self) -> Result<Option<ConfirmQuestion>, &'static str> {
        let Some(entry) = self.files.selected_row() else {
            return Ok(None);
        };
        // Deleting a file is said as such, whatever was selected in it.
        let confirm = |what, path: &RepoPath, op: GitOp| {
            let what = if matches!(op, GitOp::DeleteUntracked(_)) {
                DiscardWhat::Untracked
            } else {
                what
            };
            what.into_question(path.display().to_string(), op)
        };
        let view = match (&self.focus, self.hunk_view()) {
            (Focus::Files, _) => {
                let op = GitOp::discard_for(entry)?;
                return Ok(op.map(|op| confirm(DiscardWhat::File, &entry.path, op)));
            }
            (_, Some(view)) => view,
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
        if let (PromptKind::NewBranchAtHead, Some(id)) = (kind, &self.branches.pending_head) {
            return format!("New branch at {}", self.commits.short_oid(id));
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

    /// Select file `index` and start loading its diff. With the Diff pane
    /// focused, its patch reloads too, on the same side while the file is
    /// the same one.
    ///
    /// While a commit's or a stash's files are open the pane is theirs:
    /// the selection moves, but nothing loads.
    pub(crate) fn select(&mut self, index: usize) -> Vec<Effect> {
        self.select_from(index, DiffStart::Keep)
    }

    /// `select`, with the diff starting from `start`.
    pub(crate) fn select_from(&mut self, index: usize, start: DiffStart) -> Vec<Effect> {
        self.files.selected = index;
        // The pane shows another diff; leaving it streams this one.
        if self.files_open() || self.review_files_shown() {
            self.diff.rewind(start);
            return vec![];
        }
        let request = DiffRequest::for_entry(&self.files.rows[index], self.diff_options);
        let mut effects = self.open_diff(request.clone(), start);
        if let Some(view) = self.hunk_view_mut() {
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
    pub(crate) fn set_diff_options(&mut self, options: DiffOptions) -> Vec<Effect> {
        let options = DiffOptions {
            context: options.context.clamp(MIN_CONTEXT, MAX_CONTEXT),
            ..options
        };
        if options == self.diff_options {
            return vec![];
        }
        self.diff_options = options;
        if self.commits.open.is_some() {
            return self.commit_file_diff(DiffStart::Keep);
        }
        if self.stash.open.is_some() {
            return self.stash_file_diff(DiffStart::Keep);
        }
        if self.files.selected_row().is_some() {
            self.select(self.files.selected)
        } else {
            vec![]
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
                    self.diff.set_search("");
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
        if self.files.selected_row().is_some() {
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
        let Some(entry) = self.files.selected_row() else {
            return vec![];
        };
        if entry.kind == EntryKind::Conflicted {
            return self.open_conflict();
        }
        let request = DiffRequest::for_entry(entry, self.diff_options);
        self.focus_hunk_view(HunkView::new(request.path, request.side));
        let mut effects = self.load_patch();
        self.remap();
        effects.extend(self.render_shown());
        effects
    }

    /// Start loading the focused view's patch. A user effect, so the busy
    /// indicator shows and a refused key can say "Loading…".
    pub(crate) fn load_patch(&mut self) -> Vec<Effect> {
        let seq = self.take_seq();
        let Some(view) = focused_hunks_mut(&self.focus, &mut self.diff.hunks) else {
            return vec![];
        };
        view.seq = Some(seq);
        // Renames are off in patch diffs, so the original path is not needed.
        let request = DiffRequest {
            path: view.path.clone(),
            orig_path: None,
            side: view.side,
            options: self.diff_options,
            commit: view.commit.clone(),
        };
        let tag = self.user_tag(seq);
        vec![Effect::LoadPatch { tag, request }]
    }

    pub(crate) fn toggle_hunk_stage(&mut self) -> Vec<Effect> {
        if let Some(view) = self.hunk_view()
            && view.commit.is_some()
        {
            let lines = view.hunk_lines();
            self.patch_toggle_lines(lines);
            return vec![];
        }
        match self.hunk_view() {
            Some(view) => self.toggle_selection_stage(PatchSelection::Hunk(view.hunk)),
            None => vec![],
        }
    }

    pub(crate) fn toggle_lines_stage(&mut self) -> Vec<Effect> {
        if let Some(view) = self.hunk_view()
            && view.commit.is_some()
        {
            let lines = view.selected_lines();
            self.patch_toggle_lines(lines);
            return vec![];
        }
        match self.hunk_view() {
            Some(view) => self.toggle_selection_stage(PatchSelection::Lines(view.selected_lines())),
            None => vec![],
        }
    }

    /// Show the diff of the file under the cursor after a refresh. The
    /// list has already kept the cursor on the same file when it still
    /// exists, else near the same place, and moved it off a hidden row.
    fn reselect(&mut self) -> Vec<Effect> {
        if self.files.rows.is_empty() {
            if self.files_open() || self.review_files_shown() {
                return vec![];
            }
            self.rendered = None;
            // Nothing is left for the diff or a blame to show.
            if !self.focus.is_side() {
                self.focus = Focus::side(self.expanded);
            }
            return self.close_diff();
        }
        self.select(self.files.selected)
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
        if self.commits.reflog.list.loaded {
            effects.extend(self.load_reflog(false));
        }
        effects
    }

    /// The Branches panel's selected branch, when its Local tab shows.
    pub(crate) fn local_branch_selected(&self) -> Option<&Branch> {
        (self.branches.tab == BranchesTab::Local)
            .then(|| self.branches.selected_branch())
            .flatten()
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
                effects.extend(self.reload_repo());
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
        self.files.rows.clear();
        self.files.selected = 0;
        self.branch = None;
        self.diff.clear();
        self.rendered = None;
        self.files.error = None;
        self.clear_op_error();
        self.status_selection = Selection::KeepPath;
        self.branches = BranchesPanel::default();
        self.commits = CommitsPanel::default();
        self.stash = StashList::default();
        self.stash_draft = None;
        // The ids name commits of the repository being left.
        self.copied.clear();
        // A filter and a range were made over the old repository's rows.
        self.input = None;
        self.files.filter.clear();
        self.files.range_anchor = None;
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
        self.files.begin(seq);
        vec![Effect::LoadStatus {
            tag: self.user_tag(seq),
        }]
    }

    fn status_loaded(&mut self, tag: Tag, result: Result<RepoStatus, String>) -> Vec<Effect> {
        // Every user effect reports back exactly once, stale or not, so
        // decrement before the staleness check or the counter would leak.
        if tag.user {
            self.user_done(tag);
        } else {
            self.watch.status_pending = false;
        }
        // Nobody asked for a background load, so it neither interrupts with
        // an error nor reloads a diff that has not changed. Either way it
        // changes nothing, so it may skip the staleness check below.
        if !tag.user {
            match &result {
                Err(_) => return vec![],
                Ok(status) if self.status_unchanged(status) => return vec![],
                Ok(_) => {}
            }
        }
        let old_range = self.files_range_paths();
        let had_range = !old_range.is_empty();
        match result {
            Ok(status) => {
                if !self.files.loaded(tag, self.generation, Ok(status.files)) {
                    return vec![];
                }
                let branch = status.branch;
                let rebasing = branch.in_progress.contains(&Operation::Rebase);
                let before = self.branch.replace(branch);
                self.announce_bisect(before.and_then(|b| b.bisect));
                let mut effects = match self.status_selection {
                    Selection::KeepPath => self.reselect(),
                };
                // A range whose files changed is not the set the user
                // marked, so nothing may act on it.
                // That includes a range whose first file went, cleared
                // by the reload.
                if had_range && self.files_range_paths() != old_range {
                    self.files.range_anchor = None;
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
                // The list keeps the files shown and records the error.
                let before = self.files.selected;
                if !self
                    .files
                    .loaded(tag, self.generation, Err(message.clone()))
                {
                    return vec![];
                }
                self.push_error(ErrorPopup::new(
                    "Could not read the repository status",
                    &message,
                    "Close this, fix the cause, then press r to retry.",
                ));
                // A cursor the filter hid (go to file can land there) moved
                // to the first file shown; the diff follows it.
                if self.files.selected != before {
                    return self.select(self.files.selected);
                }
                vec![]
            }
        }
    }
}

/// A commit opened to its files from the Commits panel. The Diff pane
/// shows the selected file's change in that commit until it closes.
impl AppState {
    /// Stream the open commit's or stash's file again, as when it was
    /// picked; nothing when neither is open.
    pub(super) fn reload_open_file_diff(&mut self) -> Vec<Effect> {
        if self.commits.open.is_some() {
            self.commit_file_diff(DiffStart::Top)
        } else if self.stash.open.is_some() {
            self.stash_file_diff(DiffStart::Top)
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
        if self.files.selected_row().is_some() {
            self.select_from(self.files.selected, DiffStart::Top)
        } else {
            self.close_diff()
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
    /// It takes the Files list's load, so a user refresh started later wins.
    pub(crate) fn background_status(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace
            || self.is_busy()
            || self.op_is_running()
            || self.watch.status_pending
        {
            return vec![];
        }
        let tag = self.background_tag();
        self.files.begin(tag.seq);
        self.status_selection = Selection::KeepPath;
        self.watch.status_pending = true;
        vec![Effect::LoadStatus { tag }]
    }

    pub(crate) fn background_tag(&mut self) -> Tag {
        Tag {
            generation: self.generation,
            seq: self.take_seq(),
            user: false,
        }
    }

    /// Whether `status` shows what is already on screen. After an error
    /// the screen is out of date, so anything is news.
    fn status_unchanged(&self, status: &RepoStatus) -> bool {
        self.files.error.is_none()
            && self.files.rows == status.files
            && self.branch.as_ref() == Some(&status.branch)
    }
}

/// Shared actions several features call: reloading the branch list,
/// taking the selected branch for an action, starting an op attached or
/// detached, and copying the selected commit's text.
impl AppState {
    /// Load the branch list again, keeping what it shows until it lands.
    pub(crate) fn reload_branches(&mut self) -> Vec<Effect> {
        self.load_branches(true)
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

    /// Copy the selected commit's hash, short hash or subject now, or
    /// read its full message first. Pure, so it works while an operation
    /// runs.
    pub(crate) fn copy_commit_text(&mut self, what: CopyWhat) -> Vec<Effect> {
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
            CopyWhat::Subject => self.commits.list.rows[self.commits.list.selected]
                .subject
                .clone(),
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
        self.notice =
            Some(format!("Copied {}: {}", what.label(), clipboard::preview(&text)).into());
        vec![Effect::Clipboard(text)]
    }

    /// Copy the message read for the last request; an older one is dropped.
    pub(crate) fn commit_message_loaded(
        &mut self,
        tag: Tag,
        oid: CommitId,
        result: Result<String, String>,
    ) -> Vec<Effect> {
        self.user_done(tag);
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
}

mod confirm_guard;

// The slot's failure texts, kept reachable where they used to live.
#[cfg(test)]
pub(crate) use crate::op_slot::failure_next;
pub(crate) use crate::op_slot::failure_title;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confirm_question::{ConfirmQuestion, Then};
    use crate::diff_pane::DIFF_PREFETCH;
    use crate::features::branches::BranchesIntent;
    use crate::features::branches::remotes::RemotesIntent;
    use crate::features::branches::tags::TagsIntent;
    use crate::features::commit::CommitIntent;
    use crate::features::commits::copy::CopyIntent;
    use crate::features::commits::custom_patch::CustomPatchIntent;
    use crate::features::commits::rewrite::RewriteIntent;
    use crate::features::conflicts::ConflictsIntent;
    use crate::features::diff::DiffIntent;
    use crate::features::files::FilesIntent;
    use crate::features::operation::OperationIntent;
    use crate::features::operation::abort_question;
    use crate::features::pull_requests::PullRequestsIntent;
    use crate::features::stash::StashMode;
    use crate::features::sync::SyncIntent;
    use crate::intent::Scroll;
    use crate::keymap::KeyPress;
    use crate::menu::MenuId;
    use crate::op::ResetMode;
    use crate::popup::{DiscardWhat, PromptKind};
    use crate::refresh::{RefreshPlan, Scope};
    use crate::test_support::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use kika_git::diff::DiffSide;
    use kika_git::rebase_plan;
    use kika_git::status::{EntryKind, RepoStatus};
    use kika_git::submodule::SubmoduleState;

    #[test]
    fn quitting_while_an_op_runs_interrupts_git() {
        let mut state = AppState::new(Context::Workspace);
        state.hold_op(4);
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
        assert_eq!(state.busy_count(), 0);
    }

    #[test]
    fn latest_status_result_is_applied() {
        let mut state = AppState::new(Context::Workspace);
        let tag = refresh_tag(&mut state);

        state.update(loaded(tag, &["a.txt"]));

        assert!(state.files.loaded);
        assert_eq!(state.files.rows, [untracked("a.txt")]);
    }

    #[test]
    fn result_for_superseded_seq_is_dropped() {
        let mut state = AppState::new(Context::Workspace);
        let old = refresh_tag(&mut state);
        let new = refresh_tag(&mut state);

        state.update(loaded(old, &["stale.txt"]));
        assert!(!state.files.loaded);
        assert_eq!(state.files.rows, []);

        state.update(loaded(new, &["fresh.txt"]));
        assert_eq!(state.files.rows, [untracked("fresh.txt")]);

        state.update(loaded(old, &["stale.txt"]));
        assert_eq!(state.files.rows, [untracked("fresh.txt")]);
    }

    #[test]
    fn result_from_older_generation_is_dropped() {
        let mut state = AppState::new(Context::Workspace);
        let tag = refresh_tag(&mut state);
        state.generation += 1;

        state.update(loaded(tag, &["other-repo.txt"]));

        assert!(!state.files.loaded);
        assert_eq!(state.files.rows, []);
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
        assert_eq!(state.files.error.as_deref(), Some("boom"));
    }

    #[test]
    fn a_branch_only_plan_loads_status_once() {
        let (mut state, _) = with_files(three_files(), 24);
        let busy = state.busy_count();
        let effects = state.apply_plan(RefreshPlan {
            scopes: [Scope::Branch].into(),
            selection: Selection::KeepPath,
        });
        assert!(
            matches!(effects.as_slice(), [Effect::LoadStatus { .. }]),
            "{effects:?}"
        );
        assert_eq!(state.busy_count(), busy + 1);
    }

    #[test]
    fn error_result_is_recorded_and_cleared_by_the_next_success() {
        let mut state = AppState::new(Context::Workspace);
        let tag = refresh_tag(&mut state);
        state.update(AppIntent::StatusLoaded {
            tag,
            result: Err("fatal: not a git repository".into()),
        });
        assert!(state.files.loaded);
        assert_eq!(
            state.files.error.as_deref(),
            Some("fatal: not a git repository")
        );

        let tag = refresh_tag(&mut state);
        state.update(loaded(tag, &["a.txt"]));
        assert_eq!(state.files.error, None);
        assert_eq!(state.files.rows, [untracked("a.txt")]);
    }

    #[test]
    fn busy_rises_per_refresh_and_falls_per_result_even_when_stale() {
        let mut state = AppState::new(Context::Workspace);
        let old = refresh_tag(&mut state);
        assert_eq!(state.busy_count(), 1);
        let new = refresh_tag(&mut state);
        assert_eq!(state.busy_count(), 2);

        state.update(loaded(old, &[]));
        assert_eq!(state.busy_count(), 1);
        state.update(loaded(new, &[]));
        assert_eq!(state.busy_count(), 0);
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
        assert_eq!(state.busy_count(), 0);
    }

    #[test]
    fn non_user_results_leave_busy_alone() {
        let mut state = AppState::new(Context::Workspace);
        let mut tag = refresh_tag(&mut state);
        tag.user = false;
        state.update(loaded(tag, &[]));
        assert_eq!(state.busy_count(), 1);
    }

    #[test]
    fn a_status_load_selects_the_first_file_and_requests_its_diff() {
        // Height 24: 21 diff rows.
        let (state, effects) = with_files(three_files(), 24);
        assert_eq!(state.files.selected, 0);
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
    fn a_refresh_keeps_the_selected_path_and_its_scroll() {
        let (mut state, effects) = with_files(three_files(), 10);
        state.update(chunk(diff_tag(&effects), 50, true));
        state.update(AppIntent::Files(FilesIntent::Next));
        let tag = diff_tag(&state.update(AppIntent::Files(FilesIntent::Next)));
        state.update(chunk(tag, 50, true));
        state.update(AppIntent::ScrollDiff(Scroll::LineDown));
        assert_eq!(state.files.selected, 2);

        // c.txt moved to the front; the selection follows it.
        let status = refresh_tag(&mut state);
        let effects = state.update(AppIntent::StatusLoaded {
            tag: status,
            result: Ok(repo(vec![untracked("c.txt"), changed("a.rs", '.', 'M')])),
        });
        diff_tag(&effects);
        assert_eq!(state.files.selected, 0);
        assert_eq!(state.diff.scroll, 1);
    }

    #[test]
    fn a_refresh_that_drops_the_selected_file_clamps_and_resets_scroll() {
        let (mut state, _) = with_files(three_files(), 10);
        state.update(AppIntent::Files(FilesIntent::Next));
        let tag = diff_tag(&state.update(AppIntent::Files(FilesIntent::Next)));
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
        assert_eq!(state.files.selected, 1);
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
        assert_eq!(state.files.selected_row(), None);
        assert_eq!(state.diff, DiffPane::default());
    }

    #[test]
    fn a_failed_refresh_moves_a_hidden_cursor_to_a_shown_file_and_its_diff() {
        let (mut state, _) = with_files(three_files(), 10);
        state.files.filter = "rs".into();
        // Go to file searches every file, so it may land on a hidden one.
        state.update(AppIntent::Files(FilesIntent::GoToFile("c.txt".into())));
        assert_eq!(state.files.selected, 2);
        let status = refresh_tag(&mut state);
        let effects = state.update(AppIntent::StatusLoaded {
            tag: status,
            result: Err("boom".into()),
        });
        assert_eq!(state.files.selected, 0);
        assert_eq!(diff_request(&effects).path, "a.rs".into());
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
        assert_eq!(state.files.selected, 0);
        assert_eq!(state.diff.lines.len(), 5);
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
        state.update(AppIntent::Diff(DiffIntent::Slow { tag }));
        assert!(state.diff.show_loading);

        state.update(chunk(tag, 1, true));
        assert!(!state.diff.show_loading);
    }

    #[test]
    fn slow_after_the_first_chunk_does_nothing() {
        let (mut state, effects) = with_files(three_files(), 13);
        let tag = diff_tag(&effects);
        state.update(chunk(tag, 1, false));
        state.update(AppIntent::Diff(DiffIntent::Slow { tag }));
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
            state.busy_count(),
            4,
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
        assert_eq!(state.files.selected, 0);
        press(&mut state, KeyCode::Char('q'));
        assert!(state.should_quit);
    }

    #[test]
    fn the_next_key_clears_a_notice_made_at_run_time() {
        let mut state = copy_ready();
        state.update(AppIntent::Commits(CommitsIntent::Copy(
            CopyIntent::CopyText(CopyWhat::Hash),
        )));
        assert!(
            state
                .notice
                .as_ref()
                .is_some_and(|n| n.starts_with("Copied "))
        );
        press(&mut state, KeyCode::Char('x'));
        assert_eq!(state.notice, None);
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
    fn a_finished_op_refreshes_status_and_keeps_the_selected_path() {
        let (mut state, _) = with_files(three_files(), 24);
        select_path(&mut state, "b.rs");
        let (tag, op) = op_of(&state.update(AppIntent::Files(FilesIntent::ToggleStage)));
        let busy = state.busy_count();

        let effects = state.update(finished(tag, op, Ok(())));
        // The op's slot is released and the status load takes one.
        assert_eq!(state.busy_count(), busy);
        assert_eq!(state.held_seq(), None);
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
        assert_eq!(state.files.selected, 0);
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
        let (tag, op) = op_of(&state.update(AppIntent::Files(FilesIntent::ToggleStage)));

        let effects = state.update(finished(tag, op, Err("fatal: index.lock".into())));

        match state.popups.as_slice() {
            [Popup::Error(error)] => {
                assert_eq!(error.title, "Could not stage");
                assert_eq!(error.message, "fatal: index.lock");
                assert!(!error.next.is_empty());
            }
            other => panic!("expected one error popup, got {other:?}"),
        }
        assert_eq!(state.op_error(), None);
        assert_eq!(state.held_seq(), None);
        assert!(
            matches!(effects.as_slice(), [Effect::LoadStatus { .. }]),
            "{effects:?}"
        );
    }

    #[test]
    fn a_successful_op_clears_an_earlier_error() {
        let (mut state, _) = with_files(three_files(), 24);
        let (tag, op) = op_of(&state.update(AppIntent::Files(FilesIntent::ToggleStage)));
        state.set_op_error("left over".into());
        state.update(finished(tag, op, Ok(())));
        assert_eq!(state.op_error(), None);
    }

    #[test]
    fn a_stale_generation_op_lowers_busy_and_does_nothing_else() {
        let (mut state, _) = with_files(three_files(), 24);
        let (tag, op) = op_of(&state.update(AppIntent::Files(FilesIntent::ToggleStage)));
        let busy = state.busy_count();
        state.generation += 1;

        let effects = state.update(finished(tag, op, Err("boom".into())));

        assert_eq!(effects, vec![]);
        assert_eq!(state.busy_count(), busy - 1);
        assert_eq!(state.op_error(), None);
        assert_eq!(state.held_seq(), None, "the slot is freed even when stale");
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
        assert_eq!(state.files.selected, 1);
        assert_eq!(state.diff.path, Some("b.rs".into()));
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
        assert_eq!(state.files.selected, 0);
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
        assert_eq!(state.popups, [Popup::Question(quit_question())]);
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
            state.update(AppIntent::AskQuit);
            assert_eq!(press(&mut state, key), vec![]);
            assert_eq!(state.popups, []);
            assert!(!state.should_quit);
        }
    }

    fn failed_stage(state: &mut AppState, message: &str) -> Vec<Effect> {
        let (tag, op) = op_of(&state.update(AppIntent::Files(FilesIntent::ToggleStage)));
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
        let (tag, op) = op_of(&state.update(AppIntent::Files(FilesIntent::ToggleStage)));
        state.generation += 1;
        state.update(finished(tag, op, Err("boom".into())));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn a_failed_unstage_all_names_what_failed() {
        let (mut state, _) = with_files(vec![changed("b.rs", 'M', '.')], 24);
        let (tag, op) = op_of(&state.update(AppIntent::Files(FilesIntent::ToggleStageAll)));
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
            state.files.error.as_deref(),
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

    fn discard_popup(state: &AppState) -> &ConfirmQuestion {
        match state.popups.as_slice() {
            [Popup::Question(q)] if q.scope == KeyScope::DiscardConfirm => q,
            other => panic!("expected one discard popup, got {other:?}"),
        }
    }

    #[test]
    fn d_on_a_modified_file_asks_and_y_discards_it() {
        let (mut state, _) = with_files(three_files(), 24);
        assert_eq!(press(&mut state, KeyCode::Char('d')), vec![]);
        assert_eq!(
            discard_popup(&state),
            &DiscardWhat::File
                .into_question("a.rs".into(), GitOp::DiscardWorktree(vec!["a.rs".into()]))
        );
        assert_eq!(state.key_focus(), KeyScope::DiscardConfirm);
        assert_eq!(state.held_seq(), None);

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
            assert_eq!(state.held_seq(), None, "{key:?}");
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
    fn only_a_yes_to_a_range_discard_forgets_the_files_range() {
        // A one-file range and a single file build the same op; only the
        // question knows which was asked.
        let op = || GitOp::DiscardWorktree(vec!["a.rs".into()]);
        let range = DiscardWhat::Range {
            restore: 1,
            delete: 0,
        };
        for (what, anchor) in [(DiscardWhat::File, Some("b.rs".into())), (range, None)] {
            let (mut state, _) = with_files(three_files(), 24);
            state.files.range_anchor = Some("b.rs".into());
            state.popups = vec![Popup::Question(what.into_question("a.rs".into(), op()))];
            let (_, ran) = op_of(&state.update(AppIntent::ConfirmYes));
            assert_eq!(ran, op(), "{what:?}");
            assert_eq!(state.files.range_anchor, anchor, "{what:?}");
        }
    }

    #[test]
    fn d_on_an_untracked_file_asks_to_delete_it() {
        let (mut state, _) = with_files(three_files(), 24);
        select_path(&mut state, "c.txt");
        press(&mut state, KeyCode::Char('d'));
        assert_eq!(
            discard_popup(&state),
            &DiscardWhat::Untracked
                .into_question("c.txt".into(), GitOp::DeleteUntracked(vec!["c.txt".into()]))
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
        review.files.rows = three_files();
        review.files.selected = 0;
        let (mut running, _) = with_files(three_files(), 24);
        running.hold_op(9);
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
            &DiscardWhat::Hunk
                .into_question("a.rs".into(), GitOp::ApplyWorktree { patch: expected })
        );
        let (_, op) = op_of(&press(&mut state, KeyCode::Char('y')));
        assert!(matches!(op, GitOp::ApplyWorktree { .. }), "{op:?}");
    }

    #[test]
    fn d_on_the_only_hunk_discards_the_whole_file() {
        let mut state = focused(three_files(), ONE_HUNK);
        press(&mut state, KeyCode::Char('d'));
        let confirm = discard_popup(&state);
        assert_eq!(confirm.title, DiscardWhat::Hunk.title());
        assert_eq!(
            confirm.then,
            Then::Op(GitOp::DiscardWorktree(vec!["a.rs".into()]))
        );

        let new_file = b"diff --git a/n.txt b/n.txt\nnew file mode 100644\n\
            --- /dev/null\n+++ b/n.txt\n@@ -0,0 +1,2 @@\n+x\n+y\n";
        let mut state = focused(vec![untracked("n.txt")], new_file);
        press(&mut state, KeyCode::Char('d'));
        let confirm = discard_popup(&state);
        assert_eq!(confirm.title, DiscardWhat::Untracked.title());
        assert_eq!(
            confirm.then,
            Then::Op(GitOp::DeleteUntracked(vec!["n.txt".into()]))
        );
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
            &DiscardWhat::Lines
                .into_question("a.rs".into(), GitOp::ApplyWorktree { patch: expected })
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
        state.update(AppIntent::Diff(DiffIntent::PatchLoaded {
            tag,
            result: Err("fatal: bad".into()),
        }));
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

    #[test]
    fn more_and_less_context_reload_the_selected_diff_with_the_new_size() {
        let (mut state, _) = with_files(three_files(), 24);
        let effects = state.update(AppIntent::Diff(DiffIntent::MoreContext));
        assert_eq!(state.diff_options.context, 4);
        assert_eq!(effects.len(), 2, "{effects:?}");
        assert_eq!(diff_request(&effects).options.context, 4);

        let effects = state.update(AppIntent::Diff(DiffIntent::LessContext));
        let effects2 = state.update(AppIntent::Diff(DiffIntent::LessContext));
        assert_eq!(state.diff_options.context, 2);
        assert_eq!(diff_request(&effects).options.context, 3);
        assert_eq!(diff_request(&effects2).options.context, 2);
    }

    #[test]
    fn a_context_change_in_diff_focus_reloads_the_patch_with_the_same_options() {
        let mut state = focused(three_files(), TWO_HUNKS);
        let effects = state.update(AppIntent::Diff(DiffIntent::MoreContext));
        let (_, patch) = patch_load(&effects);
        assert_eq!(diff_request(&effects).options, state.diff_options);
        assert_eq!(patch.options, state.diff_options);
        assert_eq!(patch.options.context, 4);
        assert_eq!(view(&state).seq, Some(patch_load(&effects).0.seq));
    }

    #[test]
    fn context_stops_at_its_limits_and_the_keys_say_why() {
        use kika_git::diff::{MAX_CONTEXT, MIN_CONTEXT};
        let (mut state, _) = with_files(three_files(), 24);
        state.diff_options.context = MAX_CONTEXT;
        for c in ['+', '='] {
            assert_eq!(press_char(&mut state, c), vec![], "{c}");
            assert_eq!(state.notice, Some("Already at the most context".into()));
        }
        assert_eq!(
            state.update(AppIntent::Diff(DiffIntent::MoreContext)),
            vec![]
        );
        assert_eq!(state.diff_options.context, MAX_CONTEXT);

        state.diff_options.context = MIN_CONTEXT;
        assert_eq!(press_char(&mut state, '-'), vec![]);
        assert_eq!(state.notice, Some("Already at the least context".into()));
        assert_eq!(
            state.update(AppIntent::Diff(DiffIntent::LessContext)),
            vec![]
        );
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
        assert_eq!(
            state.update(AppIntent::Diff(DiffIntent::MoreContext)),
            vec![]
        );
        assert_eq!(
            state.update(AppIntent::Diff(DiffIntent::ToggleWhitespace)),
            vec![]
        );
        assert_eq!(
            state.update(AppIntent::Diff(DiffIntent::LessContext)),
            vec![]
        );
        assert_eq!(
            state.update(AppIntent::Diff(DiffIntent::LessContext)),
            vec![]
        );
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
        let effects = state.update(AppIntent::Diff(DiffIntent::ToggleWhitespace));
        assert!(state.diff_options.ignore_whitespace);
        assert!(diff_request(&effects).options.ignore_whitespace);
        assert!(patch_load(&effects).1.options.ignore_whitespace);

        let effects = state.update(AppIntent::Diff(DiffIntent::ToggleWhitespace));
        assert!(!state.diff_options.ignore_whitespace);
        assert!(!patch_load(&effects).1.options.ignore_whitespace);
    }

    #[test]
    fn hidden_whitespace_refuses_every_key_that_applies_a_patch() {
        const REASON: &str = "Whitespace is hidden; press w to stage";
        let mut state = focused(three_files(), TWO_HUNKS);
        let effects = state.update(AppIntent::Diff(DiffIntent::ToggleWhitespace));
        let (tag, _) = patch_load(&effects);
        state.update(AppIntent::Diff(DiffIntent::PatchLoaded {
            tag,
            result: Ok(patch_of(TWO_HUNKS)),
        }));
        for c in [' ', 'v', 'd', 'e'] {
            assert_eq!(press_char(&mut state, c), vec![], "{c:?}");
            assert_eq!(state.notice, Some(REASON.into()), "{c:?}");
            assert_eq!(state.popups, [], "{c:?}");
            assert_eq!(state.panel_focus(), KeyScope::Diff, "{c:?}");
        }

        // Line mode, entered before whitespace was hidden.
        state.update(AppIntent::Diff(DiffIntent::ToggleWhitespace));
        let (tag, _) = patch_load(&state.update(AppIntent::FocusDiff));
        state.update(AppIntent::Diff(DiffIntent::PatchLoaded {
            tag,
            result: Ok(patch_of(TWO_HUNKS)),
        }));
        press_char(&mut state, 'v');
        assert_eq!(state.panel_focus(), KeyScope::Lines);
        let (tag, _) = patch_load(&state.update(AppIntent::Diff(DiffIntent::ToggleWhitespace)));
        state.update(AppIntent::Diff(DiffIntent::PatchLoaded {
            tag,
            result: Ok(patch_of(TWO_HUNKS)),
        }));
        for c in [' ', 'd'] {
            assert_eq!(press_char(&mut state, c), vec![], "{c:?}");
            assert_eq!(state.notice, Some(REASON.into()), "{c:?}");
            assert_eq!(state.popups, [], "{c:?}");
        }

        // The intents themselves refuse too, whatever sent them.
        for intent in [
            AppIntent::Diff(DiffIntent::ToggleHunkStage),
            AppIntent::Diff(DiffIntent::ToggleLinesStage),
            AppIntent::Discard,
            AppIntent::Diff(DiffIntent::EditHunk),
        ] {
            assert_eq!(state.update(intent.clone()), vec![], "{intent:?}");
        }
        assert_eq!(state.popups, []);
    }

    #[test]
    fn hidden_whitespace_leaves_file_level_stage_and_discard_alone() {
        let (mut state, _) = with_files(three_files(), 24);
        state.update(AppIntent::Diff(DiffIntent::ToggleWhitespace));
        let effects = press_char(&mut state, ' ');
        assert!(
            matches!(effects.as_slice(), [Effect::RunOp { .. }]),
            "{effects:?}"
        );
        state.free_op();
        press_char(&mut state, 'd');
        assert!(
            matches!(state.popups.as_slice(), [Popup::Question(_)]),
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

        let effects = state.update(AppIntent::Diff(DiffIntent::MoreContext));
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
        state.update(AppIntent::Diff(DiffIntent::NextHunk));
        state.update(AppIntent::Diff(DiffIntent::NextHunk));
        assert_eq!(view(&state).hunk, 2);

        let effects = state.update(AppIntent::Diff(DiffIntent::MoreContext));
        assert_eq!(view(&state).hunk, 2);
        let (tag, _) = patch_load(&effects);
        state.update(AppIntent::Diff(DiffIntent::PatchLoaded {
            tag,
            result: Ok(patch_of(THREE_HUNKS)),
        }));
        assert_eq!(view(&state).hunk, 2);
    }

    #[test]
    fn changed_options_last_for_the_session() {
        let (mut state, _) = with_files(three_files(), 24);
        state.update(AppIntent::Diff(DiffIntent::MoreContext));
        state.update(AppIntent::Diff(DiffIntent::ToggleWhitespace));
        let changed = state.diff_options;
        assert_ne!(changed, DiffOptions::default());

        assert_eq!(
            diff_request(&state.update(AppIntent::Files(FilesIntent::Next))).options,
            changed
        );
        let (_, request) = patch_load(&state.update(AppIntent::FocusDiff));
        assert_eq!(request.options, changed);
        state.update(AppIntent::FocusLeft);
        assert_eq!(
            diff_request(&state.update(AppIntent::Files(FilesIntent::Prev))).options,
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
                assert!(matches!(state.focus, Focus::Diff), "{right:?}");
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
        assert_eq!(state.commits.list.rows, []);
        assert!(!state.commits.list.loaded);
        let (tag, _, skip) = commits_load(&effects);
        assert_eq!((tag.generation, skip), (state.generation, 0));
    }

    // --- A commit's files ---

    #[test]
    fn esc_goes_back_to_the_same_commit_and_reloads_the_worktree_diff() {
        let mut state = on_commit_five();
        opened(&mut state, two_commit_files());
        press(&mut state, KeyCode::Char('j'));

        let effects = press(&mut state, KeyCode::Esc);

        assert!(!state.should_quit);
        assert_eq!(state.commits.open, None);
        assert_eq!(state.commits.list.selected, 5);
        assert_eq!(state.key_focus(), KeyScope::Commits);
        let request = diff_request(&effects);
        assert_eq!((request.commit, request.path), (None, "a.rs".into()));
    }

    #[test]
    fn closing_with_no_worktree_file_empties_the_diff() {
        let mut state = on_commit_five();
        opened(&mut state, two_commit_files());
        state.files.rows.clear();
        state.files.selected = 0;
        assert_eq!(
            state.update(AppIntent::Commits(CommitsIntent::CloseFiles)),
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
            assert_eq!(
                state.files.selected_row().map(|_| state.files.selected),
                (!files.is_empty()).then_some(0)
            );
        }
    }

    #[test]
    fn new_diff_options_reload_the_open_commits_file() {
        let mut state = on_commit_five();
        opened(&mut state, two_commit_files());
        let request = diff_request(&state.update(AppIntent::Diff(DiffIntent::ToggleWhitespace)));
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
            key_reason(&state, KeyScope::CommitFiles, 'l'),
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

    use crate::watch::WatchEvent;
    use kika_git::refs::RefsSnapshot;

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
        assert_eq!(state.files.seq, Some(tag.seq));
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
        assert_eq!(state.busy_count(), 0);
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
        assert_eq!(state.busy_count(), 0);
        assert_eq!(state.files.seq, Some(tag.seq));
        assert!(state.watch.status_pending);

        state.update(AppIntent::StatusLoaded {
            tag,
            result: Ok(repo(three_files())),
        });
        assert_eq!(state.busy_count(), 0);
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
        assert_eq!(state.busy_count(), 0);
    }

    #[test]
    fn background_status_is_skipped_while_busy_an_op_runs_or_one_is_pending() {
        let mut state = idle_with_files();
        state.set_busy(1);
        assert!(watch(&mut state, WatchEvent::PollFiles).is_empty());
        assert_eq!(state.busy_count(), 1);

        let mut state = idle_with_files();
        state.hold_op(42);
        assert!(watch(&mut state, WatchEvent::PollFiles).is_empty());

        let mut state = idle_with_files();
        background_status_tag(&watch(&mut state, WatchEvent::PollFiles));
        let seq = state.files.seq;
        for _ in 0..3 {
            assert!(watch(&mut state, WatchEvent::PollFiles).is_empty());
        }
        assert_eq!(state.files.seq, seq);
    }

    #[test]
    fn focus_gained_while_busy_still_takes_a_snapshot() {
        let mut state = idle_with_files();
        state.set_busy(1);
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
        assert_eq!(state.files.selected, before.files.selected);
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

        assert_eq!(state.files.rows.len(), 3);
        assert_eq!(state.files.selected, 0);
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
        assert!(state.files.rows.is_empty());
    }

    #[test]
    fn a_stale_unchanged_or_failed_background_status_leaves_the_user_load_in_flight() {
        let mut state = idle_with_files();
        let background = background_status_tag(&watch(&mut state, WatchEvent::PollFiles));
        let user = refresh_tag(&mut state);
        let (files, branch, diff) = (
            state.files.clone(),
            state.branch.clone(),
            state.diff.clone(),
        );

        for result in [Ok(repo(three_files())), Err("boom".to_string())] {
            let effects = state.update(AppIntent::StatusLoaded {
                tag: background,
                result,
            });
            assert!(effects.is_empty(), "{effects:?}");
            assert_eq!(state.files.seq, Some(user.seq));
            assert_eq!(state.files, files);
            assert_eq!(state.branch, branch);
            assert_eq!(state.diff, diff);
            assert!(state.popups.is_empty());
        }

        state.update(loaded(user, &["fresh.txt"]));
        assert_eq!(state.files.rows, [untracked("fresh.txt")]);
        assert_eq!(state.files.seq, None);
    }

    #[test]
    fn a_failed_background_status_is_dropped_without_a_popup() {
        let mut state = idle_with_files();
        let files = state.files.rows.clone();
        let tag = background_status_tag(&watch(&mut state, WatchEvent::PollFiles));

        let effects = state.update(AppIntent::StatusLoaded {
            tag,
            result: Err("fatal: index file corrupt".into()),
        });

        assert!(effects.is_empty());
        assert!(state.popups.is_empty());
        assert_eq!(state.files.error, None);
        assert_eq!(state.files.rows, files);
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
        assert_eq!(state.files.rows, three_files());
        assert!(!state.watch.status_pending);
        assert_eq!(state.busy_count(), 1);
        state.update(loaded(user, &["x.txt"]));
        assert_eq!(state.busy_count(), 0);
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
        assert_eq!(state.files.rows.len(), 2);
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
        state.update(AppIntent::Branches(BranchesIntent::Loaded {
            tag: list,
            result: Ok(vec![]),
        }));
        state.update(AppIntent::Branches(BranchesIntent::Remotes(
            RemotesIntent::Loaded {
                tag: remotes,
                result: Ok(vec![]),
            },
        )));
        state.update(AppIntent::Branches(BranchesIntent::Tags(
            TagsIntent::Loaded {
                tag: tags,
                result: Ok(vec![]),
            },
        )));
        assert_eq!(state.busy_count(), 0);

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
        assert_eq!(space.held_seq(), None);
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

    // --- Merge, rebase and the operation in progress ---

    #[test]
    fn a_bisect_alone_stops_a_merge_until_it_is_reset() {
        let mut state = in_progress(&[Operation::Bisect]);
        state.update(AppIntent::Branches(BranchesIntent::Next));
        assert_eq!(
            state.update(AppIntent::Operation(OperationIntent::Merge(
                MergeStrategy::FastForwardOnly
            ))),
            vec![]
        );
        assert_eq!(state.notice, Some(bisect::RESET_FIRST.into()));
    }

    #[test]
    fn enter_still_answers_the_other_questions() {
        let mut state = AppState::new(Context::Workspace);
        state.update(AppIntent::AskQuit);
        assert_eq!(state.key_focus(), KeyScope::Confirm);
        press(&mut state, KeyCode::Enter);
        assert!(state.should_quit);
        assert_eq!(state.popups, []);
    }

    #[test]
    fn a_finished_squash_says_the_changes_wait_to_be_committed() {
        let mut state = in_progress(&[]);
        state.update(AppIntent::Branches(BranchesIntent::Next));
        let (tag, op) = op_of(&state.update(AppIntent::Operation(OperationIntent::Merge(
            MergeStrategy::Squash,
        ))));
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
                on_main(AppIntent::Commits(CommitsIntent::Checkout)),
                checkout.clone(),
            ),
            (
                "checkout from a detached HEAD".to_string(),
                checked(AppIntent::Commits(CommitsIntent::Checkout), Ok(vec![])).0,
                checkout,
            ),
            (
                "hard reset on a branch".to_string(),
                on_main(AppIntent::Commits(CommitsIntent::ResetTo(ResetMode::Hard))),
                hard.clone(),
            ),
            (
                "hard reset from a detached HEAD".to_string(),
                checked(
                    AppIntent::Commits(CommitsIntent::ResetTo(ResetMode::Hard)),
                    Ok(vec![]),
                )
                .0,
                hard,
            ),
        ];
        for (intent, op) in moves() {
            let (state, _) = checked(intent.clone(), Ok(ids(2)));
            questions.push((format!("leave commits for {intent:?}"), state, op));
        }
        for (name, state, _) in &questions {
            assert_eq!(state.popups.len(), 1, "{name}");
            assert_eq!(state.held_seq(), None, "{name}");
        }
        questions
    }

    /// Answer the open question with `y`, as the key does.
    fn answer_yes(state: &mut AppState) -> Vec<Effect> {
        match state.popups.last() {
            Some(Popup::Question(q)) if q.scope == KeyScope::LeaveCommits => state.update(
                AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::ConfirmLeaveCommits)),
            ),
            _ => press_char(state, 'y'),
        }
    }

    fn start(state: &mut AppState, operations: &[Operation]) {
        state.notice = None;
        state.branch.as_mut().unwrap().in_progress = operations.to_vec();
    }

    fn assert_refused(state: &AppState, effects: &[Effect], reason: &str, name: &str) {
        assert_eq!(effects, [], "{name}");
        assert_eq!(state.notice.as_deref(), Some(reason), "{name}");
        assert_eq!(state.popups, [], "{name}");
        assert_eq!(state.held_seq(), None, "{name}");
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
                    assert_eq!(state.held_seq(), Some(tag.seq), "{name}");
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
            assert_eq!(state.held_seq(), Some(tag.seq), "{name}");
            assert_eq!(state.popups, [], "{name}");
            assert_eq!(state.notice, None, "{name}");
        }
    }

    #[test]
    fn a_row_whose_id_is_not_hex_starts_nothing() {
        let mut state = on_commit();
        state.commits.list.rows[1].oid = "--upload-pack=x".into();
        for intent in [
            AppIntent::Commits(CommitsIntent::Checkout),
            AppIntent::Commits(CommitsIntent::Revert),
            AppIntent::Commits(CommitsIntent::ResetTo(ResetMode::Soft)),
            AppIntent::Commits(CommitsIntent::ResetTo(ResetMode::Hard)),
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
        assert_eq!(state.held_seq(), None);
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
        // Skip checks for work since the stop first; with none, it skips.
        let check = press_char(&mut state, 's');
        let [
            Effect::CheckStopWork {
                tag,
                operation,
                head,
            },
        ] = check.as_slice()
        else {
            panic!("expected one CheckStopWork, got {check:?}");
        };
        let checked = state.update(AppIntent::Operation(OperationIntent::StopWorkChecked {
            tag: *tag,
            operation: *operation,
            head: head.clone(),
            result: Ok(false),
        }));
        let (tag, op) = attached_of(&checked);
        assert_eq!(op, GitOp::Skip(Operation::Revert));
        state.update(finished(tag, op, Ok(())));
        press_char(&mut state, 'a');
        assert_eq!(
            state.popups,
            [Popup::Question(abort_question(Operation::Revert).unwrap())]
        );
        let (_, op) = op_of(&press_char(&mut state, 'y'));
        assert_eq!(op, GitOp::Abort(Operation::Revert));
    }

    #[test]
    fn a_credential_answer_never_reaches_the_command_log() {
        use crate::features::command_log::CommandLogIntent;
        use kika_git::command_line::display_line;
        use kika_git::recording::{CommandEvent, CommandOutcome};
        use kika_git::remote::{AskpassEnv, remote_command};

        let (mut state, _) = typing_password("hunter2");
        let askpass = AskpassEnv {
            program: "/usr/bin/kika".into(),
            socket: "/tmp/kika-askpass-1/socket".into(),
            git_askpass: true,
        };
        let line = display_line(&remote_command(&PUSH, Some(&askpass)));
        state.update(AppIntent::CommandLog(CommandLogIntent::Logged(
            CommandEvent::Started {
                id: 0,
                line,
                attached: false,
            },
        )));
        state.update(AppIntent::CredentialAnswered(Some(Secret::new("hunter2"))));
        state.update(AppIntent::CommandLog(CommandLogIntent::Logged(
            CommandEvent::Finished {
                id: 0,
                outcome: CommandOutcome::Ok,
                elapsed: std::time::Duration::from_millis(5),
            },
        )));

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

    // --- Failure titles: an interactive rebase and an undo or redo ---

    #[test]
    fn a_failed_interactive_rebase_points_at_the_status_panel() {
        let op = GitOp::InteractiveRebase(rebase_plan::sample());
        assert_eq!(failure_title(&op), "Could not rebase");
        assert_eq!(
            failure_next(&op),
            "If git stopped on conflicts, resolve them, then continue or abort from the Status panel."
        );
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
        state.branches.list.rows[0].upstream = Some("origin/main".into());
        reload_remotes(
            &mut state,
            vec![
                git_remote("fork", true, &[]),
                git_remote("origin", true, &["main"]),
            ],
        );
        reload_tags(&mut state, two_tags());
        state.set_busy(0);
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
        state.branches.list.rows[0].upstream = None;
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
        assert_eq!(state.held_seq(), None);
    }

    #[test]
    fn a_while_only_bisecting_asks_then_y_resets_and_only_y_answers() {
        let mut state = testing_c1(&[Operation::Bisect]);
        assert_eq!(press_char(&mut state, 'a'), vec![]);
        let question = [Popup::Question(state.bisect_reset_question())];
        assert_eq!(state.popups, question);
        assert_eq!(state.key_focus(), KeyScope::AbortConfirm);
        assert!(
            state
                .bisect_reset_question()
                .text
                .contains("HEAD goes back to main;")
        );

        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.popups, question);
        assert_eq!(state.held_seq(), None);

        let (_, op) = op_of(&press_char(&mut state, 'y'));
        assert_eq!(op, GitOp::BisectReset);
        assert_eq!(state.popups, []);

        for no in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut state = testing_c1(&[Operation::Bisect]);
            press_char(&mut state, 'a');
            assert_eq!(press(&mut state, no), vec![], "{no:?}");
            assert_eq!(state.popups, []);
            assert_eq!(state.held_seq(), None);
        }
    }

    #[test]
    fn a_with_a_rebase_and_a_bisect_still_aborts_the_rebase() {
        let mut state = testing_c1(&[Operation::Rebase, Operation::Bisect]);
        press_char(&mut state, 'a');
        assert_eq!(
            state.popups,
            [Popup::Question(abort_question(Operation::Rebase).unwrap())]
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
            (
                "open commit",
                to_commit,
                vec![],
                AppIntent::Commit(CommitIntent::Open),
            ),
            (
                "open amend",
                to_commit,
                vec![],
                AppIntent::Commit(CommitIntent::OpenAmend),
            ),
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
                AppIntent::Commits(CommitsIntent::Checkout),
            ),
            (
                "revert",
                on_commit,
                vec![],
                AppIntent::Commits(CommitsIntent::Revert),
            ),
            ("reset menu", on_commit, vec![], key('X')),
            (
                "paste",
                one_copied,
                vec![],
                AppIntent::Commits(CommitsIntent::Copy(CopyIntent::Paste)),
            ),
            (
                "move up",
                on_linear_log,
                vec![],
                AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::Move(Direction::Up))),
            ),
            (
                "move down",
                on_linear_log,
                vec![],
                AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::Move(Direction::Down))),
            ),
            (
                "autosquash",
                with_a_fixup,
                vec![],
                AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::Autosquash)),
            ),
            (
                "create fixup",
                with_a_fixup,
                vec![],
                AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::CreateFixup)),
            ),
            (
                "checkout branch",
                with_branches,
                vec![AppIntent::Branches(BranchesIntent::Next)],
                AppIntent::Branches(BranchesIntent::Checkout),
            ),
            (
                "new branch",
                with_branches,
                vec![],
                AppIntent::Branches(BranchesIntent::New),
            ),
            (
                "new branch named",
                with_branches,
                vec![AppIntent::Branches(BranchesIntent::New)],
                AppIntent::Branches(BranchesIntent::CreateNamed("x".into())),
            ),
            (
                "track",
                on_remote_topic,
                vec![],
                AppIntent::Branches(BranchesIntent::Remotes(RemotesIntent::Track)),
            ),
            (
                "track named",
                on_remote_topic,
                vec![AppIntent::Branches(BranchesIntent::Remotes(
                    RemotesIntent::Track,
                ))],
                AppIntent::Branches(BranchesIntent::Remotes(RemotesIntent::TrackNamed(
                    "x".into(),
                ))),
            ),
            (
                "merge key",
                with_branches,
                vec![AppIntent::Branches(BranchesIntent::Next)],
                key('m'),
            ),
            (
                "rebase onto",
                with_branches,
                vec![AppIntent::Branches(BranchesIntent::Next)],
                AppIntent::Operation(OperationIntent::RebaseOnto),
            ),
        ];
        for mode in [ResetMode::Soft, ResetMode::Mixed, ResetMode::Hard] {
            cases.push((
                "reset to",
                on_commit,
                vec![],
                AppIntent::Commits(CommitsIntent::ResetTo(mode)),
            ));
        }
        for edit in [Edit::Squash, Edit::Fixup, Edit::Drop, Edit::Reword] {
            cases.push((
                "rewrite",
                on_linear_log,
                vec![],
                AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::Edit(edit))),
            ));
        }
        for edit in [Edit::Squash, Edit::Fixup, Edit::Drop] {
            cases.push((
                "range rewrite",
                on_linear_log,
                range(),
                AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::Edit(edit))),
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
                vec![AppIntent::Branches(BranchesIntent::Next)],
                AppIntent::Operation(OperationIntent::Merge(strategy)),
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
            assert_eq!(state.held_seq(), None, "{name} {action:?}");
        }
    }

    #[test]
    fn an_editor_open_when_a_bisect_began_does_not_commit() {
        let mut state = editing();
        type_text(&mut state, "fix");
        let before = state.commit.clone();
        assert!(matches!(before, CommitFlow::Editing(_)));
        start_bisecting(&mut state);
        let effects = state.update(AppIntent::Commit(CommitIntent::Submit));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::ProbeCommit { .. })),
            "{effects:?}"
        );
        assert_eq!(effects, vec![]);
        assert_eq!(state.notice, Some(bisect::RESET_FIRST.into()));
        assert_eq!(state.commit, before);
        assert_eq!(state.held_seq(), None);
    }

    #[test]
    fn marks_and_reset_still_run_during_a_bisect() {
        let mut state = testing_c1(&[Operation::Bisect]);
        let (_, op) = op_of(&state.update(AppIntent::Commits(CommitsIntent::Bisect(
            BisectIntent::MarkHead(BisectMark::Good),
        ))));
        assert_eq!(op, bisect_mark(BisectMark::Good, "c1", false));
        state.free_op();
        let (_, op) = op_of(&state.update(AppIntent::Commits(CommitsIntent::Bisect(
            BisectIntent::MarkSelected(BisectMark::Bad),
        ))));
        assert_eq!(op, bisect_mark(BisectMark::Bad, "c0", false));
        state.free_op();
        state.update(AppIntent::Commits(CommitsIntent::Bisect(
            BisectIntent::AskReset,
        )));
        let (_, op) = op_of(&state.update(AppIntent::Commits(CommitsIntent::Bisect(
            BisectIntent::ConfirmReset,
        ))));
        assert_eq!(op, GitOp::BisectReset);
        assert_eq!(state.notice, None);
    }

    #[test]
    fn without_a_bisect_head_moves_are_not_refused_for_one() {
        let state = ready_to_bisect();
        let guards: [fn(&AppState) -> Option<Reason>; 6] = [
            crate::features::commit::keys::commit_disabled,
            crate::features::commit::keys::amend_disabled,
            crate::features::commits::keys::checkout_commit_disabled,
            crate::features::commits::keys::reset_disabled,
            crate::features::operation::keys::merge_disabled,
            crate::features::branches::keys::new_branch_disabled,
        ];
        for guard in guards {
            assert_ne!(guard(&state), Some(bisect::RESET_FIRST.into()));
        }
        let mut state = ready_to_bisect();
        state.commits.list.selected = 1;
        state.update(AppIntent::Commits(CommitsIntent::Checkout));
        assert_eq!(state.notice, None);
        assert_ne!(state.popups, []);
        let mut state = ready_to_bisect();
        state.commits.list.selected = 1;
        assert_ne!(
            state.update(AppIntent::Commits(CommitsIntent::ResetTo(ResetMode::Soft))),
            vec![]
        );
        assert_eq!(state.notice, None);
    }

    #[test]
    fn with_a_rebase_and_a_bisect_the_rebase_is_named_first() {
        let mut state = testing_c1(&[Operation::Rebase, Operation::Bisect]);
        state.update(AppIntent::FocusPanel(layout::Panel::Side(
            SidePanel::Commits,
        )));
        for action in [
            AppIntent::Commits(CommitsIntent::Revert),
            AppIntent::Commits(CommitsIntent::ResetTo(ResetMode::Soft)),
            AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::Edit(
                rebase_plan::Edit::Drop,
            ))),
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
        assert_eq!(state.files.range_anchor, None);
        assert!(!state.should_quit, "Esc cleared the range first");
    }

    #[test]
    fn a_filter_hiding_every_row_refuses_row_keys() {
        let (mut state, _) = with_files(three_files(), 24);
        type_text(&mut state, "/zzz\n");
        assert!(state.files.visible().is_empty());
        assert_eq!(state.files.selected, 0, "nothing to move to");
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
        assert_eq!(state.files.range_anchor, None);
    }

    #[test]
    fn c_copies_a_range_skipping_merges_and_again_takes_them_out() {
        let mut state = with_merge();
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        press_char(&mut state, 'j');
        assert_eq!(state.commits.list.range(), [0, 1, 2]);
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
            state.notice.as_deref(),
            Some("Copied 2 (1 merge skipped, 2 copied)")
        );
        assert_eq!(state.commits.list.range_anchor, None);

        press_char(&mut state, 'V');
        press_char(&mut state, 'k');
        press_char(&mut state, 'k');
        press_char(&mut state, 'c');
        assert!(state.copied.is_empty());
        assert_eq!(
            state.notice.as_deref(),
            Some("Removed 2 (1 merge skipped, 0 copied)")
        );
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
        assert_eq!(open_files(&state).list.visible(), [0, 1]);
        for _ in 0..3 {
            press(&mut state, KeyCode::Backspace);
        }
        type_text(&mut state, "legacy");
        assert_eq!(
            open_files(&state).list.visible(),
            [1],
            "a rename's old name"
        );
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.input, None);
        assert_eq!(open_files(&state).list.filter, "legacy");
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
        assert_eq!(open_files(&state).list.selected, 2);
        let request = diff_request(&effects);
        assert_eq!(request.path, "docs/guide.md".into());
        assert_eq!(request.commit, Some(range_of("c5", "c6")));
        // Typing on with the cursor still shown loads nothing more.
        assert_eq!(typed(&mut state, " guide"), vec![]);
        // Hiding every row leaves the cursor and loads nothing.
        assert_eq!(typed(&mut state, "zzz"), vec![]);
        assert_eq!(open_files(&state).list.selected, 2);
        assert!(open_files(&state).list.visible().is_empty());
    }

    #[test]
    fn j_and_k_in_a_commits_files_step_over_hidden_rows() {
        let mut state = commit_files_open();
        type_text(&mut state, "/rs\n"); // lib.rs and the rename
        assert_eq!(open_files(&state).list.visible(), [0, 1]);
        let request = diff_request(&press_char(&mut state, 'j'));
        assert_eq!(request.path, "src/new.rs".into());
        assert_eq!(
            press_char(&mut state, 'j'),
            vec![],
            "stops at the last shown"
        );
        assert_eq!(open_files(&state).list.selected, 1);
        press_char(&mut state, 'k');
        assert_eq!(open_files(&state).list.selected, 0);

        // `/` starts from `rs`: `d` in its place shows the rename and the doc.
        press_char(&mut state, '/');
        press(&mut state, KeyCode::Backspace);
        press(&mut state, KeyCode::Backspace);
        type_text(&mut state, "d\n");
        assert_eq!(open_files(&state).list.visible(), [1, 2]);
        assert_eq!(open_files(&state).list.selected, 1);
        press_char(&mut state, 'j');
        assert_eq!(open_files(&state).list.selected, 2);
        press_char(&mut state, 'k');
        press_char(&mut state, 'k');
        assert_eq!(open_files(&state).list.selected, 1, "row 0 is hidden");
    }

    #[test]
    fn the_commit_files_filter_stops_at_its_limit() {
        let mut state = commit_files_open();
        press_char(&mut state, '/');
        for _ in 0..300 {
            press_char(&mut state, 'z');
        }
        assert_eq!(
            open_files(&state).list.filter.chars().count(),
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
        assert_eq!(open_files(&state).list.filter, "");
    }

    #[test]
    fn a_hidden_commit_file_is_neither_picked_nor_focused() {
        let mut state = commit_files_open();
        type_text(&mut state, "/zzz\n");
        assert!(open_files(&state).list.selected_hidden());
        assert_eq!(
            crate::features::commits::custom_patch::keys::patch_file_disabled(&state).as_deref(),
            Some(crate::keymap::FILTER_HIDES_ROW)
        );
        assert_eq!(
            state.update(AppIntent::Commits(CommitsIntent::CustomPatch(
                CustomPatchIntent::ToggleFile
            ))),
            vec![]
        );
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
        let kept = open_files(&state).list.filter.clone();
        assert!(kept.starts_with("--output=x -- "), "{kept:?}");
        assert!(kept.ends_with('\u{202e}'), "{kept:?}");
        for key in ['j', 'k', 'l'] {
            effects.extend(press_char(&mut state, key));
        }
        effects.extend(state.update(AppIntent::Diff(DiffIntent::ToggleWhitespace)));
        let refresh = press_char(&mut state, 'r');
        assert!(!leaks(&effects), "{effects:?}");
        assert!(!leaks(&refresh), "{refresh:?}");
        assert!(!format!("{effects:?}{refresh:?}").contains(&kept));
        // The list git was asked for is the same as without a filter.
        assert_eq!(open_files(&state).range, open_files(&twin).range);
        let mut twin = twin;
        twin.update(AppIntent::Diff(DiffIntent::ToggleWhitespace));
        assert_eq!(refresh, press_char(&mut twin, 'r'));
    }

    #[test]
    fn the_input_stops_at_its_limit_but_still_edits() {
        let (mut state, _) = with_files(three_files(), 24);
        press_char(&mut state, '/');
        for _ in 0..300 {
            press_char(&mut state, 'z');
        }
        assert_eq!(state.files.filter.chars().count(), list_filter::MAX_INPUT);
        assert_eq!(state.notice, Some(list_filter::input_full_notice().into()));
        press(&mut state, KeyCode::Backspace);
        assert_eq!(
            state.files.filter.chars().count(),
            list_filter::MAX_INPUT - 1
        );
    }

    #[test]
    fn a_changed_filter_says_it_cleared_the_selection() {
        let (mut state, _) = with_files(three_files(), 24);
        press_char(&mut state, 'V');
        type_text(&mut state, "/a");
        assert_eq!(state.files.range_anchor, None);
        assert_eq!(state.notice, Some("Selection cleared".into()));
    }

    // --- Copying the selected commit's text ---

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
        state.commits.list.rows[1].short_oid = "1111111".into();
        assert_eq!(press_char(&mut state, 'y'), vec![]);
        assert_eq!(open_menu(&state), MenuId::Copy);

        let cases = [
            (CopyWhat::Hash, COPIED_OID, "hash"),
            (CopyWhat::ShortHash, "1111111", "short hash"),
            (CopyWhat::Subject, "commit 1", "subject"),
        ];
        for (what, text, label) in cases {
            let effects = state.update(AppIntent::Commits(CommitsIntent::Copy(
                CopyIntent::CopyText(what),
            )));
            assert_eq!(effects, [Effect::Clipboard(text.into())], "{what:?}");
            assert_eq!(
                state.notice.as_deref(),
                Some(format!("Copied {label}: {text}").as_str()),
                "{what:?}"
            );
        }
        // The notice keeps the first 40 characters; the copy has them all.
        state.commits.list.rows[1].subject = "s".repeat(41);
        let effects = state.update(AppIntent::Commits(CommitsIntent::Copy(
            CopyIntent::CopyText(CopyWhat::Subject),
        )));
        assert_eq!(effects, [Effect::Clipboard("s".repeat(41))]);
        assert_eq!(
            state.notice.as_deref(),
            Some(format!("Copied subject: {}…", "s".repeat(40)).as_str())
        );
    }

    #[test]
    fn a_short_hash_that_is_not_hex_is_not_copied() {
        let mut state = copy_ready();
        state.commits.list.rows[1].short_oid = "-x".into();
        assert_eq!(
            state.update(AppIntent::Commits(CommitsIntent::Copy(
                CopyIntent::CopyText(CopyWhat::ShortHash)
            ))),
            vec![]
        );
        assert_eq!(state.notice, Some("Not a commit id git can use".into()));
    }

    #[test]
    fn with_no_commit_selected_nothing_is_copied() {
        let mut state = AppState::new(Context::Workspace);
        for what in [CopyWhat::Hash, CopyWhat::Message] {
            assert_eq!(
                state.update(AppIntent::Commits(CommitsIntent::Copy(
                    CopyIntent::CopyText(what)
                ))),
                vec![]
            );
            assert_eq!(state.notice, Some("No commit selected".into()));
        }
        for item in menu::items(MenuId::Copy) {
            assert_eq!((item.disabled)(&state), Some("No commit selected".into()));
            // A forge URL needs a remote on a forge host as well.
            let ready = (item.disabled)(&copy_ready());
            if item.intent
                == AppIntent::Commits(CommitsIntent::Copy(CopyIntent::CopyText(
                    CopyWhat::ForgeUrl,
                )))
            {
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
        let busy = state.busy_count();
        let first = state.update(AppIntent::Commits(CommitsIntent::Copy(
            CopyIntent::CopyText(CopyWhat::Message),
        )));
        let [Effect::LoadCommitMessage { tag: old, oid }] = first.as_slice() else {
            panic!("expected a message load, got {first:?}");
        };
        assert_eq!(oid, &cid(COPIED_OID));
        assert!(old.user);
        assert_eq!(state.busy_count(), busy + 1);
        let second = state.update(AppIntent::Commits(CommitsIntent::Copy(
            CopyIntent::CopyText(CopyWhat::Message),
        )));
        let [Effect::LoadCommitMessage { tag: new, .. }] = second.as_slice() else {
            panic!("expected a message load, got {second:?}");
        };
        assert_eq!(state.busy_count(), busy + 2);

        let stale = state.update(AppIntent::Commits(CommitsIntent::Copy(
            CopyIntent::MessageLoaded {
                tag: *old,
                oid: cid(COPIED_OID),
                result: Ok("old".into()),
            },
        )));
        assert_eq!(stale, vec![]);
        assert_eq!(state.busy_count(), busy + 1);
        let effects = state.update(AppIntent::Commits(CommitsIntent::Copy(
            CopyIntent::MessageLoaded {
                tag: *new,
                oid: cid(COPIED_OID),
                result: Ok("subj\n\nbody".into()),
            },
        )));
        assert_eq!(effects, [Effect::Clipboard("subj\n\nbody".into())]);
        assert_eq!(state.notice.as_deref(), Some("Copied message: subj  body"));
        assert_eq!(
            (state.busy_count(), state.pending_copy.clone()),
            (busy, None)
        );
    }

    #[test]
    fn a_failed_read_or_copy_shows_an_error() {
        let mut state = copy_ready();
        let effects = state.update(AppIntent::Commits(CommitsIntent::Copy(
            CopyIntent::CopyText(CopyWhat::Message),
        )));
        let [Effect::LoadCommitMessage { tag, oid }] = effects.as_slice() else {
            panic!("expected a message load, got {effects:?}");
        };
        let failed = AppIntent::Commits(CommitsIntent::Copy(CopyIntent::MessageLoaded {
            tag: *tag,
            oid: oid.clone(),
            result: Err("bad object".into()),
        }));
        assert_eq!(state.update(failed), vec![]);
        assert!(matches!(state.popups.last(), Some(Popup::Error(e)) if e.message == "bad object"));

        let mut state = copy_ready();
        state.update(AppIntent::Commits(CommitsIntent::Copy(
            CopyIntent::CopyText(CopyWhat::Hash),
        )));
        state.update(AppIntent::CopyFailed(clipboard::TOO_LONG.into()));
        assert_eq!(state.notice, None);
        assert!(
            matches!(state.popups.last(), Some(Popup::Error(e)) if e.message == clipboard::TOO_LONG)
        );

        // A clipboard program's failure arrives after "Copied …" is shown.
        let mut state = copy_ready();
        state.update(AppIntent::Commits(CommitsIntent::Copy(
            CopyIntent::CopyText(CopyWhat::Hash),
        )));
        assert!(state.notice.is_some());
        let late = "false failed: exit status: 1. Check clipboard.command in the global config.";
        state.update(AppIntent::CopyFailed(late.into()));
        assert_eq!(state.notice, None);
        assert!(matches!(
            state.popups.last(),
            Some(Popup::Error(e)) if e.title == "Could not copy" && e.message == late
        ));
    }

    // --- Stash failures and the repository switch ---

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
        state.stash_draft = Some(crate::features::stash::StashDraft {
            mode: StashMode::All,
            paths: vec![],
            restore: vec![],
            unstage: vec![],
            from_range: false,
        });
        state.switch_repo();
        assert_eq!(state.stash, crate::features::stash::StashList::default());
        assert_eq!(state.stash_draft, None);
    }

    // --- A Files range across a status reload ---

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
        assert_eq!(state.files.range(), [0, 1, 2]);
        reload(
            &mut state,
            vec![changed("a.rs", '.', 'M'), changed("c.rs", '.', 'M')],
        );
        assert_eq!(state.files.range_anchor, None);
        assert_eq!(state.notice, Some(RANGE_FILES_CHANGED.into()));
    }

    #[test]
    fn a_reload_that_drops_the_ranges_first_file_clears_it_and_says_so() {
        let files = vec![changed("a.rs", '.', 'M'), changed("b.rs", '.', 'M')];
        let (mut state, _) = with_files(files, 24);
        press_char(&mut state, 'V');
        press_char(&mut state, 'j');
        reload(&mut state, vec![changed("b.rs", '.', 'M')]);
        assert_eq!(state.files.range_anchor, None);
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
        assert_eq!(state.files.range_anchor, None);
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
        assert_eq!(state.files.range_anchor, Some("a.rs".into()));
        assert_eq!(state.files.range(), [0, 1]);
        assert_eq!(state.notice, None);
    }

    // --- Start-up facts ---

    #[test]
    fn with_git_version_records_what_main_found() {
        let state = AppState::new(Context::Workspace).with_git_version(version(38));
        assert!(state.git_has(kika_git::version::UPDATE_REFS));
        assert!(!AppState::new(Context::Workspace).git_has(kika_git::version::BREAK));
    }

    // --- Closing a popup: each feature forgets its own draft ---

    /// A workspace holding one draft of every feature that keeps one,
    /// over the Worktrees popup, whose list keeps the new worktree's
    /// branch.
    fn with_every_draft() -> AppState {
        use crate::features::branches::remotes::RemotePending;
        use crate::features::branches::tags::PendingTag;
        use crate::features::custom_commands::{CustomCommand, OutputMode, PendingCommand, Target};
        use crate::features::files::IgnoreDraft;
        use crate::features::pull_requests::PrDraft;
        use crate::features::stash::{StashDraft, StashMode, StashRef};
        use crate::features::worktrees::WorktreeList;
        use kika_forge::ForgeRepo;
        let mut state = AppState::new(Context::Workspace);
        state.pending_command = Some(PendingCommand {
            command: CustomCommand {
                scope: KeyScope::Files,
                keys: vec![],
                description: "Copy".into(),
                argv: vec![],
                prompts: vec!["To".into()],
                output: OutputMode::None,
            },
            target: Target {
                path: "a.rs".into(),
                line: None,
                commit: None,
            },
            answers: vec![],
        });
        state.branches.pending = Some("topic".into());
        state.branches.remotes.pending = Some(RemotePending::Rename("origin".into()));
        state.branches.pending_tag = Some(PendingTag {
            target: cid("c1"),
            annotated: true,
            name: None,
        });
        state.stash_draft = Some(StashDraft {
            mode: StashMode::All,
            paths: vec![],
            restore: vec![],
            unstage: vec![],
            from_range: false,
        });
        state.stash.pending = Some(StashRef {
            index: 0,
            oid: cid("c1"),
        });
        state.branches.pending_head = kika_git::commit_files::CommitId::parse("c0");
        let mut worktrees = WorktreeList::loading(0);
        worktrees.pending_branch = Some("topic".into());
        state.popups = vec![Popup::Worktrees(worktrees)];
        state.ignore_draft = Some(IgnoreDraft {
            shown: "*.log".into(),
            bytes: b"*.log".to_vec(),
        });
        state.review.check_urls = vec!["https://ci.example/1".into()];
        let repo = ForgeRepo {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "widgets".into(),
        };
        state.pr_draft = Some(PrDraft {
            branch: "topic".into(),
            head_remote: "origin".into(),
            head_repo: repo.clone(),
            head: "topic".into(),
            targets: vec![],
            target: Some(("origin".into(), repo)),
            bases: vec!["main".into()],
            base: Some("main".into()),
            title: "Add login".into(),
        });
        state
    }

    /// Which drafts `state` still holds, by name.
    fn drafts_kept(state: &AppState) -> Vec<&'static str> {
        let worktree_branch = state
            .popups
            .iter()
            .any(|popup| matches!(popup, Popup::Worktrees(list) if list.pending_branch.is_some()));
        [
            ("command", state.pending_command.is_some()),
            ("branch", state.branches.pending.is_some()),
            ("branch at head", state.branches.pending_head.is_some()),
            ("remote", state.branches.remotes.pending.is_some()),
            ("tag", state.branches.pending_tag.is_some()),
            ("stash message", state.stash_draft.is_some()),
            ("stash rename", state.stash.pending.is_some()),
            ("worktree", worktree_branch),
            ("ignore", state.ignore_draft.is_some()),
            ("check urls", !state.review.check_urls.is_empty()),
            ("pull request", state.pr_draft.is_some()),
        ]
        .into_iter()
        .filter_map(|(name, kept)| kept.then_some(name))
        .collect()
    }

    fn prompt(kind: PromptKind) -> Popup {
        Popup::Prompt {
            kind,
            input: LineInput::default(),
        }
    }

    fn pick(purpose: crate::pick_list::PickPurpose) -> Popup {
        Popup::PickList(crate::pick_list::PickList::new(
            purpose,
            vec!["a".into()],
            0,
        ))
    }

    #[test]
    fn closing_a_popup_forgets_only_the_draft_its_feature_kept_for_it() {
        use crate::pick_list::PickPurpose;
        let every = drafts_kept(&with_every_draft());
        assert_eq!(every.len(), 11, "{every:?}");
        let cases = [
            (prompt(PromptKind::Command), Some("command")),
            (prompt(PromptKind::NewBranch), Some("branch")),
            (prompt(PromptKind::NewBranchAtHead), Some("branch at head")),
            (prompt(PromptKind::RenameBranch), Some("branch")),
            (prompt(PromptKind::SetUpstream), Some("branch")),
            (prompt(PromptKind::TrackBranch), Some("remote")),
            (prompt(PromptKind::RemoteName), Some("remote")),
            (prompt(PromptKind::RemoteUrl), Some("remote")),
            (prompt(PromptKind::RenameRemote), Some("remote")),
            (prompt(PromptKind::TagName), Some("tag")),
            (prompt(PromptKind::TagMessage), Some("tag")),
            (prompt(PromptKind::StashMessage), Some("stash message")),
            (prompt(PromptKind::RenameStash), Some("stash rename")),
            (prompt(PromptKind::WorktreePath), Some("worktree")),
            (prompt(PromptKind::IgnorePattern), Some("ignore")),
            (pick(PickPurpose::CheckDetails), Some("check urls")),
            (pick(PickPurpose::TargetRemote), Some("pull request")),
            (pick(PickPurpose::BaseBranch), Some("pull request")),
            (pick(PickPurpose::MergeMethod), Some("pull request")),
            (pick(PickPurpose::ReviewCommit), None),
            (prompt(PromptKind::PullRequestTitle), Some("pull request")),
            (
                Popup::Question(crate::features::pull_requests::create_question(
                    &with_every_draft(),
                )),
                Some("pull request"),
            ),
            // Popups no draft belongs to.
            (prompt(PromptKind::GoToFile), None),
            (Popup::Question(quit_question()), None),
            (
                Popup::Menu {
                    id: MenuId::Actions,
                    cursor: 0,
                },
                None,
            ),
        ];
        for (popup, forgotten) in cases {
            let mut state = with_every_draft();
            let under = state.popups.clone();
            state.popups.push(popup.clone());
            assert_eq!(state.update(AppIntent::PopPopup), vec![], "{popup:?}");
            assert_eq!(state.popups.len(), under.len(), "{popup:?}");
            let expected: Vec<_> = every
                .iter()
                .copied()
                .filter(|name| Some(*name) != forgotten)
                .collect();
            assert_eq!(drafts_kept(&state), expected, "{popup:?}");
        }
    }

    #[test]
    fn closing_the_last_popup_leaves_none_and_returns_no_effects() {
        let mut state = with_every_draft();
        state.popups = vec![prompt(PromptKind::NewBranch)];
        assert_eq!(state.update(AppIntent::PopPopup), vec![]);
        assert_eq!(state.popups, []);
        assert_eq!(state.branches.pending, None);
        // Nothing to close: nothing changes.
        assert_eq!(state.update(AppIntent::PopPopup), vec![]);
        assert_eq!(state.popups, []);
    }

    #[test]
    fn no_on_the_create_question_closes_it_and_forgets_the_draft_yes_keeps_both() {
        let mut state = with_every_draft();
        state.popups = vec![Popup::Question(
            crate::features::pull_requests::create_question(&state),
        )];
        let asked = state.popups.clone();
        assert_eq!(state.update(AppIntent::ConfirmYes), vec![]);
        assert_eq!(state.popups, asked);
        assert!(state.pr_draft.is_some());
        assert_eq!(state.update(AppIntent::ConfirmNo), vec![]);
        assert_eq!(state.popups, []);
        assert_eq!(state.pr_draft, None);
        // Another feature's drafts stay.
        assert!(state.branches.pending.is_some());
    }

    #[test]
    fn no_binding_in_the_create_question_sends_pop_popup() {
        // Its `n` and Esc forget the draft through `ConfirmNo`; nothing
        // in its scope closes it any other way.
        for binding in crate::keymap::registry() {
            if binding.scope == KeyScope::CreatePullRequest {
                assert_ne!(binding.intent, AppIntent::PopPopup, "{}", binding.id);
            }
        }
    }

    /// Every question whose yes is an intent, with that intent and
    /// whether Enter answers it too.
    fn moved_questions(state: &AppState) -> Vec<(ConfirmQuestion, AppIntent, bool)> {
        use crate::features::commits::custom_patch::PatchAction;
        use crate::features::commits::rewrite::{autosquash_question, drop_range_question};
        use crate::features::conflicts::stage_resolved_question;
        use crate::features::pull_requests::create_question;
        use crate::features::sync::force_push_question;
        let ids = vec![cid("c0"), cid("c1")];
        vec![
            (quit_question(), AppIntent::Quit, true),
            (
                force_push_question(),
                AppIntent::Sync(SyncIntent::ForcePush),
                true,
            ),
            (
                stage_resolved_question(),
                AppIntent::Conflicts(ConflictsIntent::StageResolved),
                true,
            ),
            (
                abort_question(Operation::Merge).unwrap(),
                AppIntent::Operation(OperationIntent::ConfirmAbort(Operation::Merge)),
                false,
            ),
            (
                state.bisect_reset_question(),
                AppIntent::Commits(CommitsIntent::Bisect(BisectIntent::ConfirmReset)),
                false,
            ),
            (
                autosquash_question(),
                AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::ConfirmAutosquash)),
                false,
            ),
            (
                drop_range_question(ids.clone()),
                AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::ConfirmDropRange(ids))),
                false,
            ),
            (
                state.patch_action_question(PatchAction::Remove),
                AppIntent::Commits(CommitsIntent::CustomPatch(CustomPatchIntent::Confirm(
                    PatchAction::Remove,
                ))),
                false,
            ),
            (
                create_question(state),
                AppIntent::PullRequests(PullRequestsIntent::ConfirmCreate),
                false,
            ),
        ]
    }

    #[test]
    fn each_moved_question_answers_with_its_keys_and_sends_its_intent() {
        let base = status_focused(&[Operation::Merge]);
        for (question, intent, enter) in moved_questions(&base) {
            let name = question.title.clone();
            assert_eq!(question.answer == Answer::YOrEnter, enter, "{name}");
            assert_eq!(
                question.then,
                Then::Intent(Box::new(intent.clone())),
                "{name}"
            );
            let yes_keys = if enter {
                vec![KeyCode::Char('y'), KeyCode::Enter]
            } else {
                vec![KeyCode::Char('y')]
            };
            for key in yes_keys {
                // What the intent alone does, from the same state.
                let mut expected = base.clone();
                let want = expected.update(intent.clone());
                let mut state = base.clone();
                state.popups = vec![Popup::Question(question.clone())];
                assert_eq!(state.key_focus(), question.scope, "{name}");
                assert_eq!(press(&mut state, key), want, "{name} {key:?}");
                assert_eq!(state.popups, expected.popups, "{name} {key:?}");
                assert_eq!(state.should_quit, expected.should_quit, "{name} {key:?}");
                assert_eq!(state.notice, expected.notice, "{name} {key:?}");
                assert_eq!(state.held_seq(), expected.held_seq(), "{name} {key:?}");
            }
            if !enter {
                let mut state = base.clone();
                state.popups = vec![Popup::Question(question.clone())];
                let asked = state.popups.clone();
                assert_eq!(press(&mut state, KeyCode::Enter), vec![], "{name}");
                assert_eq!(state.popups, asked, "{name}");
                assert_eq!(state.held_seq(), None, "{name}");
            }
            for no in [KeyCode::Char('n'), KeyCode::Esc] {
                let mut state = base.clone();
                state.popups = vec![Popup::Question(question.clone())];
                assert_eq!(press(&mut state, no), vec![], "{name} {no:?}");
                assert_eq!(state.popups, [], "{name} {no:?}");
                assert_eq!(state.held_seq(), None, "{name} {no:?}");
                assert!(!state.should_quit, "{name} {no:?}");
            }
        }
    }

    #[test]
    fn the_menu_asks_to_quit_and_to_reset_the_bisect() {
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(state.update(AppIntent::AskQuit), vec![]);
        assert_eq!(state.popups, [Popup::Question(quit_question())]);

        let mut state = testing_c1(&[Operation::Bisect]);
        assert_eq!(
            state.update(AppIntent::Commits(CommitsIntent::Bisect(
                BisectIntent::AskReset
            ))),
            vec![]
        );
        assert_eq!(
            state.popups,
            [Popup::Question(state.bisect_reset_question())]
        );
        assert_eq!(state.key_focus(), KeyScope::AbortConfirm);
    }
}
