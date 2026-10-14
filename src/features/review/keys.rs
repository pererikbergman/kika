//! The review-mode key bindings: `G` from the workspace, the queue, detail
//! and files screens, and the merge question. `CHUNKS` in `keymap.rs`
//! places both chunks, so their position there is behaviour.

use super::commit_view::review_commit_disabled;
use super::files::{review_files_disabled, review_search_step_disabled};
use super::line_link::open_line_disabled;
use super::merge::merge_disabled;
use super::state::{
    open_check_disabled, open_pr_disabled, queue_keys_disabled, refresh_review_disabled,
};
use super::threads::thread_nav_disabled;
use super::viewed::review_viewed_disabled;
use crate::confirm_question::Answer;
use crate::features::review::ReviewIntent;
use crate::intent::{AppIntent, Scroll};
use crate::keymap::{Binding, KeyPattern, KeyScope, NONE, never};
use crossterm::event::{KeyCode, KeyModifiers};

// Chunk 27: review
pub(crate) static REVIEW_KEYS: &[Binding] = &[
    // Free in the workspace, every panel and the global scope.
    Binding {
        id: "workspace.review",
        keys: &[KeyPattern::any(KeyCode::Char('G'))],
        scope: KeyScope::Workspace,
        intent: AppIntent::Review(ReviewIntent::Enter),
        description: "Review this repository's pull requests on GitHub",
        label: "review",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "review.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Review,
        intent: AppIntent::Review(ReviewIntent::Next),
        description: "Select the next pull request or file, or scroll the open one down",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "review.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Review,
        intent: AppIntent::Review(ReviewIntent::Prev),
        description: "Select the previous pull request or file, or scroll the open one up",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "review.open",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Review,
        intent: AppIntent::Review(ReviewIntent::OpenPr),
        description: "Open the selected pull request",
        label: "open",
        bar: true,
        disabled: open_pr_disabled,
    },
    Binding {
        id: "review.next_queue",
        keys: &[KeyPattern::any(KeyCode::Tab)],
        scope: KeyScope::Review,
        intent: AppIntent::Review(ReviewIntent::NextQueue),
        description: "Show the next queue",
        label: "queue",
        bar: true,
        disabled: queue_keys_disabled,
    },
    Binding {
        id: "review.prev_queue",
        keys: &[KeyPattern::any(KeyCode::BackTab)],
        scope: KeyScope::Review,
        intent: AppIntent::Review(ReviewIntent::PrevQueue),
        description: "Show the previous queue",
        label: "",
        bar: false,
        disabled: queue_keys_disabled,
    },
    Binding {
        id: "review.filter",
        keys: &[KeyPattern::any(KeyCode::Char('/'))],
        scope: KeyScope::Review,
        intent: AppIntent::OpenFilter,
        description: "Filter the queue by number, title or author",
        label: "filter",
        bar: false,
        disabled: queue_keys_disabled,
    },
    Binding {
        id: "review.refresh",
        keys: &[KeyPattern::exact(KeyCode::Char('r'), NONE)],
        scope: KeyScope::Review,
        intent: AppIntent::Review(ReviewIntent::Refresh),
        description: "Ask GitHub again for the queues or the open pull request",
        label: "refresh",
        bar: true,
        disabled: refresh_review_disabled,
    },
    Binding {
        id: "review.open_check",
        keys: &[KeyPattern::exact(KeyCode::Char('o'), NONE)],
        scope: KeyScope::Review,
        intent: AppIntent::Review(ReviewIntent::OpenCheckDetails),
        description: "Open a failing check's page in the browser",
        label: "check page",
        bar: true,
        disabled: open_check_disabled,
    },
    Binding {
        id: "review.merge",
        keys: &[KeyPattern::any(KeyCode::Char('M'))],
        scope: KeyScope::Review,
        intent: AppIntent::Review(ReviewIntent::OpenMergePr),
        description: "Merge the open pull request on GitHub, after a question only y answers",
        label: "merge",
        bar: true,
        disabled: merge_disabled,
    },
    Binding {
        id: "review.back",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::Review,
        intent: AppIntent::Review(ReviewIntent::CloseDetail),
        description: "Back to the pull request from its files, else to the queue; in the queue, clear the filter, else back to the workspace",
        label: "back",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "review.files",
        keys: &[KeyPattern::exact(KeyCode::Char('f'), NONE)],
        scope: KeyScope::Review,
        intent: AppIntent::Review(ReviewIntent::OpenFiles),
        description: "Show the pull request's changed files and diff, fetching its commits once if they are not here",
        label: "files",
        bar: true,
        disabled: review_files_disabled,
    },
    Binding {
        id: "review_files.focus",
        keys: &[KeyPattern::any(KeyCode::Tab)],
        scope: KeyScope::ReviewFiles,
        intent: AppIntent::Review(ReviewIntent::ToggleFilesFocus),
        description: "Move the keys between the file list and the diff",
        label: "list/diff",
        bar: true,
        disabled: never,
    },
    // Reaches GitHub, so a repository file may not move it.
    Binding {
        id: "review_files.viewed",
        keys: &[KeyPattern::exact(KeyCode::Char('v'), NONE)],
        scope: KeyScope::ReviewFiles,
        intent: AppIntent::Review(ReviewIntent::ToggleViewed),
        description: "Mark the selected file viewed on GitHub, or not viewed if it is",
        label: "viewed",
        bar: true,
        disabled: review_viewed_disabled,
    },
    // Local git only, so a repository file may move it.
    Binding {
        id: "review_files.commit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), NONE)],
        scope: KeyScope::ReviewFiles,
        intent: AppIntent::Review(ReviewIntent::PickCommit),
        description: "Review the whole pull request or one of its commits",
        label: "commit",
        bar: true,
        disabled: review_commit_disabled,
    },
    // Moves through threads already read, so a repository file may move it.
    Binding {
        id: "review_files.next_thread",
        keys: &[KeyPattern::exact(KeyCode::Char(']'), NONE)],
        scope: KeyScope::ReviewFiles,
        intent: AppIntent::Review(ReviewIntent::NextThread),
        description: "Go to the next unresolved review thread",
        label: "next thread",
        bar: true,
        disabled: thread_nav_disabled,
    },
    Binding {
        id: "review_files.prev_thread",
        keys: &[KeyPattern::exact(KeyCode::Char('['), NONE)],
        scope: KeyScope::ReviewFiles,
        intent: AppIntent::Review(ReviewIntent::PrevThread),
        description: "Go to the previous unresolved review thread",
        label: "prev thread",
        bar: false,
        disabled: thread_nav_disabled,
    },
    // Starts the browser, so a repository file may not move it.
    Binding {
        id: "review_files.open_line",
        keys: &[KeyPattern::exact(KeyCode::Char('o'), NONE)],
        scope: KeyScope::ReviewFiles,
        intent: AppIntent::Review(ReviewIntent::OpenLineOnForge),
        description: "Open the selected thread's line, else the top line, on GitHub's pull request page",
        label: "open top line on GitHub",
        bar: false,
        disabled: open_line_disabled,
    },
    Binding {
        id: "review_diff.down",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::ReviewDiff,
        intent: AppIntent::ScrollDiff(Scroll::LineDown),
        description: "Scroll the diff down a line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "review_diff.up",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::ReviewDiff,
        intent: AppIntent::ScrollDiff(Scroll::LineUp),
        description: "Scroll the diff up a line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "review_diff.page_down",
        keys: &[KeyPattern::any(KeyCode::PageDown)],
        scope: KeyScope::ReviewDiff,
        intent: AppIntent::ScrollDiff(Scroll::PageDown),
        description: "Scroll the diff down a page",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "review_diff.page_up",
        keys: &[KeyPattern::any(KeyCode::PageUp)],
        scope: KeyScope::ReviewDiff,
        intent: AppIntent::ScrollDiff(Scroll::PageUp),
        description: "Scroll the diff up a page",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "review_diff.search",
        keys: &[KeyPattern::any(KeyCode::Char('/'))],
        scope: KeyScope::ReviewDiff,
        intent: AppIntent::OpenSearch,
        description: "Search the lines of the diff loaded so far for typed text",
        label: "search",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "review_diff.search_next",
        keys: &[KeyPattern::exact(KeyCode::Char('n'), NONE)],
        scope: KeyScope::ReviewDiff,
        intent: AppIntent::SearchNext,
        description: "Go to the next match of the search",
        label: "next match",
        bar: false,
        disabled: review_search_step_disabled,
    },
    Binding {
        id: "review_diff.search_prev",
        keys: &[KeyPattern::any(KeyCode::Char('N'))],
        scope: KeyScope::ReviewDiff,
        intent: AppIntent::SearchPrev,
        description: "Go to the previous match of the search",
        label: "previous match",
        bar: false,
        disabled: review_search_step_disabled,
    },
    Binding {
        id: "review_diff.escape",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::ReviewDiff,
        intent: AppIntent::DiffEscape,
        description: "Clear the search, else go back to the file list",
        label: "back",
        bar: true,
        disabled: never,
    },
];

