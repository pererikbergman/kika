//! Commit: the commit popup. Opening it to commit or to amend `HEAD`,
//! reading `HEAD`'s message for the amend, the message editor (`editor.rs`),
//! cancelling, discarding and resuming a typed message, submitting it
//! through the terminal probe, the message back from the external editor,
//! and leaving the popup when git is done. `view.rs` draws the popup and
//! `keys.rs` holds its two key chunks and their guards.
//! `Effect::LoadHeadMessage` and `Effect::ProbeCommit` are its effects, and
//! the `git commit` argv is its git code. The commit itself runs through
//! the core op slot.
//!
//! Its intents are nested (ADR-0002): core wraps [`CommitIntent`] in
//! `AppIntent::Commit`, and its one `update` arm calls [`update`] here.
//!
//! Its `pub(crate)` API:
//! - `CommitEditor`, the typed message, which `CommitFlow` holds;
//! - the intent enum [`CommitIntent`] and the free function [`update`];
//! - the handlers in `update.rs`: `open_commit`, `open_amend`,
//!   `head_message_loaded`, `submit_commit`, `commit_probed`,
//!   `commit_message_edited`, `cancel_commit`, `discard_commit`,
//!   `resume_commit`, `interrupt_commit`, `dismiss_commit_error`, and
//!   `end_commit`, which core `op_finished` calls;
//! - `view::render_commit`, which core `render` calls;
//! - `keys::{COMMIT_FROM_FILES, COMMIT_KEYS}`, which `CHUNKS` places, and
//!   `keys::{commit_disabled, amend_disabled}`, which the core actions menu
//!   uses too;
//! - `effects::{load_head_message, probe}`;
//! - `git::{MessageSource, commit_command}`, which `op.rs` and the
//!   terminal hand-off (`handoff.rs`) build the commit with.
//!
//! Kept cross-feature and core reads: the handlers read `files` (whether
//! anything is staged), `context`, `generation`, the op slot and
//! `commit_hooks_detached`, and change the op slot through its methods (a
//! stale probe frees it), `commit_on_terminal`, `notice` and `popups` (the
//! error popup).
//!
//! What stays in core, and why:
//! - `CommitFlow` (`model.rs`): `AppState` holds it, and tests in
//!   `commits/rewrite` and `pull_requests` name it, as do `test_support.rs`
//!   and the end-to-end tests;
//! - `git/comment_prefix.rs`: `commits/rewrite` and `git/rebase_plan.rs`
//!   use it too;
//! - `handoff.rs`: it also runs the fixup commit for `commits/rewrite`;
//! - `ProbeCommit`'s dispatch: the rewrite fixup is probed too, so core
//!   `Runtime` calls `effects::probe` and rewrite never imports commit;
//! - `COMMIT_WIDTH` (`view.rs`): core popups use the same width;
//! - `char_len` and `byte_at` (`popup.rs`): the core one-line prompt edits
//!   its text with them too;
//! - the popup's question box, `render_question`, and the text helpers
//!   `clip`, `wrap` and `tail` (`view.rs`): shared drawing code;
//! - `op_finished`, `start_op_as`, `bisect_refuses` and the `GitOp::Commit`
//!   arm of `op_command`: shared op-slot code (`op_slot.rs`, `op.rs`).

pub(crate) mod editor;
pub(crate) mod effects;
pub(crate) mod git;
mod intent;
pub(crate) mod keys;
mod update;
pub(crate) mod view;

pub(crate) use intent::CommitIntent;
pub(crate) use update::update;

pub(crate) use editor::CommitEditor;
