//! Remotes: the Branches panel's Remotes tab, checking out a remote branch
//! as a tracking branch, and adding, renaming and removing a remote. The
//! feature owns the tab's rows and selection and what an open prompt acts
//! on (`list.rs`), its `update` handlers (`update.rs`), its key bindings and
//! guards (`keys.rs`), the tab's body and a remote's details (`view.rs`),
//! the I/O behind `Effect::LoadRemotes` (`effects.rs`) and the argv for
//! its four ops (`git.rs`).
//!
//! Its `pub(crate)` API:
//! - the intent enum `RemotesIntent`, nested in `BranchesIntent::Remotes`,
//!   and the free function `update` that `branches::update` passes it to;
//! - the types [`RemotesList`], [`RemoteLine`] and [`RemotePending`], and
//!   `lines`, which flattens a load's remotes into the tab's rows;
//! - the handlers in `update.rs`: `remotes_loaded`, `move_remote`,
//!   `ask_track_name`, `track_named`, `ask_remote_name`, `remote_named`,
//!   `remote_url`, `ask_remote_rename`, `rename_remote_to`,
//!   `ask_remove_remote`, `confirm_remove_remote` and
//!   `remotes_popup_closed`;
//! - the chunks `keys::{REMOTES_KEYS, REMOTES_REMOVE_CONFIRM}` and the
//!   guards `keys::{add_remote_disabled, track_disabled,
//!   change_remote_disabled, rename_remote_disabled}`;
//! - `effects::load`;
//! - `git::{track_command, add_command, rename_command, remove_command}`;
//! - `view::{list_body, render_remote}`.
//!
//! Kept cross-feature reads and calls:
//! - `bisect_refuses` on `AppState` and `bisect::head_move_disabled`
//!   (bisect, in core), so no tracking checkout moves `HEAD` mid-bisect;
//! - `branches.*` and `load_branch_log` (the parent, `features/branches`):
//!   the tab, its remotes and the selected branch's log;
//! - `start_op` and the op slot (core), git one operation at a time.
//!
//! What stays in core: the tag push and selection guards `push_tag_disabled`,
//! `push_tag_remote` and `tag_selected_disabled` and `op_running` in
//! `keymap.rs`; `src/git/remotes.rs`; the `GitOp` remote variants with the
//! `RemoveRemotes` steps, refusals, redaction, failure messages and refresh
//! plan; the upstream code; and the remote branches of shared dispatch: the
//! range remove arm, `key_focus`, `open_filter`, `toggle_range`, the
//! `TrackBranch`, `RemoteName`, `RemoteUrl` and `RenameRemote` prompt
//! submits and titles, the refresh-all load and `range_delete.rs`. The
//! remove question is built here (`remove_remote_question`) on the core
//! `confirm_question.rs`. The parent owns `BranchesPanel`, the panel's
//! `view.rs` and the range question's builder (`lists.rs`).

pub(crate) mod effects;
pub(crate) mod git;
mod intent;
pub(crate) mod keys;
mod list;
mod update;
pub(crate) mod view;

pub(crate) use intent::RemotesIntent;
pub(crate) use list::{RemoteLine, RemotePending, RemotesList, lines};
pub(crate) use update::update;
// Core's popup tests build the question as the feature asks it.
#[cfg(test)]
pub(crate) use update::remove_remote_question;
