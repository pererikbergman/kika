//! The Diff pane's features. This module owns the streamed preview
//! (`stream`: one `git diff` at a time, read only as far as the pane asks),
//! taking its chunks (`diff_loaded`), chunk 5 (`keys::DIFF_WORKSPACE`, the
//! context and whitespace keys with their guards) and the effects
//! `LoadDiff`, `DiffDemand`, `CancelDiff` and `DiffLoadingDelay`.
//!
//! Its `pub(crate)` API: `DiffStreamer`, the handler `diff_loaded`
//! (`update.rs`), `keys::DIFF_WORKSPACE` and `effects::loading_delay`.
//!
//! Kept cross-feature reads: none. Its code reads only core state
//! (`generation`, `diff`, `focus`, `diff_options`).
//!
//! What stays in core `model.rs`, and why:
//! - `set_diff_options` reads `commits.open`, `stash.open`, `selected`,
//!   `files` and `diff_options`, and calls stash's `stash_file_diff` and
//!   core `select`/`commit_file_diff`. A feature does not call a sibling's
//!   behaviour, so core keeps it;
//! - `stream_diff` and `remap`, because `stash`, `review` and
//!   `custom_patch` call them;
//! - `blame_open`/`BLAME_OPEN` in `keymap.rs` and `DIFF_LOADING_DELAY` in
//!   `effect.rs`, which this feature imports.
//!
//! The child `renderers` draws the pane with an external pager or tool.

pub(crate) mod effects;
pub(crate) mod keys;
pub(crate) mod renderers;
pub(crate) mod stream;
mod update;

pub(crate) use stream::DiffStreamer;
