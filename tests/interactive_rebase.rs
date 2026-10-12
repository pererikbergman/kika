//! Kika's interactive rebase against real git: the built binary is the
//! sequence editor, run with the argv and env Kika's op builds. Unix only:
//! the scripted editors are `sh` scripts.
#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const KIKA: &str = env!("CARGO_BIN_EXE_kika");

/// A fresh directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "kika-it-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// git in `dir`, reading only the repository's own config.
fn git_command(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_EDITOR")
        .env_remove("VISUAL")
        .env_remove("EDITOR")
        .env_remove("GIT_SEQUENCE_EDITOR")
        .env_remove("KIKA_REBASE_PLAN")
        .stdin(Stdio::null());
    command
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_command(dir).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// A repo with four commits on one line, each adding one file:
/// `first` adds `a`, `second` `b`, `third` `c`, `fourth` `d`.
fn four_commits() -> TempDir {
    let tmp = TempDir::new("rebase");
    let dir = &tmp.0;
    git(dir, &["init", "-q", "--template="]);
    for (key, value) in [
        ("user.name", "Kika Test"),
        ("user.email", "test@example.com"),
        ("commit.gpgsign", "false"),
    ] {
        git(dir, &["config", key, value]);
    }
    for (file, message) in [
        ("a", "first"),
        ("b", "second"),
        ("c", "third"),
        ("d", "fourth"),
    ] {
        fs::write(dir.join(file), format!("{file}\n")).unwrap();
        git(dir, &["add", file]);
        git(dir, &["commit", "-q", "-m", message]);
    }
    tmp
}

fn rev(dir: &Path, rev: &str) -> String {
    git(dir, &["rev-parse", rev]).trim().to_string()
}

/// Each commit from `HEAD` down: (tree, message).
fn history(dir: &Path) -> Vec<(String, String)> {
    let count: usize = git(dir, &["rev-list", "--count", "HEAD"])
        .trim()
        .parse()
        .unwrap();
    (0..count)
        .map(|i| {
            let commit = format!("HEAD~{i}");
            (
                rev(dir, &format!("{commit}^{{tree}}")),
                git(dir, &["log", "-1", "--format=%B", &commit])
                    .trim_end()
                    .to_string(),
            )
        })
        .collect()
}

/// `GIT_SEQUENCE_EDITOR` for Kika at `exe`, quoted for `sh` as Kika
/// quotes it.
fn editor_value(exe: &Path) -> OsString {
    let mut out = b"'".to_vec();
    for &b in exe.as_os_str().as_bytes() {
        if b == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(b);
        }
    }
    out.push(b'\'');
    OsString::from_vec(out)
}

/// A `sh` script in `dir` that git can run as its editor.
fn script(dir: &Path, name: &str, body: &str) -> OsString {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    editor_value(&path)
}

/// Run Kika's interactive rebase in `dir` onto `base` with `plan`, and
/// `editor` as git's editor for messages.
fn rebase(dir: &Path, base: &str, plan: &str, editor: &OsString) -> Output {
    rebase_with(dir, base, plan, editor, false)
}

/// [`rebase`], with `--update-refs` as Kika passes it on git 2.38 or newer.
fn rebase_with(dir: &Path, base: &str, plan: &str, editor: &OsString, update_refs: bool) -> Output {
    let mut args = vec!["-c"];
    if !update_refs {
        args.extend(["rebase.updateRefs=false", "-c"]);
    }
    args.extend([
        "rebase.rebaseMerges=false",
        "-c",
        "core.commentChar=#",
        "rebase",
        "-i",
    ]);
    if update_refs {
        args.push("--update-refs");
    }
    args.extend(["--autostash", "--no-autosquash", "--keep-empty", base]);
    git_command(dir)
        .args(args)
        .env("KIKA_REBASE_PLAN", plan)
        .env("GIT_SEQUENCE_EDITOR", editor_value(Path::new(KIKA)))
        .env("GIT_EDITOR", editor)
        .output()
        .unwrap()
}

