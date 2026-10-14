//! Reflog: the Commits panel's Reflog tab, undo (Ctrl+Z) and redo
//! (Ctrl+R). It reads `HEAD`'s reflog, plans the step that reverses or
//! replays the last action, asks first, and builds the `GitOp::Travel`
//! argv. Chunks 9 (`workspace.undo`, `workspace.redo`), 15 (`reflog.*`),
//! 65 (`travel.*`) and 75 (`reflog.filter_path`, `reflog.filter_author`)
//! are its key bindings, and `Effect::LoadReflog` is its effect. Its git
//! op runs through the core op slot.
//!
//! Its `pub(crate)` API:
//! - the intent enum `ReflogIntent`, nested in `CommitsIntent::Reflog`,
//!   and the free function `update` that `commits::update` passes it to;
//! - the state types `ReflogList` and `ReflogHead` (`list.rs`), `Reflog`
//!   (`git.rs`) and the planner's `Direction`, `Step` and `Expected`
//!   (`undo.rs`);
//! - `keys::{REFLOG_WORKSPACE, REFLOG_KEYS, REFLOG_TRAVEL_CONFIRM,
//!   REFLOG_LOG_FILTER}`;
//! - the handlers in `update.rs`: `load_reflog`, `reflog_loaded`,
//!   `travel` and `confirm_travel`;
//! - `effects::load`, `view::reflog_lines`, and `git::travel_command`
//!   and `git::read_head` for `op.rs`.
//!
//! Kept cross-feature reads:
//! - the handlers read and write `commits.reflog` and read `commits.tab`
//!   (the parent `commits` panel);
//! - `plan_travel` reads `branches.list.rows` and `files`;
//! - `head_unborn` and `undo_disabled` read `branch`, the op slot and
//!   `repo`; `travel` and `plan_travel` push onto `popups`, and the
//!   handlers check `context`.
//!
//! What stays elsewhere, and why:
//! - `CommitsTab`, `COMMITS_SWITCH_TAB` and `switch_commits_tab` are in
//!   the parent `commits`: the tabs belong to the Commits panel;
//! - in core: `KeyScope::Reflog`/`KeyScope::Travel`, the confirm question
//!   module (`confirm_question.rs`) the undo/redo question is built on,
//!   and the reflog branches of `key_focus`, `open_filter`, the filter
//!   apply and `list_escape`: shared core dispatch;
//! - the `GitOp::Travel` variant, the `HEAD_MOVED` check in `run_op`,
//!   `failure_title`/`failure_next` and the refresh plan (`refresh.rs`).
//!
//! The reflog fixtures live here, in `fixtures/`; core `view.rs` and
//! `test_support.rs` tests read them too.

pub(crate) mod effects;
pub(crate) mod git;
mod intent;
pub(crate) mod keys;
pub(crate) mod list;
pub(crate) mod undo;
mod update;
pub(crate) mod view;

pub(crate) use git::Reflog;
pub(crate) use intent::ReflogIntent;
pub(crate) use list::ReflogList;
pub(crate) use undo::{Direction, Expected, Step};
pub(crate) use update::update;
// Core's popup tests build the question as the feature asks it.
#[cfg(test)]
pub(crate) use update::travel_question;
