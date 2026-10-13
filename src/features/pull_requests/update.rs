//! The pull requests feature's `update`: core's one
//! `AppIntent::PullRequests` arm lands here and dispatches to the handlers,
//! which stay in their own files.

use super::PullRequestsIntent;
use crate::effect::Effect;
use crate::model::AppState;
use crate::popup::ErrorPopup;

/// The pull requests feature's half of `update`: core's one
/// `AppIntent::PullRequests` arm lands here. A free function, so its name
/// cannot clash with handlers.
pub(crate) fn update(state: &mut AppState, intent: PullRequestsIntent) -> Vec<Effect> {
    match intent {
        PullRequestsIntent::RepoNone { tag } => {
            if tag.generation == state.generation {
                state.forge.repo_none(tag);
            }
        }
        PullRequestsIntent::Cached { tag, repo, badges } => {
            if tag.generation == state.generation {
                state.forge.cached(tag, repo, badges);
            }
        }
        PullRequestsIntent::Fetched {
            tag,
            result,
            limited_until,
        } => {
            return state.pull_requests_fetched(tag, result, limited_until);
        }
        PullRequestsIntent::OpenForgeUrl => return state.open_forge_url(),
        PullRequestsIntent::CopyForgeUrl => return state.copy_forge_url(),
        PullRequestsIntent::Refresh => return state.refresh_pull_requests(),
        PullRequestsIntent::OpenUrlFailed(message) => state.push_error(ErrorPopup::new(
            "Could not open the browser",
            &message,
            "Check forge.browser in the global config.",
        )),
        PullRequestsIntent::CreateStart => state.create_pr_start(),
        PullRequestsIntent::PickNext => state.move_pick_cursor(1),
        PullRequestsIntent::PickPrev => state.move_pick_cursor(-1),
        PullRequestsIntent::PickSelect => return state.pick_select(),
        PullRequestsIntent::Titled(text) => state.pull_request_titled(&text),
        PullRequestsIntent::ConfirmCreate => return state.confirm_create_pull_request(),
        PullRequestsIntent::Created { tag, result } => {
            return state.pull_request_created(tag, result);
        }
    }
    Vec::new()
}
