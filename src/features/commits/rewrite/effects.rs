//! The I/O behind the rewrites' effects: reading the stopped rebase's
//! todo list, writing an edited one back, looking for the commits the
//! staged lines come from, and listing the commits a detached `HEAD` would
//! leave behind, off the event loop, each reporting back with its tag.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use super::fixup_base::find_fixup_base;
use super::left_behind::read_left_behind;
use super::todo::{TodoFile, nothing_done, serialize};
use crate::background;
use crate::effect::Tag;
use crate::features::commits::CommitsIntent;
use crate::features::commits::rewrite::RewriteIntent;
use crate::git::GitRunner;
use crate::git::comment_prefix::read_comment_prefix;
use crate::git::commit_files::CommitId;
use crate::git::rebase_plan::{parse_todo, validate_edit};
use crate::git::repo_state::{git_dir_command, git_dir_from};
use crate::intent::AppIntent;
use crate::op::GitOp;
use crate::sequence_editor::MAX_TODO;
use crate::worktree_file;

/// Read the todo list and report it as `RewriteIntent::TodoLoaded`.
pub(crate) fn load_todo(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    string_too: bool,
) {
    background::run(
        tx,
        move || read_rebase_todo(runner.as_ref(), string_too),
        // A panic still reports back, or the panel would wait forever.
        background::crashed("Reading the todo list"),
        move |result| {
            AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::TodoLoaded {
                tag,
                result,
            }))
        },
    );
}

/// Look for the commits the staged lines come from and report them as
/// `RewriteIntent::FixupBaseFound`.
pub(crate) fn find_base(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>, tag: Tag) {
    background::run(
        tx,
        move || find_fixup_base(runner.as_ref()),
        // A panic still reports back, or the busy counter would stay raised.
        background::crashed("Finding the fixup base"),
        move |result| {
            AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::FixupBaseFound {
                tag,
                result,
            }))
        },
    );
}

/// List the commits moving the detached `head` to `target` would leave
/// behind and report them as `RewriteIntent::LeftBehindChecked`.
pub(crate) fn check_left_behind(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    op: GitOp,
    head: CommitId,
    target: CommitId,
) {
    let from = head.clone();
    background::run(
        tx,
        move || read_left_behind(runner.as_ref(), &from, &target),
        // A panic still reports back, or the busy counter would stay raised.
        background::crashed("git rev-list"),
        move |result| {
            AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::LeftBehindChecked {
                tag,
                op,
                head,
                result,
            }))
        },
    );
}

/// Write the edited todo list and report it as `RewriteIntent::TodoWritten`.
pub(crate) fn write_todo(
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    path: PathBuf,
    expected: Vec<u8>,
    content: Vec<u8>,
    comment: Vec<u8>,
) {
    background::run(
        tx,
        move || write_rebase_todo(&path, &expected, &content, &comment),
        // A panic still reports back, or the todo would stay locked.
        background::crashed("Writing the todo list"),
        move |result| {
            AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::TodoWritten {
                tag,
                result,
            }))
        },
    );
}

/// The todo list of the rebase in progress and its `done` list, from
/// `<git dir>/rebase-merge/`, with the comment prefix git reads them by.
/// `None` when there is no todo file: a rebase that is not interactive,
/// or one between steps. A prefix that cannot be read still loads the
/// list, to be shown read-only with why.
fn read_rebase_todo(runner: &dyn GitRunner, string_too: bool) -> Result<Option<TodoFile>, String> {
    let output = runner
        .run_checked(&git_dir_command())
        .map_err(|e| e.to_string())?;
    let dir = git_dir_from(&output.stdout).join("rebase-merge");
    let path = dir.join("git-rebase-todo");
    if std::fs::symlink_metadata(&path).is_err() {
        return Ok(None);
    }
    let todo = worktree_file::read_file(&path, MAX_TODO)?;
    let done = worktree_file::read_file(&dir.join("done"), MAX_TODO).ok();
    let comment = read_comment_prefix(runner, string_too);
    Ok(Some(TodoFile {
        path,
        todo,
        done,
        comment,
    }))
}

