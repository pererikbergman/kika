//! Stash: the Stash panel's list and an entry's files, apply, pop, drop
//! (one entry or a range) and rename, stashing from the Stash menu and
//! the Files panel, and the effects that load the list and an entry's
//! files. Chunks 19 (`stash.*`, `stash_files.*`), 39 (`files.stash`) and
//! 60 (`drop_stash.*`) are its key bindings, and `Effect::LoadStashes`
//! and `Effect::LoadStashFiles` are its effects. Its git ops run through
//! the core op slot.
//!
//! Its `pub(crate)` API:
//! - the state types `StashList` and `StashDraft` (`list.rs`);
//! - `keys::{STASH_KEYS, STASH_FROM_FILES, STASH_DROP_CONFIRM}` and the
//!   guards `stash_action_disabled`, `drop_stash_disabled`,
//!   `stash_files_disabled`, the Stash menu's `stash_*_disabled`, and
//!   `STASH_APART`, `STASH_RENAME_TAKEN` and `STASH_DELETED_TAKEN`;
//! - the handlers in `update.rs`, `reload_stashes` among them (core
//!   refresh and `test_support` call it), and `files_open`;
//! - `effects::{load, load_files}` and `view::render_stash_panel`.
//!
//! Kept cross-feature reads:
//! - stashing from the Files panel reads `files` and `files_marked()`,
//!   and clears `files_anchor`;
//! - the handlers read `op_seq`, `focus`, `context`, `generation` and
//!   `popups`, and write `diff` and `notice`;
//! - `files_open` reads `commits.open`. It answers whether the Diff pane
//!   belongs to a commit's or a stash's files, and core dispatch calls
//!   it. It stays here rather than in `model.rs` so that hubs only
//!   shrink; `commits.open` is core state (`src/commits.rs`).
//!
//! What stays in core, and why:
//! - `Focus::Stash`/`SidePanel::Stash`, `KeyScope::Stash`/`StashFiles`/
//!   `DropStash`, `Popup::DropStash`, `PromptKind::StashMessage`/
//!   `RenameStash`, `RangeKind::Stashes` and `model/branch_lists.rs`: shared
//!   core dispatch, as are the stash branches of `key_focus`,
//!   `open_filter`, the filter apply, `list_escape` and `range_toggle`;
//! - the refresh plan (`Scope::Stash`), the repository switch reset, and
//!   `failure_title`/`failure_next`;
//! - the `STASH` menu table (`menu.rs`), the drop question's rendering
//!   (`popup_view.rs`) and the bottom bar (`view.rs`).
//!
//! Its git reads and argv builders (`git/stash.rs` and the stash code in
//! `op.rs`) arrive as `git.rs` in the next slice.

pub(crate) mod effects;
pub(crate) mod keys;
pub(crate) mod list;
mod update;
pub(crate) mod view;

pub(crate) use list::{StashDraft, StashList};
