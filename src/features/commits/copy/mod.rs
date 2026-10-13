//! Copy and paste: the copied-commit set, copying the selected commit or
//! a range into it (`c`), clearing it (`C`), and pasting it as a
//! cherry-pick (`v`) whose hand-off asks git for parents first. Chunk 14
//! (`commits.copy`, `commits.paste`, `commits.clear_copied`,
//! `commits.copy_menu`) is its key bindings, and it does the I/O behind
//! `Effect::LoadCommitMessage`. Its git ops run through the core op slot.
//!
//! Its `pub(crate)` API:
//! - the intent enum `CopyIntent`, nested in `CommitsIntent::Copy`, and
//!   the free function `update` that `commits::update` passes it to;
//! - `CopiedCommits`, the set `AppState.copied` holds;
//! - `keys::COPY_COMMITS`;
//! - the handlers in `update.rs`: `copy_commit`, `copy_range` and
//!   `paste_commits`;
//! - `git::{ordered, is_full_id}`;
//! - `effects::load_message`.
//!
//! Kept cross-feature reads and calls:
//! - the handlers read `commits.list.rows`, `commits.list.selected`,
//!   `commits.list.range()` and `commits.list.range_anchor`, and call
//!   `commit_for_action` (commits);
//! - `paste_disabled` reads `branch`, `files` and the op slot.
//!
//! What stays in core, and why:
//! - the clipboard plumbing (`clipboard.rs`, `Effect::Clipboard`,
//!   `CopyFailed`, `copy_to_clipboard`, `CopyWhat` and `MenuId::Copy`):
//!   other features copy text too;
//! - `copy_commit_text`, `commit_message_loaded` and `pending_copy`:
//!   copying a commit's forge URL reads the pull-request feature's
//!   `commit_page`, which would be a sibling import, and `copy_text` is
//!   shared with that feature's PR creation;
//! - the `GitOp::CherryPick` arms in `op.rs`, `prompts.rs` and
//!   `handoff.rs`, which every op shares.

pub(crate) mod copied;
pub(crate) mod effects;
pub(crate) mod git;
mod intent;
pub(crate) mod keys;
mod update;

pub(crate) use copied::CopiedCommits;
pub(crate) use intent::CopyIntent;
pub(crate) use update::update;
