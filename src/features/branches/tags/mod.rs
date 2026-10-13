//! Tags: the Branches panel's Tags tab, making a tag from the Commits panel,
//! and deleting and pushing the selected tag. The feature owns the tab's
//! list and the tag a prompt will make (`list.rs`), its `update` handlers
//! (`update.rs`), its key bindings and guards (`keys.rs`), the tab's body
//! (`view.rs`), the I/O behind `Effect::LoadTags` (`effects.rs`) and the
//! argv for making and deleting a tag (`git.rs`).
//!
//! Its `pub(crate)` API:
//! - the intent enum `TagsIntent`, nested in `BranchesIntent::Tags`, and
//!   the free function `update` that `branches::update` passes it to;
//! - the types [`TagsList`] and [`PendingTag`];
//! - the handlers in `update.rs`: `move_tag`, `tags_loaded`, `ask_tag_name`,
//!   `tag_named`, `tag_messaged`, `create_tag`, `ask_delete_tag`,
//!   `confirm_delete_tag`, `push_tag` and `tags_popup_closed`;
//! - the chunks `keys::{TAGS_KEYS, TAGS_DELETE_CONFIRM}` and the guards
//!   `keys::{tag_commit_disabled, delete_tag_disabled}`;
//! - `effects::load`;
//! - `git::{create_command, delete_command}`;
//! - `view::list_body`.
//!
//! Kept cross-feature reads and calls:
//! - `commits.list.rows` and `commits.list.selected` (commits), to take the commit a
//!   new tag points at;
//! - `push_tag` calls `start_remote` (sync) on `AppState`, an accepted
//!   cross-feature call listed in ADR-0001's Outcome;
//! - `branches.*` (the parent, `features/branches`): the tab, its tags and
//!   the pending tag, and `load_branch_log`.
//!
//! What stays in core: the push and selection guards `push_tag_disabled`,
//! `push_tag_remote` and `tag_selected_disabled` in `keymap.rs`, which
//! sync's push uses too; `src/git/tags.rs`; the `GitOp` tag variants with
//! their steps, refusals and refresh plan; and the tag branches of shared
//! dispatch: the range delete arm, `key_focus`, `open_filter`,
//! `toggle_range`, the `TagName` prompt submit and `range_delete.rs`. The
//! delete question is built here (`delete_tag_question`) on the core
//! `confirm_question.rs`. The parent owns `BranchesPanel` and the range
//! question's builder (`lists.rs`).

pub(crate) mod effects;
pub(crate) mod git;
mod intent;
pub(crate) mod keys;
mod list;
mod update;
pub(crate) mod view;

pub(crate) use intent::TagsIntent;
pub(crate) use list::{PendingTag, TagsList};
pub(crate) use update::update;
// Core's popup tests build the question as the feature asks it.
#[cfg(test)]
pub(crate) use update::delete_tag_question;
