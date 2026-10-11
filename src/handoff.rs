//! Work that needs the real terminal: Kika steps aside, a process runs on
//! it, and Kika takes it back. This module is the blocking work only; the
//! loop in `app` suspends and restores the screen around [`execute`], so a
//! new kind of hand-off is one variant here and nothing there.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::effect::Tag;
use crate::git::GitRunner;
use crate::git::diff::terminal_safe;
use crate::git::ops::{GitOp, commit_command, run_op};
use crate::git::path::RepoPath;
use crate::git::runner::GitCommand;
use crate::intent::AppIntent;

/// Something to run with the terminal handed over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandOff {
    /// Open a worktree file in the user's editor, at `line` when the editor
    /// takes one.
    OpenFile { path: RepoPath, line: Option<u32> },
    /// Let the user edit one hunk before it is staged. `header` is the
    /// `diff --git`/`---`/`+++` lines, `hunk` the `@@` line and its lines.
    EditHunk { header: Vec<u8>, hunk: Vec<u8> },
    /// Let the user write the commit message in their editor, starting
    /// from `initial`, the popup's text.
    EditCommitMessage { initial: String },
    /// Run a commit that may ask something on the terminal: a signing
    /// passphrase or a hook's question. Answered like a detached run.
    Commit { tag: Tag, op: GitOp },
}

impl HandOff {
    /// Split a one-hunk patch at its first `@@` line, so only the hunk is
    /// shown for editing and the header goes back on unchanged.
    pub fn edit_hunk(patch: &[u8]) -> Self {
        let at = patch
            .split_inclusive(|&b| b == b'\n')
            .take_while(|line| !line.starts_with(b"@@"))
            .map(<[u8]>::len)
            .sum();
        HandOff::EditHunk {
            header: patch[..at].to_vec(),
            hunk: patch[at..].to_vec(),
        }
    }
}

/// Run `job` on the terminal, which the caller has handed over, and say how
/// it went. Blocking; knows nothing about the screen.
pub fn execute(job: HandOff, runner: &dyn GitRunner, workdir: &Path) -> AppIntent {
    execute_with(job, runner, workdir, &wait_for_enter)
}

/// [`execute`], with `pause` run after a failed commit so git's output
/// stays readable; tests pass one that returns at once.
pub fn execute_with(
    job: HandOff,
    runner: &dyn GitRunner,
    workdir: &Path,
    pause: &dyn Fn(),
) -> AppIntent {
    match job {
        HandOff::Commit { tag, op } => AppIntent::OpFinished {
            result: commit_on_terminal(&op, runner, pause),
            tag,
            op,
        },
        HandOff::OpenFile { path, line } => {
            let file = workdir.join(path.as_os_str());
            let result = editor(runner).and_then(|editor| launch(&editor, &file, line, workdir));
            AppIntent::EditorClosed { result }
        }
        HandOff::EditHunk { header, hunk } => AppIntent::HunkEdited {
            result: edit_hunk(&header, &hunk, runner, workdir),
        },
        HandOff::EditCommitMessage { initial } => AppIntent::CommitMessageEdited {
            result: edit_commit_message(&initial, runner, workdir),
        },
    }
}

/// The editor git would use: `GIT_EDITOR`, `core.editor`, `VISUAL`,
/// `EDITOR`, then git's default, as `git var` resolves them.
fn editor(runner: &dyn GitRunner) -> Result<String, String> {
    let output = runner
        .run(&GitCommand::new("var").arg("GIT_EDITOR"))
        .map_err(|e| terminal_safe(e.to_string().as_bytes()))?;
    if !output.success {
        let message = terminal_safe(&output.stderr);
        return Err(if message.is_empty() {
            "git var GIT_EDITOR failed".into()
        } else {
            message
        });
    }
    let editor = output.stdout.trim_ascii();
    if editor.is_empty() {
        return Err("No editor is configured".into());
    }
    String::from_utf8(editor.to_vec()).map_err(|_| "The editor setting is not valid UTF-8".into())
}

