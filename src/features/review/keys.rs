//! The review-mode key bindings: `G` from the workspace, the queue, detail
//! and files screens, and the merge question. `CHUNKS` in `keymap.rs`
//! places both chunks, so their position there is behaviour.

use super::commit_view::review_commit_disabled;
use super::conversation::{conversation_disabled, page_disabled};
use super::files::{review_files_disabled, review_search_step_disabled};
use super::merge::merge_disabled;
use super::pending::{comment_disabled, pending_list_disabled};
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

// Chunk 27b: review comments
pub(crate) static REVIEW_COMMENT_KEYS: &[Binding] = &[
    // Local files only, so a repository file may move them.
    Binding {
        id: "review_files.comment",
        keys: &[KeyPattern::exact(KeyCode::Char('a'), NONE)],
        scope: KeyScope::ReviewFiles,
        intent: AppIntent::Review(ReviewIntent::StartComment),
        description: "Pick a diff line or range and write a comment on it, kept in the pending review",
        label: "comment",
        bar: true,
        disabled: comment_disabled,
    },
    Binding {
        id: "review_files.pending",
        keys: &[KeyPattern::any(KeyCode::Char('P'))],
        scope: KeyScope::ReviewFiles,
        intent: AppIntent::Review(ReviewIntent::OpenPendingList),
        description: "List the pending review's comments, to edit or delete them",
        label: "pending",
        bar: true,
        disabled: pending_list_disabled,
    },
    Binding {
        id: "review_pick.down",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::ReviewPick,
        intent: AppIntent::Review(ReviewIntent::StepCommentPick(true)),
        description: "Move to the next numbered line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "review_pick.up",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::ReviewPick,
        intent: AppIntent::Review(ReviewIntent::StepCommentPick(false)),
        description: "Move to the previous numbered line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "review_pick.range",
        keys: &[KeyPattern::exact(KeyCode::Char('v'), NONE)],
        scope: KeyScope::ReviewPick,
        intent: AppIntent::Review(ReviewIntent::ToggleCommentRange),
        description: "Start a range at this line, or drop it",
        label: "range",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "review_pick.confirm",
        keys: &[
            KeyPattern::any(KeyCode::Enter),
            KeyPattern::exact(KeyCode::Char('a'), NONE),
        ],
        scope: KeyScope::ReviewPick,
        intent: AppIntent::Review(ReviewIntent::ConfirmCommentPick),
        description: "Write a comment on the line or range picked",
        label: "comment",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "review_pick.cancel",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::ReviewPick,
        intent: AppIntent::Review(ReviewIntent::CancelCommentPick),
        description: "Stop picking and go back to the files",
        label: "cancel",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "review_pick.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::ReviewPick,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    // Every other key is typed into the comment.
    Binding {
        id: "review_comment.save",
        keys: &[KeyPattern::exact(KeyCode::Char('s'), KeyModifiers::CONTROL)],
        scope: KeyScope::ReviewComment,
        intent: AppIntent::Review(ReviewIntent::SaveComment),
        description: "Keep the comment in the pending review; nothing is posted",
        label: "save",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "review_comment.cancel",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::ReviewComment,
        intent: AppIntent::Review(ReviewIntent::EscapeComment),
        description: "Close the comment, asking first when it has text",
        label: "cancel",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "review_comment.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::ReviewComment,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "review_pending.down",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::ReviewPending,
        intent: AppIntent::Review(ReviewIntent::MovePendingList(1)),
        description: "Select the next pending comment",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "review_pending.up",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::ReviewPending,
        intent: AppIntent::Review(ReviewIntent::MovePendingList(-1)),
        description: "Select the previous pending comment",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "review_pending.edit",
        keys: &[
            KeyPattern::any(KeyCode::Enter),
            KeyPattern::exact(KeyCode::Char('e'), NONE),
        ],
        scope: KeyScope::ReviewPending,
        intent: AppIntent::Review(ReviewIntent::EditPending),
        description: "Edit the selected comment",
        label: "edit",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "review_pending.delete",
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::ReviewPending,
        intent: AppIntent::Review(ReviewIntent::AskDeletePending),
        description: "Delete the selected comment, after a question only y answers",
        label: "delete",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "review_pending.close",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::ReviewPending,
        intent: AppIntent::Review(ReviewIntent::ClosePendingList),
        description: "Close the list",
        label: "close",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "review_pending.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::ReviewPending,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];

// Chunk 71c: review comments
pub(crate) static REVIEW_COMMENT_CONFIRM: &[Binding] = &[
    // The comment is lost, so Enter is left unbound.
    Binding {
        id: "review_comment_confirm.yes",
        keys: Answer::YOnly.keys(),
        scope: KeyScope::ReviewCommentConfirm,
        intent: AppIntent::ConfirmYes,
        description: "Yes, delete or discard the comment",
        label: "yes",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "review_comment_confirm.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::ReviewCommentConfirm,
        intent: AppIntent::ConfirmNo,
        description: "No, keep the comment",
        label: "no",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "review_comment_confirm.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::ReviewCommentConfirm,
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
    use crate::keymap::{KeyPress, REPO_LOCKED, registry};

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

    /// The binding `key` reaches from `focus`, enabled or not, by id.
    fn owner(focus: KeyScope, key: KeyPress) -> Option<&'static str> {
        focus.chain().find_map(|scope| {
            registry()
                .iter()
                .find(|b| b.scope == scope && b.matches(key))
                .map(|b| b.id)
        })
    }

    fn plain(c: char) -> KeyPress {
        KeyPress::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn a_and_p_reach_the_files_and_the_diff_and_the_popups_keep_their_keys() {
        for focus in [KeyScope::ReviewFiles, KeyScope::ReviewDiff] {
            assert_eq!(owner(focus, plain('a')), Some("review_files.comment"));
            let shift_p = KeyPress::new(KeyCode::Char('P'), KeyModifiers::SHIFT);
            assert_eq!(owner(focus, shift_p), Some("review_files.pending"));
        }
        for focus in [
            KeyScope::ReviewPick,
            KeyScope::ReviewComment,
            KeyScope::ReviewPending,
        ] {
            assert_eq!(focus.parent(), None, "{focus:?}");
            for c in ['q', '?', ']', 'o'] {
                assert_eq!(owner(focus, plain(c)), None, "{focus:?} {c}");
            }
            let quit = KeyPress::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
            let id = owner(focus, quit).unwrap();
            assert!(id.ends_with(".quit") && REPO_LOCKED.contains(&id), "{id}");
        }
        // In the composer these are typed, and ctrl-e is left for later.
        let comment = KeyScope::ReviewComment;
        assert_eq!(comment.bar_note(), Some("q and ? are typed"));
        let enter = KeyPress::new(KeyCode::Enter, KeyModifiers::NONE);
        let ctrl_e = KeyPress::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
        for key in [enter, plain('q'), plain('?'), plain('a'), ctrl_e] {
            assert_eq!(owner(comment, key), None, "{key:?}");
        }
        let ctrl_s = KeyPress::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert_eq!(owner(comment, ctrl_s), Some("review_comment.save"));
        assert_eq!(
            owner(KeyScope::ReviewPick, plain('v')),
            Some("review_pick.range")
        );
        assert_eq!(
            owner(KeyScope::ReviewPick, enter),
            Some("review_pick.confirm")
        );
        assert_eq!(
            owner(KeyScope::ReviewPending, plain('d')),
            Some("review_pending.delete")
        );
        assert_eq!(KeyScope::ReviewPick.title(), "Pick lines to comment on");
        // Local files only: not locked, quitting aside.
        for id in [
            "review_files.comment",
            "review_files.pending",
            "review_pending.delete",
        ] {
            assert!(!REPO_LOCKED.contains(&id), "{id}");
        }
    }

    #[test]
    fn the_help_menu_gives_each_reason_a_is_off() {
        use crate::features::review::files::ReviewView;
        use crate::features::review::pending::tests::{commenting, loaded, loading};
        use crate::keymap::available;
        use kika_forge::pending::PendingError;
        let help = |state: &crate::model::AppState| {
            available(registry(), &state.keymap, KeyScope::ReviewFiles, state)
                .into_iter()
                .find(|(b, _)| b.id == "review_files.comment")
                .and_then(|(_, reason)| reason)
                .map(|r| r.to_string())
        };
        let mut states = Vec::new();
        let (state, _) = loading();
        states.push(state);
        let (mut state, tag) = loading();
        loaded(
            &mut state,
            tag,
            Err(PendingError::Unreadable("it is too large")),
        );
        states.push(state);
        let mut state = commenting();
        state.review.files.as_mut().unwrap().view = ReviewView::Commit(0);
        states.push(state);
        let mut state = commenting();
        state.diff.lines.truncate(4);
        states.push(state);
        for state in &states {
            let reason = comment_disabled(state).map(|r| r.to_string());
            assert!(reason.is_some());
            assert_eq!(help(state), reason);
        }
        assert_eq!(help(&commenting()), None);
    }
}
