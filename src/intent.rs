use std::collections::HashMap;
use std::path::PathBuf;

use crossterm::event::{Event, KeyEventKind};

use crate::ansi::StyledRow;
use crate::clipboard::CopyWhat;
use crate::config::ConfigError;
use crate::conflict::Choice;
use crate::custom_command::{CommandFailure, OutputMode};
use crate::custom_patch::{PatchAction, PatchChoice};
use crate::effect::Tag;
use crate::external_diff::ExternalRenderer;
use crate::forge::review::{PrDetail, Queues, ReviewError};
use crate::forge::{CreateError, ForgeRepo, ForgeStatus, PrBadge};
use crate::git::askpass::Secret;
use crate::git::bisect::BisectMark;
use crate::git::blame::BlameLine;
use crate::git::branch_tips::Tip;
use crate::git::branches::{Branch, BranchName, LogRow};
use crate::git::commit_files::{CommitFile, CommitId, CommitRange};
use crate::git::diff::{DiffChunk, DiffLine};
use crate::git::fixup_base::FixupBase;
use crate::git::log::{LogPage, Markers};
use crate::git::ops::{GitOp, MergeStrategy, ResetMode};
use crate::git::patch::FilePatch;
use crate::git::path::RepoPath;
use crate::git::prompts::PromptFacts;
use crate::git::rebase_plan::{Direction, Edit};
use crate::git::recording::CommandEvent;
use crate::git::reflog::Reflog;
use crate::git::refs::RefsSnapshot;
use crate::git::remote::{RemoteFailure, RemoteOp};
use crate::git::remotes::Remote;
use crate::git::repo_state::Operation;
use crate::git::stash::{StashEntry, StashFile};
use crate::git::status::RepoStatus;
use crate::git::submodule::Submodule;
use crate::git::tags::TagRef;
use crate::git::worktrees::Worktree;
use crate::gitignore::IgnoreAdded;
use crate::keymap::{KeyPress, Keymap};
use crate::layout::Panel;
use crate::list_filter::LogFilterField;
use crate::menu::MenuId;
use crate::popup::{ConfirmKind, PromptKind};
use crate::rebase_todo::TodoFile;
use crate::watch::WatchEvent;