/// Run the commit `op` attached to the terminal, staging everything first
/// when asked. On failure, `pause` keeps git's output on screen until the
/// user is done reading it, since nothing of it is captured.
fn commit_on_terminal(op: &GitOp, runner: &dyn GitRunner, pause: &dyn Fn()) -> Result<(), String> {
    let GitOp::Commit {
        message,
        stage_all,
        amend,
        no_verify,
    } = op
    else {
        // Only commits are handed over; anything else runs as usual.
        return run_op(runner, op);
    };
    if *stage_all {
        run_op(runner, &GitOp::StageAll)?;
    }
    // Stdin is the terminal now, so the message goes in a file. Kept until
    // git has exited.
    let file = match message {
        Some(message) => {
            let (temp, mut file) = TempFile::create("kika-commit-msg", "txt")
                .map_err(|e| io_error("Could not create the message file", &e))?;
            file.write_all(message.as_bytes())
                .map_err(|e| io_error("Could not write the message file", &e))?;
            drop(file);
            Some(temp)
        }
        None => None,
    };
    let cmd = commit_command(*amend, *no_verify, file.as_ref().map(|f| f.path.as_path()));
    let exit = runner
        .run_attached(&cmd)
        .map_err(|e| io_error("Could not start git commit", &e))?;
    if exit.success {
        return Ok(());
    }
    pause();
    Err(format!(
        "git commit exited with {}; its output was shown in the terminal",
        exit.status
    ))
}

/// Keep the terminal as git left it until the user presses Enter.
fn wait_for_enter() {
    // On its own line, whatever git printed last.
    print!("\ngit commit failed. Press Enter to return to Kika.");
    let _ = io::stdout().flush();
    let mut line = String::new();
    let _ = io::stdin().lock().read_line(&mut line);
}

/// Known editors' ways of opening a file at a line, by the basename of the
/// editor's first word.
#[cfg_attr(not(unix), allow(dead_code))]
fn line_style(editor: &str) -> LineStyle {
    let program = editor
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(['"', '\'']);
    let name = program.rsplit('/').next().unwrap_or(program);
    match name {
        "vi" | "vim" | "nvim" | "nano" | "emacs" | "emacsclient" | "kak" | "micro" => {
            LineStyle::PlusLine
        }
        "code" | "code-insiders" | "codium" => LineStyle::GotoFlag,
        "subl" | "zed" | "hx" => LineStyle::Suffix,
        _ => LineStyle::None,
    }
}

#[cfg_attr(not(unix), allow(dead_code))]
enum LineStyle {
    /// `+N file`
    PlusLine,
    /// `-g file:N`
    GotoFlag,
    /// `file:N`
    Suffix,
    None,
}

/// The argv that runs `editor` on `file`, the way git runs editors: through
/// `sh` so the setting may carry arguments, with the file and line only as
/// positional parameters, never inside the shell string.
#[cfg_attr(not(unix), allow(dead_code))]
pub fn editor_argv(editor: &str, file: &Path, line: Option<u32>) -> Vec<OsString> {
    let with_line = |n: u32| {
        let mut arg = file.as_os_str().to_os_string();
        arg.push(format!(":{n}"));
        arg
    };
    let args: Vec<OsString> = match (line, line_style(editor)) {
        (Some(n), LineStyle::PlusLine) => vec![format!("+{n}").into(), file.into()],
        (Some(n), LineStyle::GotoFlag) => vec!["-g".into(), with_line(n)],
        (Some(n), LineStyle::Suffix) => vec![with_line(n)],
        _ => vec![file.into()],
    };
    let mut argv: Vec<OsString> = vec![
        "sh".into(),
        "-c".into(),
        format!("{editor} \"$@\"").into(),
        editor.into(),
    ];
    argv.extend(args);
    argv
}

/// Run the editor on the inherited terminal and wait for it.
#[cfg(unix)]
fn launch(editor: &str, file: &Path, line: Option<u32>, workdir: &Path) -> Result<(), String> {
    let argv = editor_argv(editor, file, line);
    let status = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(workdir)
        .status()
        .map_err(|e| terminal_safe(format!("Could not start the editor: {e}").as_bytes()))?;
    if status.success() {
        Ok(())
    } else {
        Err(terminal_safe(
            format!("The editor exited with {status}").as_bytes(),
        ))
    }
}

/// Editors are launched through `sh`, which is not there (#48).
#[cfg(not(unix))]
fn launch(_: &str, _: &Path, _: Option<u32>, _: &Path) -> Result<(), String> {
    Err("Opening an editor is not supported on this platform yet".into())
}

