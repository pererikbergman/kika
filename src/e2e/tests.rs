//! Example end-to-end tests: each one drives the real app over a real repo.

use std::ffi::OsStr;

use crossterm::event::{KeyCode, KeyModifiers};

use crate::e2e::harness::Harness;
use crate::e2e::repo::TestRepo;
use crate::git::CliRunner;
use crate::git::fake::{TempDir, git};
use crate::git::repo_root::resolve_workdir;
use crate::git::status::EntryKind;
use crate::intent::AppIntent;
use crate::watch::WatchEvent;

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

/// A hook may ask the user something, so the commit runs on the terminal;
/// what it printed went there, and the popup says so.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_failing_pre_commit_hook_runs_on_the_terminal_and_commits_nothing() {
    let repo = modified_repo()
        .stage("a.txt")
        .hook("pre-commit", "#!/bin/sh\ntouch .git/ran\nexit 1\n");
    let head = repo.head();
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('c')).await;
    app.type_text("Try this").await;
    app.press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)
        .await;

    assert!(repo.exists(".git/ran"));
    app.assert_screen_contains(" Commit failed ");
    app.assert_screen_contains("git commit exited with status 1");
    assert_eq!(repo.head(), head);
    assert_eq!(repo.staged(), ["a.txt"]);

    // The message is kept for another try.
    app.press(KeyCode::Esc).await;
    app.assert_screen_contains("Try this");
}

