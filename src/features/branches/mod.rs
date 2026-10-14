//! Branches: the Branches panel and its Local tab. The feature owns the
//! panel's state (`panel.rs`), its rendering and the selected branch's log
//! (`view.rs`), loading the list and the log, moving between rows and
//! tabs, checking out, creating, renaming and deleting one branch,
//! creating one at a detached `HEAD`, and setting or unsetting its
//! upstream (`update.rs`), the tab filters and the
//! range questions' builders (`lists.rs`), its key bindings and guards
//! (`keys.rs`), the argv for its branch ops (`git.rs`) and the I/O behind
//! `Effect::LoadBranches` and `Effect::LoadBranchLog` (`effects.rs`). Its
//! children `remotes` and `tags` own the other two tabs.
//!
//! Its intents are nested (ADR-0002): core wraps [`BranchesIntent`] in
//! `AppIntent::Branches`, and its one `update` arm calls [`update`] here,
//! which handles the panel's own variants and passes each child's enum
//! (`BranchesIntent::Remotes(RemotesIntent)` and
//! `BranchesIntent::Tags(TagsIntent)`) to that child's `update`. A child
//! may build its parent's intents; neither builds the other's.
//!
//! Its `pub(crate)` API:
//! - the types [`BranchesPanel`] and [`BranchesTab`];
//! - the intent enum [`BranchesIntent`] and the free function [`update`];
//! - `load_branches` and `load_branch_log` on `AppState`, which the
//!   children and core reloads call;
//! - the handlers in `update.rs`: `background_branches`, `branches_loaded`,
//!   `branch_log_loaded`, `switch_branches_tab`, `move_branch`,
//!   `checkout_branch`, `ask_branch_name`, `create_branch_named`,
//!   `ask_branch_at_head`, `create_branch_at_head_named`,
//!   `rename_branch_to`, `ask_delete_branch`, `confirm_delete_branch`,
//!   `ask_upstream`, `set_upstream_to`, `unset_upstream` and
//!   `branches_popup_closed`; and in
//!   `lists.rs`: `apply_branches_filter` and the range builders
//!   `branches_range_confirm`, `tags_range_confirm` and
//!   `remotes_range_confirm`;
//! - the chunks `keys::{BRANCHES_KEYS, BRANCHES_CHANGE, BRANCHES_UPSTREAM,
//!   BRANCHES_TABS, BRANCHES_AT_HEAD, BRANCHES_DELETE_CONFIRM}` and the
//!   guards `keys::{checkout_branch_disabled, new_branch_disabled,
//!   branch_at_head_disabled, delete_branch_disabled,
//!   unset_upstream_disabled}`;
//! - the argv builders `git::{create_command, create_at_command,
//!   rename_command, delete_command, set_upstream_command,
//!   unset_upstream_command}`;
//! - `effects::{load, load_log}`;
//! - `view::{render_panel, render_log, RowMarks}`.
//!
//! Kept cross-feature reads and calls:
//! - `branches_loaded` writes `forge`'s branch names, and asks core
//!   `apply_plan` (`reload.rs`) for `Scope::PullRequests` to reload the
//!   badges instead of calling pull_requests' load;
//! - `watch.branches_pending`, the op slot, `take_seq`, `start_op`,
//!   `bisect_refuses` and `branch_for_action` (core), and
//!   `crate::bisect::head_move_disabled` (core), so no checkout moves
//!   `HEAD` mid-bisect;
//! - `branches.remotes.branch_named` and `branches.remotes.remotes()` (own
//!   child), which the upstream prompt reads.
//!
//! What stays in core: `local_branch_selected`, which worktrees reads;
//! `reload_branches`, which repos, worktrees and submodules call;
//! `branch_for_action`, which operation calls; `range_delete.rs` with
//! `RangeKind` and the range question's keys, because the stash range uses
//! them too, and the `DeleteBranches` argv; the shared dispatch branches
//! (`key_focus`, `open_filter`, the filter apply, `list_escape`,
//! `toggle_range`, `prompt_title`); the `DeleteBranch` popups and their
//! render; `checkout_command` in `op.rs`, which reflog builds too; and
//! `src/git/branches.rs`.

pub(crate) mod effects;
pub(crate) mod git;
mod intent;
pub(crate) mod keys;
mod lists;
mod panel;
pub(crate) mod remotes;
pub(crate) mod tags;
mod update;
pub(crate) mod view;

pub(crate) use intent::BranchesIntent;
pub(crate) use panel::{BranchesPanel, BranchesTab};
pub(crate) use update::update;
// Core's popup tests build the question as the feature asks it.
#[cfg(test)]
pub(crate) use update::delete_branch_question;
