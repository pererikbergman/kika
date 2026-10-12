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
use crate::features::custom_commands::{CommandFailure, OutputMode};
use crate::git::GitRunner;
use crate::git::cherry_pick;
use crate::git::custom_patch;
use crate::git::diff::terminal_safe;
use crate::git::path::RepoPath;
use crate::git::runner::GitCommand;
use crate::intent::AppIntent;
use crate::op::{GitOp, MessageSource, commit_command, op_command, run_op};
use crate::sequence_editor::editor_value;

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
    /// Run a git op that may make commits on the terminal, so its hooks,
    /// signing and git's editor work as on the command line: a merge
    /// commit, a rebase, and continuing or skipping one. Answered like a
    /// detached run.
    Attached { tag: Tag, op: GitOp },
    /// Run the user's command, its argv already expanded, on the terminal.
    RunCommand { title: String, argv: Vec<OsString> },
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

/// [`execute`], with `pause` run after a failed attached git so its output
/// stays readable; tests pass one that returns at once.
pub fn execute_with(
    job: HandOff,
    runner: &dyn GitRunner,
    workdir: &Path,
    pause: &dyn Fn(),
) -> AppIntent {
    execute_as(job, runner, workdir, pause, &std::env::current_exe)
}

/// [`execute_with`], with `exe` saying which program git runs as the
/// sequence editor of an interactive rebase: Kika itself, except in tests
/// that cannot run the built binary.
pub fn execute_as(
    job: HandOff,
    runner: &dyn GitRunner,
    workdir: &Path,
    pause: &dyn Fn(),
    exe: &dyn Fn() -> io::Result<PathBuf>,
) -> AppIntent {
    match job {
        HandOff::Commit { tag, op } => AppIntent::OpFinished {
            result: commit_on_terminal(&op, runner, pause),
            tag,
            op,
        },
        HandOff::Attached { tag, op } => {
            // A paste picks parents first, which only git can say; asked
            // here, just before the picks, so the order is the branch's now.
            let op = match op {
                GitOp::CherryPick(picks) => match cherry_pick::ordered(runner, &picks) {
                    Ok(ordered) => GitOp::CherryPick(ordered),
                    Err(error) => {
                        return AppIntent::OpFinished {
                            tag,
                            op: GitOp::CherryPick(picks),
                            result: Err(error),
                        };
                    }
                },
                op => op,
            };
            let result = attached_command(&op, exe).and_then(|cmd| match &op {
                GitOp::CustomPatch {
                    plan,
                    action,
                    picks,
                    orig_head,
                } => {
                    let rewrite = custom_patch::Rewrite {
                        plan,
                        action,
                        picks,
                        orig_head,
                    };
                    custom_patch::run(runner, &rewrite, cmd, &|step| {
                        run_on_terminal(step, runner, pause)
                    })
                }
                _ => run_on_terminal(cmd, runner, pause),
            });
            AppIntent::OpFinished { result, tag, op }
        }
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
        HandOff::RunCommand { title, argv } => AppIntent::CommandFinished {
            title,
            output: OutputMode::Terminal,
            result: run_command(&argv, workdir).map(|()| String::new()),
        },
    }
}

/// The command for an attached `op`. An interactive rebase, a custom
/// patch's first step among them, also names
/// Kika, at `exe`, as git's sequence editor, which rewrites the todo list
/// to the plan; without that path nothing may run.
fn attached_command(
    op: &GitOp,
    exe: impl FnOnce() -> io::Result<PathBuf>,
) -> Result<GitCommand, String> {
    let cmd = op_command(op);
    if !matches!(op, GitOp::InteractiveRebase(_) | GitOp::CustomPatch { .. }) {
        return Ok(cmd);
    }
    let exe = exe().map_err(|e| io_error("Could not find Kika's own program", &e))?;
    Ok(cmd.env("GIT_SEQUENCE_EDITOR", editor_value(&exe)))
}

/// Run `argv` directly, with no shell in between, and wait for it.
fn run_command(argv: &[OsString], workdir: &Path) -> Result<(), CommandFailure> {
    let Some((program, args)) = argv.split_first() else {
        return Err(CommandFailure::Launch("The command is empty".into()));
    };
    let status = crate::program::command(program)
        .and_then(|mut command| command.args(args).current_dir(workdir).status())
        .map_err(|e| CommandFailure::Launch(start_error(program, &e)))?;
    if status.success() {
        Ok(())
    } else {
        Err(CommandFailure::Exited(terminal_safe(
            format!("exited with {status}").as_bytes(),
        )))
    }
}