/// Everything that can change the model: user input and async task results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppIntent {
    /// A raw key press, resolved against the keymap in `update`.
    Key(KeyPress),
    Quit,
    OpenHelp,
    CloseHelp,
    /// Scroll the help list, which can be taller than the screen.
    ScrollHelp(Scroll),
    Resize {
        width: u16,
        height: u16,
    },
    /// Reload `git status`. Also issued once at startup.
    RefreshStatus,
    /// A status load finished; `tag` says which one.
    StatusLoaded {
        tag: Tag,
        result: Result<RepoStatus, String>,
    },
    /// Move the file selection down or up.
    SelectNext,
    SelectPrev,
    ScrollDiff(Scroll),
    /// A chunk of the diff stream for `tag`. A stream reports more than once.
    DiffLoaded {
        tag: Tag,
        result: Result<DiffChunk, String>,
    },
    /// The diff for `tag` has taken long enough to show "Loading…".
    DiffSlow {
        tag: Tag,
    },
    /// Give the Diff pane focus, to work on the selected file's hunks.
    FocusDiff,
    /// Back to the side panel last focused, which stayed expanded.
    FocusLeft,
    /// Give a panel focus: a side panel expands; the diff works on hunks.
    FocusPanel(Panel),
    /// Focus the next panel in screen order, or the previous with `back`.
    CycleFocus {
        back: bool,
    },
    /// Zoom the focused panel: normal, half, full, normal again.
    CycleScreenMode,
    /// Move the hunk cursor down or up.
    NextHunk,
    PrevHunk,
    /// Stage the selected hunk, or unstage it in the staged view.
    ToggleHunkStage,
    /// Start or stop picking changed lines inside the selected hunk.
    EnterLineMode,
    ExitLineMode,
    /// Move the line cursor down or up; with `extend`, grow the range.
    NextLine {
        extend: bool,
    },
    PrevLine {
        extend: bool,
    },
    /// Stage the selected lines, or unstage them in the staged view.
    ToggleLinesStage,
    /// Switch the focused Diff pane between unstaged and staged changes.
    ToggleDiffSide,
    /// Show one more unchanged line around each change in the diff.
    MoreContext,
    /// Show one fewer unchanged line around each change in the diff.
    LessContext,
    /// Hide or show changes that only touch whitespace.
    ToggleWhitespace,
    /// A whole-file patch load finished; `tag` says which one.
    PatchLoaded {
        tag: Tag,
        result: Result<FilePatch, String>,
    },
    /// Stage the selected file, or unstage it when it is fully staged.
    ToggleStage,
    /// Stage everything, or unstage everything when nothing is left to stage.
    ToggleStageAll,
    /// Ask to throw away the unstaged work under the cursor: the selected
    /// file, hunk or lines, by focus. The answer is `ConfirmYes`/`ConfirmNo`.
    Discard,
    /// Open the commit popup from the Files panel.
    OpenCommit,
    /// Nothing was staged: yes, stage everything and commit.
    ConfirmStageAll,
    /// Close the commit popup; the index is left as it is. A typed
    /// message is only thrown away after `DiscardCommit`.
    CancelCommit,
    /// Yes, throw the typed message away.
    DiscardCommit,
    /// Back to the message instead of discarding it.
    ResumeCommit,
    /// Commit with the typed message.
    SubmitCommit,
    /// Ctrl+C in the commit popup: cancel while typing (asking before a
    /// typed message is lost), quit while git runs.
    InterruptCommit,
    /// Move the cursor between the subject and the body.
    ToggleCommitField,
    /// Close the commit error and go back to the kept message.
    DismissCommitError,
    /// Open the commit popup to amend the last commit, its message filled in.
    OpenAmend,
    /// `HEAD`'s message was read for amending; `tag` says which load.
    HeadMessageLoaded {
        tag: Tag,
        result: Result<String, String>,
    },
    /// Turn skipping the commit hooks on or off for this commit.
    ToggleNoVerify,
    /// Write the message in the user's editor, then commit with it.
    EditCommitExternally,
    /// The editor opened by `EditCommitExternally` has closed: the message
    /// to commit, `None` if the user emptied it.
    CommitMessageEdited {
        result: Result<Option<String>, String>,
    },
    /// The config and hooks a commit depends on were read; `facts` is
    /// `None` when they could not be. Decides where the commit runs.
    CommitProbed {
        tag: Tag,
        op: GitOp,
        facts: Option<PromptFacts>,
    },
    /// A Git operation finished, successfully or not.
    OpFinished {
        tag: Tag,
        op: GitOp,
        result: Result<(), String>,
    },
    /// Push a menu, over any menu already open.
    OpenMenu(MenuId),
    /// Move the top menu's cursor down or up.
    MenuNext,
    MenuPrev,
    /// Run the item under the top menu's cursor.
    MenuSelect,
    /// Ask a yes/no question; the answer comes back as the kind's intent.
    Ask(ConfirmKind),
    ConfirmYes,
    ConfirmNo,
    /// Ask for a line of text; the text comes back as the kind's intent.
    OpenPrompt(PromptKind),
    SubmitPrompt,
    /// Select the first file whose path contains this text, ignoring case.
    GoToFile(String),
    /// Close the top popup: back in a menu, cancel in a prompt, dismiss an error.
    PopPopup,
    /// Open the selected file in the user's editor, at the selected line
    /// when there is one.
    OpenInEditor,
    /// Edit the selected hunk in the user's editor, then stage the result.
    EditHunk,
    /// The editor opened by `OpenInEditor` has closed.
    EditorClosed {
        result: Result<(), String>,
    },
    /// The hunk edit finished: the patch to stage, `None` if the user
    /// cancelled.
    HunkEdited {
        result: Result<Option<Vec<u8>>, String>,
    },
    /// Time to look for changes made outside Kika.
    Watch(WatchEvent),
    /// A refs snapshot finished; `tag` says which one.
    RefsLoaded {
        tag: Tag,
        result: Result<RefsSnapshot, String>,
    },
    /// A background fetch finished.
    FetchFinished {
        tag: Tag,
        result: Result<(), String>,
    },
    /// A page of the log finished; `tag` says which load, `skip` which
    /// page. `Ok` also carries the unix time it was read at.
    CommitsLoaded {
        tag: Tag,
        skip: usize,
        result: Result<(LogPage, i64), String>,
    },
    /// The push and merge markers finished; `tag` says which load.
    MarkersLoaded {
        tag: Tag,
        result: Result<Markers, String>,
    },
    /// Move the commit cursor down or up.
    CommitNext,
    CommitPrev,
    /// Show the selected commit's files in place of the log.
    OpenCommitFiles,
    /// Back from a commit's files to the log, at the same commit.
    CloseCommitFiles,
    /// Move the cursor in a commit's files down or up.
    CommitFileNext,
    CommitFilePrev,
    /// A commit's files were read; `tag` says which load.
    CommitFilesLoaded {
        tag: Tag,
        result: Result<Vec<CommitFile>, String>,
    },
    /// The stash list was read; `tag` says which load.
    StashesLoaded {
        tag: Tag,
        result: Result<Vec<StashEntry>, String>,
    },
    /// Move the stash cursor down or up.
    StashNext,
    StashPrev,
    /// Show the selected stash's files in place of the list.
    OpenStashFiles,
    /// Back from a stash's files to the list, at the same entry.
    CloseStashFiles,
    /// Move the cursor in a stash's files down or up.
    StashFileNext,
    StashFilePrev,
    /// A stash's files were read; `tag` says which load.
    StashFilesLoaded {
        tag: Tag,
        result: Result<Vec<StashFile>, String>,
    },
    /// Apply the selected stash and keep it.
    ApplyStash,
    /// Apply the selected stash and drop it.
    PopStash,
    /// Ask before dropping the selected stash.
    DropStash,
    /// Drop the stash the question was about.
    ConfirmDropStash,
    /// Ask for a new message for the selected stash.
    RenameStash,
    /// The new message typed for the stash the prompt was about.
    RenameStashTo(String),
    /// Ask for a message, then stash every tracked change.
    StashAll,
    /// Ask for a message, then stash every change, untracked files too.
    StashIncludingUntracked,
    /// Ask for a message, then stash only the staged changes.
    StashStaged,
    /// Ask for a message, then stash only the selected file.
    StashFile,
    /// The message typed for the stash being made; empty for git's own.
    StashMessageAnswered(String),
    /// Open the list of submodules and load it.
    OpenSubmodules,
    /// The submodule list was read; `tag` says which load.
    SubmodulesLoaded {
        tag: Tag,
        result: Result<Vec<Submodule>, String>,
    },
    /// Move the submodule cursor down or up.
    SubmoduleNext,
    SubmodulePrev,
    /// Initialize or update the submodule under the cursor.
    InitSubmodule,
    UpdateSubmodule,
    /// Work in the submodule under the cursor as if Kika started there.
    EnterSubmodule,
    /// Back to the repository the current one was entered from.
    LeaveSubmodule,
    /// Open the list of worktrees and load it.
    OpenWorktrees,
    /// The worktree list was read; `tag` says which load.
    WorktreesLoaded {
        tag: Tag,
        result: Result<Vec<Worktree>, String>,
    },
    /// Move the worktree cursor down or up.
    WorktreeNext,
    WorktreePrev,
    /// Switch Kika to the worktree under the cursor.
    OpenWorktree,
    /// Ask where to add a worktree for the Branches panel's local branch.
    AddWorktree,
    /// The typed path for the worktree `AddWorktree` asked about.
    AddWorktreeAt(String),
    /// A worktree add finished: where it was made, or why not.
    WorktreeAdded {
        tag: Tag,
        result: Result<PathBuf, String>,
    },
    /// Ask before removing the worktree under the cursor.
    RemoveWorktree,
    /// Remove the worktree the question named.
    ConfirmRemoveWorktree,
    /// A worktree remove finished.
    WorktreeRemoved {
        tag: Tag,
        result: Result<(), String>,
    },
    /// A git command started or ended; the runner reports every one.
    CommandLogged(CommandEvent),
    /// Show the git commands Kika ran this session.
    OpenCommandLog,
    /// Move the Command log cursor a row or a page down or up.
    CommandLogNext,
    CommandLogPrev,
    CommandLogPageDown,
    CommandLogPageUp,
    /// Blame the selected file: who last changed each line, and when.
    OpenBlame,
    /// A blame load finished; `tag` says which one.
    BlameLoaded {
        tag: Tag,
        result: Result<Vec<BlameLine>, String>,
    },
    /// Move the blame cursor a line or a page down or up.
    BlameNext,
    BlamePrev,
    BlamePageDown,
    BlamePageUp,
    /// Show the commit that last changed the line under the blame cursor.
    OpenBlameCommit,
    /// A commit load finished; `tag` says which one.
    CommitLoaded {
        tag: Tag,
        result: Result<Vec<DiffLine>, String>,
    },
    /// Back from the commit to the blame, at the same line.
    CloseBlameCommit,
    ScrollCommit(Scroll),
    /// Back from the blame to the Files panel.
    CloseBlame,
    /// The conflicted file the view opened on was read: its bytes and its
    /// marker size, or why not. `tag` says which load.
    ConflictLoaded {
        tag: Tag,
        path: RepoPath,
        result: Result<(Vec<u8>, usize), String>,
    },
    /// Move the conflict view's cursor to the next or previous block.
    ConflictNext,
    ConflictPrev,
    /// Keep this side of the selected block, and write the file.
    PickConflict(Choice),
    /// A resolved file was written: what is now on disk, or why not.
    ResolvedWritten {
        tag: Tag,
        path: RepoPath,
        result: Result<Vec<u8>, String>,
    },
    /// Stage the file the conflict view resolved, as the question asked.
    StageResolved,
    /// Resolve the selected conflicted file in the user's merge tool.
    OpenMergeTool,
    /// Show the diff through the next configured renderer, or the
    /// built-in view after the last.
    CycleRenderer,
    /// The renderers configured for this session, found at boot.
    RenderersFound(Vec<ExternalRenderer>),
    /// A renderer's rows for the shown diff; `tag` says which render.
    DiffRendered {
        tag: Tag,
        result: Result<Vec<StyledRow>, String>,
    },
    /// The first intent: record the repository Kika started in and load
    /// it, or, outside one, welcome the user with the recent list.
    Start,
    /// Show the recent repositories, to switch to one.
    OpenRecent,
    /// The recent list as now recorded on disk, answering request `seq`.
    RecentLoaded {
        seq: u64,
        list: Vec<PathBuf>,
    },
    /// Move the recent list's cursor down or up.
    RecentNext,
    RecentPrev,
    /// Open the row under the recent list's cursor.
    RecentSelect,
    /// Switch the whole workspace to the repository at this top level.
    OpenRepo(PathBuf),
    /// Make the launch directory a repository.
    InitRepo,
    /// `git init` finished: the new top level, or why it failed.
    RepoInitialised {
        tag: Tag,
        result: Result<PathBuf, String>,
    },
    /// The answer to a custom command's current prompt.
    CommandPromptAnswered(String),
    /// A custom command finished. `Ok` holds the captured output, empty
    /// unless `output` is `Popup`; `Err` says how it failed. Text is
    /// terminal-safe.
    CommandFinished {
        title: String,
        output: OutputMode,
        result: Result<String, CommandFailure>,
    },
    /// Stop the captured command that is running, if one is.
    CancelCommand,
    /// The branch list was read; `tag` says which load.
    BranchesLoaded {
        tag: Tag,
        result: Result<Vec<Branch>, String>,
    },
    /// The log of the full ref `refname` was read; `tag` says which load.
    BranchLogLoaded {
        tag: Tag,
        refname: Vec<u8>,
        result: Result<Vec<LogRow>, String>,
    },
    /// The remotes and their branches were read; `tag` says which load.
    RemotesLoaded {
        tag: Tag,
        result: Result<Vec<Remote>, String>,
    },
    /// Show the Branches panel's next tab: local branches, remotes, tags.
    SwitchBranchesTab,
    /// Show the Branches panel's previous tab.
    PrevBranchesTab,
    /// The tags were read; `tag` says which load.
    TagsLoaded {
        tag: Tag,
        result: Result<Vec<TagRef>, String>,
    },
    /// Move the Tags tab's selection down or up.
    TagNext,
    TagPrev,
    /// Ask for a name, to tag the selected commit with a lightweight tag.
    NewTag,
    /// Ask for a name and then a message, to tag the selected commit with
    /// an annotated tag.
    NewAnnotatedTag,
    /// The tag's name, typed into the `TagName` prompt.
    TagNamed(String),
    /// The annotated tag's message, typed into the `TagMessage` prompt.
    TagMessaged(String),
    /// Ask before deleting the selected tag.
    DeleteTag,
    /// Delete the tag the open question names.
    ConfirmDeleteTag,
    /// Push the selected tag to the current branch's remote, or the only
    /// one.
    PushTag,
    /// Move the Remotes tab's selection down or up, over remote and
    /// branch rows alike.
    RemoteNext,
    RemotePrev,
    /// Ask for a local name, to check out the selected remote branch as a
    /// new branch tracking it.
    TrackRemoteBranch,
    /// The local name, typed into the `TrackBranch` prompt.
    TrackRemoteBranchNamed(String),
    /// Ask for a new remote's name, then its URL.
    AddRemote,
    /// The new remote's name, typed into the `RemoteName` prompt.
    AddRemoteNamed(String),
    /// The new remote's URL, typed into the `RemoteUrl` prompt.
    AddRemoteUrl(String),
    /// Ask for a new name for the selected remote, the old one filled in.
    RenameRemote,
    /// The new name, typed into the `RenameRemote` prompt.
    RenameRemoteTo(String),
    /// Ask before removing the selected remote.
    RemoveRemote,
    /// Remove the remote the open question names.
    ConfirmRemoveRemote,
    /// Ask which remote branch the selected local branch should track.
    SetUpstream,
    /// The upstream, as `remote/branch`, typed into the `SetUpstream` prompt.
    SetUpstreamTo(String),
    /// Stop the selected local branch tracking its upstream.
    UnsetUpstream,
    /// Move the branch selection down or up.
    BranchNext,
    BranchPrev,
    /// Switch to the selected branch.
    CheckoutBranch,
    /// Ask for a name, to make a branch at the selected one.
    NewBranch,
    /// Ask for a new name for the selected branch, the old one filled in.
    RenameBranch,
    /// Ask before deleting the selected branch.
    DeleteBranch,
    /// The new branch's name, typed into the `NewBranch` prompt.
    CreateBranchNamed(String),
    /// The new name, typed into the `RenameBranch` prompt.
    RenameBranchTo(String),
    /// Delete the branch the open question names: plainly, or with
    /// `force` when its commits are not merged.
    ConfirmDeleteBranch {
        force: bool,
    },
    /// Fetch every remote, pull into the current branch, or push it. Each
    /// reaches the network, so only a key the user pressed sends one.
    Fetch,
    Pull,
    Push,
    /// Push again with `--force-with-lease`, after a plain push was rejected.
    ForcePush,
    /// Git or ssh, running for the remote operation `tag`, asks something.
    /// `confirm` is a yes/no question. The prompt is terminal-safe.
    // Only the unix bridge asks.
    #[cfg_attr(not(unix), allow(dead_code))]
    CredentialAsked {
        tag: Tag,
        prompt: String,
        confirm: bool,
    },
    /// Answer with the text typed into the credential popup.
    SubmitCredential,
    /// The answer to the open credential question; `None` cancels it.
    CredentialAnswered(Option<Secret>),
    /// Ssh, for the remote operation `tag`, says something that needs no
    /// answer, such as asking to touch a security key. Terminal-safe.
    // Only the unix bridge passes these on.
    #[cfg_attr(not(unix), allow(dead_code))]
    RemoteNotice {
        tag: Tag,
        text: String,
    },
    /// A fetch, pull or push finished.
    RemoteFinished {
        tag: Tag,
        op: RemoteOp,
        result: Result<(), RemoteFailure>,
    },
    /// Time to draw the next frame of the remote operation's spinner.
    SpinnerTick,
    /// Merge the selected branch into the current one this way.
    Merge(MergeStrategy),
    /// Rebase the current branch onto the selected one.
    RebaseOnto,
    /// Go on with the merge, rebase, cherry-pick or revert in progress.
    ContinueOperation,
    /// Drop the commit the rebase, cherry-pick or revert stopped on and go on.
    SkipOperation,
    /// Ask before aborting the operation in progress.
    AbortOperation,
    /// Abort this operation, as the question asked.
    ConfirmAbort(Operation),
    /// Ask before checking out the selected commit as a detached `HEAD`.
    CheckoutCommit,
    /// Check out the commit the question was about.
    ConfirmCheckoutCommit,
    /// Make a commit that undoes the selected one.
    RevertCommit,
    /// Add the selected commit to the copied set, or take it out.
    CopyCommit,
    /// Empty the copied set.
    ClearCopied,
    /// Cherry-pick the copied commits onto the current branch.
    PasteCommits,
    /// Put the selected commit's hash, subject or message on the system
    /// clipboard.
    CopyCommitText(CopyWhat),
    /// A commit's full message was read to copy it; `tag` says which read.
    CommitMessageLoaded {
        tag: Tag,
        oid: CommitId,
        result: Result<String, String>,
    },
    /// The text could not be sent to the clipboard, and why.
    CopyFailed(String),
    /// Ask which pattern to add to `.gitignore` for the selected
    /// untracked file, offering the one that matches exactly it.
    OpenIgnore,
    /// The pattern the `.gitignore` prompt was accepted with.
    IgnorePatternAnswered(String),
    /// The pattern was added to `.gitignore`, or was there already, or
    /// why not. The text is the pattern as shown.
    IgnoreWritten {
        tag: Tag,
        result: Result<IgnoreAdded, String>,
    },
    /// Reset the current branch to the selected commit. A hard reset asks
    /// first.
    ResetTo(ResetMode),
    /// Reset hard, as the question asked.
    ConfirmResetHard,
    /// The read of the commits a detached `HEAD` would leave behind: which
    /// op was planned, from which `HEAD`, and what was found or why not.
    LeftBehindChecked {
        tag: Tag,
        op: GitOp,
        head: CommitId,
        result: Result<Vec<CommitId>, String>,
    },
    /// Leave the commits behind, as the question asked.
    ConfirmLeaveCommits,
    /// Rewrite the selected commit with git's interactive rebase, or
    /// change the selected todo row's action while a rebase is stopped.
    RewriteCommit(Edit),
    /// Move the selected commit or todo row up (newer) or down (older).
    MoveCommit(Direction),
    /// Commit the staged change as a `fixup!` of the selected commit.
    CreateFixup,
    /// Ask, then fold every `fixup!` commit into the commit it fixes.
    Autosquash,
    /// Autosquash, as the question asked.
    ConfirmAutosquash,
    /// Find which commit last touched the staged lines.
    FindFixupBase,
    /// The commits the staged lines come from, or why they are not known.
    FixupBaseFound {
        tag: Tag,
        result: Result<FixupBase, String>,
    },
    /// Pick the selected commit file whole into the custom patch, or drop
    /// it.
    PatchToggleFile,
    /// Show what can be done with the custom patch.
    OpenPatchMenu,
    /// A rewrite chosen from the patch menu: ask its message first, or
    /// ask whether to go ahead.
    ChoosePatchAction(PatchChoice),
    /// The new commit's message for "New commit after source", as typed.
    PatchCommitMessage(String),
    /// Run the rewrite, as the question asked.
    ConfirmPatchAction(PatchAction),
    /// Drop the custom patch. Only Kika's selection goes; the repository
    /// does not change.
    ResetPatch,
    /// Stop the rebase after the selected commit, or toggle the stop
    /// after the selected todo row.
    InsertBreak,
    /// The branch tips were read; `tag` says which load.
    BranchTipsLoaded {
        tag: Tag,
        result: Result<Vec<Tip>, String>,
    },
    /// The stopped rebase's todo list was read; `None` when there is none
    /// to edit, as in a rebase that is not interactive.
    RebaseTodoLoaded {
        tag: Tag,
        result: Result<Option<TodoFile>, String>,
    },
    /// An edited todo list was written, or why not.
    RebaseTodoWritten {
        tag: Tag,
        result: Result<(), String>,
    },
    /// Mark the selected commit in the Commits panel; with no bisect in
    /// progress, the mark starts one.
    BisectSelected(BisectMark),
    /// Mark the commit git checked out to test.
    BisectHead(BisectMark),
    /// End the bisect, as the question asked.
    ConfirmBisectReset,
    /// Show the Commits panel's other tab: the log or the reflog.
    SwitchCommitsTab,
    /// Move the Reflog tab's selection down or up.
    ReflogNext,
    ReflogPrev,
    /// `HEAD`'s reflog was read; `tag` says which load.
    ReflogLoaded {
        tag: Tag,
        result: Result<Reflog, String>,
    },
    /// Reverse the last action, or reapply the last one undone. Each reads
    /// the reflog afresh, then asks before anything moves.
    Undo,
    Redo,
    /// Run the undo or redo the open question describes.
    ConfirmTravel,
    /// The config files were read: the keys now in force, and the files
    /// that were ignored and why.
    ConfigLoaded {
        /// The top level the files were read for, so a result that
        /// arrives after a repository switch can be told apart.
        repo: Option<PathBuf>,
        keymap: Keymap,
        errors: Vec<ConfigError>,
    },
    /// Type into the focused list's filter, which narrows it live.
    OpenFilter,
    /// Type a text to search the focused Diff pane for.
    OpenSearch,
    /// Close the text input, keeping what it typed; a search goes to its
    /// current match.
    InputAccept,
    /// Close the text input and clear what it typed.
    InputCancel,
    /// Start a range selection at the cursor, or clear the one there is.
    ToggleRange,
    /// Clear the range, else the filter, else quit.
    ListEscape,
    /// Clear the search, else go back to the side panel.
    DiffEscape,
    /// Go to the next or previous match of the search.
    SearchNext,
    SearchPrev,
    /// Type the path or the author git narrows the log to.
    OpenLogFilter(LogFilterField),
    /// Show the selected file's history in the Commits panel.
    LogOfFile,
    /// Mark the selected commit or local branch to diff against, or clear
    /// the mark when it is already there.
    ToggleDiffMark,
    /// Compare the diff mark with the selected local branch.
    OpenDiff,
    /// Both ends of a comparison were read as commits; `tag` says which.
    DiffResolved {
        tag: Tag,
        result: Result<CommitRange, String>,
    },
    /// The remote is on none of the forge hosts: no badges.
    ForgeRepoNone {
        tag: Tag,
    },
    /// The forge repository found, and its cached badges; `None` is a miss.
    PullRequestsCached {
        tag: Tag,
        repo: ForgeRepo,
        badges: Option<HashMap<BranchName, PrBadge>>,
    },
    /// The forge answered, or why it could not. `limited_until` is set
    /// when GitHub rate limited the host: no call is made before it.
    PullRequestsFetched {
        tag: Tag,
        result: Result<HashMap<BranchName, PrBadge>, ForgeStatus>,
        limited_until: Option<i64>,
    },
    /// Open the selected branch's pull request, or its merge requests, in
    /// the browser the global config names.
    OpenForgeUrl,
    /// Put the selected branch's pull request or merge request list URL
    /// on the clipboard.
    CopyForgeUrl,
    /// The browser could not be started, and why.
    OpenUrlFailed(String),
    /// Start a pull request for the selected branch: pick the target
    /// remote and base branch where more than one fits, then the title.
    CreatePrStart,
    /// Move the pick list's cursor down or up.
    PickNext,
    PickPrev,
    /// Take the row under the pick list's cursor.
    PickSelect,
    /// The title the pull request prompt was accepted with.
    PullRequestTitled(String),
    /// Create the pull request the open question describes.
    ConfirmCreatePullRequest,
    /// GitHub answered a create: the new number and page, or why not.
    PullRequestCreated {
        tag: Tag,
        result: Result<(u64, String), CreateError>,
    },
    /// Leave the workspace for review mode and load its queues.
    EnterReview,
    /// Show the next review queue, or the previous one.
    NextQueue,
    PrevQueue,
    /// Open the pull request under the cursor in the queue.
    OpenPr,
    /// Back from a pull request to its queue, or from the queue to the
    /// workspace.
    CloseReviewDetail,
    /// Ask GitHub again for what review mode shows.
    RefreshReview,
    /// Open a failing check's page in the browser, picking one when
    /// several fail.
    OpenCheckDetails,
    /// Move the queue's cursor, or scroll the pull request, by a line.
    ReviewNext,
    ReviewPrev,
    /// The review queues, or why not; `repo` is the repository found,
    /// when one was.
    ReviewQueuesLoaded {
        tag: Tag,
        repo: Option<ForgeRepo>,
        result: Result<Queues, ReviewError>,
    },
    /// Pull request `number`, `None` when GitHub has no such one, or why
    /// it could not be read; `repo` as for the queues.
    PrDetailLoaded {
        tag: Tag,
        number: u64,
        repo: Option<ForgeRepo>,
        result: Result<Option<PrDetail>, ReviewError>,
    },
}

