//! Commit: the commit popup's flow. Opening it to commit or to amend
//! `HEAD`, reading `HEAD`'s message for the amend, cancelling, discarding
//! and resuming a typed message, submitting it through the terminal probe,
//! the message back from the external editor, and leaving the popup when
//! git is done. `Effect::LoadHeadMessage` and `Effect::ProbeCommit` are its
//! effects, and the `git commit` argv is its git code. The commit itself
//! runs through the core op slot.
//!
//! Its `pub(crate)` API:
//! - the handlers in `update.rs`: `open_commit`, `open_amend`,
//!   `head_message_loaded`, `submit_commit`, `commit_probed`,
//!   `commit_message_edited`, `cancel_commit`, `discard_commit`,
//!   `resume_commit`, `interrupt_commit`, `dismiss_commit_error`, and
//!   `end_commit`, which core `op_finished` calls;
//! - `effects::{load_head_message, probe}`;
//! - `git::{MessageSource, commit_command}`, which `op.rs` and the
//!   terminal hand-off (`handoff.rs`) build the commit with.
//!
//! Kept cross-feature and core reads: the handlers read `files` (whether
//! anything is staged), `context`, `generation`, `op_seq` and
//! `commit_hooks_detached`, and write `busy`, `op_seq` (a stale probe frees
//! it), `commit_on_terminal`, `notice` and `popups` (the error popup).
//!
//! What stays in core, and why:
//! - `CommitFlow` (`model.rs`): tests in `commits/rewrite` and
//!   `pull_requests` name it, as do `keymap.rs`, `view.rs`,
//!   `test_support.rs` and the end-to-end tests;
//! - `git/comment_prefix.rs`: `commits/rewrite` and `git/rebase_plan.rs`
//!   use it too;
//! - `handoff.rs`: it also runs the fixup commit for `commits/rewrite`;
//! - `ProbeCommit`'s dispatch: the rewrite fixup is probed too, so core
//!   `Runtime` calls `effects::probe` and rewrite never imports commit;
//! - `commit_disabled`, `amend_disabled` and `commit_editor_disabled`
//!   (`keymap.rs`), `commit_editor.rs`, the popup's rendering and its key
//!   chunks, until part 2 moves them;
//! - the inline `ConfirmStageAll`, `ToggleCommitField`, `ToggleNoVerify` and
//!   `EditCommitExternally` arms, which are not verbatim moves yet;
//! - `op_finished`, `start_op_as`, `bisect_refuses` and the `GitOp::Commit`
//!   arm of `op_command`: shared op-slot code.

pub(crate) mod effects;
pub(crate) mod git;
mod update;
