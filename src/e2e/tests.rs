//! Example end-to-end tests: each one drives the real app over a real repo.

use crossterm::event::{KeyCode, KeyModifiers};

use crate::e2e::harness::Harness;
use crate::e2e::repo::TestRepo;
use crate::git::CliRunner;
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

    assert!(
        !app.screen()
            .iter()
            .any(|row| row.contains("Commit message"))
    );
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

    // Collapsed, the panel shows only the branch; expanded, the rest.
    app.assert_screen_contains("│work ");
    app.press(KeyCode::Char('1')).await;
    app.assert_screen_contains("│base ↑2 ↓1 ");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_branch_without_upstream_says_so() {
    let repo = on_main();
    let mut app = Harness::start(&repo).await;
    app.assert_screen_contains("│main ");
    app.press(KeyCode::Char('1')).await;
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
    app.press(KeyCode::Char('1')).await;

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
    app.press(KeyCode::Char('1')).await;

    app.assert_screen_contains("│rebasing ");
}

#[tokio::test(flavor = "multi_thread")]
async fn committing_in_the_app_updates_the_ahead_count() {
    let repo = on_main();
    repo.git(["branch", "base"]);
    repo.git(["branch", "-q", "--set-upstream-to=base"]);
    let repo = repo.write("a.txt", "two\n").stage("a.txt");
    let mut app = Harness::start(&repo).await;
    app.press(KeyCode::Char('1')).await;
    app.assert_screen_contains("│base ↑0 ↓0 ");

    app.press(KeyCode::Char('2')).await;
    app.press(KeyCode::Char('c')).await;
    app.type_text("Ahead").await;
    app.press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)
        .await;

    app.press(KeyCode::Char('1')).await;
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
    app.intent(AppIntent::RecentLoaded(vec![
        a_top.clone(),
        b.path().to_path_buf(),
    ]))
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
