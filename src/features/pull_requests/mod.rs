//! Pull requests: the per-branch badges and their refresh (`badges.rs`),
//! opening or copying a branch's or commit's forge URL, and creating a pull
//! request through the target-remote and base-branch pickers (`create.rs`).
//! The feature also owns its key bindings (`keys.rs`) and the I/O behind
//! `LoadPullRequests`, `OpenUrl` and `CreatePullRequest` (`effects.rs`).
//! `ForgeState`, `src/forge`, merging and review mode stay outside it.
//!
//! Its `pub(crate)` API:
//! - the types [`PrDraft`] and [`PrCreating`], which `AppState` holds;
//! - the builders `with_forge_hosts` and `with_forge_links` on `AppState`;
//! - the guards `open_url_disabled`, `copy_url_disabled`,
//!   `commit_url_disabled` and `create_disabled`, which the menu uses, and
//!   `question` and `commit_page`, which core `update` uses;
//! - the handlers in `badges.rs` and `create.rs` that `update` calls;
//! - the chunks `keys::{PULL_REQUESTS_BRANCHES, PULL_REQUESTS_PICK_CREATE}`;
//! - `effects::{load, create, open_url}`.
//!
//! Kept cross-feature reads and calls:
//! - `create_disabled`, `branch_page` and friends read the branch
//!   selection and remotes (branches);
//! - `commit_url_disabled` and `commit_page` read the selected commit
//!   (commits);
//! - `pick_select` dispatches to `pick_check` and `pick_merge_method`
//!   (review and merge);
//! - core `merge_pr` uses `REFRESH_LIMITED`, `same_repo` and
//!   `record_rate_limit`. Whichever slice moves `merge_pr` into a feature
//!   must first lift these into core;
//! - core calls `load_pull_requests` when a branch list loads
//!   (`branches_loaded`) and after a merge (`merge_pr`). A fetch, pull or
//!   push sets `forge.want_refresh()` in sync's `remote_finished`, and the
//!   branch reload that follows starts the load.

mod badges;
mod create;
pub(crate) mod effects;
pub(crate) mod keys;

pub(crate) use create::{
    PrCreating, PrDraft, REFRESH_LIMITED, commit_page, commit_url_disabled, copy_url_disabled,
    create_disabled, open_url_disabled, question, same_repo,
};
