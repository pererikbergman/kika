//! Submodules: the Submodules popup (its list state, loading it, the cursor,
//! and init and update on the selected submodule) and entering and leaving a
//! submodule. The feature owns the list state (`list.rs`), its `update`
//! handlers (`update.rs`), its key bindings and guards (`keys.rs`) and the
//! load behind `Effect::LoadSubmodules` (`effects.rs`).
//!
//! Its intents are nested (ADR-0002): core wraps [`SubmodulesIntent`] in
//! `AppIntent::Submodules`, and its one `update` arm calls [`update`] here.
//!
//! Its `pub(crate)` API: the type [`SubmoduleList`]; the intent enum
//! [`SubmodulesIntent`] and the free function [`update`]; the query methods
//! `selected_submodule()` and `nested()` on `AppState`; `reload_submodules()`,
//! which the core refresh plan calls; the chunks
//! `keys::{SUBMODULES_WORKSPACE, SUBMODULES_FILES, SUBMODULES_KEYS}`; and
//! `effects::load`.
//!
//! Kept cross-feature reads and calls, left as they are because this slice
//! only moves code:
//! - it reads and writes `files` (its rows, `loaded` and cursor) and
//!   `branch` (files);
//! - it reads `repo` (repos);
//! - it calls core `set_repo`, `switch_repo` and `select` (`model.rs`);
//! - `enter_submodule` and `leave_submodule` call core `reload_repo`
//!   (`reload.rs`), which reloads the status, branches, log and stashes
//!   through the refresh plan.
//!
//! What stays in core: `ParentRepo` and `AppState::repo_stack`, which repos
//! (`ParkedRepo`) and worktrees also use; the `key_focus` branch; and
//! `render_submodules` in `popup_view.rs`.

pub(crate) mod effects;
mod intent;
pub(crate) mod keys;
mod list;
mod update;

pub(crate) use intent::SubmodulesIntent;
pub(crate) use list::SubmoduleList;
pub(crate) use update::update;

use crate::git::path::RepoPath;
use crate::git::submodule::Submodule;
use crate::model::AppState;
use crate::popup::Popup;

/// The Submodules feature's queries, for core and other features.
impl AppState {
    /// The submodule under the popup's cursor.
    pub fn selected_submodule(&self) -> Option<&Submodule> {
        self.popups.iter().rev().find_map(|popup| match popup {
            Popup::Submodules(list) => list.selected(),
            _ => None,
        })
    }

    /// The paths from the launch repository to the current one.
    pub(crate) fn nested(&self) -> Vec<RepoPath> {
        self.repo_stack.iter().map(|p| p.path.clone()).collect()
    }
}
