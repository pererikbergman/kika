//! Compare: the diff mark and the comparison of two commits or branches.
//! `M` marks the selected log commit or local branch (again to clear),
//! and Enter on another one reads both ends as commits and opens what
//! changed between them in the commit-files view. Chunks 74
//! (`commits.mark`) and 76 (`branches.mark`, `branches.diff`) are its
//! key bindings, and it does the I/O behind `Effect::ResolveDiff`.
//!
//! Its `pub(crate)` API:
//! - the intent enum `CompareIntent`, nested in `CommitsIntent::Compare`,
//!   and the free function `update` that `commits::update` passes it to;
//! - `PendingDiff`, the comparison `AppState.pending_diff` holds;
//! - `MARK_CLEARED`, which core's Esc chain shows too;
//! - `keys::{COMPARE_COMMITS, COMPARE_BRANCHES}` and
//!   `keys::commits_mark_disabled`;
//! - the handlers in `update.rs`: `toggle_diff_mark`, `open_branch_diff`,
//!   `open_comparison` and `diff_resolved`;
//! - `effects::resolve`.
//!
//! Kept cross-feature reads and calls:
//! - the handlers read `commits.list.rows`, `commits.list.selected`,
//!   `commits.open`, `commits.tab` and `branches.selected_branch()`, and call
//!   `close_commit_files` and `focus_panel`;
//! - `commits_mark_disabled` calls core `list_guard`,
//!   `commits_range_disabled` and `no_commit_selected`, and
//!   `branches_diff_disabled` calls `no_branch_selected`.
//!
//! What stays in core, and why:
//! - `diff_mark.rs` (`DiffMark`, `DiffEnd`), the `diff_mark` and
//!   `pending_diff` fields, and the mark's style in `rows.rs`: the Commits
//!   and Branches views both draw the mark;
//! - `no_commit_selected` and `no_branch_selected`: `menu.rs`, `model.rs`
//!   and the pull-request feature use them too;
//! - the view tests that draw the mark and a comparison's title, which
//!   test the parent's `view.rs` and the Branches feature's `view.rs`;
//! - the open-commit path that calls `open_comparison` when a mark is
//!   set, and the Esc chain that clears the mark, both in `model.rs`;
//! - the `ResolveDiff` runtime test in `effect.rs`, and the keymap tests
//!   that name other features' ids and guards.

pub(crate) mod effects;
mod intent;
pub(crate) mod keys;
mod pending;
mod update;

pub(crate) use intent::CompareIntent;
pub(crate) use pending::PendingDiff;
pub(crate) use update::{MARK_CLEARED, update};
