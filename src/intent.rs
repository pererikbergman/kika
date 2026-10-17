use std::path::PathBuf;

use crossterm::event::{Event, KeyEventKind, MouseButton, MouseEvent, MouseEventKind};

use crate::config::ConfigError;
use crate::effect::Tag;
use crate::features::blame::BlameIntent;
use crate::features::branches::BranchesIntent;
use crate::features::command_log::CommandLogIntent;
use crate::features::commit::CommitIntent;
use crate::features::commits::CommitsIntent;
use crate::features::conflicts::ConflictsIntent;
use crate::features::custom_commands::CustomCommandsIntent;
use crate::features::diff::DiffIntent;
use crate::features::files::FilesIntent;
use crate::features::operation::OperationIntent;
use crate::features::pull_requests::PullRequestsIntent;
use crate::features::repos::ReposIntent;
use crate::features::review::ReviewIntent;
use crate::features::stash::StashIntent;
use crate::features::submodules::SubmodulesIntent;
use crate::features::sync::SyncIntent;
use crate::features::worktrees::WorktreesIntent;
use crate::keymap::{KeyPress, Keymap};
use crate::layout::Panel;
use crate::menu::MenuId;
use crate::op::GitOp;
use crate::popup::PromptKind;
use crate::watch::WatchEvent;
use kika_git::askpass::Secret;
use kika_git::refs::RefsSnapshot;
use kika_git::status::RepoStatus;

