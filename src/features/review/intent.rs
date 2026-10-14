//! Review mode's intents. Core wraps them in one variant,
//! `AppIntent::Review`, and hands them to this feature's `update`
//! (see `docs/adr/0002-nested-intents.md`).

use super::{CommitsHere, PrCommit, PrFileRow};
use crate::effect::Tag;
use kika_forge::discussion::PrDiscussion;
use kika_forge::review::{PrDetail, Queues, ReviewError};
use kika_forge::{CreateError, ForgeHost, ForgeRepo};
use kika_git::commit_files::CommitRange;

// `PrDetailLoaded` carries the pull request as it did in `AppIntent`,
// which wraps this enum and is larger still, so boxing it would save
// nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReviewIntent {
    /// Leave the workspace for review mode and load its queues.
    Enter,
    /// Show the next review queue, or the previous one.
    NextQueue,
    PrevQueue,
    /// Open the pull request under the cursor in the queue.
    OpenPr,
    /// Back from a pull request to its queue, or from the queue to the
    /// workspace.
    CloseDetail,
    /// Ask GitHub again for what review mode shows.
    Refresh,
    /// Open a failing check's page in the browser, picking one when
    /// several fail.
    OpenCheckDetails,
    /// Merge the open pull request: pick a method when several are
    /// allowed, then ask.
    OpenMergePr,
    /// Merge the pull request the open question describes.
    ConfirmMergePr,
    /// GitHub answered a merge of pull request `number`, or why not.
    Merged {
        tag: Tag,
        number: u64,
        result: Result<(), CreateError>,
    },
    /// Move the queue's cursor, or scroll the pull request, by a line.
    Next,
    Prev,
    /// The review queues, or why not; `repo` is the repository found,
    /// when one was.
    QueuesLoaded {
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
    /// Show the open pull request's changed files and their diff.
    OpenFiles,
    /// Move the keys between the file list and the diff on the files screen.
    ToggleFilesFocus,
    /// Whether the pull request's head and base commits are here, or why
    /// git could not say.
    ObjectsChecked {
        tag: Tag,
        result: Result<CommitsHere, String>,
    },
    /// The fetch of the missing commits ended, or why it failed.
    HeadFetched {
        tag: Tag,
        result: Result<(), String>,
    },
    /// Mark the selected file viewed on GitHub, or not viewed if it is.
    ToggleViewed,
    /// GitHub answered the viewed mark of `path` in pull request `number`,
    /// or why not.
    ViewedSet {
        tag: Tag,
        /// The host the mark went to, where a rate limit is recorded.
        host: ForgeHost,
        number: u64,
        path: String,
        viewed: bool,
        result: Result<(), CreateError>,
    },
    /// Review the whole pull request or one of its commits: read the
    /// commits once, then pick from them.
    PickCommit,
    /// The pull request's commits, oldest first, and whether the list was
    /// cut, or why git could not list them.
    CommitsLoaded {
        tag: Tag,
        result: Result<(Vec<PrCommit>, bool), String>,
    },
    /// Go to the next unresolved review thread, or the previous one,
    /// across the files.
    NextThread,
    PrevThread,
    /// Open the line in view on GitHub's page of the pull request, in the
    /// browser. Nothing is asked of GitHub.
    OpenLineOnForge,
    /// Pull request `number`'s review threads and conversation, `None`
    /// when GitHub has no such pull request, or why they could not be
    /// read; `repo` as for the queues.
    DiscussionLoaded {
        tag: Tag,
        number: u64,
        repo: Option<ForgeRepo>,
        result: Result<Option<PrDiscussion>, ReviewError>,
    },
    /// The range shown, merge base to head or one commit against its first
    /// parent, and its changed files, or why not.
    FilesLoaded {
        tag: Tag,
        result: Result<(CommitRange, Vec<PrFileRow>), String>,
    },
}
