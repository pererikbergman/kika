//! Branches: the Branches panel and its Local tab. The feature owns the
//! panel's state (`panel.rs`), its rendering and the selected branch's log
//! (`view.rs`), loading the list and the log and moving between rows and
//! tabs and checking out (`update.rs`), the tab filters and the range
//! questions' builders (`lists.rs`), its key bindings and guards
//! (`keys.rs`) and the I/O behind `Effect::LoadBranches` and
//! `Effect::LoadBranchLog` (`effects.rs`). Its children `remotes` and
//! `tags` own the other two tabs.
//!
//! Its `pub(crate)` API:
//! - the types [`BranchesPanel`] and [`BranchesTab`];
//! - `load_branches` and `load_branch_log` on `AppState`, which the
//!   children and core reloads call;
//! - the handlers in `update.rs`: `background_branches`, `branches_loaded`,
//!   `branch_log_loaded`, `switch_branches_tab`, `move_branch` and
//!   `checkout_branch`; and in `lists.rs`: `apply_branches_filter` and the
//!   range builders `branches_range_confirm`, `tags_range_confirm` and
//!   `remotes_range_confirm`;
//! - the chunks `keys::{BRANCHES_KEYS, BRANCHES_TABS}` and the guards
//!   `keys::{checkout_branch_disabled, new_branch_disabled}`;
//! - `effects::{load, load_log}`;
//! - `view::{render_panel, render_log, RowMarks}`.
//!
//! Kept cross-feature reads and calls:
//! - `branches_loaded` calls `load_pull_requests` (pull_requests) on
//!   `AppState` and writes `forge`'s branch names;
//! - `watch.branches_pending`, `busy`, `take_seq`, `start_op` and
//!   `branch_for_action` (core), and `crate::bisect::head_move_disabled`
//!   (core), so no checkout moves `HEAD` mid-bisect.
//!
//! What stays in core: `local_branch_selected`, which worktrees reads;
//! `reload_branches`, which repos, worktrees and submodules call;
//! `branch_for_action`, which operation calls; `range_delete.rs` with
//! `RangeKind` and the range question's keys, because the stash range uses
//! them too; the shared dispatch branches (`key_focus`, `open_filter`, the
//! filter apply, `list_escape`, `toggle_range`); `checkout_command` in
//! `op.rs`, which reflog builds too; `src/git/branches.rs`; and creating,
//! renaming, deleting and the upstream code.

pub(crate) mod effects;
pub(crate) mod keys;
mod lists;
mod panel;
pub(crate) mod remotes;
pub(crate) mod tags;
mod update;
pub(crate) mod view;

pub(crate) use panel::{BranchesPanel, BranchesTab};
