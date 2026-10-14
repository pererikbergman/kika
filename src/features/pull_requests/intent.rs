//! The pull requests feature's intents. Core wraps them in one variant,
//! `AppIntent::PullRequests`, and hands them to this feature's `update`
//! (see `docs/adr/0002-nested-intents.md`).

use std::collections::HashMap;

use crate::effect::Tag;
use kika_forge::{CreateError, ForgeRepo, ForgeStatus, PrBadge};
use kika_git::branches::BranchName;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PullRequestsIntent {
    /// The remote is on none of the forge hosts: no badges.
    RepoNone {
        tag: Tag,
    },
    /// The forge repository found, and its cached badges; `None` is a miss.
    Cached {
        tag: Tag,
        repo: ForgeRepo,
        badges: Option<HashMap<BranchName, PrBadge>>,
    },
    /// The forge answered, or why it could not. `limited_until` is set
    /// when GitHub rate limited the host: no call is made before it.
    Fetched {
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
    /// Ask the forge again for the branches' pull requests, at the user's
    /// request: only on an opted-in host, never past a running call or a
    /// rate limit.
    Refresh,
    /// The browser could not be started, and why.
    OpenUrlFailed(String),
    /// Start a pull request for the selected branch: pick the target
    /// remote and base branch where more than one fits, then the title.
    CreateStart,
    /// Move the pick list's cursor down or up.
    PickNext,
    PickPrev,
    /// Take the row under the pick list's cursor.
    PickSelect,
    /// The title the pull request prompt was accepted with.
    Titled(String),
    /// Create the pull request the open question describes.
    ConfirmCreate,
    /// GitHub answered a create: the new number and page, or why not.
    Created {
        tag: Tag,
        result: Result<(u64, String), CreateError>,
    },
}