/// Why `program` did not start, safe to draw.
pub fn start_error(program: &OsString, error: &io::Error) -> String {
    let program = program.to_string_lossy();
    terminal_safe(format!("Could not start {program}: {error}").as_bytes())
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
/// when asked.
fn commit_on_terminal(op: &GitOp, runner: &dyn GitRunner, pause: &dyn Fn()) -> Result<(), String> {
    let GitOp::Commit {
        message,
        stage_all,
        amend,
        no_verify,
        fixup,
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
    let source = match (&file, fixup) {
        (Some(file), _) => MessageSource::File(&file.path),
        (None, Some(target)) => MessageSource::Fixup(target),
        (None, None) => MessageSource::Keep,
    };
    let cmd = commit_command(*amend, *no_verify, source);
    run_on_terminal(cmd, runner, pause)
}

/// Run `cmd` attached to the terminal, with `GPG_TTY` so signing can ask
/// there. On failure, `pause` keeps git's output on screen until the user
/// is done reading it, since nothing of it is captured.
fn run_on_terminal(
    mut cmd: GitCommand,
    runner: &dyn GitRunner,
    pause: &dyn Fn(),
) -> Result<(), String> {
    let subcommand = cmd.subcommand().to_string_lossy().into_owned();
    if let Some(tty) = gpg_tty(std::env::var_os("GPG_TTY"), stdin_tty()) {
        cmd = cmd.env("GPG_TTY", tty);
    }
    let exit = runner
        .run_attached(&cmd)
        .map_err(|e| io_error(&format!("Could not start git {subcommand}"), &e))?;
    if exit.success {
        return Ok(());
    }
    pause();
    Err(format!(
        "git {subcommand} exited with {}; its output was shown in the terminal",
        exit.status
    ))
}

/// The `GPG_TTY` to give an attached commit: Kika's terminal, `tty`, unless
/// the user already set one (an empty value counts as unset). Git pipes
/// gpg's stdin, so a terminal pinentry cannot find the terminal by itself;
/// without this, signing fails with "Operation cancelled".
fn gpg_tty(existing: Option<OsString>, tty: Option<OsString>) -> Option<OsString> {
    match existing {
        Some(value) if !value.is_empty() => None,
        _ => tty,
    }
}

/// The name of the terminal on Kika's stdin, such as `/dev/ttys003`; `None`
/// when stdin is not a terminal.
#[cfg(unix)]
fn stdin_tty() -> Option<OsString> {
    use std::os::unix::ffi::OsStrExt;

    let mut buf = [0u8; 256];
    // SAFETY: `buf` is valid for `buf.len()` bytes, and `ttyname_r` writes
    // a NUL-terminated name within it or returns an error.
    let failed = unsafe { libc::ttyname_r(0, buf.as_mut_ptr().cast(), buf.len()) };
    if failed != 0 {
        return None;
    }
    let len = buf.iter().position(|&b| b == 0)?;
    Some(std::ffi::OsStr::from_bytes(&buf[..len]).to_os_string())
}

#[cfg(not(unix))]
fn stdin_tty() -> Option<OsString> {
    None
}

/// Keep the terminal as git left it until the user presses Enter.
fn wait_for_enter() {
    // On its own line, whatever git printed last.
    print!("\ngit failed. Press Enter to return to Kika.");
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
    let Some((program, args)) = argv.split_first() else {
        return Err("Could not start the editor: no program".into());
    };
    let status = crate::program::command(program)
        .and_then(|mut command| command.args(args).current_dir(workdir).status())
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

/// How many directory names [`TempFile::create`] tries before giving up.
const TEMP_ATTEMPTS: u32 = 16;

/// A private file in a fresh directory of its own under the temp dir. The
/// file is read back by name after the editor exits, and an editor may save
/// by deleting and re-creating it; inside a directory only this user can
/// write to, no other user can put a file at that name in the gap. The
/// directory and all in it go when this is dropped, whatever happened in
/// between, panics included.
struct TempFile {
    path: PathBuf,
    dir: PathBuf,
}

impl TempFile {
    /// A new file only this user can read, named `<prefix>-<pid>-<n>.<extension>`
    /// so editors pick the right mode, inside a new mode-0700 directory
    /// `<prefix>-<pid>-<n>`, and unlike any other file, open for writing.
    fn create(prefix: &str, extension: &str) -> io::Result<(Self, File)> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self::create_with(&NEXT, prefix, extension)
    }

    /// [`Self::create`] counting `n` from `next`.
    fn create_with(next: &AtomicU64, prefix: &str, extension: &str) -> io::Result<(Self, File)> {
        let pid = std::process::id();
        let mut last = io::Error::from(io::ErrorKind::AlreadyExists);
        for _ in 0..TEMP_ATTEMPTS {
            let name = format!("{prefix}-{pid}-{}", next.fetch_add(1, Ordering::Relaxed));
            let dir = std::env::temp_dir().join(&name);
            match Self::create_dir(&dir) {
                Ok(()) => {}
                // Never reuse an entry already there: it may be planted.
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    last = e;
                    continue;
                }
                Err(e) => return Err(e),
            }
            let path = dir.join(format!("{name}.{extension}"));
            // Held before the file opens, so a failure there removes the
            // directory too.
            let temp = Self { path, dir };
            let file = Self::open_new(&temp.path)?;
            return Ok((temp, file));
        }
        Err(last)
    }

    /// Not recursive, so an existing directory or symlink at `dir` fails
    /// with `AlreadyExists` instead of being used.
    fn create_dir(dir: &Path) -> io::Result<()> {
        // Only Unix sets a mode on it.
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(dir)
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
        // Also clears swap and backup files the editor left beside the file.
        // Symlinks inside are removed, not followed.
        let _ = fs::remove_dir_all(&self.dir);
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

    /// Hidden: run by `program`'s decoy test, from a directory of decoys
    /// with only relative and empty `PATH` entries.
    #[cfg(unix)]
    #[test]
    fn decoy_child_run_command_and_editor() {
        use crate::program::NOT_FOUND;
        if !crate::program::in_decoy_child() {
            return;
        }
        let here = std::env::current_dir().unwrap();
        match run_command(&["kika-decoy".into()], &here) {
            Err(CommandFailure::Launch(e)) => assert!(e.contains(NOT_FOUND), "{e}"),
            other => panic!("{other:?}"),
        }
        let e = launch("kika-decoy", Path::new("file"), None, &here).unwrap_err();
        assert!(e.starts_with("Could not start the editor"), "{e}");
        assert!(e.contains(NOT_FOUND), "{e}");
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
            fixup: None,
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

    #[test]
    fn an_interactive_rebase_runs_attached_with_kika_as_the_sequence_editor() {
        let op = GitOp::InteractiveRebase(crate::git::rebase_plan::sample());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let record = std::sync::Arc::clone(&seen);
        let runner = FakeRunner::new()
            .expect_attached(
                &[
                    "-c",
                    "rebase.updateRefs=false",
                    "-c",
                    "rebase.rebaseMerges=false",
                    "-c",
                    "core.commentChar=#",
                    "rebase",
                    "-i",
                    "--autostash",
                    "--no-autosquash",
                    "--keep-empty",
                    &"c".repeat(40),
                ],
                true,
            )
            .on_attached(move |cmd| {
                let editor = cmd
                    .envs()
                    .iter()
                    .find(|(k, _)| k == "GIT_SEQUENCE_EDITOR")
                    .map(|(_, v)| v.clone());
                *record.lock().unwrap() = editor;
            });
        assert_eq!(attached_with(&runner, &op), (finished(&op, Ok(())), 0));
        runner.assert_done();
        let exe = std::env::current_exe().unwrap();
        assert_eq!(*seen.lock().unwrap(), Some(editor_value(&exe)));
        assert_eq!(runner.inputs(), [None]);
    }

    #[test]
    fn without_its_own_path_kika_starts_no_rebase() {
        let op = GitOp::InteractiveRebase(crate::git::rebase_plan::sample());
        let missing = || Err(io::Error::from(io::ErrorKind::NotFound));
        let error = attached_command(&op, missing).unwrap_err();
        assert!(
            error.starts_with("Could not find Kika's own program"),
            "{error}"
        );
        // Other ops do not need it.
        let other = GitOp::Continue(crate::git::repo_state::Operation::Rebase);
        assert_eq!(attached_command(&other, missing), Ok(op_command(&other)));
        let with = attached_command(&op, || Ok(PathBuf::from("/opt/k's/kika"))).unwrap();
        assert_eq!(
            with.envs().last(),
            Some(&("GIT_SEQUENCE_EDITOR".into(), r"'/opt/k'\''s/kika'".into()))
        );
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
            // Off Unix only the file's existence is checked; its mode is not read.
            #[cfg_attr(not(unix), allow(unused_variables))]
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
        // It lived in a private directory of its own, gone with it.
        let dir = path.parent().unwrap();
        assert_ne!(dir, std::env::temp_dir());
        assert!(!dir.exists(), "{dir:?} was left behind");
        // Stdin is the terminal's, never bytes from Kika.
        assert_eq!(runner.inputs(), [None]);
    }

    #[test]
    fn gpg_tty_is_kikas_terminal_unless_the_user_set_one() {
        let tty = || Some(OsString::from("/dev/ttys003"));
        assert_eq!(gpg_tty(None, tty()), tty());
        assert_eq!(gpg_tty(Some("".into()), tty()), tty());
        assert_eq!(gpg_tty(Some("/dev/pts/7".into()), tty()), None);
        assert_eq!(gpg_tty(None, None), None);
    }

    #[cfg(unix)]
    #[test]
    fn stdin_tty_names_a_device_or_nothing() {
        // Under a test runner stdin may or may not be a terminal.
        if let Some(name) = stdin_tty() {
            assert!(name.to_string_lossy().starts_with("/dev/"), "{name:?}");
        }
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
    fn a_fixup_on_the_terminal_names_its_target_and_writes_no_message_file() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let op = GitOp::Commit {
            message: None,
            stage_all: false,
            amend: false,
            no_verify: true,
            fixup: crate::git::commit_files::CommitId::parse(oid),
        };
        let fixup = format!("--fixup={oid}");
        let runner = FakeRunner::new().expect_attached(&["commit", "--no-verify", &fixup], true);
        assert_eq!(commit_with(&runner, &op), (finished(&op, Ok(())), 0));
        runner.assert_done();
        assert_eq!(runner.inputs(), [None]);
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

    /// Run `op` as an attached hand-off, counting the pauses.
    fn attached_with(runner: &FakeRunner, op: &GitOp) -> (AppIntent, usize) {
        let pauses = std::cell::Cell::new(0);
        let job = HandOff::Attached {
            tag: TAG,
            op: op.clone(),
        };
        let intent = execute_with(job, runner, Path::new("/nowhere"), &|| {
            pauses.set(pauses.get() + 1)
        });
        (intent, pauses.get())
    }

    #[test]
    fn an_attached_op_runs_its_argv_on_the_terminal_and_reports_back() {
        use crate::op::MergeStrategy;
        let op = GitOp::Merge {
            branch: "feature".into(),
            strategy: MergeStrategy::NoFastForward,
        };
        let runner =
            FakeRunner::new().expect_attached(&["merge", "--no-ff", "refs/heads/feature"], true);
        assert_eq!(attached_with(&runner, &op), (finished(&op, Ok(())), 0));
        runner.assert_done();
        // Stdin is the terminal's, so git's editor and prompts reach the user.
        assert_eq!(runner.inputs(), [None]);
    }

    #[test]
    fn a_failed_attached_op_pauses_once_and_says_where_its_output_went() {
        let op = GitOp::Continue(crate::git::repo_state::Operation::Rebase);
        let runner = FakeRunner::new().expect_attached(&["rebase", "--continue"], false);
        assert_eq!(
            attached_with(&runner, &op),
            (
                finished(
                    &op,
                    Err("git rebase exited with status 1; \
                         its output was shown in the terminal"
                        .into())
                ),
                1
            )
        );
        runner.assert_done();
    }

    #[test]
    fn a_paste_picks_in_the_order_git_gives_and_reports_the_ordered_op() {
        let (a, b) = ("a".repeat(40), "b".repeat(40));
        let id = |hex: &str| crate::git::commit_files::CommitId::parse(hex).unwrap();
        let runner = FakeRunner::new()
            .expect(
                &[
                    "rev-list",
                    "--topo-order",
                    "--reverse",
                    &b,
                    &a,
                    "--not",
                    "HEAD",
                    "--",
                ],
                ok(format!("{a}\n{b}\n").as_bytes()),
            )
            .expect_attached(&["cherry-pick", &a, &b], true);
        let copied = GitOp::CherryPick(vec![id(&b), id(&a)]);
        let picked = GitOp::CherryPick(vec![id(&a), id(&b)]);
        assert_eq!(
            attached_with(&runner, &copied),
            (finished(&picked, Ok(())), 0)
        );
        runner.assert_done();
        assert_eq!(runner.inputs(), [None, None]);
    }

    #[test]
    fn a_pick_already_on_the_branch_starts_nothing_and_says_which() {
        let (a, b) = ("a".repeat(40), "b".repeat(40));
        let id = |hex: &str| crate::git::commit_files::CommitId::parse(hex).unwrap();
        // No attached run is expected, so the fake would panic on one.
        let runner = FakeRunner::new().expect(
            &[
                "rev-list",
                "--topo-order",
                "--reverse",
                &a,
                &b,
                "--not",
                "HEAD",
                "--",
            ],
            ok(format!("{b}\n").as_bytes()),
        );
        let op = GitOp::CherryPick(vec![id(&a), id(&b)]);
        assert_eq!(
            attached_with(&runner, &op),
            (
                finished(&op, Err("aaaaaaa is already on the current branch".into())),
                0
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
        assert!(
            !temp.parent().unwrap().exists(),
            "{seen}: directory left behind"
        );
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
        assert!(
            !temp.parent().unwrap().exists(),
            "{seen}: directory left behind"
        );
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
        let editor = editor_script(
            dir,
            "ls -l \"$1\" > mode\nls -ld \"$(dirname \"$1\")\" > dir_mode\ndirname \"$1\" > parent",
        );
        let runner = FakeRunner::new().expect(&["var", "GIT_EDITOR"], ok(editor.as_bytes()));
        let job = HandOff::edit_hunk(b"diff --git a/a b/a\n@@ -1 +1 @@\n-a\n+b\n");

        execute(job, &runner, dir);

        let mode = fs::read_to_string(dir.join("mode")).unwrap();
        assert!(mode.starts_with("-rw-------"), "{mode}");
        let dir_mode = fs::read_to_string(dir.join("dir_mode")).unwrap();
        assert!(dir_mode.starts_with("drwx------"), "{dir_mode}");
        let parent = fs::read_to_string(dir.join("parent")).unwrap();
        let parent = Path::new(parent.trim_end());
        assert_eq!(
            parent.parent(),
            Some(std::env::temp_dir().as_path()),
            "{parent:?}"
        );
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
        let temp = Path::new(seen.trim_end());
        assert!(!temp.exists());
        assert!(
            !temp.parent().unwrap().exists(),
            "{seen}: directory left behind"
        );
    }

    /// Runs edit-hunk with a scripted editor that writes the file's path to
    /// `seen`; returns the result and that path's directory.
    #[cfg(unix)]
    fn edit_hunk_with(name: &str, body: &str) -> (AppIntent, PathBuf) {
        let tmp = crate::git::fake::TempDir::new(name);
        let dir = tmp.path();
        let editor = editor_script(dir, &format!("echo \"$1\" > seen\n{body}"));
        let runner = FakeRunner::new().expect(&["var", "GIT_EDITOR"], ok(editor.as_bytes()));
        let job = HandOff::edit_hunk(b"diff --git a/a b/a\n@@ -1 +1 @@\n-a\n+b\n");
        let intent = execute(job, &runner, dir);
        let seen = fs::read_to_string(dir.join("seen")).unwrap();
        let parent = Path::new(seen.trim_end()).parent().unwrap().to_path_buf();
        (intent, parent)
    }

    #[cfg(unix)]
    #[test]
    fn a_cancelled_edit_is_none_and_removes_the_directory() {
        let (intent, parent) = edit_hunk_with("handoff-cancel", ": > \"$1\"");
        assert_eq!(intent, AppIntent::HunkEdited { result: Ok(None) });
        assert!(!parent.exists(), "{parent:?} was left behind");
    }

    #[cfg(unix)]
    #[test]
    fn files_the_editor_leaves_beside_the_hunk_are_removed_too() {
        let (_, parent) = edit_hunk_with(
            "handoff-swap",
            "touch \"$1.swp\" \"$1~\"\nmkdir \"$(dirname \"$1\")/backup\"",
        );
        assert!(!parent.exists(), "{parent:?} was left behind");
    }

    #[test]
    fn a_panic_while_the_file_is_alive_still_removes_the_directory() {
        let (temp, _file) = TempFile::create("kika-test-panic", "txt").unwrap();
        let dir = temp.dir.clone();
        assert!(dir.is_dir());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _temp = temp;
            panic!("editor hand-off blew up");
        }));
        assert!(result.is_err());
        assert!(!dir.exists(), "{dir:?} was left behind");
    }

    #[test]
    fn the_file_sits_alone_in_a_new_directory_under_the_temp_dir() {
        let (temp, _file) = TempFile::create("kika-test-layout", "diff").unwrap();
        assert_eq!(temp.path.parent(), Some(temp.dir.as_path()));
        assert_eq!(temp.dir.parent(), Some(std::env::temp_dir().as_path()));
        let name = temp
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let dir_name = temp.dir.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name, format!("{dir_name}.diff"));
        assert!(name.starts_with("kika-test-layout-"), "{name}");
    }

    #[cfg(unix)]
    #[test]
    fn an_entry_already_at_the_directory_name_is_skipped_and_left_alone() {
        let tmp = crate::git::fake::TempDir::new("handoff-planted");
        let target = tmp.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("planted"), "x").unwrap();
        let prefix = "kika-test-planted";
        let pid = std::process::id();
        let planted = std::env::temp_dir().join(format!("{prefix}-{pid}-0"));
        let _ = fs::remove_file(&planted);
        std::os::unix::fs::symlink(&target, &planted).unwrap();
        // A counter of its own, so other tests cannot move `n` on.
        let next = AtomicU64::new(0);

        let created = TempFile::create_with(&next, prefix, "diff");

        let link = fs::read_link(&planted);
        let _ = fs::remove_file(&planted);
        let (temp, _file) = created.unwrap();
        assert_eq!(
            temp.dir,
            std::env::temp_dir().join(format!("{prefix}-{pid}-1"))
        );
        assert_eq!(link.unwrap(), target);
        let entries: Vec<_> = fs::read_dir(&target)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("planted")]);
    }

    #[test]
    fn creating_gives_up_after_a_bounded_number_of_taken_names() {
        let prefix = "kika-test-exhausted";
        let pid = std::process::id();
        let taken: Vec<PathBuf> = (0..TEMP_ATTEMPTS)
            .map(|n| std::env::temp_dir().join(format!("{prefix}-{pid}-{n}")))
            .collect();
        for dir in &taken {
            let _ = fs::create_dir(dir);
        }
        let next = AtomicU64::new(0);

        let created = TempFile::create_with(&next, prefix, "diff");

        for dir in &taken {
            let _ = fs::remove_dir(dir);
        }
        let error = created.err().expect("every name was taken");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
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

    #[cfg(unix)]
    #[test]
    fn a_command_runs_in_the_workdir_with_each_value_one_argument() {
        let tmp = crate::git::fake::TempDir::new("handoff-command");
        let dir = tmp.path();
        let argv: Vec<OsString> = ["sh", "-c", "pwd > cwd; printf '%s\\n' \"$@\" > args", "sh"]
            .into_iter()
            .map(OsString::from)
            .chain([OsString::from("a b;$(touch x)"), OsString::from("*")])
            .collect();
        let job = HandOff::RunCommand {
            title: "Show".into(),
            argv,
        };

        let intent = execute(job, &FakeRunner::new(), dir);

        assert_eq!(
            intent,
            AppIntent::CommandFinished {
                title: "Show".into(),
                output: OutputMode::Terminal,
                result: Ok(String::new()),
            }
        );
        let args = fs::read_to_string(dir.join("args")).unwrap();
        assert_eq!(args, "a b;$(touch x)\n*\n");
        assert!(!dir.join("x").exists());
        let cwd = fs::canonicalize(fs::read_to_string(dir.join("cwd")).unwrap().trim_end());
        assert_eq!(cwd.unwrap(), fs::canonicalize(dir).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_or_missing_command_says_why() {
        let tmp = crate::git::fake::TempDir::new("handoff-command-fail");
        let result = |argv: &[&str]| match execute(
            HandOff::RunCommand {
                title: "t".into(),
                argv: argv.iter().map(OsString::from).collect(),
            },
            &FakeRunner::new(),
            tmp.path(),
        ) {
            AppIntent::CommandFinished { result, .. } => result,
            other => panic!("expected CommandFinished, got {other:?}"),
        };
        let Err(CommandFailure::Exited(failed)) = result(&["sh", "-c", "exit 3"]) else {
            panic!("expected an exit failure");
        };
        assert!(
            failed.starts_with("exited with") && failed.contains('3'),
            "{failed}"
        );
        let Err(CommandFailure::Launch(missing)) = result(&["kika-no-such-program\x1b[2J"]) else {
            panic!("expected a launch failure");
        };
        assert!(
            missing.starts_with("Could not start kika-no-such-program"),
            "{missing}"
        );
        assert!(!missing.contains('\x1b'), "{missing}");
    }
}
