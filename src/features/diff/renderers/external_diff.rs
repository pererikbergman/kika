//! Diff renderers the user already reads diffs through. Two kinds: a pager
//! (such as one configured for `git diff`) reads one file's plain diff on
//! stdin, and its rendered rows are mapped back to the patch lines they
//! show, so hunk and line staging still act on the parsed patch. A tool
//! (the global config's `[diff] tool`) is given the old and new contents
//! as two temp files instead; its rows have no line identity, so the pane
//! is display-only while it is active. Both are found once at boot, run
//! with the same caps, and never through a shell: a value that needs one
//! is refused.

use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::ansi::StyledRow;
use crate::git::diff::{
    DiffRequest, EmojiZwj, diff_command, run_diff, sanitize, sanitize_body, terminal_safe,
};
use crate::git::patch::FilePatch;
use crate::git::runner::{GitCommand, GitRunner};

/// One configured renderer: the program and its arguments, as argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRenderer {
    /// The program's basename, for the pane title.
    pub label: String,
    pub argv: Vec<OsString>,
    pub kind: RendererKind,
}

/// How a renderer is fed, and so whether its rows can be staged from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererKind {
    /// Reads the plain diff on stdin; rows map back to patch lines.
    Pager,
    /// Run on `-- OLD NEW` temp files; rows are for reading only.
    Tool,
}

/// What `R` says when there is nothing to cycle through, and where
/// renderers are found.
pub const NO_RENDERER: &str =
    "No renderer: set [diff] pager or tool in the global config, or git core.pager";

/// The most diff fed to a renderer, and the most output taken back.
pub const MAX_BYTES: usize = 16 * 1024 * 1024;
/// How long a renderer may run before it is killed.
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// How much of a failing renderer's stderr is kept for its message.
const MAX_STDERR: usize = 64 * 1024;

/// Bytes that only mean something to a shell. No shell is ever used, so a
/// value containing one is refused rather than run differently than meant.
const SHELL_BYTES: &[char] = &[
    '\'', '"', '\\', '$', '`', '|', ';', '&', '<', '>', '(', ')', '{', '}', '*', '?', '~', '!', '#',
];

/// Git config values that are switches, not programs.
const BOOLEANS: &[&str] = &["true", "false", "yes", "no", "on", "off", "1", "0"];

/// Programs that only show text, so they render nothing.
const VIEWERS: &[&str] = &["less", "more", "most", "cat"];

/// Split a configured command into argv on ASCII whitespace. Refused when
/// empty, when it holds a control character or shell syntax, or when the
/// program is a relative path: the renderer runs in the repository, so a
/// planted `./delta` must never be what runs.
pub fn parse_command(value: &str) -> Result<Vec<OsString>, &'static str> {
    if value.contains(|c: char| c.is_control()) {
        return Err("The command contains a control character");
    }
    if value.contains(SHELL_BYTES) {
        return Err("The command needs a shell, which is never used");
    }
    check_argv(value.split_ascii_whitespace().map(String::from).collect())
}

/// A configured argv, already split, as [`parse_command`] checks one:
/// not empty, no control character, and the program a bare name or an
/// absolute path. No shell ever sees it, so shell syntax is only text.
pub fn check_argv(argv: Vec<String>) -> Result<Vec<OsString>, &'static str> {
    if argv
        .iter()
        .any(|arg| arg.contains(|c: char| c.is_control()))
    {
        return Err("The command contains a control character");
    }
    let Some(program) = argv.first() else {
        return Err("The command is empty");
    };
    if program.is_empty() {
        return Err("The command is empty");
    }
    if program.contains('/') && !Path::new(program).is_absolute() {
        return Err("The program must be a bare name or an absolute path");
    }
    Ok(argv.into_iter().map(OsString::from).collect())
}