/// How far to scroll the diff pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    LineDown,
    LineUp,
    PageDown,
    PageUp,
}

impl AppIntent {
    /// Map a raw terminal event to an intent. Unmapped events are dropped.
    /// Key presses go to the model as they are: what a key does depends on
    /// focus and state, which only the model knows.
    pub fn from_event(event: Event) -> Option<Self> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => Some(Self::Key(key.into())),
            Event::Resize(width, height) => Some(Self::Resize { width, height }),
            Event::FocusGained => Some(Self::Watch(WatchEvent::FocusGained)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;

    #[test]
    fn a_key_press_becomes_a_key_intent() {
        let event = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(
            AppIntent::from_event(event),
            Some(AppIntent::Key(KeyPress::new(
                KeyCode::Esc,
                KeyModifiers::NONE
            )))
        );
    }

    #[test]
    fn a_key_release_is_dropped() {
        let event = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Esc,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        assert_eq!(AppIntent::from_event(event), None);
    }

    #[test]
    fn resize_is_mapped() {
        assert_eq!(
            AppIntent::from_event(Event::Resize(80, 24)),
            Some(AppIntent::Resize {
                width: 80,
                height: 24
            })
        );
    }

    #[test]
    fn focus_gained_asks_the_watch_to_look_again() {
        assert_eq!(
            AppIntent::from_event(Event::FocusGained),
            Some(AppIntent::Watch(WatchEvent::FocusGained))
        );
        assert_eq!(AppIntent::from_event(Event::FocusLost), None);
    }
}
