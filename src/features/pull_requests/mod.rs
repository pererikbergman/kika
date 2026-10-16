//! Pull requests: the per-branch badges and their refresh (`badges.rs`),
//! opening or copying a branch's or commit's forge URL, and creating a pull
//! request through the target-remote and base-branch pickers (`create.rs`).
//! The feature also owns its key bindings (`keys.rs`) and the I/O behind
//! `LoadPullRequests`, `OpenUrl` and `CreatePullRequest` (`effects.rs`).
//! `ForgeState` (in `kika_forge`), merging and review mode stay outside it.
//!
//! Its intents are nested (ADR-0002): core wraps [`PullRequestsIntent`] in
//! `AppIntent::PullRequests`, and its one `update` arm calls [`update`]
//! here. `update.rs` holds only that function; it dispatches to the
//! handlers in `badges.rs` and `create.rs`. The pick list's intents are
//! here too, because its bindings and `pick_select` are.
//!
//! Its `pub(crate)` API:
//! - the intent enum [`PullRequestsIntent`] and the free function [`update`];
//! - the types [`PrDraft`] and [`PrCreating`], which `AppState` holds;
//! - the builders `with_forge_hosts` and `with_forge_links` on `AppState`;
//! - the guards `open_url_disabled`, `copy_url_disabled`,
//!   `commit_url_disabled` and `create_disabled`, which the menu uses, and
//!   `commit_page`, which core `update` uses;
//! - the handlers in `badges.rs` and `create.rs` that `update` calls;
//! - the chunks `keys::{PULL_REQUESTS_BRANCHES, PULL_REQUESTS_PICK_CREATE}`;
//! - `effects::{load, create, open_url}`.
//!
//! Kept cross-feature reads and calls:
//! - `create_disabled`, `branch_page` and friends read the branch
//!   selection and remotes (branches);
//! - `commit_url_disabled` and `commit_page` read the selected commit
//!   (commits);
//! - `pick_select` dispatches to `pick_check`, `pick_merge_method`,
//!   `pick_review_commit` and `pick_review_event` (review), accepted
//!   cross-feature calls listed in ADR-0001's Outcome;
//! - `REFRESH_LIMITED`, `same_repo` and `ForgeState::record_rate_limit`,
//!   which review's merge shares, live in `kika_forge`;
//! - branches (`branches_loaded`, when a branch list loads) and review
//!   (after a merge) ask core (`reload.rs`) for `Scope::PullRequests`, which
//!   calls `load_pull_requests`. A fetch, pull or
//!   push sets `forge.want_refresh()` in sync's `remote_finished`, and the
//!   branch reload that follows starts the load.

mod badges;
mod create;
pub(crate) mod effects;
mod intent;
pub(crate) mod keys;
mod update;

pub(crate) use create::{
    PrCreating, PrDraft, commit_page, commit_url_disabled, copy_url_disabled, create_disabled,
    open_url_disabled,
};
pub(crate) use intent::PullRequestsIntent;
pub(crate) use update::update;

#[cfg(test)]
pub(crate) use create::create_question;