/// The renderers to offer, in cycle order: `configured` (the global Kika
/// config's `[diff] pager`), then the user's global git `pager.diff` and
/// `core.pager`, then `tool` (the global Kika config's `[diff] tool`). Only
/// global config is read: a repository's own config can come with a copy
/// from someone else, and renderers run without the user typing a command.
/// Switches and plain viewers are skipped from any pager source. The tool
/// comes from Kika's file alone: git's own external diff settings run
/// through a shell, so they are never looked up.
pub fn discover(
    runner: &dyn GitRunner,
    configured: Option<String>,
    tool: Option<String>,
) -> Vec<ExternalRenderer> {
    let git = ["pager.diff", "core.pager"]
        .into_iter()
        .map(|key| global_config(runner, key));
    let mut found: Vec<ExternalRenderer> = Vec::new();
    for value in std::iter::once(configured).chain(git).flatten() {
        if BOOLEANS.contains(&value.to_ascii_lowercase().as_str()) {
            continue;
        }
        let Ok(argv) = parse_command(&value) else {
            continue;
        };
        let label = basename(&argv[0]);
        if VIEWERS.contains(&label.as_str()) || found.iter().any(|r| r.argv == argv) {
            continue;
        }
        found.push(ExternalRenderer {
            label,
            argv,
            kind: RendererKind::Pager,
        });
    }
    if let Some(argv) = tool.as_deref().and_then(|t| parse_command(t).ok()) {
        found.push(ExternalRenderer {
            label: basename(&argv[0]),
            argv,
            kind: RendererKind::Tool,
        });
    }
    found
}

/// `git config --global --get <key>`; unset when it fails or is empty.
fn global_config(runner: &dyn GitRunner, key: &str) -> Option<String> {
    let cmd = GitCommand::new("config")
        .arg("--global")
        .arg("--get")
        .arg(key);
    let output = runner.run(&cmd).ok().filter(|o| o.success)?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn basename(program: &OsStr) -> String {
    let name = Path::new(program).file_name().unwrap_or(program);
    sanitize(name.to_string_lossy().as_bytes())
}

/// The plain diff of `request`, the renderer's only input. Refused when
/// larger than [`MAX_BYTES`].
pub fn read_diff(
    runner: &dyn GitRunner,
    request: &DiffRequest,
    label: &str,
) -> Result<Vec<u8>, String> {
    let diff = run_diff(runner, &diff_command(request), request.side)?;
    if diff.len() > MAX_BYTES {
        return Err(format!("Diff too large for {label}"));
    }
    Ok(diff)
}

/// How long a renderer may run and how much it may print.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub timeout: Duration,
    pub max_output: usize,
}

/// The caps every renderer runs under.
pub const LIMITS: Limits = Limits {
    timeout: TIMEOUT,
    max_output: MAX_BYTES,
};

/// Run `renderer` on `input` at `cols` columns, in `workdir`, within the
/// time and output caps.
pub fn run_renderer(
    renderer: &ExternalRenderer,
    workdir: &Path,
    input: &[u8],
    cols: u16,
) -> Result<Vec<u8>, String> {
    run(renderer, workdir, &[], input, cols, LIMITS)
}

