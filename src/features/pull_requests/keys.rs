//! The pull-request key bindings: `o` and `B` on the Branches panel, and
//! the target and base pickers with the create question. `CHUNKS` in
//! `keymap.rs` places both chunks, so their position there is behaviour.

use super::create::refresh_disabled;
use crate::confirm_question::Answer;
use crate::features::pull_requests::PullRequestsIntent;
use crate::intent::AppIntent;
use crate::keymap::{Binding, KeyPattern, KeyScope, NONE, never, no_branch_selected};
use crate::menu::MenuId;
use crossterm::event::{KeyCode, KeyModifiers};

// Chunk 47: pull_requests
pub(crate) static PULL_REQUESTS_BRANCHES: &[Binding] = &[
    Binding {
        id: "branches.pull_request",
        keys: &[KeyPattern::exact(KeyCode::Char('o'), NONE)],
        scope: KeyScope::Branches,
        intent: AppIntent::OpenMenu(MenuId::PullRequest),
        description: "Open, copy or create the selected branch's pull request",
        label: "pull request",
        bar: false,
        disabled: no_branch_selected,
    },
    Binding {
        id: "branches.refresh_pull_requests",
        keys: &[KeyPattern::any(KeyCode::Char('B'))],
        scope: KeyScope::Branches,
        intent: AppIntent::PullRequests(PullRequestsIntent::Refresh),
        description: "Ask GitHub again for the branches' pull requests",
        label: "refresh PRs",
        bar: false,
        disabled: refresh_disabled,
    },
];

// Chunk 71: pull_requests
pub(crate) static PULL_REQUESTS_PICK_CREATE: &[Binding] = &[
    Binding {
        id: "pick_list.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::PickList,
        intent: AppIntent::PullRequests(PullRequestsIntent::PickNext),
        description: "Select the next row",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "pick_list.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::PickList,
        intent: AppIntent::PullRequests(PullRequestsIntent::PickPrev),
        description: "Select the previous row",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "pick_list.pick",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::PickList,
        intent: AppIntent::PullRequests(PullRequestsIntent::PickSelect),
        description: "Take the selected row",
        label: "pick",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "pick_list.close",
        keys: &[
            KeyPattern::any(KeyCode::Esc),
            KeyPattern::exact(KeyCode::Char('q'), NONE),
        ],
        scope: KeyScope::PickList,
        intent: AppIntent::PopPopup,
        description: "Close the list and stop",
        label: "close",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "pick_list.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::PickList,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
    // Creating a pull request reaches GitHub, so Enter is left unbound.
    Binding {
        id: "create_pull_request.yes",
        keys: Answer::YOnly.keys(),
        scope: KeyScope::CreatePullRequest,
        intent: AppIntent::PullRequests(PullRequestsIntent::ConfirmCreate),
        description: "Yes, create the pull request on GitHub",
        label: "create",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "create_pull_request.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::CreatePullRequest,
        intent: AppIntent::ConfirmNo,
        description: "No, create nothing",
        label: "cancel",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "create_pull_request.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::CreatePullRequest,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];
