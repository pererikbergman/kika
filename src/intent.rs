use std::path::PathBuf;

use crossterm::event::{Event, KeyEventKind};

use crate::ansi::StyledRow;
use crate::config::ConfigError;
use crate::custom_command::{CommandFailure, OutputMode};
use crate::effect::Tag;
use crate::external_diff::ExternalRenderer;
use crate::git::blame::BlameLine;
use crate::git::branches::{Branch, BranchName, LogRow};
use crate::git::commit_files::CommitFile;
use crate::git::diff::{DiffChunk, DiffLine};
use crate::git::log::{LogPage, Markers};
use crate::git::ops::{GitOp, MergeStrategy, ResetMode};
use crate::git::patch::FilePatch;
use crate::git::prompts::PromptFacts;
use crate::git::rebase_plan::Edit;
use crate::git::refs::RefsSnapshot;
use crate::git::repo_state::Operation;
use crate::git::status::RepoStatus;
use crate::git::submodule::Submodule;
use crate::keymap::{KeyPress, Keymap};
use crate::layout::Panel;
use crate::menu::MenuId;
use crate::popup::{ConfirmKind, PromptKind};
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
    /// `name`'s log was read; `tag` says which load.
    BranchLogLoaded {
        tag: Tag,
        name: BranchName,
        result: Result<Vec<LogRow>, String>,
    },
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
    /// Reset the current branch to the selected commit. A hard reset asks
    /// first.
    ResetTo(ResetMode),
    /// Reset hard, as the question asked.
    ConfirmResetHard,
    /// Rewrite the selected commit with git's interactive rebase.
    RewriteCommit(Edit),
    /// The config files were read: the keys now in force, and the files
    /// that were ignored and why.
    ConfigLoaded {
        /// The top level the files were read for, so a result that
        /// arrives after a repository switch can be told apart.
        repo: Option<PathBuf>,
        keymap: Keymap,
        errors: Vec<ConfigError>,
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
