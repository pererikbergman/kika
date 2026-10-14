//! The review-mode key bindings: `G` from the workspace, the queue, detail
//! and files screens, and the merge question. `CHUNKS` in `keymap.rs`
//! places both chunks, so their position there is behaviour.

use super::commit_view::review_commit_disabled;
use super::conversation::{conversation_disabled, page_disabled};
use super::files::{review_files_disabled, review_search_step_disabled};
use super::merge::merge_disabled;
use super::reader::thread_reader_disabled;
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

/// Bar hints left out, rather than dimmed, while their key is refused:
/// they belong to one screen only.
pub(crate) const HIDDEN_WHEN_OFF: &[&str] = &["review.conversation"];

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
        description: "Back to the pull request from its files or conversation, else to the queue; in the queue, clear the filter, else back to the workspace",
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
    // Shows what was already read, so a repository file may move it.
    Binding {
        id: "review.conversation",
        keys: &[KeyPattern::any(KeyCode::Char('C'))],
        scope: KeyScope::Review,
        intent: AppIntent::Review(ReviewIntent::OpenConversation),
        description: "Show the pull request's conversation: its comments and review bodies, oldest first",
        label: "conversation",
        bar: true,
        disabled: conversation_disabled,
    },
    Binding {
        id: "review.page_down",
        keys: &[KeyPattern::any(KeyCode::PageDown)],
        scope: KeyScope::Review,
        intent: AppIntent::Review(ReviewIntent::PageDown),
        description: "Scroll the pull request or its conversation down a page",
        label: "",
        bar: false,
        disabled: page_disabled,
    },
    Binding {
        id: "review.page_up",
        keys: &[KeyPattern::any(KeyCode::PageUp)],
        scope: KeyScope::Review,
        intent: AppIntent::Review(ReviewIntent::PageUp),
        description: "Scroll the pull request or its conversation up a page",
        label: "",
        bar: false,
        disabled: page_disabled,
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
    // Reads a thread already loaded, so a repository file may move it.
    Binding {
        id: "review_files.thread",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::ReviewFiles,
        intent: AppIntent::Review(ReviewIntent::OpenThreadReader),
        description: "Read the selected review thread, or the file's first, with every comment in full",
        label: "read thread",
        bar: true,
        disabled: thread_reader_disabled,
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
    Binding {
        id: "review_thread.down",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::ReviewThread,
        intent: AppIntent::Review(ReviewIntent::ThreadReaderScroll(Scroll::LineDown)),
        description: "Scroll the thread down a line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "review_thread.up",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::ReviewThread,
        intent: AppIntent::Review(ReviewIntent::ThreadReaderScroll(Scroll::LineUp)),
        description: "Scroll the thread up a line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "review_thread.page_down",
        keys: &[KeyPattern::any(KeyCode::PageDown)],
        scope: KeyScope::ReviewThread,
        intent: AppIntent::Review(ReviewIntent::ThreadReaderScroll(Scroll::PageDown)),
        description: "Scroll the thread down a page",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "review_thread.page_up",
        keys: &[KeyPattern::any(KeyCode::PageUp)],
        scope: KeyScope::ReviewThread,
        intent: AppIntent::Review(ReviewIntent::ThreadReaderScroll(Scroll::PageUp)),
        description: "Scroll the thread up a page",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "review_thread.next",
        keys: &[KeyPattern::exact(KeyCode::Char(']'), NONE)],
        scope: KeyScope::ReviewThread,
        intent: AppIntent::Review(ReviewIntent::ThreadReaderStep(1)),
        description: "Read the file's next review thread, resolved ones included",
        label: "next thread",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "review_thread.prev",
        keys: &[KeyPattern::exact(KeyCode::Char('['), NONE)],
        scope: KeyScope::ReviewThread,
        intent: AppIntent::Review(ReviewIntent::ThreadReaderStep(-1)),
        description: "Read the file's previous review thread, resolved ones included",
        label: "prev thread",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "review_thread.close",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::ReviewThread,
        intent: AppIntent::Review(ReviewIntent::CloseThreadReader),
        description: "Close the thread and go back to the files",
        label: "close",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "review_thread.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::ReviewThread,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
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
    use crate::keymap::{REPO_LOCKED, registry};

    #[test]
    fn the_reader_and_conversation_keys_sit_in_their_scopes_unlocked_but_quit() {
        let binding = |id: &str| registry().iter().find(|b| b.id == id).unwrap();
        let thread = binding("review_files.thread");
        assert_eq!(thread.scope, KeyScope::ReviewFiles);
        assert_eq!(thread.keys, [KeyPattern::any(KeyCode::Enter)]);
        assert!(thread.bar);
        let conversation = binding("review.conversation");
        assert_eq!(conversation.scope, KeyScope::Review);
        assert!(HIDDEN_WHEN_OFF.contains(&"review.conversation"));
        let mut ids = vec!["review_files.thread", "review.conversation"];
        ids.extend(["review.page_down", "review.page_up"]);
        let in_thread: Vec<&str> = REVIEW_KEYS
            .iter()
            .filter(|b| b.scope == KeyScope::ReviewThread)
            .map(|b| b.id)
            .collect();
        assert_eq!(
            in_thread,
            [
                "review_thread.down",
                "review_thread.up",
                "review_thread.page_down",
                "review_thread.page_up",
                "review_thread.next",
                "review_thread.prev",
                "review_thread.close",
                "review_thread.quit",
            ]
        );
        ids.extend(in_thread.iter().filter(|id| **id != "review_thread.quit"));
        for id in ids {
            assert!(!REPO_LOCKED.contains(&id), "{id}");
        }
        // Quitting is locked, as in every popup.
        assert!(REPO_LOCKED.contains(&"review_thread.quit"));
        assert_eq!(KeyScope::ReviewThread.parent(), None);
        assert_eq!(KeyScope::ReviewThread.title(), "Review thread");
    }

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
}