/// Shown under the hunk in the editor; every line starts with `#`, so
/// [`edited_patch`] drops them.
const EDIT_HELP: &[u8] = b"\
# Edit the hunk above, then save and close the editor.
# Keep the first character of every line: ' ' unchanged, '+' added, '-' removed.
# To leave a '-' line out, turn its '-' into a space.
# To leave a '+' line out, delete the line.
# Lines starting with '#' are ignored. Empty the file to cancel.
";

fn edit_hunk(
    header: &[u8],
    hunk: &[u8],
    runner: &dyn GitRunner,
    workdir: &Path,
) -> Result<Option<Vec<u8>>, String> {
    let editor = editor(runner)?;
    let (temp, mut file) = TempFile::create("kika-hunk", "diff")
        .map_err(|e| io_error("Could not create the hunk file", &e))?;
    let mut contents = hunk.to_vec();
    if !contents.is_empty() && !contents.ends_with(b"\n") {
        contents.push(b'\n');
    }
    contents.extend_from_slice(EDIT_HELP);
    file.write_all(&contents)
        .map_err(|e| io_error("Could not write the hunk file", &e))?;
    // Closed before the editor opens it.
    drop(file);
    launch(&editor, &temp.path, None, workdir)?;
    let edited = fs::read(&temp.path).map_err(|e| io_error("Could not read the hunk file", &e))?;
    Ok(edited_patch(header, &edited))
}

/// Shown under the message in the editor; [`edited_message`] drops it.
const COMMIT_HELP: &str = "\
# Save and close to commit. Lines starting with '#' are ignored.
# Empty the message to abort.
";

fn edit_commit_message(
    initial: &str,
    runner: &dyn GitRunner,
    workdir: &Path,
) -> Result<Option<String>, String> {
    let editor = editor(runner)?;
    let (temp, mut file) = TempFile::create("kika-commit", "txt")
        .map_err(|e| io_error("Could not create the message file", &e))?;
    let mut contents = initial.to_string();
    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents.push('\n');
    contents.push_str(COMMIT_HELP);
    file.write_all(contents.as_bytes())
        .map_err(|e| io_error("Could not write the message file", &e))?;
    // Closed before the editor opens it.
    drop(file);
    launch(&editor, &temp.path, None, workdir)?;
    let edited =
        fs::read(&temp.path).map_err(|e| io_error("Could not read the message file", &e))?;
    edited_message(&edited)
}

/// The message to commit after an edit: the lines not starting with `#`,
/// without blank lines before them or whitespace after them. `None` when
/// nothing is left, which is how the user aborts. Git keeps `#` lines
/// given with `-F -`, so they are dropped here.
pub fn edited_message(edited: &[u8]) -> Result<Option<String>, String> {
    let text =
        std::str::from_utf8(edited).map_err(|_| "The message is not valid UTF-8".to_string())?;
    let kept: Vec<&str> = text.lines().filter(|line| !line.starts_with('#')).collect();
    let start = kept
        .iter()
        .position(|line| !line.trim().is_empty())
        .unwrap_or(kept.len());
    let message = kept[start..].join("\n").trim_end().to_string();
    Ok((!message.is_empty()).then_some(message))
}

fn io_error(what: &str, error: &io::Error) -> String {
    terminal_safe(format!("{what}: {error}").as_bytes())
}

/// The patch to apply after an edit: `header`, then the edited lines minus
/// those starting with `#`. `None` when nothing but blank lines is left,
/// which is how the user cancels. Bytes are kept as they are; only a
/// missing final newline is added, since git needs it to end the patch.
pub fn edited_patch(header: &[u8], edited: &[u8]) -> Option<Vec<u8>> {
    let rest: Vec<u8> = edited
        .split_inclusive(|&b| b == b'\n')
        .filter(|line| !line.starts_with(b"#"))
        .flatten()
        .copied()
        .collect();
    if rest.iter().all(u8::is_ascii_whitespace) {
        return None;
    }
    let mut patch = header.to_vec();
    patch.extend_from_slice(&rest);
    if !patch.ends_with(b"\n") {
        patch.push(b'\n');
    }
    Some(patch)
}

/// A private file in the temp dir, deleted when dropped, whatever happened
/// in between.
struct TempFile {
    path: PathBuf,
}

