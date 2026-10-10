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

    assert!(repo.staged().is_empty());
    assert_eq!(repo.git(["rev-list", "--count", "HEAD"]), b"1\n");
    assert!(!app.state().should_quit);
}
