//! Example end-to-end tests: each one drives the real app over a real repo.

use crossterm::event::{KeyCode, KeyModifiers};

use crate::e2e::harness::Harness;
use crate::e2e::repo::TestRepo;
use crate::git::status::EntryKind;

#[test]
fn the_setup_api_builds_commits_and_leaves_new_files_untracked() {
    let repo = TestRepo::new()
        .write("a.txt", "one\n")
        .commit("first")
        .write("dir/b.txt", "two\n")
        .commit("second")
        .write("new.txt", "fresh\n");

    assert_eq!(repo.git(["rev-list", "--count", "HEAD"]), b"2\n");
    assert_eq!(repo.read("dir/b.txt"), "two\n");
    let entry = repo.entry("new.txt").expect("new.txt is listed");
    assert_eq!(entry.kind, EntryKind::Untracked);
    assert!(repo.staged().is_empty());
}

/// `a.txt` committed as `one`, then changed in the worktree to add `two`.
fn modified_repo() -> TestRepo {
    TestRepo::new()
        .write("a.txt", "one\n")
        .commit("init")
        .write("a.txt", "one\ntwo\n")
}

#[tokio::test(flavor = "multi_thread")]
async fn start_waits_for_the_first_status_and_diff() {
    let repo = modified_repo();

    let mut app = Harness::start(&repo).await;

    let selected = app.selected_line().expect("a file is selected");
    assert!(selected.contains("a.txt"), "selected: {selected:?}");
    app.assert_screen_contains("+two");
}

