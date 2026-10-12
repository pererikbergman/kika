//! Blame: the blame of one file in the Diff pane's place (its load, cursor
//! and page), and the commit opened from a line (its load, scroll and
//! close). The feature owns the state (`state.rs`), its `update` handlers
//! (`update.rs`), its key bindings and guards (`keys.rs`), its two render
//! functions (`view.rs`) and the loads behind `Effect::LoadBlame` and
//! `Effect::LoadCommit` (`effects.rs`). `src/git/blame.rs` stays in `src/git`
//! as shared plumbing.
//!
//! Its `pub(crate)` API: the type [`BlameView`]; the handlers `open_blame`,
//! `blame_loaded`, `with_blame`, `open_blame_commit`, `commit_loaded`,
//! `close_blame_commit`, `scroll_commit` and `close_blame` on `AppState`; the
//! chunks `keys::{BLAME_FILES, BLAME_KEYS, BLAME_COMMIT}`;
//! `view::{render_blame, render_commit_view}`; and
//! `effects::{load, load_commit}`.
//!
//! Kept cross-feature reads and calls, left as they are because this slice
//! only moves code:
//! - `open_blame` and `blame_disabled` read `selected` and `files` (files);
//! - `close_blame` calls `render_if_stale` (diff renderers).
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
//! `panel_focus` branch; the `AppIntent` and `Effect` variants; and the
//! `Focus::Blame` dispatch in `view.rs::render`.

pub(crate) mod effects;
pub(crate) mod keys;
mod state;
mod update;
pub(crate) mod view;

pub(crate) use state::BlameView;
#[cfg(test)]
pub(crate) use state::CommitView;
