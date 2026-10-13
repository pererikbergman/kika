//! Stash: the Stash panel's list and an entry's files, apply, pop, drop
//! (one entry or a range) and rename, stashing from the Stash menu and
//! the Files panel, and the effects that load the list and an entry's
//! files. Chunks 19 (`stash.*`, `stash_files.*`), 39 (`files.stash`) and
//! 60 (`drop_stash.*`) are its key bindings, and `Effect::LoadStashes`
//! and `Effect::LoadStashFiles` are its effects. Its git ops run through
//! the core op slot.
//!
//! Its `pub(crate)` API:
//! - the state types `StashList` and `StashDraft` (`list.rs`). The list
//!   itself is a core `FilteredList<StashEntry>` in `StashList::list`;
//! - `keys::{STASH_KEYS, STASH_FROM_FILES, STASH_DROP_CONFIRM}` and the
//!   guards `stash_action_disabled`, `drop_stash_disabled`,
//!   `stash_files_disabled`, the Stash menu's `stash_*_disabled`, and
//!   `STASH_APART`, `STASH_RENAME_TAKEN` and `STASH_DELETED_TAKEN`;
//! - the handlers in `update.rs`, `reload_stashes` among them (core
//!   reload in `reload.rs`, `refs_loaded` and `test_support` call it), and
//!   `files_open`;
//! - `effects::{load, load_files}` and `view::render_stash_panel`.
//!
//! Kept cross-feature reads:
//! - stashing from the Files panel reads `files` and `files_marked()`,
//!   and clears `files_anchor`;
//! - the handlers read the op slot, `focus`, `context`, `generation` and
//!   `popups`, and write `diff` and `notice`;
//! - `files_open` reads `commits.open`. It answers whether the Diff pane
//!   belongs to a commit's or a stash's files, and core dispatch calls
//!   it. It stays here rather than in `model.rs` so that hubs only
//!   shrink; `commits.open` is the commits feature's state.
//!
//! What stays in core, and why:
//! - `Focus::Stash`/`SidePanel::Stash`, `KeyScope::Stash`/`StashFiles`/
//!   `DropStash`, `PromptKind::StashMessage`/
//!   `RenameStash`, `RangeKind::Stashes` and `range_delete.rs`: shared
//!   core dispatch, as are the stash branches of `key_focus`,
//!   `open_filter`, the filter apply, `list_escape` and `range_toggle`;
//! - the refresh plan (`Scope::Stash`), the repository switch reset, and
//!   `failure_title`/`failure_next`;
//! - the `STASH` menu table (`menu.rs`), the confirm question module
//!   (`confirm_question.rs`) the drop question is built on, and the
//!   bottom bar (`view.rs`).
//!
//! Its git code:
//! - `git.rs`: the list and an entry's files, `StashMode` and the argv
//!   builders and checks `op.rs` delegates to: `push_command`,
//!   `stash_command`, `store_command`, `verify_stash`,
//!   `staged_stash_supported` and `rename_stash`;
//! - `push.rs`: stashing selected files, `stash_file_for` and
//!   `stash_files_for` (which keys and handlers call), and
//!   `push_with_fixups`, which `run_op` delegates to.
//!
//! `StashEntry`, `StashFile`, `StashMode` and `StashRef` are re-exported
//! for core, which names them in `GitOp`, `AppIntent` and `Effect`.

pub(crate) mod effects;
pub(crate) mod git;
pub(crate) mod keys;
pub(crate) mod list;
pub(crate) mod push;
mod update;
pub(crate) mod view;

pub(crate) use git::{StashEntry, StashFile, StashMode, StashRef};
pub(crate) use list::{StashDraft, StashList};
// Core's popup tests build the question as the feature asks it.
#[cfg(test)]
pub(crate) use update::drop_stash_question;