fn assert_ok(out: &Output) {
    assert!(
        out.status.success(),
        "rebase failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `first`..`fourth`'s ids, oldest first.
fn ids(dir: &Path) -> [String; 4] {
    [3, 2, 1, 0].map(|i| rev(dir, &format!("HEAD~{i}")))
}

fn tree_files(dir: &Path, commit: &str) -> String {
    git(dir, &["ls-tree", "--name-only", commit])
}

#[test]
fn squash_folds_the_commit_into_the_one_below_with_both_messages() {
    let tmp = four_commits();
    let dir = &tmp.0;
    let before = history(dir);
    let [first, second, third, fourth] = ids(dir);
    let editor = script(dir, "editor.sh", "printf 'edited\\n' >> \"$1\"");
    let plan = format!("pick {second}\nsquash {third}\npick {fourth}\n");
    assert_ok(&rebase(dir, &first, &plan, &editor));

    let after = history(dir);
    assert_eq!(after.len(), 3);
    assert_eq!(after[0], before[0], "fourth keeps its tree and message");
    assert_eq!(after[2], before[3], "first is untouched");
    assert_eq!(rev(dir, "HEAD~2"), first);
    assert_eq!(tree_files(dir, "HEAD~1"), "a\nb\nc\n");
    let message = &after[1].1;
    let (s, t, e) = (
        message.find("second").unwrap(),
        message.find("third").unwrap(),
        message.find("edited").unwrap(),
    );
    assert!(s < t && t < e, "{message}");
    assert!(!message.contains('#'), "{message}");
}

#[test]
fn fixup_folds_the_commit_and_keeps_the_older_message() {
    let tmp = four_commits();
    let dir = &tmp.0;
    let before = history(dir);
    let [first, second, third, fourth] = ids(dir);
    // No editor opens for a fixup.
    let plan = format!("pick {second}\nfixup {third}\npick {fourth}\n");
    assert_ok(&rebase(dir, &first, &plan, &"false".into()));

    let after = history(dir);
    assert_eq!(after.len(), 3);
    assert_eq!(after[0], before[0]);
    assert_eq!(after[1].1, "second");
    assert_eq!(after[1].0, before[1].0, "the tree of second and third");
    assert_eq!(after[2], before[3]);
}

#[test]
fn drop_removes_the_commit_s_change_and_keeps_the_others() {
    let tmp = four_commits();
    let dir = &tmp.0;
    let before = history(dir);
    let [first, second, third, fourth] = ids(dir);
    let plan = format!("drop {third}\npick {fourth}\n");
    assert_ok(&rebase(dir, &second, &plan, &"false".into()));

    let after = history(dir);
    assert_eq!(after.len(), 3);
    assert_eq!(tree_files(dir, "HEAD"), "a\nb\nd\n");
    assert_eq!(after[0].1, "fourth");
    assert_eq!(after[1..], before[2..]);
    assert_eq!(rev(dir, "HEAD~1"), second);
    assert_eq!(rev(dir, "HEAD~2"), first);
    assert!(!dir.join("c").exists());
}

#[test]
fn a_branch_and_tag_named_like_a_short_id_do_not_change_which_commit_is_picked() {
    let tmp = four_commits();
    let dir = &tmp.0;
    let [first, second, third, fourth] = ids(dir);
    // The abbreviation git writes in the todo for `second`, as a branch
    // and a tag on `fourth`.
    let short = git(dir, &["rev-parse", "--short", &second])
        .trim()
        .to_string();
    git(dir, &["branch", &short, &fourth]);
    git(dir, &["tag", &short, &fourth]);
    assert_eq!(
        rev(dir, &short),
        fourth,
        "{short} must name the branch, not the commit"
    );

    // A move picks `second` by its id; a `drop` line would not show which
    // commit git read, since a dropped commit is never looked up.
    let plan = format!("pick {third}\npick {second}\npick {fourth}\n");
    assert_ok(&rebase(dir, &first, &plan, &"false".into()));

    let messages: Vec<String> = history(dir).into_iter().map(|(_, m)| m).collect();
    assert_eq!(messages, ["fourth", "second", "third", "first"]);
    assert_eq!(tree_files(dir, "HEAD~2"), "a\nc\n");
    assert_eq!(tree_files(dir, "HEAD~1"), "a\nb\nc\n");
    assert_eq!(tree_files(dir, "HEAD"), "a\nb\nc\nd\n");
    assert_eq!(rev(dir, "HEAD~3"), first);
    assert_eq!(rev(dir, &format!("refs/heads/{short}")), fourth);
    assert_eq!(rev(dir, &format!("refs/tags/{short}")), fourth);
}

#[test]
fn reword_changes_only_that_message() {
    let tmp = four_commits();
    let dir = &tmp.0;
    let before = history(dir);
    let [first, second, third, fourth] = ids(dir);
    let editor = script(dir, "editor.sh", "printf 'reworded\\n' > \"$1\"");
    let plan = format!("pick {second}\nreword {third}\npick {fourth}\n");
    assert_ok(&rebase(dir, &first, &plan, &editor));

    let after = history(dir);
    assert_eq!(after.len(), 4);
    for (i, ((tree_after, _), (tree_before, _))) in after.iter().zip(&before).enumerate() {
        assert_eq!(tree_after, tree_before, "HEAD~{i}");
    }
    let messages: Vec<&str> = after.iter().map(|(_, m)| m.as_str()).collect();
    assert_eq!(messages, ["fourth", "reworded", "second", "first"]);
}

#[test]
fn a_move_swaps_two_commits_and_keeps_their_changes() {
    let tmp = four_commits();
    let dir = &tmp.0;
    let [_, second, third, fourth] = ids(dir);
    let plan = format!("pick {fourth}\npick {third}\n");
    assert_ok(&rebase(dir, &second, &plan, &"false".into()));

    let messages: Vec<String> = history(dir).into_iter().map(|(_, m)| m).collect();
    assert_eq!(messages, ["third", "fourth", "second", "first"]);
    assert_eq!(tree_files(dir, "HEAD"), "a\nb\nc\nd\n");
}

#[test]
fn a_custom_comment_char_does_not_stop_kika_s_rebase() {
    let tmp = four_commits();
    let dir = &tmp.0;
    let [_, second, third, fourth] = ids(dir);
    git(dir, &["config", "core.commentChar", ";"]);
    let plan = format!("pick {fourth}\npick {third}\n");
    assert_ok(&rebase(dir, &second, &plan, &"false".into()));
    let messages: Vec<String> = history(dir).into_iter().map(|(_, m)| m).collect();
    assert_eq!(messages, ["third", "fourth", "second", "first"]);
}

#[test]
fn edit_and_break_stop_the_rebase_where_the_plan_says() {
    let tmp = four_commits();
    let dir = &tmp.0;
    let [_, second, third, fourth] = ids(dir);
    let plan = format!("edit {third}\npick {fourth}\n");
    assert_ok(&rebase(dir, &second, &plan, &"false".into()));
    assert!(dir.join(".git/rebase-merge").exists());
    assert_eq!(rev(dir, "HEAD"), third);
    git(dir, &["rebase", "--abort"]);

    let plan = format!("pick {third}\nbreak\npick {fourth}\n");
    assert_ok(&rebase(dir, &second, &plan, &"false".into()));
    assert_eq!(rev(dir, "HEAD"), third);
    let todo = fs::read_to_string(dir.join(".git/rebase-merge/git-rebase-todo")).unwrap();
    assert!(todo.starts_with(&format!("pick {fourth}")), "{todo}");
    git(dir, &["rebase", "--continue"]);
    assert_eq!(rev(dir, "HEAD"), fourth);
}

/// Whether the installed git has `rebase --update-refs` (2.38).
fn has_update_refs() -> bool {
    let out = git(Path::new("."), &["--version"]);
    let mut numbers = out
        .trim_start_matches("git version ")
        .split(|c: char| !c.is_ascii_digit())
        .map(|n| n.parse::<u32>().unwrap_or(0));
    let (major, minor) = (numbers.next().unwrap_or(0), numbers.next().unwrap_or(0));
    (major, minor) >= (2, 38)
}

#[test]
fn with_update_refs_a_stacked_branch_moves_and_a_move_across_it_aborts() {
    if !has_update_refs() {
        eprintln!("skipped: --update-refs needs Git 2.38 or newer");
        return;
    }
    let tmp = four_commits();
    let dir = &tmp.0;
    let [first, second, third, fourth] = ids(dir);
    git(dir, &["branch", "stack", &third]);

    // Moving `fourth` below `third` would put it on `stack`.
    let head = rev(dir, "HEAD");
    let plan = format!("pick {second}\npick {fourth}\npick {third}\n");
    let out = rebase_with(dir, &first, &plan, &"false".into(), true);
    assert!(!out.status.success());
    assert_eq!(rev(dir, "HEAD"), head);
    assert_eq!(rev(dir, "stack"), third);

    // Dropping `second` rewrites `third`; `stack` follows its copy.
    let plan = format!("drop {second}\npick {third}\npick {fourth}\n");
    assert_ok(&rebase_with(dir, &first, &plan, &"false".into(), true));
    assert_eq!(rev(dir, "stack"), rev(dir, "HEAD~1"));
    assert_eq!(git(dir, &["log", "-1", "--format=%s", "stack"]), "third\n");
}

/// Every file in the worktree but `.git`, with its contents.
fn worktree(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.file_name() != ".git")
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                fs::read(e.path()).unwrap(),
            )
        })
        .collect();
    files.sort();
    files
}

