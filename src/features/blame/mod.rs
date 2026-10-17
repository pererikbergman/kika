//! Blame: the blame of one file in the Diff pane's place (its load, cursor
//! and page), and the commit opened from a line (its load, scroll and
//! close). The feature owns the state (`state.rs`), its `update` handlers
//! (`update.rs`), its key bindings and guards (`keys.rs`), its two render
//! functions (`view.rs`) and the loads behind `Effect::LoadBlame` and
//! `Effect::LoadCommit` (`effects.rs`). `kika_git::blame` stays in the `kika_git`
//! crate as shared plumbing.
//!
//! Its intents are nested (ADR-0002): core wraps [`BlameIntent`] in
//! `AppIntent::Blame`, and its one `update` arm calls [`update`] here.
//!
//! Its `pub(crate)` API: the type [`BlameView`]; the intent enum
//! [`BlameIntent`] and the free function [`update`]; the handlers `open_blame`,
//! `blame_loaded`, `blame_binary`, `with_blame`, `open_blame_commit`,
//! `commit_loaded`, `close_blame_commit`, `scroll_commit` and `close_blame`
//! on `AppState`; the chunks `keys::{BLAME_FILES, BLAME_KEYS, BLAME_COMMIT}`;
//! `view::{render_blame, render_commit_view}`; and
//! `effects::{load, load_commit}`.
//!
//! Kept cross-feature reads and calls, left as they are because this slice
//! only moves code:
//! - `open_blame` and `blame_disabled` read the Files panel's selected row
//!   (`files.selected_row()`, files);
//! - `close_blame` calls core `render_if_stale` (`model.rs`).
//!
//! Others read blame state directly: `command_target` (custom commands),
//! core list input and search, `blame_open`, `pane_search_disabled` and the
//! bar check in `view.rs`.
//!
//! What stays in core: blame search (`InputTarget::BlameSearch` and
//! `InputTarget::BlameCommitSearch`, the `Focus::Blame` branches of the
//! input, search, step and escape code, `pane_search_disabled`,
//! `pane_step_disabled` and their tests); `blame_open`, `BLAME_OPEN` and
//! `diff_hidden`, which guard the diff keys; the `KeyScope::Blame` and
//! `KeyScope::BlameCommit` variants; the `Focus::Blame` variant and its
//! `panel_focus` branch; the `Effect` variants; and the
//! `Focus::Blame` dispatch in `view.rs::render`.

pub(crate) mod effects;
mod intent;
pub(crate) mod keys;
mod state;
mod update;
pub(crate) mod view;

pub(crate) use intent::BlameIntent;
pub(crate) use update::update;

pub(crate) use state::BlameView;
#[cfg(test)]
pub(crate) use state::CommitView;