// Chunk 71b: review
pub(crate) static REVIEW_MERGE_CONFIRM: &[Binding] = &[
    // Merging a pull request reaches GitHub and cannot be undone, so
    // Enter is left unbound.
    Binding {
        id: "merge_pr.yes",
        keys: Answer::YOnly.keys(),
        scope: KeyScope::MergePullRequest,
        intent: AppIntent::Review(ReviewIntent::ConfirmMergePr),
        description: "Yes, merge the pull request on GitHub",
        label: "merge",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "merge_pr.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::MergePullRequest,
        intent: AppIntent::ConfirmNo,
        description: "No, merge nothing",
        label: "cancel",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "merge_pr.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::MergePullRequest,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::REPO_LOCKED;

    #[test]
    fn the_thread_keys_sit_after_the_commit_key_in_the_files_scope_and_are_not_locked() {
        let ids: Vec<&str> = REVIEW_KEYS.iter().map(|b| b.id).collect();
        let at = ids
            .iter()
            .position(|id| *id == "review_files.commit")
            .unwrap();
        assert_eq!(
            ids[at + 1..at + 3],
            ["review_files.next_thread", "review_files.prev_thread"]
        );
        for (id, key, intent, bar) in [
            (
                "review_files.next_thread",
                ']',
                ReviewIntent::NextThread,
                true,
            ),
            (
                "review_files.prev_thread",
                '[',
                ReviewIntent::PrevThread,
                false,
            ),
        ] {
            let binding = REVIEW_KEYS.iter().find(|b| b.id == id).unwrap();
            assert_eq!(binding.scope, KeyScope::ReviewFiles, "{id}");
            assert_eq!(
                binding.keys,
                [KeyPattern::exact(KeyCode::Char(key), NONE)],
                "{id}"
            );
            assert_eq!(binding.intent, AppIntent::Review(intent), "{id}");
            assert_eq!(binding.bar, bar, "{id}");
            assert!(!REPO_LOCKED.contains(&id), "{id}");
        }
    }

    #[test]
    fn o_opens_the_line_on_the_files_screen_and_a_check_on_the_detail() {
        use crate::features::review::state::NO_BROWSER;
        use crate::features::review::state::tests::{fixture_detail, in_detail};
        use crate::features::review::threads::tests::{files_with, fixture_discussion};
        use crate::keymap::{KeyPress, Keymap, Resolution, available, registry, resolve};
        use crate::model::AppState;

        let o = KeyPress::new(KeyCode::Char('o'), NONE);
        let run = |state: &AppState, focus| match resolve(
            registry(),
            &Keymap::default(),
            focus,
            state,
            o,
        ) {
            Resolution::Run(b) => b.id,
            other => panic!("{other:?}"),
        };
        let mut files = files_with(&["src/cache.rs"], fixture_discussion());
        files.diff.pending = false;
        files.diff.path = Some("src/cache.rs".into());
        files.diff.lines = crate::features::review::threads::tests::cache_diff();
        for focus in [KeyScope::ReviewFiles, KeyScope::ReviewDiff] {
            assert_eq!(run(&files, focus), "review_files.open_line");
        }
        let detail = in_detail(fixture_detail().checks);
        assert_eq!(run(&detail, KeyScope::Review), "review.open_check");
        assert!(REPO_LOCKED.contains(&"review_files.open_line"));

        let binding = REVIEW_KEYS
            .iter()
            .find(|b| b.id == "review_files.open_line")
            .unwrap();
        assert_eq!(
            binding.intent,
            AppIntent::Review(ReviewIntent::OpenLineOnForge)
        );
        assert!(!binding.bar);
        // The help menu shows why it is off.
        files.browser = None;
        let help = available(
            registry(),
            &Keymap::default(),
            KeyScope::ReviewFiles,
            &files,
        );
        let (_, reason) = help
            .iter()
            .find(|(b, _)| b.id == "review_files.open_line")
            .unwrap();
        assert_eq!(reason.as_deref(), Some(NO_BROWSER));
    }
}
