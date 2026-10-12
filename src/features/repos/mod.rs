//! Repos: the recent-repositories store (`recent.rs`) and its popup
//! (`recent_list.rs`), opening a repository from it, `git init` outside
//! one, and parking or restoring a repository's view (`parked.rs`). The
//! handlers are in `update.rs`, the bindings in `keys.rs` and the I/O in
//! `effects.rs`.
//!
//! Its `pub(crate)` API: the types [`RecentList`], [`RecentRow`] and
//! [`ParkedRepo`]; the module `recent` (`default_path`, used by `main.rs`);
//! the handlers `open_recent`, `record_recent`, `recent_loaded`,
//! `recent_pending` (tests only), `move_recent_cursor`, `recent_select`,
//! `init_repo`, `repo_initialised` and `open_repo` on `AppState`;
//! `keys::{REPOS_WORKSPACE, REPOS_RECENT, init_repo_disabled}`; and
//! `effects::{load, record, init}`.
//!
//! Kept cross-feature reads and writes (move-only, ADR rule 3): `open_repo`
//! and `park` write `files`, `files_loaded`, `branch`, `selected` and
//! `focus`, and read `diff_options`. They call `select` and
//! `refresh_status` (files), `reload_branches` (branches),
//! `reload_commits` (commits) and `reload_stashes` (stash).
//!
//! Stays in core: `switch_repo` resets the per-repository state of almost
//! every feature, and `set_repo` and `Effect::SetRepo` are shared with
//! submodules, so both stay in `model.rs`, as do `ParentRepo`, `repo_stack`
//! and the start-up dispatch `start`. `render_recent` stays in
//! `popup_view.rs`.

pub(crate) mod effects;
pub(crate) mod keys;
mod parked;
pub(crate) mod recent;
mod recent_list;
mod update;

pub(crate) use parked::ParkedRepo;
pub(crate) use recent_list::{RecentList, RecentRow};
