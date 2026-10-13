//! Review mode: the pull-request queues and the open pull request's detail
//! (`state.rs`), its check links, its changed files and their diff
//! (`files.rs`), merging it from the detail (`merge.rs`), the markdown
//! renderer for its body (`markdown.rs`), and the local git reads behind
//! the files screen (`git.rs`). The feature also owns its key bindings
//! (`keys.rs`), its screen (`view.rs`) and the I/O behind
//! `LoadReviewQueues`, `LoadPrDetail`, `CheckReviewCommits`,
//! `FetchReviewCommits`, `LoadReviewFiles` and `MergePullRequest`
//! (`effects.rs`). The credential plumbing that `FetchReviewCommits`
//! shares with sync, `src/forge` and the merge question popup stay in core.
//!
//! Its intents are nested (ADR-0002): core wraps [`ReviewIntent`] in
//! `AppIntent::Review`, and its one `update` arm calls [`update`] here.
//! `update.rs` holds only that function; it dispatches to the handlers in
//! `state.rs`, `files.rs` and `merge.rs`. The review guards on core arms
//! (`ScrollDiff`, `OpenSearch`, `DiffEscape`) stay in core.
//!
//! Its `pub(crate)` API:
//! - the intent enum [`ReviewIntent`] and the free function [`update`];
//! - the types [`ReviewState`], [`ReviewScreen`] and [`PrMerging`], which
//!   `AppState` holds, and [`CommitsHere`] and [`PrFileRow`], which intents
//!   carry;
//! - the guards `queue_keys_disabled` and `review_diff_focused`, which core
//!   focus uses, and those in `keys.rs`;
//! - the handlers in `state.rs`, `files.rs` and `merge.rs` that `update`
//!   calls, including the `review_files_shown` guard;
//! - the chunks `keys::{REVIEW_KEYS, REVIEW_MERGE_CONFIRM}`;
//! - `view::render`;
//! - `effects::{load_queues, load_detail, check_commits, fetch_commits,
//!   load_files, merge}`.
//!
//! Kept cross-feature reads and calls:
//! - `merge.rs` reloads the badges after a merge by asking core
//!   `apply_plan` (`reload.rs`) for `Scope::PullRequests`;
//! - pull_requests' `pick_select` calls `pick_check` and
//!   `pick_merge_method`;
//! - `files.rs` reads and writes `diff` and `diff_options` (diff);
//! - `state.rs` reads `files` (files).

pub(crate) mod effects;
mod files;
mod git;
mod intent;
pub(crate) mod keys;
mod markdown;
mod merge;
mod state;
mod update;
pub(crate) mod view;

pub(crate) use files::review_diff_focused;
pub(crate) use git::{CommitsHere, PrFileRow};
pub(crate) use intent::ReviewIntent;
pub(crate) use merge::PrMerging;
pub(crate) use state::{ReviewScreen, ReviewState, queue_keys_disabled};
pub(crate) use update::update;