/// Everything that can change the model: user input and async task results.
/// Core variants come first, then the few intents shared by features, then
/// one variant per top-level feature wrapping its intent enum, in the order
/// of ADR-0001's layout (ADR-0002).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppIntent {
    // --- core: keys and loop ---
    /// The first intent: record the repository Kika started in and load
    /// it, or, outside one, welcome the user with the recent list.
    Start,
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
    /// A left click at a screen cell: selects the row under it and
    /// focuses its panel, when nothing is open over the workspace.
    Click {
        column: u16,
        row: u16,
    },
    /// A wheel notch at a screen cell, `down` or up: scrolls what is under
    /// it without moving focus.
    Wheel {
        column: u16,
        row: u16,
        down: bool,
    },

    // --- core: focus ---
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

    // --- core: popups and menus ---
    /// Push a menu, over any menu already open.
    OpenMenu(MenuId),
    /// Move the top menu's cursor down or up.
    MenuNext,
    MenuPrev,
    /// Move the top menu's cursor by this many rows, clamped at both
    /// ends: a click or a wheel notch. It never runs the item.
    MenuMoveBy(isize),
    /// Run the item under the top menu's cursor.
    MenuSelect,
    /// Ask "Quit Kika?" (the menu's Quit item).
    AskQuit,
    ConfirmYes,
    ConfirmNo,
    /// Ask for a line of text; the text comes back as the kind's intent.
    OpenPrompt(PromptKind),
    SubmitPrompt,
    /// Close the top popup: back in a menu, cancel in a prompt, dismiss an error.
    PopPopup,

    // --- core: list input ---
    /// Type into the focused list's filter, which narrows it live.
    OpenFilter,
    /// Type a text to search the focused pane for: the Diff pane, the
    /// blame, or the commit opened from it.
    OpenSearch,
    /// Type a text to search the diff preview beside a side panel for.
    OpenPreviewSearch,
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
    /// Clear the focused pane's search, else close the pane: the blame,
    /// its commit, or a commit's or a stash's files.
    PaneEscape,
    /// Go to the next or previous match of the search.
    SearchNext,
    SearchPrev,
    /// Delete the branches or tags, remove the remotes, or drop the
    /// stashes the open range question names.
    ConfirmDeleteRange,

    // --- core: op slot ---
    /// A Git operation finished, successfully or not.
    OpFinished {
        tag: Tag,
        op: GitOp,
        result: Result<(), String>,
    },

    // --- core: watch and refresh ---
    /// Time to look for changes made outside Kika.
    Watch(WatchEvent),
    /// Reload `git status`. Also issued once at startup.
    RefreshStatus,
    /// A status load finished; `tag` says which one.
    StatusLoaded {
        tag: Tag,
        result: Result<RepoStatus, String>,
    },
    /// A refs snapshot finished; `tag` says which one.
    RefsLoaded {
        tag: Tag,
        result: Result<RefsSnapshot, String>,
    },

    // --- core: config ---
    /// The config files were read: the keys now in force, and the files
    /// that were ignored and why.
    ConfigLoaded {
        /// The top level the files were read for, so a result that
        /// arrives after a repository switch can be told apart.
        repo: Option<PathBuf>,
        keymap: Keymap,
        errors: Vec<ConfigError>,
    },

    // --- core: credentials ---
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

    // --- core: clipboard ---
    /// The text could not be sent to the clipboard, and why.
    CopyFailed(String),

    // --- core: shared by features ---
    /// Go on with the merge, rebase, cherry-pick or revert in progress.
    ContinueOperation,
    /// Drop the commit the rebase, cherry-pick or revert stopped on and go on.
    SkipOperation,
    /// Ask before aborting the operation in progress.
    AbortOperation,
    /// Switch the whole workspace to the repository at this top level.
    OpenRepo(PathBuf),
    /// Ask to throw away the unstaged work under the cursor: the selected
    /// file, hunk or lines, by focus. The answer is `ConfirmYes`/`ConfirmNo`.
    Discard,
    ScrollDiff(Scroll),
    /// Open the selected file in the user's editor, at the selected line
    /// when there is one.
    OpenInEditor,

    // --- files ---
    /// Everything the Files panel does; its own `update` handles it.
    Files(FilesIntent),

    // --- diff ---
    /// Everything the Diff pane does, with its child the renderers. Its
    /// own `update` handles it.
    Diff(DiffIntent),

    // --- commit ---
    /// Everything the commit popup does; its own `update` handles it.
    Commit(CommitIntent),

    // --- stash ---
    /// Everything the Stash panel does, its files too; its own `update`
    /// handles it.
    Stash(StashIntent),

    // --- submodules ---
    /// Everything the Submodules popup does, entering and leaving a
    /// submodule too; its own `update` handles it.
    Submodules(SubmodulesIntent),

    // --- worktrees ---
    /// Everything the Worktrees popup does; its own `update` handles it.
    /// Switching to a worktree is the shared `OpenRepo`.
    Worktrees(WorktreesIntent),

    // --- repos ---
    /// Everything the recent list and `git init` do; its own `update`
    /// handles it. Opening a repository is the shared `OpenRepo`.
    Repos(ReposIntent),

    // --- command_log ---
    /// Everything the Command log does; its own `update` handles it.
    CommandLog(CommandLogIntent),

    // --- custom_commands ---
    /// Everything a custom command does once started; its own `update`
    /// handles it.
    CustomCommands(CustomCommandsIntent),

    // --- blame ---
    /// Everything the blame does; its own `update` handles it.
    Blame(BlameIntent),

    // --- conflicts ---
    /// Everything the conflict view does; its own `update` handles it.
    Conflicts(ConflictsIntent),

    // --- sync ---
    /// Everything fetch, pull and push do; its own `update` handles it.
    Sync(SyncIntent),

    // --- operation ---
    /// Merging or rebasing onto the selected branch, and aborting as
    /// asked; its own `update` handles it. Continue, skip and abort are
    /// the shared core intents.
    Operation(OperationIntent),

    // --- pull_requests ---
    /// Everything the pull request badges, forge links, creating a pull
    /// request and the pick list do; its own `update` handles it.
    PullRequests(PullRequestsIntent),

    // --- review ---
    /// Everything review mode does; its own `update` handles it.
    Review(ReviewIntent),

    // --- branches ---
    /// Everything the branches subtree does: the panel and its Remotes
    /// and Tags tabs. Its own `update` handles it.
    Branches(BranchesIntent),

    // --- commits ---
    /// Everything the commits subtree does: the panel and its
    /// sub-features. Its own `update` handles it.
    Commits(CommitsIntent),
}

/// How far to scroll the diff pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    LineDown,
    LineUp,
    PageDown,
    PageUp,
}

impl From<CommandLogIntent> for AppIntent {
    fn from(intent: CommandLogIntent) -> Self {
        Self::CommandLog(intent)
    }
}

