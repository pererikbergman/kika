//! Commits: the Commits panel and its sub-features. The feature owns the
//! panel's state (`panel.rs`), its graph (`graph.rs`) and rendering
//! (`view.rs`), paging the log, the markers and branch tips, switching to
//! the Reflog tab and the selected commit's files (`update.rs`, with the
//! list's state in `commit_files.rs`), checkout, revert and reset of the
//! selected commit (`actions.rs`), the log filter by path or author
//! (`log_query.rs`), its key bindings and guards (`keys.rs`), the I/O
//! behind `Effect::LoadCommits`, `LoadMarkers`, `LoadBranchTips` and
//! `LoadCommitFiles` (`effects.rs`) and reading the branch tips and the
//! detached checkout's argv (`git.rs`). Its children own the reflog, copy,
//! custom patch, compare, rewrite and bisect.
//!
//! Its `pub(crate)` API:
//! - the types [`CommitsPanel`], [`CommitsTab`] and [`CommitFilesList`],
//!   and [`PAGE`];
//! - the handlers in `update.rs`: `first_commits_page`, `more_commits`,
//!   `switch_commits_tab`, `commits_loaded`, `markers_loaded`,
//!   `branch_tips_loaded`, `open_commit_files`, `commit_files_loaded`,
//!   `move_commit_file` and `commit_file_diff`; in `actions.rs`:
//!   `commit_for_action`, `ask_checkout_commit`, `confirm_checkout_commit`,
//!   `revert_commit`, `reset_to` and `confirm_reset_hard`; and in
//!   `log_query.rs`: `open_log_filter`, `accept_log_query`,
//!   `set_log_query` and `log_of_file`;
//! - the chunks `keys::{COMMITS_KEYS, COMMITS_SWITCH_TAB,
//!   COMMITS_FILES_NAV, COMMITS_FILES_BACK, COMMITS_RESET_HARD_CONFIRM,
//!   COMMITS_LOG_FILTER, COMMITS_FILES_LOG}`, the guards
//!   `keys::{commit_action_disabled, checkout_commit_disabled,
//!   revert_disabled, reset_disabled}` and `keys::{TIPS_UNKNOWN,
//!   tips_failed}`;
//! - `effects::{load, load_markers, load_tips, load_files}`;
//! - `view::{render_commits_panel, PATCH_MARK}`, `git::Tip` and
//!   `git::detached_checkout`.
//!
//! Kept cross-feature reads of `state.commits`: `branches/tags` (`rows`,
//! `selected`), `stash`, `review` and `diff/renderers` (`open`) and
//! `pull_requests` (`rows`); in core, `bisect.rs`, `popup_view.rs` and
//! `menu.rs`. The handlers read `files`, `selected`, `diff`, `popups` and
//! `diff_options` (core) and call core's `close_commit_files`,
//! `focus_panel`, `background_tag`, `stream_diff`, `start_op`,
//! `run_op_where_it_belongs` and `confirm_refused`.
//!
//! What stays in core, and why: `reload_commits`, which repos and
//! submodules call; `close_commit_files`, which calls `select` and which
//! core's `focus_panel` calls; `reload_open_file_diff` and
//! `set_diff_options`, which serve the stash's files too; the commit
//! files filter's input arm and the other shared dispatch branches
//! (`key_focus`, `open_filter`, the filter apply and `list_escape`);
//! `commit_source_disabled`, `no_commit`, `in_progress_disabled` and
//! `list_guard`, which core's own guards and other features' share;
//! `REPO_LOCKED`; the confirm popups (`ConfirmKind::DetachedCheckout`,
//! `Popup::ResetHard`) and their views; and `model/confirm_guard.rs`,
//! which re-checks every move question's guard on `y`. `op.rs` keeps the
//! `Reset` and `Revert` argv arms of `op_command`.
//!
//! This module also owns the stacked-branch check that every history
//! rewrite runs before it starts: [`with_stacked_branches`] with the
//! [`Crossing`]s a plan makes. `rewrite`'s `log_plan`, `autosquash_plan`
//! and `range_log_plan` call it, and so does the custom patch's
//! `patch_plan`. The history rewrites themselves, with the stopped
//! rebase's todo list, are in `rewrite`.

mod actions;
pub(crate) mod bisect;
mod commit_files;
pub(crate) mod compare;
pub(crate) mod copy;
pub(crate) mod custom_patch;
pub(crate) mod effects;
pub(crate) mod git;
mod graph;
pub(crate) mod keys;
mod log_query;
mod panel;
pub(crate) mod reflog;
pub(crate) mod rewrite;
mod update;
pub(crate) mod view;

pub(crate) use commit_files::CommitFilesList;
pub(crate) use panel::{CommitsPanel, CommitsTab, PAGE};

use crate::git::rebase_plan::{self, RebasePlan};
use crate::git::status::Head;
use crate::git::version;
use crate::keymap::Reason;
use crate::model::AppState;
use keys::{TIPS_UNKNOWN, tips_failed};

/// A log commit a plan moves, `moved`, and the rows it moves past:
/// `len` of them from `from`. A branch on one of those would change what
/// it holds.
pub(crate) struct Crossing {
    pub(crate) moved: usize,
    pub(crate) from: usize,
    pub(crate) len: usize,
}

/// `plan` as it will run, checked against the branches stacked on its
/// commits. With git 2.38 or newer they move along (`--update-refs`), so
/// only a `crossings` move that would change what one holds is refused;
/// with an older git any stacked branch would be left behind, so the plan
/// is refused.
pub(crate) fn with_stacked_branches(
    state: &AppState,
    mut plan: RebasePlan,
    crossings: &[Crossing],
) -> Result<RebasePlan, Reason> {
    let commits = &state.commits;
    let rows = commits.rows.as_slice();
    plan.update_refs = state.git_has(version::UPDATE_REFS);
    let head = match state.branch.as_ref().map(|b| &b.head) {
        Some(Head::Branch(name)) => Some(format!("refs/heads/{name}").into_bytes()),
        _ => None,
    };
    let stacked = |tips, from: usize, len| git::stacked(&rows[from..], len, tips, head.as_deref());
    if !plan.update_refs {
        let tips = match (&commits.tips, &commits.tips_error) {
            (Some(tips), _) => tips,
            (None, Some(error)) => return Err(tips_failed(error).into()),
            (None, None) => return Err(TIPS_UNKNOWN.into()),
        };
        if let Some(name) = stacked(tips, 0, plan.steps.len()).first() {
            return Err(git::left_behind(name).into());
        }
    } else if let Some(tips) = &commits.tips {
        // Unknown tips are left to the sequence editor, which checks too.
        for crossing in crossings {
            if let Some(name) = stacked(tips, crossing.from, crossing.len).first() {
                let oid = &rows[crossing.moved].oid;
                return Err(rebase_plan::crossing(oid, name).into());
            }
        }
    }
    Ok(plan)
}
