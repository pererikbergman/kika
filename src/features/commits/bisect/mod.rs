//! Bisect: marking commits good, bad or skipped from the Commits and
//! Status panels, resetting the bisect, saying what a status load found,
//! and the `git bisect start` that the first mark runs. Chunks 51
//! (`commits.bisect`) and 53 (`status.bisect_good`, `status.bisect_bad`)
//! are its key bindings. Its git ops run through the core op slot.
//!
//! Its `pub(crate)` API:
//! - the intent enum `BisectIntent`, nested in `CommitsIntent::Bisect`,
//!   and the free function `update` that `commits::update` passes it to.
//!   Core's `SkipOperation` arm builds `BisectIntent::MarkHead` when only
//!   a bisect is in progress;
//! - `keys::{BISECT_COMMITS, BISECT_STATUS}`;
//! - the handlers in `update.rs`: `bisect_selected`, `bisect_head`,
//!   `ask_bisect_reset` (with its `bisect_reset_question`), `bisect_reset`
//!   and `announce_bisect`;
//! - `git::run_bisect_start`.
//!
//! Kept cross-feature reads and calls:
//! - `bisect_selected` calls `commit_for_action` (commits);
//! - the handlers read `branch`, `commits.short_oid`, `context` and
//!   `popups`;
//! - `commits_bisect_disabled` calls core `list_guard`, which reads the
//!   Commits list.
//!
//! What stays in core, and why:
//! - `src/bisect.rs` (the bisect guards, reasons and notices): the
//!   operation feature uses `head_move_disabled`, `continue_disabled`,
//!   `skip_disabled` and `abort_disabled`, and core `keymap.rs`,
//!   `menu.rs` and `model.rs` call it too;
//! - `bisect_lines` and `FOUND_MARK` in `view.rs`, which the operation
//!   feature's Status panel draws;
//! - `src/git/bisect.rs`, which `git/status` reads;
//! - the `update()` dispatch arms and the `bisect::bisect_only` guards,
//!   the Bisect menu (`MenuId::Bisect`) and its `BisectIntent::AskReset` intent,
//!   and the `op_command`/`run_op` arms in `op.rs`.

pub(crate) mod git;
mod intent;
pub(crate) mod keys;
mod update;

pub(crate) use intent::BisectIntent;
pub(crate) use update::update;