#[test]
fn uncommitted_work_is_stashed_and_put_back() {
    let tmp = four_commits();
    let dir = &tmp.0;
    let [_, second, third, fourth] = ids(dir);
    fs::write(dir.join("a"), "a\nchanged\n").unwrap();
    fs::write(dir.join("new"), "staged\n").unwrap();
    git(dir, &["add", "new"]);
    fs::write(dir.join("untracked"), "u\n").unwrap();
    let status_before = git(dir, &["status", "--porcelain"]);
    let files_before = worktree(dir);

    let plan = format!("drop {third}\npick {fourth}\n");
    assert_ok(&rebase(dir, &second, &plan, &"false".into()));

    assert_eq!(tree_files(dir, "HEAD"), "a\nb\nd\n");
    assert_eq!(git(dir, &["status", "--porcelain"]), status_before);
    let mut expected = files_before;
    expected.retain(|(name, _)| name != "c");
    assert_eq!(worktree(dir), expected);
    assert_eq!(git(dir, &["stash", "list"]), "");
}

#[test]
fn a_plan_that_does_not_match_makes_git_abort_and_change_nothing() {
    let tmp = four_commits();
    let dir = &tmp.0;
    let [_, second, third, fourth] = ids(dir);
    fs::write(dir.join("a"), "a\nchanged\n").unwrap();
    let head = rev(dir, "HEAD");
    let before = history(dir);
    let status_before = git(dir, &["status", "--porcelain"]);
    let files_before = worktree(dir);

    for plan in [
        // One commit twice.
        format!("pick {fourth}\ndrop {fourth}\n"),
        // One commit short.
        format!("drop {third}\n"),
        // Not a commit in the range.
        format!("drop {second}\npick {fourth}\n"),
        "garbage".into(),
    ] {
        let out = rebase(dir, &second, &plan, &"false".into());
        assert!(!out.status.success(), "{plan}");
        assert_eq!(rev(dir, "HEAD"), head, "{plan}");
        assert_eq!(history(dir), before, "{plan}");
        assert_eq!(
            git(dir, &["status", "--porcelain"]),
            status_before,
            "{plan}"
        );
        assert_eq!(worktree(dir), files_before, "{plan}");
        assert!(!dir.join(".git/rebase-merge").exists(), "{plan}");
        assert_eq!(git(dir, &["stash", "list"]), "", "{plan}");
    }
}