/// Skipped hooks cannot ask anything, so the commit runs detached and the
/// failing commit-msg hook never runs.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn with_hooks_skipped_a_commit_msg_hook_does_not_stop_the_commit() {
    let repo = modified_repo()
        .stage("a.txt")
        .hook("commit-msg", "#!/bin/sh\ntouch .git/ran\nexit 1\n");
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('c')).await;
    app.press_with(KeyCode::Char('n'), KeyModifiers::CONTROL)
        .await;
    app.type_text("wip").await;
    app.press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)
        .await;

    assert!(!repo.exists(".git/ran"));
    assert_eq!(repo.head_message(), "wip\n\n");
    assert!(repo.staged().is_empty());
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

    assert!(
        !app.screen()
            .iter()
            .any(|row| row.contains("Commit message"))
    );
    assert!(repo.staged().is_empty());
    assert_eq!(repo.git(["rev-list", "--count", "HEAD"]), b"1\n");
    assert!(!app.state().should_quit);
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

    // Collapsed, the panel shows only the branch; expanded, the rest.
    app.assert_screen_contains("│work ");
    app.press(KeyCode::Char('1')).await;
    app.assert_screen_contains("┃base ↑2 ↓1 ");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_branch_without_upstream_says_so() {
    let repo = on_main();
    let mut app = Harness::start(&repo).await;
    app.assert_screen_contains("│main ");
    app.press(KeyCode::Char('1')).await;
    app.assert_screen_contains("┃no upstream ");
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
    app.press(KeyCode::Char('1')).await;

    app.assert_screen_contains("┃merging ");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_conflicting_rebase_shows_rebasing() {
    let repo = diverged();
    assert!(
        !repo.try_git(["rebase", "other"]),
        "the rebase should conflict"
    );

    let mut app = Harness::start(&repo).await;
    app.press(KeyCode::Char('1')).await;

    app.assert_screen_contains("┃rebasing ");
}

/// `a.txt`, executable and with CRLF endings, changed at both ends on
/// `main` and on `other`, merged so it stops with two conflicts.
#[cfg(unix)]
fn two_conflicts() -> TestRepo {
    use std::os::unix::fs::PermissionsExt;

    let lines =
        |top: &str, bottom: &str| format!("{top}\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n{bottom}\r\n");
    let repo = TestRepo::new();
    repo.git(["checkout", "-q", "-b", "main"]);
    let repo = repo.write("a.txt", &lines("top", "bottom"));
    let file = repo.path().join("a.txt");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    let repo = repo.commit("init");
    repo.git(["checkout", "-q", "-b", "other"]);
    let repo = repo
        .write("a.txt", &lines("theirs top", "theirs bottom"))
        .commit("theirs");
    repo.git(["checkout", "-q", "main"]);
    let repo = repo
        .write("a.txt", &lines("ours top", "ours bottom"))
        .commit("ours");
    assert!(
        !repo.try_git(["merge", "other"]),
        "the merge should conflict"
    );
    repo
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn picking_a_side_of_each_conflict_writes_the_file_and_y_stages_it() {
    use std::os::unix::fs::PermissionsExt;

    let repo = two_conflicts();
    let mut app = Harness::start(&repo).await;
    app.assert_screen_contains("! UU a.txt");

    app.press(KeyCode::Enter).await;
    app.assert_screen_contains("Conflict 1/2 · a.txt");
    app.press(KeyCode::Char('o')).await;
    app.press(KeyCode::Char('t')).await;

    let bytes = std::fs::read(repo.path().join("a.txt")).unwrap();
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        "ours top\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6\r\ntheirs bottom\r\n"
    );
    let mode = std::fs::metadata(repo.path().join("a.txt"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);

    app.assert_screen_contains("Stage it? (y/n)");
    app.press(KeyCode::Char('y')).await;

    assert_eq!(repo.staged(), ["a.txt"]);
    let unmerged = repo.git(["diff", "--name-only", "--diff-filter=U"]);
    assert!(
        unmerged.is_empty(),
        "{}",
        String::from_utf8_lossy(&unmerged)
    );
    assert_eq!(
        repo.entry("a.txt").map(|e| e.kind),
        Some(EntryKind::Changed)
    );
    app.assert_screen_contains("M  a.txt");
}

#[tokio::test(flavor = "multi_thread")]
async fn committing_in_the_app_updates_the_ahead_count() {
    let repo = on_main();
    repo.git(["branch", "base"]);
    repo.git(["branch", "-q", "--set-upstream-to=base"]);
    let repo = repo.write("a.txt", "two\n").stage("a.txt");
    let mut app = Harness::start(&repo).await;
    app.press(KeyCode::Char('1')).await;
    app.assert_screen_contains("┃base ↑0 ↓0 ");

    app.press(KeyCode::Char('2')).await;
    app.press(KeyCode::Char('c')).await;
    app.type_text("Ahead").await;
    app.press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)
        .await;

    app.press(KeyCode::Char('1')).await;
    app.assert_screen_contains("┃base ↑1 ↓0 ");
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

/// `a.txt` of 20 committed lines, then lines 1 and 20 changed in the
/// worktree: two hunks far enough apart to stay separate.
fn two_hunk_repo() -> TestRepo {
    let lines: String = (1..=20).map(|i| format!("line {i}\n")).collect();
    let changed = lines
        .replace("line 1\n", "line one\n")
        .replace("line 20\n", "line twenty\n");
    TestRepo::new()
        .write("a.txt", &lines)
        .commit("init")
        .write("a.txt", &changed)
}

fn diff_text(repo: &TestRepo, cached: bool) -> String {
    let args: &[&str] = if cached {
        &["diff", "--cached"]
    } else {
        &["diff"]
    };
    String::from_utf8(repo.git(args)).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn space_in_the_diff_stages_one_of_two_hunks() {
    let repo = two_hunk_repo();
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Enter).await;
    app.press(KeyCode::Char('j')).await;
    app.press(KeyCode::Char(' ')).await;

    let cached = diff_text(&repo, true);
    assert!(cached.contains("+line twenty"), "{cached}");
    assert!(!cached.contains("line one"), "{cached}");
    let worktree = diff_text(&repo, false);
    assert!(worktree.contains("+line one"), "{worktree}");
    assert!(!worktree.contains("line twenty"), "{worktree}");
    let entry = repo.entry("a.txt").expect("a.txt is listed");
    assert_eq!((entry.index, entry.worktree), ('M', 'M'));
    assert!(app.state().popups.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn space_in_the_staged_view_unstages_one_hunk() {
    let repo = two_hunk_repo().stage("a.txt");
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Enter).await;
    app.assert_screen_contains(" Staged ");
    app.press(KeyCode::Char(' ')).await;

    let cached = diff_text(&repo, true);
    assert!(cached.contains("+line twenty"), "{cached}");
    assert!(!cached.contains("line one"), "{cached}");
    let worktree = diff_text(&repo, false);
    assert!(worktree.contains("+line one"), "{worktree}");
    assert!(!worktree.contains("line twenty"), "{worktree}");
    assert!(app.state().popups.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn t_switches_between_the_unstaged_and_staged_changes() {
    // Stage only the line-20 change, then make the line-1 change on top.
    let lines: String = (1..=20).map(|i| format!("line {i}\n")).collect();
    let line_20 = lines.replace("line 20\n", "line twenty\n");
    let both = line_20.replace("line 1\n", "line one\n");
    let repo = TestRepo::new()
        .write("a.txt", &lines)
        .commit("init")
        .write("a.txt", &line_20)
        .stage("a.txt")
        .write("a.txt", &both);
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Enter).await;
    app.assert_screen_contains(" Unstaged ");
    app.assert_screen_contains("+line one");
    assert!(!app.screen().iter().any(|row| row.contains("+line twenty")));

    app.press(KeyCode::Char('t')).await;
    app.assert_screen_contains(" Staged ");
    app.assert_screen_contains("+line twenty");
    assert!(!app.screen().iter().any(|row| row.contains("+line one")));

    app.press(KeyCode::Esc).await;
    assert!(!app.state().should_quit);
    app.assert_screen_contains(" Diff ");
}

/// `a.txt` of 20 committed lines, then line 10 replaced by three in the
/// worktree: one hunk of `-line 10`, `+ten a`, `+ten b`, `+ten c`.
fn three_line_repo() -> TestRepo {
    let lines: String = (1..=20).map(|i| format!("line {i}\n")).collect();
    let changed = lines.replace("line 10\n", "ten a\nten b\nten c\n");
    TestRepo::new()
        .write("a.txt", &lines)
        .commit("init")
        .write("a.txt", &changed)
}

/// The `+`/`-` lines of a diff, without the file headers.
fn changes(diff: &str) -> Vec<&str> {
    diff.lines()
        .filter(|l| l.starts_with(['+', '-']) && !l.starts_with("+++") && !l.starts_with("---"))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn space_in_line_mode_stages_a_single_added_line() {
    let repo = three_line_repo();
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Enter).await;
    app.press(KeyCode::Char('v')).await;
    app.press(KeyCode::Char('j')).await;
    app.press(KeyCode::Char(' ')).await;

    assert_eq!(changes(&diff_text(&repo, true)), ["+ten a"]);
    assert_eq!(
        changes(&diff_text(&repo, false)),
        ["-line 10", "+ten b", "+ten c"]
    );
    assert!(app.state().popups.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_range_of_lines_stages_exactly_those_lines() {
    let repo = three_line_repo();
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Enter).await;
    app.press(KeyCode::Char('v')).await;
    app.press_with(KeyCode::Char('J'), KeyModifiers::SHIFT)
        .await;
    app.press_with(KeyCode::Char('J'), KeyModifiers::SHIFT)
        .await;
    app.press(KeyCode::Char(' ')).await;

    assert_eq!(
        changes(&diff_text(&repo, true)),
        ["-line 10", "+ten a", "+ten b"]
    );
    assert_eq!(changes(&diff_text(&repo, false)), ["+ten c"]);
    assert!(app.state().popups.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn d_then_y_discards_a_modified_file_and_n_keeps_it() {
    let repo = TestRepo::new()
        .write("a.txt", "one\n")
        .write("b.txt", "keep\n")
        .commit("init")
        .write("a.txt", "one\ntwo\n")
        .write("b.txt", "keep\nmore\n");
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('d')).await;
    app.assert_screen_contains(" Discard file ");
    app.press(KeyCode::Char('n')).await;
    assert_eq!(repo.read("a.txt"), "one\ntwo\n");
    assert!(repo.entry("a.txt").is_some());
    assert!(app.state().popups.is_empty());

    app.press(KeyCode::Char('d')).await;
    app.press(KeyCode::Char('y')).await;

    assert_eq!(repo.read("a.txt"), "one\n");
    assert_eq!(repo.entry("a.txt"), None);
    assert_eq!(listed(&app), ["b.txt"]);
    // Only the selected file went back.
    assert_eq!(repo.read("b.txt"), "keep\nmore\n");
    assert!(app.state().popups.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn d_then_y_in_line_mode_discards_one_added_line_and_leaves_the_index() {
    let repo = three_line_repo();
    let index_before = repo.git(["show", ":a.txt"]);
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Enter).await;
    app.press(KeyCode::Char('v')).await;
    app.press(KeyCode::Char('j')).await;
    app.press(KeyCode::Char('d')).await;
    app.assert_screen_contains(" Discard lines ");
    app.press(KeyCode::Char('y')).await;

    assert_eq!(
        changes(&diff_text(&repo, false)),
        ["-line 10", "+ten b", "+ten c"]
    );
    assert!(diff_text(&repo, true).is_empty());
    assert_eq!(repo.git(["show", ":a.txt"]), index_before);
    assert!(app.state().popups.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn d_then_y_on_an_untracked_file_deletes_it_from_disk() {
    let repo = TestRepo::new()
        .write("a.txt", "one\n")
        .commit("init")
        .write("new.txt", "fresh\n");
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('d')).await;
    app.assert_screen_contains(" Delete file ");
    app.press(KeyCode::Char('y')).await;

    assert!(!repo.exists("new.txt"));
    assert!(repo.exists("a.txt"));
    app.assert_screen_contains("No changes");
}

/// `a.txt` and `d/a.txt`, each 20 committed lines with lines 1 and 20 then
/// changed in the worktree, so both have two hunks, but with different text
/// (`line` at the root, `sub` below `d/`). `d/e/` exists, holding only a
/// committed placeholder, so Kika can be started there.
fn same_name_repo() -> TestRepo {
    let numbered = |word: &str| -> String { (1..=20).map(|i| format!("{word} {i}\n")).collect() };
    let changed = |word: &str| {
        numbered(word)
            .replace(&format!("{word} 1\n"), &format!("{word} one\n"))
            .replace(&format!("{word} 20\n"), &format!("{word} twenty\n"))
    };
    TestRepo::new()
        .write("a.txt", &numbered("line"))
        .write("d/a.txt", &numbered("sub"))
        .write("d/e/keep", "")
        .commit("init")
        .write("a.txt", &changed("line"))
        .write("d/a.txt", &changed("sub"))
}

fn listed(app: &Harness) -> Vec<String> {
    app.state()
        .files
        .iter()
        .map(|entry| entry.path.display().to_string())
        .collect()
}

/// Move the Files selection down until `name` is highlighted.
/// `a.txt` committed as `one`, then `two` and `three` added, with `body`
/// as the editor. The editor writes the file it was given to
/// `.git/edited-path` first, so a test can check it is gone afterwards.
#[cfg(unix)]
fn edit_repo(body: &str) -> TestRepo {
    TestRepo::new()
        .write("a.txt", "one\n")
        .commit("init")
        .write("a.txt", "one\ntwo\nthree\n")
        .editor(&format!("echo \"$1\" > .git/edited-path\n{body}"))
}

/// The hunk file the editor was given no longer exists.
#[cfg(unix)]
fn assert_hunk_file_removed(repo: &TestRepo) {
    let path = repo.read(".git/edited-path");
    let path = std::path::Path::new(path.trim_end());
    assert!(path.starts_with(std::env::temp_dir()), "{path:?}");
    assert!(!path.exists(), "{path:?} was left behind");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn e_stages_the_hunk_as_edited_in_the_editor() {
    let repo = edit_repo("grep -v '^+three$' \"$1\" > \"$1.new\" && mv \"$1.new\" \"$1\"");
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Enter).await;
    app.press(KeyCode::Char('e')).await;

    assert_eq!(repo.git(["show", ":a.txt"]), b"one\ntwo\n");
    assert_eq!(repo.read("a.txt"), "one\ntwo\nthree\n");
    assert_eq!(app.state().popups, []);
    assert_hunk_file_removed(&repo);
    // The view reloaded: only `three` is left to stage.
    app.assert_screen_contains("+three");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_garbage_edit_shows_an_error_and_stages_nothing() {
    let repo = edit_repo("printf 'garbage\\n' > \"$1\"");
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Enter).await;
    app.press(KeyCode::Char('e')).await;

    app.assert_screen_contains("Could not stage the edited hunk");
    assert!(repo.staged().is_empty());
    assert_eq!(repo.git(["diff", "--cached"]), b"");
    assert_hunk_file_removed(&repo);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn emptying_the_hunk_file_cancels_the_edit() {
    let repo = edit_repo(": > \"$1\"");
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Enter).await;
    app.press(KeyCode::Char('e')).await;

    assert!(repo.staged().is_empty());
    assert_eq!(app.state().notice, Some("Edit cancelled"));
    assert_eq!(app.state().popups, []);
    assert_hunk_file_removed(&repo);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn o_opens_the_file_and_its_change_shows_after_the_editor_closes() {
    let repo = edit_repo("echo appended >> \"$1\"");
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('o')).await;

    assert_eq!(repo.read("a.txt"), "one\ntwo\nthree\nappended\n");
    assert_eq!(
        repo.read(".git/edited-path").trim_end(),
        repo.path().join("a.txt").display().to_string()
    );
    app.assert_screen_contains("+appended");
    assert_eq!(app.state().popups, []);
}

async fn select_file(app: &mut Harness, name: &str) {
    for _ in 0..listed(app).len() {
        if app.selected_line().is_some_and(|line| line.ends_with(name)) {
            return;
        }
        app.press(KeyCode::Char('j')).await;
    }
    panic!("{name} never selected:\n{}", app.screen().join("\n"));
}

#[tokio::test(flavor = "multi_thread")]
async fn started_in_a_subdirectory_kika_acts_on_root_relative_paths() {
    let repo = same_name_repo();
    let from_root = listed(&Harness::start(&repo).await);
    let sub_before = repo.read("d/a.txt");

    let mut app = Harness::start_from(&repo, "d/e").await;

    // Status: the same files as from the root.
    assert_eq!(listed(&app), ["a.txt", "d/a.txt"]);
    assert_eq!(listed(&app), from_root);
    assert!(app.state().status_error.is_none());

    // Diff: the root a.txt, not d/a.txt.
    let selected = app.selected_line().expect("a file is selected");
    assert!(
        selected.ends_with("a.txt") && !selected.contains("d/"),
        "selected: {selected:?}"
    );
    app.assert_screen_contains("+line one");
    let screen = app.screen();
    assert!(
        !screen.iter().any(|row| row.contains("sub ")),
        "{}",
        screen.join("\n")
    );

    // Hunk staging: the second hunk of the root a.txt only.
    app.press(KeyCode::Enter).await;
    app.press(KeyCode::Char('j')).await;
    app.press(KeyCode::Char(' ')).await;
    assert!(app.state().popups.is_empty(), "{}", app.screen().join("\n"));
    assert_eq!(repo.staged(), ["a.txt"]);
    let cached = diff_text(&repo, true);
    assert!(cached.contains("+line twenty"), "{cached}");
    assert!(!cached.contains("line one"), "{cached}");
    assert!(!cached.contains("sub "), "{cached}");
    assert_eq!(repo.read("d/a.txt"), sub_before);
    let entry = repo.entry("d/a.txt").expect("d/a.txt is listed");
    assert_eq!((entry.index, entry.worktree), ('.', 'M'));

    // Whole-file staging and unstaging of d/a.txt.
    app.press(KeyCode::Esc).await;
    select_file(&mut app, "d/a.txt").await;
    app.press(KeyCode::Char(' ')).await;
    assert_eq!(repo.staged(), ["a.txt", "d/a.txt"]);
    app.press(KeyCode::Char(' ')).await;
    assert_eq!(repo.staged(), ["a.txt"]);
    let cached = diff_text(&repo, true);
    assert!(cached.contains("+line twenty"), "{cached}");

    // Unstage the root a.txt, outside the launch directory, from its staged view.
    app.press(KeyCode::Char('k')).await;
    let selected = app.selected_line().expect("a file is selected");
    assert!(!selected.contains("d/"), "selected: {selected:?}");
    app.press(KeyCode::Enter).await;
    app.press(KeyCode::Char('t')).await;
    app.assert_screen_contains(" Staged ");
    app.press(KeyCode::Char(' ')).await;
    assert!(app.state().popups.is_empty(), "{}", app.screen().join("\n"));
    assert!(repo.staged().is_empty());
    assert!(!app.state().should_quit);
}

#[tokio::test(flavor = "multi_thread")]
async fn started_outside_a_repository_the_files_panel_shows_gits_error() {
    let outside = crate::git::fake::TempDir::new("outside");
    let launch = outside.path().to_path_buf();
    let top = resolve_workdir(&CliRunner::isolated(&launch), launch.clone());
    assert_eq!(top, launch);

    let mut app = Harness::start_with(CliRunner::isolated(top), 80, 24).await;

    // The welcome first, with no status asked for yet.
    app.assert_screen_contains("Not a git repository");
    app.assert_screen_contains("Initialise a repository here");
    assert_eq!(app.state().status_error, None);

    // Closed, git's own error shows as it always did.
    app.press(KeyCode::Esc).await;
    let error = app.state().status_error.clone().expect("status failed");
    assert!(error.contains("not a git repository"), "{error}");
    app.assert_screen_contains("not a git repository");
    assert!(!app.state().should_quit);
    app.press(KeyCode::Esc).await;
    app.press(KeyCode::Char('r')).await;
    assert!(!app.state().should_quit);
    app.assert_screen_contains("not a git repository");

    // And O brings the welcome back.
    app.press(KeyCode::Esc).await;
    app.press_with(KeyCode::Char('O'), KeyModifiers::SHIFT)
        .await;
    app.assert_screen_contains("Initialise a repository here");
}

#[tokio::test(flavor = "multi_thread")]
async fn started_outside_a_repository_i_initialises_it_and_the_files_load() {
    let outside = crate::git::fake::TempDir::new("outside");
    let launch = outside.path().to_path_buf();
    std::fs::write(launch.join("notes.txt"), "hello\n").unwrap();

    let mut app = Harness::start_with(CliRunner::isolated(&launch), 80, 24).await;
    app.assert_screen_contains("Initialise a repository here");
    app.press(KeyCode::Char('i')).await;

    assert!(launch.join(".git").is_dir());
    let repo = app.state().repo.clone().expect("a repository is open");
    assert_eq!(repo.canonicalize().unwrap(), launch.canonicalize().unwrap());
    assert!(app.state().popups.is_empty(), "{}", app.screen().join("\n"));
    assert_eq!(app.state().status_error, None);
    assert_eq!(app.selected_line().as_deref(), Some("?? notes.txt"));
    app.assert_screen_contains("+hello");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_recent_list_switches_repositories_and_back_restores_the_view() {
    use crate::intent::AppIntent;
    use crate::model::Focus;

    let a = TestRepo::new()
        .write("a1.txt", "one\n")
        .write("a2.txt", "one\n")
        .commit("init")
        .write("a1.txt", "two\n")
        .write("a2.txt", "two\n");
    let b = TestRepo::new()
        .write("b.txt", "one\n")
        .commit("init")
        .write("b.txt", "bee\n");
    let mut app = Harness::start(&a).await;
    let a_top = app.state().repo.clone().expect("started in a");
    app.press(KeyCode::Char('j')).await;
    app.press(KeyCode::Enter).await;
    assert_eq!(app.selected_line().as_deref(), Some("M a2.txt"));

    // O, with B on the list, then Enter: B's files.
    app.press_with(KeyCode::Char('O'), KeyModifiers::SHIFT)
        .await;
    // The harness has no store, so the real load answered with an empty
    // list; this answers the same request with A and B instead.
    app.intent(AppIntent::RecentLoaded {
        seq: app.state().recent_seq,
        list: vec![a_top.clone(), b.path().to_path_buf()],
    })
    .await;
    app.assert_screen_contains(" Recent repositories ");
    app.press(KeyCode::Enter).await;
    assert_eq!(app.state().repo.as_deref(), Some(b.path()));
    assert_eq!(app.selected_line().as_deref(), Some("M b.txt"));
    app.assert_screen_contains("+bee");
    assert_eq!(app.state().focus, Focus::Files);

    // Back to A: the same file, with the Diff pane focused.
    app.intent(AppIntent::OpenRepo(a_top.clone())).await;
    assert_eq!(app.state().repo, Some(a_top));
    assert_eq!(app.selected_line().as_deref(), Some("M a2.txt"));
    assert!(matches!(app.state().focus, Focus::Diff(_)));
    app.assert_screen_contains("+two");
    assert_eq!(app.state().status_error, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn w_hides_a_change_that_only_touches_indentation_and_shows_it_again() {
    let repo = TestRepo::new()
        .write("a.txt", "fn main() {\nindent me\n}\n")
        .commit("init")
        .write("a.txt", "fn main() {\n    indent me\n}\nadded\n");
    let mut app = Harness::start(&repo).await;
    app.assert_screen_contains("-indent me");
    app.assert_screen_contains("+    indent me");
    app.assert_screen_contains("+added");

    app.press(KeyCode::Char('w')).await;

    app.assert_screen_contains("whitespace hidden");
    app.assert_screen_contains("+added");
    let screen = app.screen();
    assert!(
        !screen
            .iter()
            .any(|row| row.contains("-indent me") || row.contains("+    indent me")),
        "the indentation change is still shown:\n{}",
        screen.join("\n")
    );

    app.press(KeyCode::Char('w')).await;

    app.assert_screen_contains("-indent me");
    app.assert_screen_contains("+    indent me");
    let screen = app.screen();
    assert!(
        !screen.iter().any(|row| row.contains("whitespace hidden")),
        "{}",
        screen.join("\n")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn shift_a_then_ctrl_s_folds_the_staged_change_into_head_keeping_its_message() {
    let repo = modified_repo().stage("a.txt");
    let mut app = Harness::start(&repo).await;

    app.press_with(KeyCode::Char('A'), KeyModifiers::SHIFT)
        .await;
    app.assert_screen_contains(" Amend last commit ");
    app.assert_screen_contains("init");
    app.press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)
        .await;

    assert_eq!(repo.git(["rev-list", "--count", "HEAD"]), b"1\n");
    assert_eq!(repo.git(["show", "HEAD:a.txt"]), b"one\ntwo\n");
    assert_eq!(repo.head_message(), "init\n\n");
    assert!(repo.staged().is_empty());
    assert_eq!(app.state().commit, crate::model::CommitFlow::Closed);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_n_skips_a_failing_hook_when_amending_with_a_new_message() {
    let repo = modified_repo()
        .stage("a.txt")
        .hook("pre-commit", "#!/bin/sh\necho 'lint failed'\nexit 1\n");
    let mut app = Harness::start(&repo).await;

    app.press_with(KeyCode::Char('A'), KeyModifiers::SHIFT)
        .await;
    app.press_with(KeyCode::Char('n'), KeyModifiers::CONTROL)
        .await;
    app.assert_screen_contains(" hooks skipped ");
    app.type_text(" again").await;
    app.press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)
        .await;

    assert_eq!(repo.git(["rev-list", "--count", "HEAD"]), b"1\n");
    assert_eq!(repo.head_message(), "init again\n\n");
    assert!(repo.staged().is_empty());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_e_commits_the_message_written_in_the_editor() {
    let repo = modified_repo().stage("a.txt").editor(
        "grep -q '^# Save and close to commit' \"$1\" && \
         printf 'From the editor\\n\\nWith a body.\\n# dropped\\n' > \"$1\"",
    );
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('c')).await;
    app.type_text("typed").await;
    app.press_with(KeyCode::Char('e'), KeyModifiers::CONTROL)
        .await;

    assert_eq!(repo.head_message(), "From the editor\n\nWith a body.\n\n");
    assert_eq!(repo.git(["rev-list", "--count", "HEAD"]), b"2\n");
    assert!(repo.staged().is_empty());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn emptying_the_message_in_the_editor_aborts_and_keeps_the_popup() {
    let repo = modified_repo().stage("a.txt").editor(": > \"$1\"");
    let head = repo.head();
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('c')).await;
    app.type_text("typed").await;
    app.press_with(KeyCode::Char('e'), KeyModifiers::CONTROL)
        .await;

    assert_eq!(repo.head(), head);
    assert_eq!(repo.staged(), ["a.txt"]);
    assert_eq!(app.state().notice, Some("Empty message; commit aborted"));
    app.assert_screen_contains("typed");
}

/// A parent with `a.txt` and `b.txt` changed, and a submodule `sub` cloned
/// from another local repository holding `inner.txt`. Returns both, since
/// the parent's submodule is cloned from the other's path.
fn repo_with_submodule() -> (TestRepo, TestRepo) {
    let inner = TestRepo::new().write("inner.txt", "one\n").commit("init");
    let source = inner.path().to_str().expect("a UTF-8 temp dir").to_string();
    let parent = TestRepo::new()
        .write("a.txt", "one\n")
        .write("b.txt", "one\n")
        .commit("init");
    parent.git([
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        "-q",
        source.as_str(),
        "sub",
    ]);
    let parent = parent
        .commit("add sub")
        .write("a.txt", "two\n")
        .write("b.txt", "two\n");
    (parent, inner)
}

/// Git refuses local clones for submodules by default; only this test's
/// runner allows them, through the environment.
fn runner_allowing_local_clones(repo: &TestRepo) -> CliRunner {
    repo.runner()
        .with_env("GIT_CONFIG_COUNT", "1")
        .with_env("GIT_CONFIG_KEY_0", "protocol.file.allow")
        .with_env("GIT_CONFIG_VALUE_0", "always")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_submodule_is_listed_updated_entered_and_left() {
    use crate::git::submodule::SubmoduleState;
    use crate::popup::Popup;

    let (parent, _inner) = repo_with_submodule();
    let mut app = Harness::start_with(runner_allowing_local_clones(&parent), 80, 24).await;
    app.press(KeyCode::Char('j')).await;
    assert_eq!(app.selected_line().as_deref(), Some("M b.txt"));

    let states = |app: &Harness| match app.state().popups.last() {
        Some(Popup::Submodules(list)) => list
            .items
            .iter()
            .flatten()
            .map(|s| (s.path.display().to_string(), s.state))
            .collect::<Vec<_>>(),
        other => panic!("expected the Submodules popup, got {other:?}"),
    };

    // Listed as checked out.
    app.press_with(KeyCode::Char('S'), KeyModifiers::SHIFT)
        .await;
    app.assert_screen_contains(" Submodules ");
    app.assert_screen_contains("sub");
    assert_eq!(states(&app), [("sub".into(), SubmoduleState::UpToDate)]);
    app.press(KeyCode::Esc).await;

    // Deinitialized outside Kika, then brought back with `u`.
    parent.git(["submodule", "deinit", "-q", "-f", "sub"]);
    assert!(!parent.exists("sub/inner.txt"));
    app.press_with(KeyCode::Char('S'), KeyModifiers::SHIFT)
        .await;
    assert_eq!(
        states(&app),
        [("sub".into(), SubmoduleState::Uninitialized)]
    );
    app.assert_screen_contains("not initialized");
    app.press(KeyCode::Char('u')).await;
    assert_eq!(states(&app), [("sub".into(), SubmoduleState::UpToDate)]);
    app.assert_screen_contains("up to date");
    assert_eq!(parent.read("sub/inner.txt"), "one\n");

    // Entered: its own changes show, and the Status title says where.
    std::fs::write(parent.path().join("sub/inner.txt"), "changed\n").unwrap();
    app.press(KeyCode::Enter).await;
    assert!(app.state().popups.is_empty());
    assert_eq!(app.selected_line().as_deref(), Some("M inner.txt"));
    app.assert_screen_contains("Status · sub");
    app.assert_screen_contains("+changed");
    let screen = app.screen().join("\n");
    assert!(!screen.contains("a.txt"), "{screen}");

    // Left: the parent's files and selection are back.
    app.press(KeyCode::Backspace).await;
    assert_eq!(app.selected_line().as_deref(), Some("M b.txt"));
    app.assert_screen_contains("a.txt");
    app.assert_screen_contains("+two");
    let screen = app.screen().join("\n");
    assert!(!screen.contains("Status · sub"), "{screen}");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_line_is_staged_through_a_renderer_view() {
    use crate::external_diff::ExternalRenderer;

    let repo = TestRepo::new()
        .write("a.txt", "one\n")
        .commit("init")
        .write("a.txt", "one\ntwo\nthree\n");
    let mut app = Harness::start(&repo).await;
    app.use_renderers(vec![ExternalRenderer {
        label: "cat".into(),
        argv: vec!["cat".into()],
    }])
    .await;

    app.press(KeyCode::Enter).await;
    app.press_with(KeyCode::Char('R'), KeyModifiers::SHIFT)
        .await;
    app.assert_screen_contains(" Unstaged a.txt · cat ");
    app.press(KeyCode::Char('v')).await;
    app.assert_screen_contains(" Unstaged a.txt · lines · cat ");
    let shown = app
        .state()
        .rendered_shown()
        .expect("the rendered rows are shown");
    assert!(shown.map.iter().any(Option::is_some));
    app.press(KeyCode::Char(' ')).await;

    let staged = String::from_utf8(repo.git(["diff", "--cached"])).unwrap();
    assert!(staged.contains("+two\n"), "{staged}");
    assert!(!staged.contains("+three"), "{staged}");
    app.assert_screen_contains(" Unstaged a.txt · lines · cat ");
    app.assert_screen_contains("+three");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_edited_outside_appears_on_the_next_files_poll() {
    let repo = TestRepo::new().write("a.txt", "one\n").commit("init");
    let mut app = Harness::start(&repo).await;
    assert!(app.state().files.is_empty());

    let _repo = repo.write("outside.txt", "new\n");
    app.intent(AppIntent::Watch(WatchEvent::PollFiles)).await;

    app.assert_screen_contains("outside.txt");
    assert_eq!(app.state().busy, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_commit_made_outside_disappears_from_the_list_on_focus_in() {
    let repo = modified_repo();
    let mut app = Harness::start(&repo).await;
    app.assert_screen_contains("a.txt");

    repo.git(["commit", "-q", "-am", "elsewhere"]);
    app.intent(AppIntent::Watch(WatchEvent::FocusGained)).await;

    let screen = app.screen().join("\n");
    assert!(!screen.contains("a.txt"), "a.txt still listed:\n{screen}");
    assert!(app.state().files.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_commit_made_outside_is_noticed_by_the_refs_poll() {
    let repo = modified_repo();
    let mut app = Harness::start(&repo).await;
    // The first snapshot is the baseline.
    app.intent(AppIntent::Watch(WatchEvent::PollRefs)).await;
    app.assert_screen_contains("a.txt");

    repo.git(["commit", "-q", "-am", "elsewhere"]);
    app.intent(AppIntent::Watch(WatchEvent::PollRefs)).await;

    assert!(app.state().files.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_commits_panel_shows_head_and_loads_more_as_the_cursor_nears_the_end() {
    let repo = TestRepo::new().history(260);
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('4')).await;
    app.press(KeyCode::Char('z')).await;
    app.assert_screen_contains("commit 260");
    assert_eq!(app.state().commits.rows.len(), 200);

    for _ in 0..210 {
        app.press(KeyCode::Char('j')).await;
    }

    let commits = &app.state().commits;
    assert_eq!(commits.rows.len(), 260);
    assert!(commits.done);
    assert_eq!(commits.selected, 210);
    assert_eq!(commits.rows[210].subject, "commit 50");
    app.assert_screen_contains("commit 50");
}

/// Every local branch's name, as git lists them.
fn branch_names(repo: &TestRepo) -> Vec<String> {
    String::from_utf8(repo.git(["for-each-ref", "--format=%(refname:short)", "refs/heads"]))
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

fn current_branch(repo: &TestRepo) -> String {
    String::from_utf8(repo.git(["symbolic-ref", "--short", "HEAD"]))
        .unwrap()
        .trim()
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn space_in_the_branches_panel_checks_out_the_selected_branch() {
    let repo = on_main();
    repo.git(["branch", "side"]);
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('3')).await;
    app.assert_screen_contains("* main");
    app.press(KeyCode::Char('j')).await;
    app.assert_screen_contains("[0] Commits · side");
    app.press(KeyCode::Char(' ')).await;

    assert_eq!(current_branch(&repo), "side");
    app.assert_screen_contains("* side");
    // The Status panel follows, collapsed to its branch row.
    let status = app.screen()[1].clone();
    assert!(status.contains("│side "), "{status:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn n_creates_a_branch_from_the_selection_and_checks_it_out() {
    let repo = on_main();
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('3')).await;
    app.press(KeyCode::Char('n')).await;
    app.assert_screen_contains("New branch from main");
    app.type_text("fresh\n").await;

    assert_eq!(current_branch(&repo), "fresh");
    assert_eq!(branch_names(&repo), ["fresh", "main"]);
    app.assert_screen_contains("* fresh");
}

#[tokio::test(flavor = "multi_thread")]
async fn d_then_shift_d_force_deletes_an_unmerged_branch() {
    let repo = on_main();
    repo.git(["checkout", "-q", "-b", "wip"]);
    let repo = repo.write("b.txt", "wip\n").commit("wip work");
    repo.git(["checkout", "-q", "main"]);
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('3')).await;
    app.press(KeyCode::Char('j')).await;
    app.press(KeyCode::Char('d')).await;
    app.assert_screen_contains("wip is not fully merged; its commits may be lost.");
    // `y` does not answer this question.
    app.press(KeyCode::Char('y')).await;
    assert_eq!(branch_names(&repo), ["main", "wip"]);
    app.press_with(KeyCode::Char('D'), KeyModifiers::SHIFT)
        .await;

    assert_eq!(branch_names(&repo), ["main"]);
    let screen = app.screen();
    assert!(
        !screen.iter().any(|row| row.contains("wip")),
        "{}",
        screen.join("\n")
    );
}

/// `on_main` with `origin`, a bare repository on disk, as its upstream,
/// and another clone of it to push from as someone else would.
fn with_origin() -> (TestRepo, TempDir) {
    let repo = on_main();
    let remote = TempDir::new("origin");
    let bare = remote.path().join("origin.git");
    let other = remote.path().join("other");
    git(remote.path(), &["init", "-q", "--bare", "origin.git"]);
    repo.git([
        OsStr::new("remote"),
        OsStr::new("add"),
        OsStr::new("origin"),
        bare.as_os_str(),
    ]);
    repo.git(["push", "-q", "-u", "origin", "main"]);
    let clone = [
        OsStr::new("clone"),
        OsStr::new("-q"),
        OsStr::new("-b"),
        OsStr::new("main"),
    ];
    let paths = [bare.as_os_str(), other.as_os_str()];
    git(remote.path(), &[&clone[..], &paths[..]].concat());
    for (key, value) in [("user.name", "Other"), ("user.email", "o@example.com")] {
        git(&other, &["config", key, value]);
    }
    (repo, remote)
}

#[tokio::test(flavor = "multi_thread")]
async fn shift_p_pushes_and_f_then_p_fetch_and_pull_over_a_file_path() {
    let (repo, remote) = with_origin();
    let repo = repo.write("a.txt", "two\n").commit("ahead");
    let mut app = Harness::start(&repo).await;
    app.press(KeyCode::Char('1')).await;
    app.assert_screen_contains("┃origin/main ↑1 ↓0 ");

    app.press_with(KeyCode::Char('P'), KeyModifiers::SHIFT)
        .await;

    app.assert_screen_contains("┃origin/main ↑0 ↓0 ");
    let pushed = git(
        remote.path().join("origin.git").as_path(),
        &["rev-parse", "main"],
    );
    assert_eq!(
        String::from_utf8(pushed).unwrap().trim(),
        repo.head().trim()
    );

    // Someone else pushes; a fetch sees it, a pull brings it in.
    let other = remote.path().join("other");
    git(&other, &["pull", "-q"]);
    std::fs::write(other.join("b.txt"), "theirs\n").unwrap();
    git(&other, &["add", "b.txt"]);
    git(&other, &["commit", "-q", "-m", "theirs"]);
    git(&other, &["push", "-q"]);

    app.press(KeyCode::Char('f')).await;
    app.assert_screen_contains("┃origin/main ↑0 ↓1 ");
    assert!(!repo.exists("b.txt"));

    app.press(KeyCode::Char('p')).await;
    app.assert_screen_contains("┃origin/main ↑0 ↓0 ");
    assert_eq!(repo.read("b.txt"), "theirs\n");
    assert_eq!(app.state().popups, []);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_push_behind_the_remote_offers_a_lease_and_y_forces_it() {
    let (repo, remote) = with_origin();
    let other = remote.path().join("other");
    std::fs::write(other.join("b.txt"), "theirs\n").unwrap();
    git(&other, &["add", "b.txt"]);
    git(&other, &["commit", "-q", "-m", "theirs"]);
    git(&other, &["push", "-q"]);
    // Fetched, so the lease knows what it replaces.
    repo.git(["fetch", "-q"]);
    let repo = repo.write("a.txt", "mine\n").commit("mine");
    let mut app = Harness::start(&repo).await;

    app.press_with(KeyCode::Char('P'), KeyModifiers::SHIFT)
        .await;
    app.assert_screen_contains("The remote has commits this branch does not.");
    app.press(KeyCode::Char('y')).await;

    let bare = remote.path().join("origin.git");
    let pushed = git(bare.as_path(), &["rev-parse", "main"]);
    assert_eq!(
        String::from_utf8(pushed).unwrap().trim(),
        repo.head().trim()
    );
    assert_eq!(app.state().popups, []);
}

/// What `git status` says, branch line included, to compare before and after.
fn porcelain_status(repo: &TestRepo) -> String {
    String::from_utf8(repo.git(["status", "--porcelain=v2", "--branch"])).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_conflicting_merge_commit_is_aborted_from_the_status_panel() {
    let repo = diverged();
    let head = repo.head();
    let status = porcelain_status(&repo);
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('3')).await;
    app.press(KeyCode::Char('j')).await;
    app.assert_screen_contains("[0] Commits · other");
    app.press(KeyCode::Char('m')).await;
    app.assert_screen_contains("Merge / rebase");
    app.press(KeyCode::Char('j')).await;
    app.press(KeyCode::Enter).await;

    // Git stopped on the conflict; its output went to the terminal.
    app.assert_screen_contains("Could not merge");
    app.press(KeyCode::Esc).await;
    assert_eq!(repo.entry("a.txt").unwrap().kind, EntryKind::Conflicted);
    app.press(KeyCode::Char('1')).await;
    app.assert_screen_contains("┃merging ");

    app.press(KeyCode::Char('a')).await;
    app.assert_screen_contains("Abort the merge and return to where it started?");
    app.press(KeyCode::Char('y')).await;

    assert_eq!(repo.head(), head);
    assert_eq!(current_branch(&repo), "main");
    assert_eq!(repo.read("a.txt"), "ours\n");
    assert_eq!(porcelain_status(&repo), status);
    let in_progress = &app.state().branch.as_ref().unwrap().in_progress;
    assert!(in_progress.is_empty(), "{in_progress:?}");
    let screen = app.screen();
    assert!(
        !screen.iter().any(|row| row.contains("merging")),
        "{}",
        screen.join("\n")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rebasing_onto_a_branch_from_the_menu_replays_the_current_one_on_its_tip() {
    let repo = on_main();
    repo.git(["checkout", "-q", "-b", "feature"]);
    let repo = repo.write("b.txt", "feature\n").commit("on feature");
    repo.git(["checkout", "-q", "main"]);
    let repo = repo.write("c.txt", "main\n").commit("on main");
    let main = repo.head();
    repo.git(["checkout", "-q", "feature"]);
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('3')).await;
    app.press(KeyCode::Char('j')).await;
    app.assert_screen_contains("[0] Commits · main");
    app.press(KeyCode::Char('m')).await;
    for _ in 0..3 {
        app.press(KeyCode::Char('j')).await;
    }
    app.press(KeyCode::Enter).await;

    assert_eq!(current_branch(&repo), "feature");
    assert!(
        repo.try_git(["merge-base", "--is-ancestor", main.trim(), "HEAD"]),
        "main's tip is not under feature"
    );
    assert_eq!(repo.head_message(), "on feature\n\n");
    assert_eq!(repo.read("b.txt"), "feature\n");
    assert_eq!(repo.read("c.txt"), "main\n");
    assert_eq!(
        porcelain_status(&repo),
        "# branch.oid ".to_string() + repo.head().trim() + "\n# branch.head feature\n"
    );
    let screen = app.screen();
    assert!(
        !screen.iter().any(|row| row.contains("Could not")),
        "{}",
        screen.join("\n")
    );
}

/// `a.txt` committed on `main` as `one`, then as `two`. Returns the repo
/// and the first commit's id.
fn two_commits() -> (TestRepo, String) {
    let repo = on_main();
    let first = repo.head().trim().to_string();
    (repo.write("a.txt", "two\n").commit("second"), first)
}

#[tokio::test(flavor = "multi_thread")]
async fn space_then_y_in_the_commits_panel_detaches_head_at_the_commit() {
    let (repo, first) = two_commits();
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('4')).await;
    app.press(KeyCode::Char('j')).await;
    app.press(KeyCode::Char(' ')).await;
    app.assert_screen_contains("Detached checkout");
    app.assert_screen_contains(&format!("Check out {}", &first[..7]));
    app.press(KeyCode::Char('y')).await;

    assert_eq!(repo.head().trim(), first);
    assert!(!repo.try_git(["symbolic-ref", "-q", "HEAD"]));
    assert_eq!(repo.read("a.txt"), "one\n");
    app.assert_screen_contains(&format!("detached {}", &first[..7]));
}

#[tokio::test(flavor = "multi_thread")]
async fn capital_r_reverts_the_selected_commit_with_gits_editor() {
    let (repo, first) = two_commits();
    repo.git(["config", "core.editor", "true"]);
    let second = repo.head().trim().to_string();
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('4')).await;
    app.press(KeyCode::Char('R')).await;

    assert_eq!(current_branch(&repo), "main");
    let parent = String::from_utf8(repo.git(["rev-parse", "HEAD^"])).unwrap();
    assert_eq!(parent.trim(), second);
    assert_eq!(repo.read("a.txt"), "one\n");
    assert!(repo.head_message().starts_with("Revert \"second\""));
    assert_ne!(repo.head().trim(), first);
    let screen = app.screen();
    assert!(
        !screen.iter().any(|row| row.contains("Could not")),
        "{}",
        screen.join("\n")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hard_reset_from_the_menu_asks_then_leaves_a_clean_worktree_at_the_target() {
    let (repo, first) = two_commits();
    let repo = repo.write("a.txt", "dirty\n");
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('4')).await;
    app.press(KeyCode::Char('j')).await;
    app.press(KeyCode::Char('X')).await;
    app.assert_screen_contains("Reset to commit");
    app.press(KeyCode::Char('j')).await;
    app.press(KeyCode::Char('j')).await;
    app.press(KeyCode::Enter).await;
    app.assert_screen_contains("throw away every uncommitted change");
    // Enter does not answer; the work is still there.
    app.press(KeyCode::Enter).await;
    assert_eq!(repo.read("a.txt"), "dirty\n");
    app.press(KeyCode::Char('y')).await;

    assert_eq!(current_branch(&repo), "main");
    assert_eq!(repo.head().trim(), first);
    assert_eq!(repo.read("a.txt"), "one\n");
    assert_eq!(
        porcelain_status(&repo),
        format!("# branch.oid {first}\n# branch.head main\n")
    );
}

fn tag_names(repo: &TestRepo) -> Vec<String> {
    String::from_utf8(repo.git(["for-each-ref", "--format=%(refname:short)", "refs/tags"]))
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

/// What `git cat-file -t` says the tag ref holds: `commit` or `tag`.
fn tag_kind(repo: &TestRepo, name: &str) -> String {
    let kind = repo.git(["cat-file", "-t", &format!("refs/tags/{name}")]);
    String::from_utf8(kind).unwrap().trim().to_string()
}

/// Focus the Branches panel and show its Tags tab.
async fn show_tags(app: &mut Harness) {
    app.press(KeyCode::Char('3')).await;
    app.press(KeyCode::Char(']')).await;
    app.press(KeyCode::Char(']')).await;
    app.assert_screen_contains("Loc Rem [Tags]");
}

#[tokio::test(flavor = "multi_thread")]
async fn t_and_shift_t_tag_a_commit_and_the_tags_tab_lists_and_deletes_them() {
    let (repo, first) = two_commits();
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('4')).await;
    app.press(KeyCode::Char('j')).await;
    app.press(KeyCode::Char('t')).await;
    app.assert_screen_contains(&format!("New tag on {}", &first[..7]));
    app.type_text("light\n").await;
    app.press(KeyCode::Char('T')).await;
    app.type_text("heavy\n").await;
    app.assert_screen_contains("Message of tag heavy");
    app.type_text("-m looks like an option\n").await;

    assert_eq!(tag_kind(&repo, "light"), "commit");
    assert_eq!(tag_kind(&repo, "heavy"), "tag");
    for peeled in ["light^{commit}", "heavy^{commit}"] {
        let oid = String::from_utf8(repo.git(["rev-parse", peeled])).unwrap();
        assert_eq!(oid.trim(), first, "{peeled}");
    }
    let message = repo.git(["tag", "-l", "--format=%(contents:subject)", "heavy"]);
    assert_eq!(message, b"-m looks like an option\n");

    show_tags(&mut app).await;
    app.assert_screen_contains("◆ heavy");
    app.assert_screen_contains(&format!("  light  {}", &first[..7]));
    if app.state().branches.tags.selected_tag().unwrap().name != "light".into() {
        app.press(KeyCode::Char('j')).await;
    }
    app.assert_screen_contains("Commits · light");
    app.press(KeyCode::Char('d')).await;
    app.assert_screen_contains("Delete tag light? It is not deleted on any remote.");
    // Enter does not answer; the tag is still there.
    app.press(KeyCode::Enter).await;
    assert_eq!(tag_names(&repo), ["heavy", "light"]);
    app.press(KeyCode::Char('y')).await;

    assert_eq!(tag_names(&repo), ["heavy"]);
    let screen = app.screen();
    assert!(
        !screen.iter().any(|row| row.contains("  light  ")),
        "{}",
        screen.join("\n")
    );
    app.assert_screen_contains("◆ heavy");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hand_made_tag_named_like_an_option_is_listed_and_deleted() {
    let repo = on_main();
    repo.git(["update-ref", "refs/tags/-x", "HEAD"]);
    let mut app = Harness::start(&repo).await;

    show_tags(&mut app).await;
    app.assert_screen_contains("  -x  ");
    app.press(KeyCode::Char('d')).await;
    app.assert_screen_contains("Delete tag -x?");
    app.press(KeyCode::Char('y')).await;

    assert!(tag_names(&repo).is_empty());
    app.assert_screen_contains("No tags");
    assert_eq!(app.state().popups, []);
}

#[tokio::test(flavor = "multi_thread")]
async fn shift_p_on_the_tags_tab_pushes_the_tag_to_the_upstreams_remote() {
    let (repo, remote) = with_origin();
    repo.git(["tag", "-a", "-m", "one", "v1"]);
    let mut app = Harness::start(&repo).await;

    show_tags(&mut app).await;
    app.press_with(KeyCode::Char('P'), KeyModifiers::SHIFT)
        .await;

    let listed = git(
        remote.path().join("origin.git").as_path(),
        &["ls-remote", "--tags", "."],
    );
    let listed = String::from_utf8(listed).unwrap();
    assert!(listed.contains("refs/tags/v1"), "{listed}");
    assert_eq!(app.state().popups, []);
}

/// Deletes a directory made beside a test repository, which the repo's
/// own temp dir does not cover, even when the test fails.
#[cfg(unix)]
struct Beside(std::path::PathBuf);

#[cfg(unix)]
impl Drop for Beside {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `main` with a branch `feature`, and where Kika's suggested worktree
/// for it goes: `../<repo dir>-feature`, removed when dropped.
#[cfg(unix)]
fn with_feature() -> (TestRepo, Beside) {
    let repo = on_main();
    repo.git(["branch", "feature"]);
    let top = repo.path().canonicalize().unwrap();
    let name = top.file_name().unwrap().to_str().unwrap().to_string();
    let beside = Beside(top.parent().unwrap().join(format!("{name}-feature")));
    (repo, beside)
}

/// What `git worktree list --porcelain` says, as text.
#[cfg(unix)]
fn worktree_list(repo: &TestRepo) -> String {
    String::from_utf8(repo.git(["worktree", "list", "--porcelain"])).unwrap()
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_worktree_is_added_switched_to_and_removed_from_the_main_one() {
    let (repo, beside) = with_feature();
    let mut app = Harness::start(&repo).await;
    let main = app.state().repo.clone().expect("started in the repository");

    // Add one for `feature`, at the path the prompt suggests.
    app.press(KeyCode::Char('3')).await;
    app.press(KeyCode::Char('j')).await;
    app.press_with(KeyCode::Char('W'), KeyModifiers::SHIFT)
        .await;
    app.assert_screen_contains(" Worktrees ");
    app.press(KeyCode::Char('a')).await;
    app.assert_screen_contains("New worktree for feature");
    app.press(KeyCode::Enter).await;
    assert_eq!(
        app.state().notice,
        Some("Worktree added; Enter switches to it")
    );
    let listed = worktree_list(&repo);
    let wt = beside.0.canonicalize().unwrap();
    assert!(
        listed.contains(&format!("worktree {}\n", wt.display())),
        "{listed}"
    );
    assert!(listed.contains("branch refs/heads/feature"), "{listed}");

    // Enter on it switches Kika there.
    app.press(KeyCode::Char('j')).await;
    app.assert_screen_contains("feature");
    app.press(KeyCode::Enter).await;
    assert_eq!(app.state().repo.as_deref(), Some(wt.as_path()));
    let status = app.screen()[1].clone();
    assert!(status.contains("│feature "), "{status:?}");

    // Back in the main worktree, d then y removes the clean one.
    app.press_with(KeyCode::Char('W'), KeyModifiers::SHIFT)
        .await;
    app.press(KeyCode::Enter).await;
    assert_eq!(app.state().repo.as_deref(), Some(main.as_path()));
    app.press_with(KeyCode::Char('W'), KeyModifiers::SHIFT)
        .await;
    app.press(KeyCode::Char('j')).await;
    app.press(KeyCode::Char('d')).await;
    app.assert_screen_contains("Remove worktree");
    app.press(KeyCode::Char('y')).await;
    let listed = worktree_list(&repo);
    assert!(!listed.contains("feature"), "{listed}");
    assert!(!wt.exists());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_worktree_with_an_untracked_file_is_not_removed() {
    let (repo, beside) = with_feature();
    repo.git([
        OsStr::new("worktree"),
        OsStr::new("add"),
        OsStr::new("-q"),
        beside.0.as_os_str(),
        OsStr::new("feature"),
    ]);
    std::fs::write(beside.0.join("keep.txt"), "mine\n").unwrap();
    let mut app = Harness::start(&repo).await;

    app.press_with(KeyCode::Char('W'), KeyModifiers::SHIFT)
        .await;
    app.press(KeyCode::Char('j')).await;
    app.press(KeyCode::Char('d')).await;
    app.press(KeyCode::Char('y')).await;

    app.assert_screen_contains("Could not remove worktree");
    assert!(worktree_list(&repo).contains("branch refs/heads/feature"));
    assert_eq!(
        std::fs::read_to_string(beside.0.join("keep.txt")).unwrap(),
        "mine\n"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn adding_at_a_directory_that_is_not_empty_is_refused_and_leaves_it_alone() {
    let (repo, _beside) = with_feature();
    let repo = repo.write("full/a.txt", "kept\n");
    let mut app = Harness::start(&repo).await;

    app.press(KeyCode::Char('3')).await;
    app.press(KeyCode::Char('j')).await;
    app.press_with(KeyCode::Char('W'), KeyModifiers::SHIFT)
        .await;
    app.press(KeyCode::Char('a')).await;
    // Clear the suggestion, then type the occupied directory.
    for _ in 0..200 {
        app.press(KeyCode::Backspace).await;
    }
    app.type_text("full\n").await;

    app.assert_screen_contains("Could not add worktree");
    assert!(!worktree_list(&repo).contains("feature"));
    assert_eq!(repo.read("full/a.txt"), "kept\n");
    assert_eq!(
        std::fs::read_dir(repo.path().join("full")).unwrap().count(),
        1
    );
}
