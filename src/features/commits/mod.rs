//! The Commits panel's sub-features, and what more than one of them
//! needs. It owns the stacked-branch check that every history rewrite
//! runs before it starts: [`with_stacked_branches`] with the
//! [`Crossing`]s a plan makes. Core `log_plan`, `autosquash_plan` and
//! `range_log_plan` call it, and so does the custom patch's
//! `patch_plan`.

pub(crate) mod bisect;
pub(crate) mod compare;
pub(crate) mod copy;
pub(crate) mod custom_patch;
pub(crate) mod reflog;

use crate::git::rebase_plan::{self, RebasePlan};
use crate::git::status::Head;
use crate::git::{branch_tips, version};
use crate::keymap::{Reason, TIPS_UNKNOWN, tips_failed};
use crate::model::AppState;

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
    let stacked =
        |tips, from: usize, len| branch_tips::stacked(&rows[from..], len, tips, head.as_deref());
    if !plan.update_refs {
        let tips = match (&commits.tips, &commits.tips_error) {
            (Some(tips), _) => tips,
            (None, Some(error)) => return Err(tips_failed(error).into()),
            (None, None) => return Err(TIPS_UNKNOWN.into()),
        };
        if let Some(name) = stacked(tips, 0, plan.steps.len()).first() {
            return Err(branch_tips::left_behind(name).into());
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
