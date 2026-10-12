//! The Diff pane's features. This module owns the streamed preview
//! (`stream`: one `git diff` at a time, read only as far as the pane asks),
//! taking its chunks (`diff_loaded`), chunk 5 (`keys::DIFF_WORKSPACE`, the
//! context and whitespace keys with their guards) and the effects
//! `LoadDiff`, `DiffDemand`, `CancelDiff` and `DiffLoadingDelay`.
//!
//! It also owns hunk and line staging: chunks 29, 32 and 34
//! (`keys::DIFF_FILES`, `keys::DIFF_KEYS`, `keys::DIFF_LINES`) with their
//! guards `hunk_disabled`, `side_disabled` and `files_hunks_disabled`; the
//! staging handlers in `update.rs`; and `effects::load_patch`, the I/O
//! behind `LoadPatch`. The ops it starts (`ApplyCached`, `Stage`,
//! `Unstage`) stay core `GitOp`s, built and run by `op.rs`.
//!
//! Its `pub(crate)` API: `DiffStreamer`; the handlers `diff_loaded`,
//! `patch_loaded`, `with_lines`, `next_hunk`, `prev_hunk`,
//! `enter_line_mode`, `toggle_selection_stage` and `toggle_diff_side`
//! (`update.rs`); the chunks `keys::DIFF_WORKSPACE`, `keys::DIFF_FILES`,
//! `keys::DIFF_KEYS` and `keys::DIFF_LINES`; and `effects::loading_delay`
//! and `effects::load_patch`.
//!
//! Kept cross-feature reads: `hunk_disabled` reads the Files panel's
//! `files` and `selected`, to refuse a conflicted or renamed file. The rest
//! reads core state (`generation`, `diff`, `focus`, `diff_options`,
//! `op_seq`).
//!
//! What stays in core, and why:
//! - `set_diff_options` reads `commits.open`, `stash.open`, `selected`,
//!   `files` and `diff_options`, and calls stash's `stash_file_diff` and
//!   core `select`/`commit_file_diff`. A feature does not call a sibling's
//!   behaviour, so core keeps it;
//! - `stream_diff` and `remap`, because `stash`, `review` and
//!   `custom_patch` call them;
//! - `focus_diff`, because it calls `focus_commit_diff` (custom_patch),
//!   `files_open` (stash) and `open_conflict` (conflicts);
//! - `toggle_hunk_stage` and `toggle_lines_stage`, because a commit's
//!   change goes to custom_patch's `patch_toggle_lines`. They call the
//!   moved `toggle_selection_stage` for the rest;
//! - `load_patch` and the `display_only` method, which custom_patch and the
//!   renderers call;
//! - in `keymap.rs`: `DISPLAY_ONLY`, `WHITESPACE_HIDDEN`, `NOT_IN_COMMIT`
//!   and the guards `display_only`, `whitespace_hidden`, `in_commit`,
//!   `no_file_selected`, `open_disabled`, `discard_disabled` and
//!   `edit_hunk_disabled`, because core guards, custom_patch or the
//!   renderers use them too; and `blame_open`/`BLAME_OPEN`;
//! - discard (`ask_discard`, `discard_confirm`) and the `ApplyWorktree`
//!   tests in `op.rs`, which exercise the discard op;
//! - `DIFF_LOADING_DELAY` in `effect.rs`, which this feature imports.
//!
//! The child `renderers` draws the pane with an external pager or tool.

pub(crate) mod effects;
pub(crate) mod keys;
pub(crate) mod renderers;
pub(crate) mod stream;
mod update;

pub(crate) use stream::DiffStreamer;
