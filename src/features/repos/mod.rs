//! Repos: the recent-repositories store (`recent.rs`) and its popup
//! (`recent_list.rs`), opening a repository from it, `git init` outside
//! one, and parking or restoring a repository's view (`parked.rs`). The
//! handlers are in `update.rs`, the bindings in `keys.rs` and the I/O in
//! `effects.rs`.
//!
//! Its intents are nested (ADR-0002): core wraps [`ReposIntent`] in
//! `AppIntent::Repos`, and its one `update` arm calls [`update`] here.
//!
//! Its `pub(crate)` API: the types [`RecentList`], [`RecentRow`] and
//! [`ParkedRepo`]; the intent enum [`ReposIntent`] and the free function
//! [`update`]; the module `recent` (`default_path`, used by `main.rs`);
//! the handlers `open_recent`, `record_recent`, `recent_loaded`,
//! `recent_pending` (tests only), `move_recent_cursor`, `recent_select`,
//! `init_repo`, `repo_initialised` and `open_repo` on `AppState`;
//! `keys::{REPOS_WORKSPACE, REPOS_RECENT, init_repo_disabled}`; and
//! `effects::{load, record, init}`.
//!
//! Kept cross-feature reads and writes (move-only, ADR rule 3): `open_repo`
//! and `park` write `files` (its rows, `loaded` and cursor), `branch` and
//! `focus`, and read `diff_options`. They call core `select` (`model.rs`)
//! and core `reload_repo` (`reload.rs`), which reloads the status,
//! branches, log and stashes through the refresh plan.
//!
//! Stays in core: the shared intent `AppIntent::OpenRepo`, because
//! worktrees switch repository through it too; `switch_repo` resets the per-repository state of almost
//! every feature, and `set_repo` and `Effect::SetRepo` are shared with
//! submodules, so both stay in `model.rs`, as do `ParentRepo`, `repo_stack`
//! and the start-up dispatch `start`. `render_recent` stays in
//! `popup_view.rs`.

pub(crate) mod effects;
mod intent;
pub(crate) mod keys;
mod parked;
pub(crate) mod recent;
mod recent_list;
mod update;

pub(crate) use intent::ReposIntent;
pub(crate) use update::update;

pub(crate) use parked::ParkedRepo;
pub(crate) use recent_list::{RecentList, RecentRow};
