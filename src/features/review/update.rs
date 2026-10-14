//! Review mode's `update`: core's one `AppIntent::Review` arm lands here
//! and dispatches to the handlers, which stay in their own files.

use super::ReviewIntent;
use crate::effect::Effect;
use crate::model::AppState;

/// Review mode's half of `update`: core's one `AppIntent::Review` arm lands
/// here. A free function, so its name cannot clash with handlers.
pub(crate) fn update(state: &mut AppState, intent: ReviewIntent) -> Vec<Effect> {
    match intent {
        ReviewIntent::Enter => return state.enter_review(),
        ReviewIntent::NextQueue => state.step_review_queue(false),
        ReviewIntent::PrevQueue => state.step_review_queue(true),
        ReviewIntent::OpenPr => return state.open_pr(),
        ReviewIntent::CloseDetail => return state.close_review(),
        ReviewIntent::Refresh => return state.refresh_review(),
        ReviewIntent::OpenCheckDetails => return state.open_check_details(),
        ReviewIntent::OpenMergePr => state.open_merge_pr(),
        ReviewIntent::ConfirmMergePr => return state.confirm_merge_pr(),
        ReviewIntent::Merged {
            tag,
            number,
            result,
        } => return state.pull_request_merged(tag, number, result),
        ReviewIntent::Next => return state.review_move(1),
        ReviewIntent::Prev => return state.review_move(-1),
        ReviewIntent::OpenFiles => return state.open_review_files(),
        ReviewIntent::ToggleFilesFocus => state.toggle_review_files_focus(),
        ReviewIntent::ObjectsChecked { tag, result } => {
            return state.review_objects_checked(tag, result);
        }
        ReviewIntent::HeadFetched { tag, result } => {
            return state.review_head_fetched(tag, result);
        }
        ReviewIntent::FilesLoaded { tag, result } => {
            return state.review_files_loaded(tag, result);
        }
        ReviewIntent::ToggleViewed => return state.toggle_viewed(),
        ReviewIntent::ViewedSet {
            tag,
            number,
            path,
            viewed,
            result,
        } => state.viewed_set(tag, number, path, viewed, result),
        ReviewIntent::QueuesLoaded { tag, repo, result } => {
            state.review_queues_loaded(tag, repo, result)
        }
        ReviewIntent::PrDetailLoaded {
            tag,
            number,
            repo,
            result,
        } => state.pr_detail_loaded(tag, number, repo, result),
    }
    Vec::new()
}