/// Write an edited todo list, once it is sure to be git's todo file and a
/// safe edit of what was read, both read with `comment`, the prefix the
/// list was loaded with. The model checked the same; this is the check
/// that counts.
fn write_rebase_todo(
    path: &Path,
    expected: &[u8],
    content: &[u8],
    comment: &[u8],
) -> Result<(), String> {
    let dir = path.parent();
    let is_todo = path
        .file_name()
        .is_some_and(|name| name == "git-rebase-todo")
        && dir
            .and_then(Path::file_name)
            .is_some_and(|name| name == "rebase-merge");
    if !is_todo {
        return Err(format!("Not a rebase todo list: {}", path.display()));
    }
    // A list whose prefix could not be read is never edited.
    if comment.is_empty() {
        return Err("No comment prefix to read the todo list with".into());
    }
    let before = parse_todo(expected, comment)?;
    let after = parse_todo(content, comment)?;
    // Kika writes no comments or blank lines, so every line of `content`
    // must be a command git runs; one the prefix hides would be skipped.
    if serialize(&after) != content {
        return Err("A line of the edit would be read as a comment".into());
    }
    let done_empty = dir
        .and_then(|dir| worktree_file::read_file(&dir.join("done"), MAX_TODO).ok())
        .is_some_and(|done| nothing_done(&done, comment));
    validate_edit(&before, &after, done_empty)?;
    worktree_file::replace_file(path, expected, content)
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::*;
    use crate::effect::{Effect, Runtime};
    use crate::git::fake::{FakeRunner, fail, ok};

    /// A `rebase-merge` dir in a temp git dir, with `todo` and `done`.
    fn rebase_dir(tmp: &crate::git::fake::TempDir, todo: &[u8], done: &[u8]) -> PathBuf {
        let dir = tmp.path().join("rebase-merge");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("git-rebase-todo"), todo).unwrap();
        std::fs::write(dir.join("done"), done).unwrap();
        dir.join("git-rebase-todo")
    }

    const TODO: &[u8] = b"pick 1111 one\nupdate-ref refs/heads/x\npick 2222 two\n# help\n";

    #[test]
    fn a_todo_write_checks_the_file_and_the_edit_before_writing() {
        let tmp = crate::git::fake::TempDir::new("effect-todo");
        let path = rebase_dir(&tmp, TODO, b"pick 0000 z\n");
        for bad in [
            &b"pick 1111 one\nupdate-ref refs/heads/x\npick 2222 two\npick 3333 more\n"[..],
            b"pick 2222 two\nupdate-ref refs/heads/x\npick 1111 one\n",
            b"pick 1111 one\nupdate-ref refs/heads/x\npick 2222 two\nexec rm -rf /\n",
            b"pick 1111 one\npick 2222 two\n",
        ] {
            assert!(
                write_rebase_todo(&path, TODO, bad, b"#").is_err(),
                "{bad:?}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), TODO);
        }
        // A line git rejects is not a comment: a list holding one is not
        // edited, whatever the edit.
        for odd in [&b"\xef\xbb\xbf"[..], b"-", b"\x0b"] {
            let mut listed = TODO.to_vec();
            listed.extend_from_slice(odd);
            listed.extend_from_slice(b"pick 3333 three\n");
            std::fs::write(&path, &listed).unwrap();
            let edit = b"pick 1111 one\nupdate-ref refs/heads/x\ndrop 2222 two\n";
            assert!(
                write_rebase_todo(&path, &listed, edit, b"#").is_err(),
                "{odd:?}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), listed);
        }
        // Git stops at a NUL; a list holding one is not edited.
        let nul = b"pick 1111 one\nupdate-ref refs/heads/x\npick 2222 two\0\n";
        std::fs::write(&path, nul).unwrap();
        let moved = b"pick 1111 one\nupdate-ref refs/heads/x\npick 2222 two\n";
        assert!(write_rebase_todo(&path, nul, moved, b"#").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), nul);
        std::fs::write(&path, TODO).unwrap();
        let other = tmp.path().join("git-rebase-todo");
        std::fs::write(&other, TODO).unwrap();
        let edit = b"pick 1111 one\nbreak\nupdate-ref refs/heads/x\ndrop 2222 two\n";
        assert!(write_rebase_todo(&other, TODO, edit, b"#").is_err());
        assert_eq!(
            // The same commands, but not the bytes on disk.
            write_rebase_todo(&path, &TODO[..TODO.len() - 7], edit, b"#"),
            Err(crate::worktree_file::CHANGED_ON_DISK.to_string())
        );
        assert_eq!(write_rebase_todo(&path, TODO, edit, b"#"), Ok(()));
        assert_eq!(std::fs::read(&path).unwrap(), edit);
    }

    #[test]
    fn a_todo_write_refuses_a_first_fold_only_when_nothing_is_done() {
        let tmp = crate::git::fake::TempDir::new("effect-todo");
        let todo = b"pick 1111 one\npick 2222 two\n";
        let path = rebase_dir(&tmp, todo, b"");
        let fold = b"fixup 1111 one\npick 2222 two\n";
        assert!(write_rebase_todo(&path, todo, fold, b"#").is_err());
        std::fs::write(path.with_file_name("done"), "pick 0000 z\n").unwrap();
        assert_eq!(write_rebase_todo(&path, todo, fold, b"#"), Ok(()));
    }

    const COMMENT_CONFIG: [&str; 4] = [
        "config",
        "-z",
        "--get-regexp",
        r"^core\.comment(char|string)$",
    ];

    #[test]
    fn the_todo_is_read_from_git_s_dir_with_its_comment_prefix_and_none_without_one() {
        let tmp = crate::git::fake::TempDir::new("effect-todo");
        let mut git_dir = tmp.path().as_os_str().as_encoded_bytes().to_vec();
        git_dir.push(b'\n');
        let runner = FakeRunner::new()
            .expect(&["rev-parse", "--absolute-git-dir"], ok(&git_dir))
            .expect(&["rev-parse", "--absolute-git-dir"], ok(&git_dir))
            .expect(&COMMENT_CONFIG, ok(b"core.commentstring\n//\0"))
            .expect(&["rev-parse", "--absolute-git-dir"], ok(&git_dir))
            .expect(&COMMENT_CONFIG, ok(b"core.commentstring\n//\0"));
        assert_eq!(read_rebase_todo(&runner, true), Ok(None));
        let path = rebase_dir(&tmp, TODO, b"pick 0000 z\n");
        let file = |comment: &[u8]| {
            Ok(Some(TodoFile {
                path: path.clone(),
                todo: TODO.to_vec(),
                done: Some(b"pick 0000 z\n".to_vec()),
                comment: Ok(comment.to_vec()),
            }))
        };
        assert_eq!(read_rebase_todo(&runner, true), file(b"//"));
        // An older git ignores `core.commentString`.
        assert_eq!(read_rebase_todo(&runner, false), file(b"#"));
        runner.assert_done();
    }

    #[test]
    fn a_failed_git_dir_read_shows_git_s_words_or_names_the_subcommand() {
        let argv = ["rev-parse", "--absolute-git-dir"];
        let runner = FakeRunner::new()
            .expect(&argv, fail("fatal: not a \x1b[2Jrepo\n"))
            .expect(&argv, fail(""));
        assert_eq!(
            read_rebase_todo(&runner, true),
            Err("fatal: not a \u{FFFD}[2Jrepo".into())
        );
        assert_eq!(
            read_rebase_todo(&runner, true),
            Err("git rev-parse failed".into())
        );
        runner.assert_done();
    }

    #[test]
    fn a_comment_prefix_that_cannot_be_read_still_loads_the_todo() {
        let tmp = crate::git::fake::TempDir::new("effect-todo");
        let mut git_dir = tmp.path().as_os_str().as_encoded_bytes().to_vec();
        git_dir.push(b'\n');
        let runner = FakeRunner::new()
            .expect(&["rev-parse", "--absolute-git-dir"], ok(&git_dir))
            .expect(&COMMENT_CONFIG, fail("fatal: bad config line 1\n"));
        rebase_dir(&tmp, TODO, b"");
        let file = read_rebase_todo(&runner, true).unwrap().unwrap();
        assert_eq!(file.todo, TODO);
        assert_eq!(file.comment, Err("fatal: bad config line 1".into()));
        runner.assert_done();
    }

    #[test]
    fn a_todo_write_refuses_a_line_the_prefix_would_hide() {
        let tmp = crate::git::fake::TempDir::new("effect-todo");
        let todo = b"b Rebase 1..2 onto 1\npick 1111 one\npick 2222 two\n";
        let path = rebase_dir(&tmp, todo, b"pick 0000 z\n");
        // The break would read as a comment, and git would not stop.
        let stop = b"pick 1111 one\nbreak\npick 2222 two\n";
        assert!(write_rebase_todo(&path, todo, stop, b"b").is_err());
        let moved = b"pick 2222 two\npick 1111 one\n";
        assert!(write_rebase_todo(&path, todo, moved, b"p").is_err());
        // Anything but what Kika serialises is refused, blank lines too.
        let blank = b"pick 2222 two\n\npick 1111 one\n";
        assert!(write_rebase_todo(&path, todo, blank, b"b").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), todo);
    }

    #[test]
    fn a_todo_write_reads_both_lists_with_the_loaded_comment_prefix() {
        let tmp = crate::git::fake::TempDir::new("effect-todo");
        let todo = b"; Rebase 1..2 onto 1\npick 1111 one\npick 2222 two\n; help\n";
        let path = rebase_dir(&tmp, todo, b"pick 0000 z\n");
        let edit = b"pick 2222 two\npick 1111 one\n";
        assert!(write_rebase_todo(&path, todo, edit, b"#").is_err());
        assert!(write_rebase_todo(&path, todo, edit, b"").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), todo);
        assert_eq!(write_rebase_todo(&path, todo, edit, b";"), Ok(()));
        assert_eq!(std::fs::read(&path).unwrap(), edit);
    }

    #[tokio::test]
    async fn check_left_behind_answers_with_the_op_head_and_ids() {
        let head = CommitId::parse("aa11").unwrap();
        let target = CommitId::parse("bb22").unwrap();
        let op = GitOp::CheckoutCommit(target.clone());
        let runner = Arc::new(FakeRunner::new().expect(
            &[
                "rev-list",
                "aa11",
                "--not",
                "bb22",
                "--branches",
                "--tags",
                "--remotes",
                "--",
            ],
            ok(b"aa11\ncc33\n"),
        ));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 4,
            user: true,
        };

        runtime.spawn(Effect::CheckLeftBehind {
            tag,
            op: op.clone(),
            head: head.clone(),
            target,
        });

        assert_eq!(
            rx.recv().await,
            Some(AppIntent::Commits(CommitsIntent::Rewrite(
                RewriteIntent::LeftBehindChecked {
                    tag,
                    op,
                    head,
                    result: Ok(vec![
                        CommitId::parse("aa11").unwrap(),
                        CommitId::parse("cc33").unwrap()
                    ]),
                }
            )))
        );
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_panicking_left_behind_check_still_reports_back() {
        // No expectations queued, so the fake panics on the call.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(FakeRunner::new()), tx);
        let tag = Tag {
            generation: 0,
            seq: 2,
            user: true,
        };
        let id = CommitId::parse("aa").unwrap();
        runtime.spawn(Effect::CheckLeftBehind {
            tag,
            op: GitOp::CheckoutCommit(id.clone()),
            head: id.clone(),
            target: id,
        });
        match rx.recv().await {
            Some(AppIntent::Commits(CommitsIntent::Rewrite(
                RewriteIntent::LeftBehindChecked {
                    tag: got,
                    result: Err(message),
                    ..
                },
            ))) => {
                assert_eq!(got, tag);
                assert!(message.contains("git rev-list crashed"), "{message}");
            }
            other => panic!("expected a failed LeftBehindChecked, got {other:?}"),
        }
    }
}
