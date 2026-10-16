//! Review mode: the pull-request queues and the open pull request's detail
//! (`state.rs`), its check links, its changed files and their diff
//! (`files.rs`), the whole pull request or one of its commits
//! (`commit_view.rs`), marking a file viewed on GitHub (`viewed.rs`),
//! its review threads placed on the diff and `]`/`[` between them
//! (`threads.rs`, drawn by `threads_view.rs`), one thread read in full
//! over the files screen (`reader.rs`, drawn by `reader_view.rs`), its
//! conversation of comments and review bodies (`conversation.rs`, drawn
//! by `conversation_view.rs`), replying to a thread, resolving it and
//! commenting on the pull request on GitHub (`discussion_write.rs`),
//! opening the line in view on GitHub (`line_link.rs`), merging it
//! from the detail
//! (`merge.rs`), checking its head out as the local branch `pr/<number>`
//! and going to the workspace (`checkout.rs`), the markdown renderer for
//! its body (`markdown.rs`), the pending review of line comments kept on
//! disk (`pending.rs`), picking the lines a comment goes on
//! (`comment_pick.rs`), starting a code suggestion on them
//! (`suggestion.rs`), and typing it (`comment_editor.rs`) or writing it in
//! the user's editor (`comment_external.rs`), submitting the pending
//! review to GitHub (`submit.rs`), all drawn by
//! `pending_view.rs`, and the local git reads behind the files screen and
//! the checkout (`git.rs`). The feature
//! also owns its key bindings (`keys.rs`), its screen (`view.rs`) and the
//! I/O behind `LoadReviewQueues`, `LoadPrDetail`, `LoadPrDiscussion`,
//! `CheckReviewCommits`, `FetchReviewCommits`, `LoadReviewFiles`,
//! `LoadReviewCommits`, `LoadReviewRange`, `SetFileViewed`,
//! `WriteDiscussion`, `PlanPrCheckout`, `MergePullRequest`, `LoadPendingReview` and
//! `SavePendingReview`, `ReadSuggestionLines` and `RemoveCommentFile` (`effects.rs`). The credential
//! plumbing that `FetchReviewCommits` shares with sync and the merge
//! question popup stay in core; the forge client is the `kika_forge` crate.
//!
//! Its intents are nested (ADR-0002): core wraps [`ReviewIntent`] in
//! `AppIntent::Review`, and its one `update` arm calls [`update`] here.
//! `update.rs` holds only that function; it dispatches to the handlers in
//! `state.rs`, `files.rs`, `commit_view.rs`, `viewed.rs`, `threads.rs`,
//! `reader.rs`, `conversation.rs`, `discussion_write.rs`, `line_link.rs`, `merge.rs`,
//! `checkout.rs`, `pending.rs`, `comment_pick.rs`, `suggestion.rs`,
//! `comment_editor.rs` and `comment_external.rs` and `submit.rs`.
//! The review guards on core arms (`ScrollDiff`, `OpenSearch`,
//! `DiffEscape`) stay in core.
//!
//! Its `pub(crate)` API:
//! - the intent enum [`ReviewIntent`] and the free function [`update`];
//! - the types [`ReviewState`], [`ReviewScreen`] and [`PrMerging`], which
//!   `AppState` holds, and [`CommitsHere`], [`PrCommit`] and [`PrFileRow`],
//!   which intents carry, and [`ComposerStamp`], [`EditedComment`],
//!   [`CommentEditError`] and [`edited_comment`], which core's editor
//!   hand-off uses;
//! - the guards `queue_keys_disabled`, `review_diff_focused`,
//!   `thread_reader_open`, `composer_open`, `pending_list_open` and
//!   `comment_pick_open`, which core focus uses, and those in `keys.rs`;
//! - the handlers in `state.rs`, `files.rs`, `commit_view.rs`, `viewed.rs`,
//!   `threads.rs`, `line_link.rs`, `merge.rs`, `checkout.rs` and
//!   `submit.rs` that `update` calls, including the `review_files_shown` guard and
//!   `open_line_disabled`;
//! - `pr_branch`, `verify_pr_branch` and `pr_checkout_commands`
//!   (`git.rs`), which core's `GitOp::CheckoutPrBranch` is named, checked
//!   and run by;
//! - `review_jump_landed` (`threads.rs`), which core's `AppIntent::Diff`
//!   arm calls after each diff chunk, so a thread `]`/`[` waits for is
//!   scrolled to once its row streams in;
//! - the chunks `keys::{REVIEW_KEYS, REVIEW_COMMENT_KEYS,
//!   REVIEW_MERGE_CONFIRM, REVIEW_CHECKOUT_CONFIRM, REVIEW_COMMENT_CONFIRM}`,
//!   and `keys::HIDDEN_WHEN_OFF`, the bar hints left out while refused;
//! - `view::render`;
//! - `effects::{load_queues, load_detail, load_discussion, check_commits,
//!   fetch_commits, load_files, load_commits, load_range_files,
//!   set_file_viewed, write_discussion, plan_checkout, merge, load_pending,
//!   save_pending, read_suggestion_lines, remove_comment_file}`.
//!
//! Kept cross-feature reads and calls:
//! - `merge.rs` reloads the badges after a merge by asking core
//!   `apply_plan` (`reload.rs`) for `Scope::PullRequests`;
//! - pull_requests' `pick_select` calls `pick_check`, `pick_merge_method`,
//!   `pick_review_commit` and `pick_review_event`;
//! - `files.rs` reads and writes `diff` and `diff_options` (diff), and
//!   `comment_pick.rs` reads the streamed lines of `diff` and moves its
//!   scroll to keep the pick in view, which `pending_view.rs` draws;
//! - `state.rs` reads `files` (files).

mod checkout;
mod comment_editor;
mod comment_external;
mod comment_pick;
mod commit_view;
mod conversation;
mod conversation_view;
mod discussion_write;
pub(crate) mod effects;
mod files;
mod git;
mod intent;
pub(crate) mod keys;
mod line_link;
mod markdown;
mod merge;
mod pending;
mod pending_view;
mod reader;
mod reader_view;
mod state;
mod submit;
mod suggestion;
mod threads;
mod threads_view;
mod update;
pub(crate) mod view;
mod viewed;

#[cfg(test)]
pub(crate) use comment_editor::ComposerKind;
pub(crate) use comment_editor::composer_open;
pub(crate) use comment_external::{CommentEditError, ComposerStamp, EditedComment, edited_comment};
pub(crate) use comment_pick::comment_pick_open;
pub(crate) use files::review_diff_focused;
pub(crate) use git::{
    CommitsHere, PrCommit, PrFileRow, pr_branch, pr_checkout_commands, verify_pr_branch,
};
pub(crate) use intent::ReviewIntent;
pub(crate) use merge::PrMerging;
pub(crate) use pending::{CommentTarget, pending_list_open};
pub(crate) use reader::thread_reader_open;
pub(crate) use state::{ReviewScreen, ReviewState, queue_keys_disabled};
pub(crate) use update::update;