impl TempFile {
    /// A new file only this user can read, named `<prefix>-<pid>-<n>.<extension>`
    /// so editors pick the right mode, and unlike any other file, open for
    /// writing.
    fn create(prefix: &str, extension: &str) -> io::Result<(Self, File)> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("{prefix}-{}-{n}.{extension}", std::process::id()));
            match Self::open_new(&path) {
                Ok(file) => return Ok((Self { path }, file)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }

    fn open_new(path: &Path) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(path)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        // Already gone is fine; the editor may have replaced or removed it.
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::fake::{ANY_ARG, FakeRunner, fail, ok};

    fn argv(editor: &str, line: Option<u32>) -> Vec<String> {
        editor_argv(editor, Path::new("/r/a b.txt"), line)
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect()
    }

    fn sh(editor: &str, args: &[&str]) -> Vec<String> {
        let mut out = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("{editor} \"$@\""),
            editor.to_string(),
        ];
        out.extend(args.iter().map(|a| a.to_string()));
        out
    }

    #[test]
    fn terminal_editors_take_plus_line_before_the_file() {
        assert_eq!(argv("vim", Some(12)), sh("vim", &["+12", "/r/a b.txt"]));
        assert_eq!(
            argv("/usr/bin/nvim -u x", Some(3)),
            sh("/usr/bin/nvim -u x", &["+3", "/r/a b.txt"])
        );
    }

    #[test]
    fn code_takes_a_goto_flag_and_subl_a_suffix() {
        assert_eq!(
            argv("code --wait", Some(7)),
            sh("code --wait", &["-g", "/r/a b.txt:7"])
        );
        assert_eq!(argv("subl -w", Some(7)), sh("subl -w", &["/r/a b.txt:7"]));
    }

    #[test]
    fn an_unknown_editor_or_no_line_gets_the_file_only() {
        assert_eq!(argv("ed", Some(7)), sh("ed", &["/r/a b.txt"]));
        assert_eq!(argv("vim", None), sh("vim", &["/r/a b.txt"]));
        assert_eq!(
            argv("code --wait", None),
            sh("code --wait", &["/r/a b.txt"])
        );
    }

    #[test]
    fn the_shell_string_never_holds_the_file_name() {
        for editor in ["vim", "code --wait", "subl -w", "ed"] {
            let argv = argv(editor, Some(1));
            assert_eq!(argv[2], format!("{editor} \"$@\""));
            assert!(!argv[2].contains("a b.txt"), "{argv:?}");
        }
    }

    const HEADER: &[u8] = b"diff --git a/a b/a\n--- a/a\n+++ b/a\n";

    #[test]
    fn comment_lines_are_dropped_and_the_header_goes_back_on() {
        let edited = b"@@ -1 +1,2 @@\n one\n+two\n# help\n#\n";
        let mut expected = HEADER.to_vec();
        expected.extend_from_slice(b"@@ -1 +1,2 @@\n one\n+two\n");
        assert_eq!(edited_patch(HEADER, edited), Some(expected));
    }

    #[test]
    fn an_empty_or_all_comment_file_cancels() {
        assert_eq!(edited_patch(HEADER, b""), None);
        assert_eq!(edited_patch(HEADER, b"# a\n# b\n"), None);
        assert_eq!(edited_patch(HEADER, b"\n# a\n"), None);
    }

    #[test]
    fn crlf_and_invalid_utf8_survive_exactly() {
        let edited = b"@@ -1 +1 @@\r\n-\xff old\r\n+\xfe new\r\n# help\r\n";
        let mut expected = HEADER.to_vec();
        expected.extend_from_slice(b"@@ -1 +1 @@\r\n-\xff old\r\n+\xfe new\r\n");
        assert_eq!(edited_patch(HEADER, edited), Some(expected));
    }

    #[test]
    fn a_missing_final_newline_is_added() {
        let mut expected = HEADER.to_vec();
        expected.extend_from_slice(b"@@ -1 +1 @@\n-a\n+b\n");
        assert_eq!(edited_patch(HEADER, b"@@ -1 +1 @@\n-a\n+b"), Some(expected));
    }

    #[test]
    fn comment_lines_are_dropped_from_an_edited_message_and_its_end_trimmed() {
        assert_eq!(
            edited_message(b"subj\n# help\n\nbody  \n#x\n\n  \n"),
            Ok(Some("subj\n\nbody".into()))
        );
        // Blank lines before the subject go too; indentation inside stays.
        assert_eq!(
            edited_message(b"\n \n# c\nsubj\n\n  indented\n"),
            Ok(Some("subj\n\n  indented".into()))
        );
    }

    #[test]
    fn an_empty_or_all_comment_message_is_none() {
        assert_eq!(edited_message(b""), Ok(None));
        assert_eq!(edited_message(b"# a\n# b\n"), Ok(None));
        assert_eq!(edited_message(b"\n  \n# a\n\t\n"), Ok(None));
    }

    #[test]
    fn an_edited_message_that_is_not_utf8_is_an_error() {
        let result = edited_message(b"subj \xff\n");
        assert!(result.is_err(), "{result:?}");
    }

    #[test]
    fn a_patch_splits_at_its_first_hunk_line() {
        let patch = b"diff --git a/x@@ b/x@@\n--- a/x@@\n+++ b/x@@\n@@ -1 +1 @@\n-a\n+b\n";
        assert_eq!(
            HandOff::edit_hunk(patch),
            HandOff::EditHunk {
                header: b"diff --git a/x@@ b/x@@\n--- a/x@@\n+++ b/x@@\n".to_vec(),
                hunk: b"@@ -1 +1 @@\n-a\n+b\n".to_vec(),
            }
        );
    }

    #[test]
    fn a_failing_git_var_is_an_error_intent() {
        let runner = FakeRunner::new().expect(&["var", "GIT_EDITOR"], fail("fatal: \x1b[2Jno\n"));
        let job = HandOff::OpenFile {
            path: "a".into(),
            line: None,
        };
        assert_eq!(
            execute(job, &runner, Path::new("/nowhere")),
            AppIntent::EditorClosed {
                result: Err("fatal: \u{FFFD}[2Jno".into())
            }
        );
        runner.assert_done();
    }

    fn commit_op(message: Option<&str>, stage_all: bool, amend: bool, no_verify: bool) -> GitOp {
        GitOp::Commit {
            message: message.map(Into::into),
            stage_all,
            amend,
            no_verify,
        }
    }

    const TAG: Tag = Tag {
        generation: 2,
        seq: 5,
        user: true,
    };

    /// Run `op` as a commit hand-off, counting the pauses.
    fn commit_with(runner: &FakeRunner, op: &GitOp) -> (AppIntent, usize) {
        let pauses = std::cell::Cell::new(0);
        let job = HandOff::Commit {
            tag: TAG,
            op: op.clone(),
        };
        let intent = execute_with(job, runner, Path::new("/nowhere"), &|| {
            pauses.set(pauses.get() + 1)
        });
        (intent, pauses.get())
    }

    fn finished(op: &GitOp, result: Result<(), String>) -> AppIntent {
        AppIntent::OpFinished {
            tag: TAG,
            op: op.clone(),
            result,
        }
    }

    /// What an attached run saw: its message file's path, contents and mode.
    type Seen = std::sync::Arc<std::sync::Mutex<Option<(PathBuf, Vec<u8>, u32)>>>;

    fn watch_message_file(runner: FakeRunner) -> (FakeRunner, Seen) {
        let seen: Seen = Default::default();
        let record = std::sync::Arc::clone(&seen);
        let runner = runner.on_attached(move |cmd| {
            let args = cmd.args();
            let at = args.iter().position(|a| a == "-F").unwrap();
            let path = PathBuf::from(&args[at + 1]);
            let meta = fs::metadata(&path).unwrap();
            #[cfg(unix)]
            let mode = std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o777;
            #[cfg(not(unix))]
            let mode = 0o600;
            *record.lock().unwrap() = Some((path.clone(), fs::read(&path).unwrap(), mode));
        });
        (runner, seen)
    }

    #[test]
    fn a_commit_message_goes_to_git_in_a_private_file_that_is_gone_afterwards() {
        let op = commit_op(Some("subj\n\nbody\n"), false, false, false);
        let (runner, seen) =
            watch_message_file(FakeRunner::new().expect_attached(&["commit", "-F", ANY_ARG], true));

        let (intent, pauses) = commit_with(&runner, &op);

        assert_eq!(intent, finished(&op, Ok(())));
        assert_eq!(pauses, 0);
        runner.assert_done();
        let (path, contents, mode) = seen.lock().unwrap().clone().expect("git ran");
        assert_eq!(contents, b"subj\n\nbody\n");
        assert_eq!(mode, 0o600);
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("kika-commit-msg-"), "{name}");
        assert!(!path.exists(), "{path:?} was left behind");
        // Stdin is the terminal's, never bytes from Kika.
        assert_eq!(runner.inputs(), [None]);
    }

    #[test]
    fn amend_without_a_message_keeps_heads_and_no_verify_is_passed_through() {
        let amend = commit_op(None, false, true, false);
        let runner = FakeRunner::new().expect_attached(&["commit", "--amend", "--no-edit"], true);
        assert_eq!(commit_with(&runner, &amend), (finished(&amend, Ok(())), 0));
        runner.assert_done();

        let skip = commit_op(Some("m\n"), false, false, true);
        let runner =
            FakeRunner::new().expect_attached(&["commit", "--no-verify", "-F", ANY_ARG], true);
        assert_eq!(commit_with(&runner, &skip), (finished(&skip, Ok(())), 0));
        runner.assert_done();
    }

    #[test]
    fn stage_all_runs_add_detached_first_and_a_failed_add_skips_the_commit() {
        let op = commit_op(Some("m\n"), true, false, false);
        let runner = FakeRunner::new()
            .expect(&["add", "-A"], ok(b""))
            .expect_attached(&["commit", "-F", ANY_ARG], true);
        assert_eq!(commit_with(&runner, &op), (finished(&op, Ok(())), 0));
        runner.assert_done();

        // No attached run is expected, so the fake would panic on one.
        let runner = FakeRunner::new().expect(&["add", "-A"], fail("fatal: index broken\n"));
        assert_eq!(
            commit_with(&runner, &op),
            (finished(&op, Err("fatal: index broken".into())), 0)
        );
        runner.assert_done();
    }

    #[test]
    fn a_failed_commit_pauses_once_and_says_where_its_output_went() {
        let op = commit_op(Some("m\n"), false, false, false);
        let runner = FakeRunner::new().expect_attached(&["commit", "-F", ANY_ARG], false);
        assert_eq!(
            commit_with(&runner, &op),
            (
                finished(
                    &op,
                    Err("git commit exited with status 1; \
                         its output was shown in the terminal"
                        .into())
                ),
                1
            )
        );
        runner.assert_done();
    }

    /// A scripted editor run through the real launch path.
    #[cfg(unix)]
    fn editor_script(dir: &Path, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("editor.sh");
        fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        script.display().to_string()
    }

    #[cfg(unix)]
    #[test]
    fn an_edited_hunk_comes_back_as_a_patch_and_the_temp_file_is_gone() {
        let tmp = crate::git::fake::TempDir::new("handoff");
        let dir = tmp.path();
        let editor = editor_script(
            dir,
            "echo \"$1\" > seen\ngrep -v '^+drop$' \"$1\" > \"$1.new\" && mv \"$1.new\" \"$1\"",
        );
        let runner = FakeRunner::new().expect(&["var", "GIT_EDITOR"], ok(editor.as_bytes()));
        let job = HandOff::EditHunk {
            header: HEADER.to_vec(),
            hunk: b"@@ -1 +1,3 @@\n a\n+keep\n+drop\n".to_vec(),
        };

        let intent = execute(job, &runner, dir);

        let mut expected = HEADER.to_vec();
        expected.extend_from_slice(b"@@ -1 +1,3 @@\n a\n+keep\n");
        assert_eq!(
            intent,
            AppIntent::HunkEdited {
                result: Ok(Some(expected))
            }
        );
        let seen = fs::read_to_string(dir.join("seen")).unwrap();
        let temp = Path::new(seen.trim_end());
        assert!(temp.starts_with(std::env::temp_dir()), "{seen}");
        assert!(!temp.exists(), "{seen} was left behind");
    }

    #[cfg(unix)]
    #[test]
    fn an_edited_commit_message_comes_back_and_the_temp_file_is_gone() {
        let tmp = crate::git::fake::TempDir::new("handoff-commit");
        let dir = tmp.path();
        let editor = editor_script(
            dir,
            "echo \"$1\" > seen\ncp \"$1\" before\nprintf 'new subj\\n\\nnew body\\n' > \"$1\"",
        );
        let runner = FakeRunner::new().expect(&["var", "GIT_EDITOR"], ok(editor.as_bytes()));
        let job = HandOff::EditCommitMessage {
            initial: "old subj\n\nold body\n".into(),
        };

        let intent = execute(job, &runner, dir);

        assert_eq!(
            intent,
            AppIntent::CommitMessageEdited {
                result: Ok(Some("new subj\n\nnew body".into()))
            }
        );
        let before = fs::read_to_string(dir.join("before")).unwrap();
        assert!(
            before.starts_with("old subj\n\nold body\n\n# "),
            "{before:?}"
        );
        assert!(before.contains("Empty the message to abort."), "{before:?}");
        let seen = fs::read_to_string(dir.join("seen")).unwrap();
        let temp = Path::new(seen.trim_end());
        assert!(temp.starts_with(std::env::temp_dir()), "{seen}");
        let name = temp.file_name().unwrap().to_string_lossy();
        assert!(
            name.starts_with("kika-commit-") && name.ends_with(".txt"),
            "{name}"
        );
        assert!(!temp.exists(), "{seen} was left behind");
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_editor_is_an_error_for_a_commit_message() {
        let tmp = crate::git::fake::TempDir::new("handoff-commit-fail");
        let dir = tmp.path();
        let editor = editor_script(dir, "exit 3");
        let runner = FakeRunner::new().expect(&["var", "GIT_EDITOR"], ok(editor.as_bytes()));
        let job = HandOff::EditCommitMessage {
            initial: "subj\n".into(),
        };

        match execute(job, &runner, dir) {
            AppIntent::CommitMessageEdited { result: Err(e) } => {
                assert!(e.starts_with("The editor exited with"), "{e}")
            }
            other => panic!("expected a failed edit, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_hunk_file_is_private_while_the_editor_runs() {
        let tmp = crate::git::fake::TempDir::new("handoff-mode");
        let dir = tmp.path();
        let editor = editor_script(dir, "ls -l \"$1\" > mode");
        let runner = FakeRunner::new().expect(&["var", "GIT_EDITOR"], ok(editor.as_bytes()));
        let job = HandOff::edit_hunk(b"diff --git a/a b/a\n@@ -1 +1 @@\n-a\n+b\n");

        execute(job, &runner, dir);

        let mode = fs::read_to_string(dir.join("mode")).unwrap();
        assert!(mode.starts_with("-rw-------"), "{mode}");
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_editor_is_an_error_and_still_removes_the_file() {
        let tmp = crate::git::fake::TempDir::new("handoff-fail");
        let dir = tmp.path();
        let editor = editor_script(dir, "echo \"$1\" > seen\nexit 3");
        let runner = FakeRunner::new().expect(&["var", "GIT_EDITOR"], ok(editor.as_bytes()));
        let job = HandOff::edit_hunk(b"diff --git a/a b/a\n@@ -1 +1 @@\n-a\n+b\n");

        match execute(job, &runner, dir) {
            AppIntent::HunkEdited { result: Err(e) } => {
                assert!(e.starts_with("The editor exited with"), "{e}")
            }
            other => panic!("expected a failed edit, got {other:?}"),
        }
        let seen = fs::read_to_string(dir.join("seen")).unwrap();
        assert!(!Path::new(seen.trim_end()).exists());
    }

    #[cfg(unix)]
    #[test]
    fn open_file_runs_the_editor_in_the_workdir_on_the_raw_path() {
        let tmp = crate::git::fake::TempDir::new("handoff-open");
        let dir = tmp.path();
        let editor = editor_script(dir, "pwd > cwd\nprintf '%s\\n' \"$@\" > args");
        let runner = FakeRunner::new().expect(&["var", "GIT_EDITOR"], ok(editor.as_bytes()));
        let job = HandOff::OpenFile {
            path: "sub/$(touch x).txt".into(),
            line: Some(4),
        };

        let intent = execute(job, &runner, dir);

        assert_eq!(intent, AppIntent::EditorClosed { result: Ok(()) });
        let args = fs::read_to_string(dir.join("args")).unwrap();
        assert_eq!(
            args,
            format!("{}\n", dir.join("sub/$(touch x).txt").display())
        );
        assert!(!dir.join("x").exists());
        let cwd = fs::canonicalize(fs::read_to_string(dir.join("cwd")).unwrap().trim_end());
        assert_eq!(cwd.unwrap(), fs::canonicalize(dir).unwrap());
    }
}