#[tokio::test(flavor = "multi_thread")]
async fn space_stages_the_selected_file_in_git() {
    let repo = modified_repo();
    let mut app = Harness::start(&repo).await;
    assert!(repo.staged().is_empty());

    app.press(KeyCode::Char(' ')).await;

    assert_eq!(repo.staged(), ["a.txt"]);
    let entry = repo.entry("a.txt").expect("a.txt is listed");
    assert_eq!((entry.index, entry.worktree), ('M', '.'));
    let selected = app.selected_line().expect("a file is selected");
    assert!(selected.contains("a.txt"), "selected: {selected:?}");
    assert_eq!(app.state().busy, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn space_unstages_a_staged_file() {
    let repo = modified_repo().stage("a.txt");
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char(' ')).await;

    assert!(repo.staged().is_empty());
    let entry = repo.entry("a.txt").expect("a.txt is listed");
    assert_eq!((entry.index, entry.worktree), ('.', 'M'));
}

#[tokio::test(flavor = "multi_thread")]
async fn j_moves_the_selection_to_the_next_file() {
    let repo = TestRepo::new()
        .write("a.txt", "one\n")
        .write("b.txt", "one\n")
        .commit("init")
        .write("a.txt", "changed\n")
        .write("b.txt", "changed\n");
    let mut app = Harness::start(&repo).await;
    let first = app.selected_line().expect("a file is selected");
    assert!(first.contains("a.txt"), "selected: {first:?}");

    app.press(KeyCode::Char('j')).await;

    let second = app.selected_line().expect("a file is selected");
    assert!(second.contains("b.txt"), "selected: {second:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn c_commits_the_staged_file_with_the_typed_message() {
    let repo = modified_repo().stage("a.txt");
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('c')).await;
    app.assert_screen_contains(" Commit message ");
    app.type_text("Add two\nThe second line.\nAnd a third.")
        .await;
    app.press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)
        .await;

    assert_eq!(
        repo.head_message(),
        "Add two\n\nThe second line.\nAnd a third.\n\n"
    );
    assert!(repo.staged().is_empty());
    assert_eq!(repo.git(["rev-list", "--count", "HEAD"]), b"2\n");
    app.assert_screen_contains("No changes");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_failing_pre_commit_hook_shows_its_output_and_commits_nothing() {
    let repo = modified_repo()
        .stage("a.txt")
        .hook("pre-commit", "#!/bin/sh\necho 'lint failed'\nexit 1\n");
    let head = repo.head();
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('c')).await;
    app.type_text("Try this").await;
    app.press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)
        .await;

    app.assert_screen_contains(" Commit failed ");
    app.assert_screen_contains("lint failed");
    assert_eq!(repo.head(), head);
    assert_eq!(repo.staged(), ["a.txt"]);

    // The message is kept for another try.
    app.press(KeyCode::Esc).await;
    app.assert_screen_contains("Try this");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_failing_commit_msg_hook_shows_its_output_and_commits_nothing() {
    let repo = modified_repo()
        .stage("a.txt")
        .hook("commit-msg", "#!/bin/sh\necho 'bad message' >&2\nexit 1\n");
    let head = repo.head();
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('c')).await;
    app.type_text("wip").await;
    app.press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)
        .await;

    app.assert_screen_contains("bad message");
    assert_eq!(repo.head(), head);
    assert_eq!(repo.staged(), ["a.txt"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn with_nothing_staged_y_stages_everything_and_commits() {
    let repo = modified_repo().write("new.txt", "fresh\n");
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('c')).await;
    app.assert_screen_contains("Nothing is staged. Stage all changes and commit? (y/n)");
    app.press(KeyCode::Char('y')).await;
    app.type_text("Everything").await;
    app.press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)
        .await;

    assert_eq!(repo.head_message(), "Everything\n\n");
    assert!(repo.git(["status", "--porcelain"]).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_the_commit_leaves_the_index_untouched() {
    let repo = modified_repo();
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('c')).await;
    app.press(KeyCode::Char('y')).await;
    app.type_text("never").await;
    app.press(KeyCode::Esc).await;
    app.assert_screen_contains("Discard the typed message? (y/n)");
    app.press(KeyCode::Char('y')).await;

    assert!(!app.screen().iter().any(|row| row.contains("Commit")));
    assert!(repo.staged().is_empty());
    assert_eq!(repo.git(["rev-list", "--count", "HEAD"]), b"1\n");
    assert!(!app.state().should_quit);
}

/// A hook that waits forever must not trap the user: Ctrl+C quits, and git
/// and the hook are stopped without leaving `index.lock` behind.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_while_a_hook_hangs_quits_and_stops_git_cleanly() {
    use std::time::{Duration, Instant};

    let repo = modified_repo()
        .stage("a.txt")
        .hook("pre-commit", "#!/bin/sh\ntouch .git/started\nsleep 30\n");
    let head = repo.head();
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('c')).await;
    app.type_text("Stuck").await;
    app.send(KeyCode::Char('s'), KeyModifiers::CONTROL);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !repo.exists(".git/started") {
        assert!(Instant::now() < deadline, "the hook never started");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    app.assert_screen_contains("ctrl-c quits Kika");

    let start = Instant::now();
    app.send(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(app.state().should_quit);
    app.settle().await;

    assert!(
        start.elapsed() < Duration::from_secs(10),
        "{:?}",
        start.elapsed()
    );
    assert!(!repo.exists(".git/index.lock"));
    assert_eq!(repo.head(), head);
    assert_eq!(repo.staged(), ["a.txt"]);
}

/// One commit on branch `main`, whatever the default branch name is.
fn on_main() -> TestRepo {
    let repo = TestRepo::new();
    repo.git(["checkout", "-q", "-b", "main"]);
    repo.write("a.txt", "one\n").commit("init")
}

/// `main` and a branch `other` that both changed `a.txt`, so merging or
/// rebasing one onto the other stops on a conflict.
fn diverged() -> TestRepo {
    let repo = on_main();
    repo.git(["checkout", "-q", "-b", "other"]);
    let repo = repo.write("a.txt", "theirs\n").commit("theirs");
    repo.git(["checkout", "-q", "main"]);
    repo.write("a.txt", "ours\n").commit("ours")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_status_panel_shows_the_branch_its_upstream_and_counts() {
    let repo = on_main();
    repo.git(["branch", "base"]);
    repo.git(["checkout", "-q", "base"]);
    let repo = repo.write("b.txt", "base\n").commit("on base");
    repo.git(["checkout", "-q", "-b", "work", "main"]);
    repo.git(["branch", "-q", "--set-upstream-to=base"]);
    let repo = repo
        .write("c.txt", "1\n")
        .commit("first")
        .write("c.txt", "2\n")
        .commit("second");

    let mut app = Harness::start(&repo).await;

    app.assert_screen_contains("│work ");
    app.assert_screen_contains("│base ↑2 ↓1 ");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_branch_without_upstream_says_so() {
    let repo = on_main();
    let mut app = Harness::start(&repo).await;
    app.assert_screen_contains("│main ");
    app.assert_screen_contains("│no upstream ");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_detached_head_shows_its_short_hash() {
    let repo = on_main();
    repo.git(["checkout", "-q", "--detach"]);
    let head = repo.head();

    let mut app = Harness::start(&repo).await;

    app.assert_screen_contains(&format!("│detached {} ", &head[..7]));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_conflicting_merge_shows_merging() {
    let repo = diverged();
    assert!(
        !repo.try_git(["merge", "other"]),
        "the merge should conflict"
    );

    let mut app = Harness::start(&repo).await;

    app.assert_screen_contains("│merging ");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_conflicting_rebase_shows_rebasing() {
    let repo = diverged();
    assert!(
        !repo.try_git(["rebase", "other"]),
        "the rebase should conflict"
    );

    let mut app = Harness::start(&repo).await;

    app.assert_screen_contains("│rebasing ");
}

#[tokio::test(flavor = "multi_thread")]
async fn committing_in_the_app_updates_the_ahead_count() {
    let repo = on_main();
    repo.git(["branch", "base"]);
    repo.git(["branch", "-q", "--set-upstream-to=base"]);
    let repo = repo.write("a.txt", "two\n").stage("a.txt");
    let mut app = Harness::start(&repo).await;
    app.assert_screen_contains("│base ↑0 ↓0 ");

    app.press(KeyCode::Char('c')).await;
    app.type_text("Ahead").await;
    app.press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)
        .await;

    app.assert_screen_contains("│base ↑1 ↓0 ");
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_menu_esc_returns_to_the_parent_menu() {
    let repo = modified_repo();
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('m')).await;
    app.assert_screen_contains(" Actions ");
    // "Stage…" is the first item.
    app.press(KeyCode::Enter).await;
    app.assert_screen_contains(" Stage ");
    app.assert_screen_contains("Selected file");
    assert_eq!(app.state().popups.len(), 2);

    app.press(KeyCode::Esc).await;
    assert_eq!(app.state().popups.len(), 1);
    app.assert_screen_contains(" Actions ");
    assert!(!app.screen().iter().any(|row| row.contains("Selected file")));

    app.press(KeyCode::Esc).await;
    assert!(app.state().popups.is_empty());
    assert!(!app.screen().iter().any(|row| row.contains(" Actions ")));
    assert!(!app.state().should_quit);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_stage_submenu_stages_the_selected_file_and_closes_the_menus() {
    let repo = modified_repo();
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('m')).await;
    app.press(KeyCode::Enter).await;
    app.press(KeyCode::Enter).await;

    assert_eq!(repo.staged(), ["a.txt"]);
    assert!(app.state().popups.is_empty());
    assert!(!app.screen().iter().any(|row| row.contains(" Stage ")));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_git_command_shows_an_error_popup_and_the_app_keeps_running() {
    let repo = modified_repo();
    let mut app = Harness::start(&repo).await;
    // Another git process seems to hold the index, so `git add` fails.
    let repo = repo.write(".git/index.lock", "");

    app.press(KeyCode::Char(' ')).await;

    app.assert_screen_contains(" Could not stage ");
    app.assert_screen_contains("index.lock");
    app.assert_screen_contains("Fix the cause above, then try again.");
    assert!(!app.state().should_quit);
    assert!(repo.staged().is_empty());

    app.press(KeyCode::Esc).await;
    assert!(app.state().popups.is_empty());
    assert!(
        !app.screen()
            .iter()
            .any(|row| row.contains("Could not stage"))
    );
    assert!(!app.state().should_quit);

    repo.remove(".git/index.lock");
    app.press(KeyCode::Char(' ')).await;
    assert_eq!(repo.staged(), ["a.txt"]);
    assert!(app.state().popups.is_empty());
}