/// Run the binary in its hidden mode on `args`, waiting a few seconds at most.
fn run_hidden(plan: &str, args: &[&Path]) -> Output {
    let mut child = Command::new(KIKA)
        .args(args)
        .env("KIKA_REBASE_PLAN", plan)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            panic!("the hidden mode did not exit");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn the_hidden_mode_rewrites_the_todo_and_exits_without_a_ui() {
    let tmp = TempDir::new("hidden");
    let dir = tmp.0.join("rebase-merge");
    fs::create_dir(&dir).unwrap();
    let todo = dir.join("git-rebase-todo");
    let (a, b) = ("a".repeat(40), "b".repeat(40));
    fs::write(
        &todo,
        format!("pick {} two\npick {} one\n# help\n", &b[..7], &a[..7]),
    )
    .unwrap();

    let plan = format!("reword {b}\npick {a}\n");
    let out = run_hidden(&plan, &[&todo]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"");
    assert_eq!(
        fs::read_to_string(&todo).unwrap(),
        format!("reword {b} two\npick {a} one\n")
    );
}

#[test]
fn the_hidden_mode_refuses_a_file_that_is_not_the_todo_and_writes_nothing() {
    let tmp = TempDir::new("hidden");
    let other = tmp.0.join("not-a-todo");
    let (a, b) = ("a".repeat(40), "b".repeat(40));
    let contents = format!("pick {} two\npick {} one\n", &b[..7], &a[..7]);
    fs::write(&other, &contents).unwrap();

    let out = run_hidden(&format!("drop {b}\npick {a}\n"), &[&other]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(out.stdout, b"");
    assert!(!out.stderr.is_empty());
    assert_eq!(fs::read_to_string(&other).unwrap(), contents);
    let entries: Vec<_> = fs::read_dir(&tmp.0).unwrap().collect();
    assert_eq!(entries.len(), 1, "nothing else was written");
}