/// The renderer's process, not yet started: its argv, then `extra_args`,
/// with the program found by [`crate::program::resolve`] and the pane
/// width in `COLUMNS` and `DFT_WIDTH` (a tool that ignores `COLUMNS` when
/// piped reads that).
/// No shell is involved.
fn command(
    renderer: &ExternalRenderer,
    extra_args: &[OsString],
    cols: u16,
) -> Result<Command, String> {
    let label = &renderer.label;
    let (program, args) = renderer
        .argv
        .split_first()
        .ok_or_else(|| format!("{label} has no command"))?;
    let mut command = crate::program::command(program).map_err(|_| format!("{label} not found"))?;
    command
        .args(args)
        .args(extra_args)
        .env("COLUMNS", cols.to_string())
        .env("DFT_WIDTH", cols.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own process group, so a kill reaches whatever it starts.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    Ok(command)
}

/// Run `renderer` with `extra_args` after its argv and `input` on stdin,
/// within `limits`. A pager gets repo content on stdin only; a tool gets
/// the names of Kika's own temp files, never a repo file name.
pub fn run(
    renderer: &ExternalRenderer,
    workdir: &Path,
    extra_args: &[OsString],
    input: &[u8],
    cols: u16,
    limits: Limits,
) -> Result<Vec<u8>, String> {
    let Limits {
        timeout,
        max_output,
    } = limits;
    let label = &renderer.label;
    let mut command = command(renderer, extra_args, cols)?;
    // A runner with no directory has nothing to offer; the program is
    // already absolute, so the current one does no harm.
    if !workdir.as_os_str().is_empty() {
        command.current_dir(workdir);
    }
    // An OS error, named after the renderer and made safe to show.
    let os_error = |e: std::io::Error| terminal_safe(format!("{label}: {e}").as_bytes());
    let mut child = command.spawn().map_err(os_error)?;

    let stdin = child.stdin.take();
    let input = input.to_vec();
    std::thread::spawn(move || {
        // Dropping the pipe sends EOF. A write error means the renderer
        // stopped reading; its exit says why.
        if let Some(mut pipe) = stdin {
            let _ = pipe.write_all(&input);
        }
    });
    let stdout = drain(child.stdout.take(), max_output);
    let stderr = drain(child.stderr.take(), MAX_STDERR);

    let deadline = Instant::now() + timeout;
    let mut output = None;
    let mut exited = false;
    loop {
        if output.is_none() {
            match stdout.try_recv() {
                Ok(Ok(bytes)) => output = Some(bytes),
                Ok(Err(Overflow)) => {
                    kill_group(&mut child);
                    return Err(format!("Output of {label} is too large"));
                }
                Err(_) => {}
            }
        }
        if !exited {
            exited = match has_exited(&mut child) {
                Ok(exited) => exited,
                Err(e) => {
                    kill_group(&mut child);
                    return Err(os_error(e));
                }
            };
        }
        if exited && let Some(bytes) = output.take() {
            // Reaped only now: until the output is in, a timeout or an
            // overflow may still need the renderer's group id reserved.
            let status = child.wait().map_err(os_error)?;
            if status.success() {
                return Ok(bytes);
            }
            let stderr = stderr
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default();
            let message = terminal_safe(&stderr);
            return Err(if message.is_empty() {
                format!("{label} failed")
            } else {
                message
            });
        }
        if Instant::now() >= deadline {
            kill_group(&mut child);
            return Err(format!("{label} timed out"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Whether the renderer has exited. On unix it is left unreaped, so its
/// pid, which is also its group id, stays reserved for [`kill_group`]; `run`
/// reaps it with `wait` once its output is in.
#[cfg(unix)]
fn has_exited(child: &mut Child) -> std::io::Result<bool> {
    let pid = libc::id_t::try_from(child.id()).map_err(std::io::Error::other)?;
    loop {
        // SAFETY: an all-zero `siginfo_t` is a valid value; `waitid` only
        // writes into it.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a valid, writable `siginfo_t`. WNOWAIT leaves the
        // child waitable, so `Child::wait` still reaps it later.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == -1 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        // With WNOHANG and no exit yet, `info` is left zeroed.
        // SAFETY: `waitid` succeeded, so `info` holds a child-state record.
        return Ok(unsafe { info.si_pid() } != 0);
    }
}

/// Whether the renderer has exited. Nothing signals a group off unix, so
/// reaping it here is safe.
#[cfg(not(unix))]
fn has_exited(child: &mut Child) -> std::io::Result<bool> {
    Ok(child.try_wait()?.is_some())
}

/// More output than the cap allows.
struct Overflow;

/// Read `pipe` to the end on its own thread, up to `cap` bytes. The result
/// arrives on the channel; past the cap, reading stops.
fn drain(
    pipe: Option<impl Read + Send + 'static>,
    cap: usize,
) -> mpsc::Receiver<Result<Vec<u8>, Overflow>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(pipe) = pipe {
            let limit = u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1);
            if pipe.take(limit).read_to_end(&mut bytes).is_err() {
                bytes.clear();
            }
        }
        let _ = tx.send(if bytes.len() > cap {
            Err(Overflow)
        } else {
            Ok(bytes)
        });
    });
    rx
}

/// Stop a child started in its own process group and everything it
/// started, then reap it. `child.kill` alone would leave the child's own
/// children holding the pipes. Renderers and the clipboard program use it.
pub(crate) fn kill_group(child: &mut Child) {
    // A direct signal, not a `kill` program, so nothing is looked up on
    // `PATH`. Callers must not have reaped the child (`run` polls with
    // WNOWAIT): the child is unreaped until `wait` below, so its pid, which
    // is also its group id, stays reserved for it.
    #[cfg(unix)]
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: `killpg` only sends a signal; `pid` leads the group the
        // child was started in. ESRCH (group already gone) is ignored.
        let _ = unsafe { libc::killpg(pid, libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// For each rendered row, the patch line it shows as `(hunk, index in the
/// hunk's lines)`, or `None` for decoration. A greedy forward match: each
/// patch line is looked for in the next few rows after the last match, by
/// whether the row's text ends with the line's, whitespace ignored. A line
/// not found is left unmapped and the search does not move.
pub fn map_rows(patch: &FilePatch, rows: &[StyledRow]) -> Vec<Option<(usize, usize)>> {
    /// How many rows past the last match a line is looked for in.
    const WINDOW: usize = 8;
    let plain: Vec<String> = rows.iter().map(|r| squeeze(&r.text())).collect();
    let mut map = vec![None; rows.len()];
    let mut next = 0;
    for (h, hunk) in patch.hunks.iter().enumerate() {
        for (i, line) in hunk.lines.iter().enumerate() {
            let text = line.text.strip_suffix(b"\n").unwrap_or(&line.text);
            let text = text.strip_suffix(b"\r").unwrap_or(text);
            // The same sanitiser `ansi` used on the rows, or the texts differ.
            let content = squeeze(&sanitize_body(text, EmojiZwj::Escape));
            let found = if content.is_empty() {
                // Only the next row, and only if nothing but a gutter is on it.
                plain
                    .get(next)
                    .filter(|row| row.chars().last().is_none_or(|c| !c.is_alphanumeric()))
                    .map(|_| next)
            } else {
                (next..rows.len().min(next + WINDOW)).find(|&r| plain[r].ends_with(&content))
            };
            if let Some(r) = found {
                map[r] = Some((h, i));
                next = r + 1;
            }
        }
    }
    map
}

/// `text` without any whitespace.
fn squeeze(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::diff::renderers::ansi::parse;
    use crate::git::diff::DiffSide;
    use crate::git::fake::{FakeRunner, fail, ok};
    use crate::git::patch;

    fn os(argv: &[&str]) -> Vec<OsString> {
        argv.iter().map(OsString::from).collect()
    }

    fn renderer(argv: &[&str]) -> ExternalRenderer {
        ExternalRenderer {
            label: basename(OsStr::new(argv[0])),
            argv: os(argv),
            kind: RendererKind::Pager,
        }
    }

    #[test]
    #[cfg_attr(windows, ignore = "/usr/bin/delta is not an absolute path on Windows")]
    fn parse_command_splits_plain_commands() {
        assert_eq!(parse_command("delta --dark"), Ok(os(&["delta", "--dark"])));
        assert_eq!(parse_command("/usr/bin/delta"), Ok(os(&["/usr/bin/delta"])));
        assert_eq!(
            parse_command("  delta   --width=80 "),
            Ok(os(&["delta", "--width=80"]))
        );
    }

    #[test]
    fn parse_command_refuses_anything_a_shell_would_read() {
        for value in [
            "delta | less",
            "$(x)",
            "\"a b\"",
            "./delta",
            "bin/delta",
            "a\nb",
            "a\tb",
            "",
            "   ",
            "delta; rm",
            "delta `x`",
            "~/delta",
        ] {
            assert!(parse_command(value).is_err(), "{value:?} accepted");
        }
    }

    const PAGER: [&str; 4] = ["config", "--global", "--get", "pager.diff"];
    const CORE: [&str; 4] = ["config", "--global", "--get", "core.pager"];

    fn labels(found: &[ExternalRenderer]) -> Vec<&str> {
        found.iter().map(|r| r.label.as_str()).collect()
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "/opt/bin/diff-so-fancy is not an absolute path on Windows"
    )]
    fn discover_puts_the_kika_pager_first_then_pager_diff_then_core_pager() {
        let runner = FakeRunner::new()
            .expect(&PAGER, ok(b"delta --dark\n"))
            .expect(&CORE, ok(b"/opt/bin/diff-so-fancy\n"));
        let found = discover(&runner, Some("riff".into()), None);
        assert_eq!(labels(&found), ["riff", "delta", "diff-so-fancy"]);
        assert_eq!(found[1].argv, os(&["delta", "--dark"]));
        runner.assert_done();
    }

    #[test]
    fn discover_reads_only_global_config() {
        let runner = FakeRunner::new()
            .expect(&PAGER, fail(""))
            .expect(&CORE, ok(b"delta\n"));
        assert_eq!(labels(&discover(&runner, None, None)), ["delta"]);
        // `expect` checked each argv in full, `--global` included.
        runner.assert_done();
    }

    #[test]
    fn discover_skips_viewers_switches_empty_and_refused_values() {
        for value in [
            &b"less -R\n"[..],
            b"true",
            b"OFF",
            b"",
            b"/usr/bin/more",
            b"cat",
            b"delta | less",
        ] {
            let runner = FakeRunner::new()
                .expect(&PAGER, ok(value))
                .expect(&CORE, ok(b""));
            assert!(discover(&runner, None, None).is_empty(), "{value:?}");
            // The same rules hold for Kika's own `[diff] pager`.
            let runner = FakeRunner::new()
                .expect(&PAGER, ok(b""))
                .expect(&CORE, ok(b""));
            let configured = String::from_utf8(value.to_vec()).unwrap();
            assert!(
                discover(&runner, Some(configured), None).is_empty(),
                "{value:?}"
            );
        }
    }

    #[test]
    fn discover_offers_the_tool_after_the_pagers_without_asking_git_about_it() {
        let runner = FakeRunner::new()
            .expect(&PAGER, ok(b"delta\n"))
            .expect(&CORE, ok(b""));
        let found = discover(
            &runner,
            Some("riff".into()),
            Some("difft --color=always".into()),
        );
        assert_eq!(labels(&found), ["riff", "delta", "difft"]);
        let kinds: Vec<RendererKind> = found.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            [RendererKind::Pager, RendererKind::Pager, RendererKind::Tool]
        );
        assert_eq!(found[2].argv, os(&["difft", "--color=always"]));
        // Only the two pager lookups: no other git config is read.
        runner.assert_done();
    }

    #[test]
    fn a_tool_is_offered_even_with_a_viewer_name_but_never_when_refused() {
        let empty = || {
            FakeRunner::new()
                .expect(&PAGER, ok(b""))
                .expect(&CORE, ok(b""))
        };
        // A tool is what the user named for it; the pager filters do not apply.
        let found = discover(&empty(), None, Some("cat".into()));
        assert_eq!(labels(&found), ["cat"]);
        assert_eq!(found[0].kind, RendererKind::Tool);
        assert!(discover(&empty(), None, Some("difft | less".into())).is_empty());
    }

    #[test]
    #[cfg_attr(windows, ignore = "sh is a Unix name")]
    fn the_tool_command_is_argv_only_with_the_pane_width() {
        let tool = ExternalRenderer {
            label: "sh".into(),
            argv: os(&["sh", "--flag"]),
            kind: RendererKind::Tool,
        };
        let extra = os(&["--", "/tmp/k/old.rs", "/tmp/k/new.rs"]);
        let command = command(&tool, &extra, 61).unwrap();
        assert_eq!(
            Some(command.get_program()),
            crate::program::resolve(OsStr::new("sh"))
                .as_deref()
                .ok()
                .map(Path::as_os_str)
        );
        let args: Vec<&OsStr> = command.get_args().collect();
        assert_eq!(args, ["--flag", "--", "/tmp/k/old.rs", "/tmp/k/new.rs"]);
        let envs: Vec<(&OsStr, Option<&OsStr>)> = command.get_envs().collect();
        assert!(envs.contains(&(OsStr::new("COLUMNS"), Some(OsStr::new("61")))));
        assert!(envs.contains(&(OsStr::new("DFT_WIDTH"), Some(OsStr::new("61")))));
    }

    #[test]
    fn discover_keeps_one_renderer_per_distinct_argv() {
        let runner = FakeRunner::new()
            .expect(&PAGER, ok(b"delta"))
            .expect(&CORE, ok(b"delta"));
        assert_eq!(
            labels(&discover(&runner, Some("delta".into()), None)),
            ["delta"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_renderer_gets_the_pane_width_in_columns() {
        let r = renderer(&["sh", "-c", "printf %s \"$COLUMNS\""]);
        let out = run_renderer(&r, Path::new("/"), b"", 57).unwrap();
        assert_eq!(out, b"57");
    }

    #[cfg(unix)]
    #[test]
    fn the_renderer_reads_the_diff_on_stdin() {
        let input = vec![b'x'; 300_000];
        let out = run_renderer(&renderer(&["cat"]), Path::new("/"), &input, 80).unwrap();
        assert_eq!(out, input);
    }

    #[cfg(unix)]
    #[test]
    fn a_renderer_past_its_time_is_killed_and_reported() {
        let r = renderer(&["sh", "-c", "sleep 30"]);
        let start = Instant::now();
        let limits = Limits {
            timeout: Duration::from_millis(200),
            max_output: 100,
        };
        let result = run(&r, Path::new("/"), &[], b"", 80, limits);
        assert_eq!(result, Err("sh timed out".into()));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn a_timed_out_renderer_takes_what_it_started_with_it() {
        let tmp = crate::git::fake::TempDir::new("external-diff-kill");
        let pidfile = tmp.path().join("pid");
        let script = format!("sleep 30 & echo $! >'{}'; wait", pidfile.display());
        let r = renderer(&["sh", "-c", &script]);
        let limits = Limits {
            timeout: Duration::from_millis(500),
            max_output: 100,
        };
        assert_eq!(
            run(&r, Path::new("/"), &[], b"", 80, limits),
            Err("sh timed out".into())
        );

        assert_gone_soon(read_pid(&pidfile));
    }

    #[cfg(unix)]
    #[test]
    fn a_renderer_that_exited_still_takes_what_it_left_holding_stdout() {
        let tmp = crate::git::fake::TempDir::new("external-diff-exited");
        let pidfile = tmp.path().join("pid");
        let script = format!("sleep 30 & echo $! >'{}'; exit 0", pidfile.display());
        let r = renderer(&["/bin/sh", "-c", &script]);
        let limits = Limits {
            timeout: Duration::from_millis(500),
            max_output: 100,
        };
        assert_eq!(
            run(&r, Path::new("/"), &[], b"", 80, limits),
            Err("sh timed out".into())
        );
        assert_gone_soon(read_pid(&pidfile));
    }

    #[cfg(unix)]
    #[test]
    fn an_exited_renderer_stays_unreaped_until_its_group_is_killed() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 0"]);
        // Its own group, so the group kill below cannot reach the tests.
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child = command.spawn().unwrap();
        let pid = libc::pid_t::try_from(child.id()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !has_exited(&mut child).unwrap() {
            assert!(Instant::now() < deadline, "child {pid} never exited");
            std::thread::sleep(Duration::from_millis(5));
        }
        // A second poll still sees it: polling does not reap.
        assert!(has_exited(&mut child).unwrap());
        // SAFETY: signal 0 only checks whether the process exists.
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            0,
            "the exited child is an unreaped zombie, so its pid is reserved"
        );
        kill_group(&mut child);
        assert_gone_soon(pid);
    }

    #[cfg(unix)]
    fn read_pid(file: &Path) -> libc::pid_t {
        std::fs::read_to_string(file)
            .expect("the renderer wrote its child's pid")
            .trim()
            .parse()
            .unwrap()
    }

    /// Wait until no process `pid` exists, or fail the test after a while.
    #[cfg(unix)]
    fn assert_gone_soon(pid: libc::pid_t) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // SAFETY: signal 0 only checks whether the process exists.
            let gone = unsafe { libc::kill(pid, 0) } == -1
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            if gone {
                break;
            }
            assert!(Instant::now() < deadline, "process {pid} still alive");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[cfg(unix)]
    #[test]
    fn too_much_output_is_refused() {
        let r = renderer(&["sh", "-c", "yes"]);
        let limits = Limits {
            timeout: Duration::from_secs(10),
            max_output: 1000,
        };
        let result = run(&r, Path::new("/"), &[], b"", 80, limits);
        assert_eq!(result, Err("Output of sh is too large".into()));
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_renderer_reports_its_stderr_made_safe() {
        let r = renderer(&["sh", "-c", "printf 'bad \\033[2J news' >&2; exit 3"]);
        let message = run_renderer(&r, Path::new("/"), b"", 80).unwrap_err();
        assert_eq!(message, "bad \u{FFFD}[2J news");
        let quiet = renderer(&["sh", "-c", "exit 1"]);
        assert_eq!(
            run_renderer(&quiet, Path::new("/"), b"", 80),
            Err("sh failed".into())
        );
    }

    #[test]
    fn a_missing_program_says_so() {
        let r = renderer(&["kika-no-such-renderer"]);
        assert_eq!(
            run_renderer(&r, Path::new("/"), b"", 80),
            Err("kika-no-such-renderer not found".into())
        );
    }

    /// One hunk: `-a +A`, context `m`, an empty context line, `-b +B`.
    const SPLIT: &[u8] = b"diff --git a/x b/x\n--- a/x\n+++ b/x\n\
        @@ -1,4 +1,4 @@\n-a\n+A\n m\n \n-b\n+B\n";

    fn rows(text: &str) -> Vec<StyledRow> {
        parse(text.as_bytes())
    }

    fn map(text: &str) -> Vec<Option<(usize, usize)>> {
        map_rows(&patch::parse(SPLIT).unwrap(), &rows(text))
    }

    #[test]
    fn identical_text_maps_every_patch_line() {
        let text = String::from_utf8(SPLIT.to_vec()).unwrap();
        let expected: Vec<_> = [None, None, None, None]
            .into_iter()
            .chain((0..6).map(|i| Some((0, i))))
            .collect();
        assert_eq!(map(&text), expected);
    }

    #[test]
    fn a_line_number_gutter_and_decoration_rows_map_around_it() {
        let map = map("┌──────┐\n│ x    │\n└──────┘\n\
             1 ⋮   │a\n  ⋮ 1 │A\n2 ⋮ 2 │m\n3 ⋮ 3 │\n4 ⋮   │b\n  ⋮ 4 │B\n");
        assert_eq!(
            map,
            [
                None,
                None,
                None,
                Some((0, 0)),
                Some((0, 1)),
                Some((0, 2)),
                Some((0, 3)),
                Some((0, 4)),
                Some((0, 5)),
            ]
        );
    }

    #[test]
    fn a_missing_row_leaves_only_its_line_unmapped() {
        let map = map("-a\n+A\n m\n \n+B\n");
        assert_eq!(
            map,
            [
                Some((0, 0)),
                Some((0, 1)),
                Some((0, 2)),
                Some((0, 3)),
                Some((0, 5))
            ]
        );
    }

    #[test]
    fn an_empty_line_only_takes_a_blank_or_gutter_row() {
        // The row after `m` has text, so the empty line stays unmapped.
        let map = map("a\nA\nm\nword\nb\nB\n");
        assert_eq!(
            map,
            [
                Some((0, 0)),
                Some((0, 1)),
                Some((0, 2)),
                None,
                Some((0, 4)),
                Some((0, 5))
            ]
        );
    }

    #[test]
    fn a_side_by_side_row_maps_only_one_of_its_lines() {
        let map = map("a │ A\nm │ m\n  │\nb │ B\n");
        assert_eq!(
            map,
            [Some((0, 1)), Some((0, 2)), Some((0, 3)), Some((0, 5))]
        );
    }

    #[test]
    fn a_line_with_a_joined_word_maps_to_its_rendered_row() {
        let patch = patch::parse(
            "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -0,0 +1 @@\n+می\u{200C}خواهم\n".as_bytes(),
        )
        .unwrap();
        let map = map_rows(&patch, &rows("decoration\n  ⋮ 1 │می\u{200C}خواهم\n"));
        assert_eq!(map, [None, Some((0, 0))]);
    }

    #[test]
    fn read_diff_feeds_the_plain_diff_and_refuses_a_failure() {
        let request = DiffRequest {
            path: "a.rs".into(),
            orig_path: None,
            side: DiffSide::Worktree,
            options: Default::default(),
            commit: None,
        };
        let argv = ["diff", "--no-color", "--no-ext-diff", "-U3", "--", "a.rs"];
        let runner = FakeRunner::new().expect(&argv, ok(b"diff\n"));
        assert_eq!(
            read_diff(&runner, &request, "delta"),
            Ok(b"diff\n".to_vec())
        );
        let runner = FakeRunner::new().expect(&argv, fail("bad \x1b[2J"));
        assert_eq!(
            read_diff(&runner, &request, "delta"),
            Err("bad \u{FFFD}[2J".into())
        );
    }
}