impl From<BranchesIntent> for AppIntent {
    fn from(intent: BranchesIntent) -> Self {
        Self::Branches(intent)
    }
}

impl From<CommitsIntent> for AppIntent {
    fn from(intent: CommitsIntent) -> Self {
        Self::Commits(intent)
    }
}

impl From<FilesIntent> for AppIntent {
    fn from(intent: FilesIntent) -> Self {
        Self::Files(intent)
    }
}

impl From<DiffIntent> for AppIntent {
    fn from(intent: DiffIntent) -> Self {
        Self::Diff(intent)
    }
}

impl From<CommitIntent> for AppIntent {
    fn from(intent: CommitIntent) -> Self {
        Self::Commit(intent)
    }
}

impl From<BlameIntent> for AppIntent {
    fn from(intent: BlameIntent) -> Self {
        Self::Blame(intent)
    }
}

impl From<ConflictsIntent> for AppIntent {
    fn from(intent: ConflictsIntent) -> Self {
        Self::Conflicts(intent)
    }
}

impl From<CustomCommandsIntent> for AppIntent {
    fn from(intent: CustomCommandsIntent) -> Self {
        Self::CustomCommands(intent)
    }
}

impl From<SyncIntent> for AppIntent {
    fn from(intent: SyncIntent) -> Self {
        Self::Sync(intent)
    }
}

impl From<OperationIntent> for AppIntent {
    fn from(intent: OperationIntent) -> Self {
        Self::Operation(intent)
    }
}

impl From<ReposIntent> for AppIntent {
    fn from(intent: ReposIntent) -> Self {
        Self::Repos(intent)
    }
}

impl From<StashIntent> for AppIntent {
    fn from(intent: StashIntent) -> Self {
        Self::Stash(intent)
    }
}

impl From<SubmodulesIntent> for AppIntent {
    fn from(intent: SubmodulesIntent) -> Self {
        Self::Submodules(intent)
    }
}

impl From<WorktreesIntent> for AppIntent {
    fn from(intent: WorktreesIntent) -> Self {
        Self::Worktrees(intent)
    }
}

impl From<PullRequestsIntent> for AppIntent {
    fn from(intent: PullRequestsIntent) -> Self {
        Self::PullRequests(intent)
    }
}

impl From<ReviewIntent> for AppIntent {
    fn from(intent: ReviewIntent) -> Self {
        Self::Review(intent)
    }
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
            // Only a left press and the wheel; modifiers are ignored.
            Event::Mouse(MouseEvent {
                kind, column, row, ..
            }) => match kind {
                MouseEventKind::Down(MouseButton::Left) => Some(Self::Click { column, row }),
                MouseEventKind::ScrollDown => Some(Self::Wheel {
                    column,
                    row,
                    down: true,
                }),
                MouseEventKind::ScrollUp => Some(Self::Wheel {
                    column,
                    row,
                    down: false,
                }),
                _ => None,
            },
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

    fn mouse(kind: MouseEventKind, modifiers: KeyModifiers) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: 7,
            row: 3,
            modifiers,
        })
    }

    #[test]
    fn a_left_press_is_a_click_and_the_wheel_scrolls_whatever_the_modifiers() {
        for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
            assert_eq!(
                AppIntent::from_event(mouse(MouseEventKind::Down(MouseButton::Left), modifiers)),
                Some(AppIntent::Click { column: 7, row: 3 })
            );
            assert_eq!(
                AppIntent::from_event(mouse(MouseEventKind::ScrollDown, modifiers)),
                Some(AppIntent::Wheel {
                    column: 7,
                    row: 3,
                    down: true
                })
            );
            assert_eq!(
                AppIntent::from_event(mouse(MouseEventKind::ScrollUp, modifiers)),
                Some(AppIntent::Wheel {
                    column: 7,
                    row: 3,
                    down: false
                })
            );
        }
    }

    #[test]
    fn other_mouse_events_are_dropped() {
        for kind in [
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Moved,
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Down(MouseButton::Middle),
            MouseEventKind::ScrollLeft,
            MouseEventKind::ScrollRight,
        ] {
            assert_eq!(
                AppIntent::from_event(mouse(kind, KeyModifiers::NONE)),
                None,
                "{kind:?}"
            );
        }
    }
}
