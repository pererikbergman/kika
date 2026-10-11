//! Diff renderers the user already reads diffs through, such as a pager
//! configured for `git diff`. Found once at boot, run on one file's plain
//! diff at a time, and never through a shell: a value that needs one is
//! refused. Rendered rows are mapped back to the patch lines they show, so
//! hunk and line staging still act on the parsed patch.

use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::ansi::StyledRow;
use crate::git::diff::{DiffRequest, diff_command, run_diff, sanitize, terminal_safe};
use crate::git::patch::FilePatch;
use crate::git::runner::{GitCommand, GitRunner};

/// One configured renderer: the program and its arguments, as argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRenderer {
    /// The program's basename, for the pane title.
    pub label: String,
    pub argv: Vec<OsString>,
}

/// What `R` says when there is nothing to cycle through.
pub const NO_RENDERER: &str = "No diff renderer configured";

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
    let argv: Vec<OsString> = value.split_ascii_whitespace().map(OsString::from).collect();
    let Some(program) = argv.first() else {
        return Err("The command is empty");
    };
    if program.to_string_lossy().contains('/') && !Path::new(program).is_absolute() {
        return Err("The program must be a bare name or an absolute path");
    }
    Ok(argv)
}

/// The renderers to offer, in cycle order: `KIKA_DIFF_PAGER` (passed in as
/// `env`), then the user's global `pager.diff` and `core.pager`. Only
/// global config is read: a repository's own config can come with a copy
/// from someone else, and renderers run without the user typing a command.
pub fn discover(runner: &dyn GitRunner, env: Option<String>) -> Vec<ExternalRenderer> {
    let mut found: Vec<ExternalRenderer> = Vec::new();
    let mut add = |argv: Vec<OsString>| {
        if !found.iter().any(|r| r.argv == argv) {
            found.push(ExternalRenderer {
                label: basename(&argv[0]),
                argv,
            });
        }
    };
    if let Some(argv) = env.and_then(|value| parse_command(&value).ok()) {
        add(argv);
    }
    for key in ["pager.diff", "core.pager"] {
        let Some(value) = global_config(runner, key) else {
            continue;
        };
        if BOOLEANS.contains(&value.to_ascii_lowercase().as_str()) {
            continue;
        }
        let Ok(argv) = parse_command(&value) else {
            continue;
        };
        if VIEWERS.contains(&basename(&argv[0]).as_str()) {
            continue;
        }
        add(argv);
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

/// Find a bare program name on `PATH` ourselves, so the working directory
/// is never searched: empty and relative `PATH` entries are skipped.
fn resolve(program: &OsStr) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    let search = std::env::var_os("PATH")?;
    std::env::split_paths(&search)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
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

/// Run `renderer` on `input` at `cols` columns, in `workdir`, within the
/// time and output caps.
pub fn run_renderer(
    renderer: &ExternalRenderer,
    workdir: &Path,
    input: &[u8],
    cols: u16,
) -> Result<Vec<u8>, String> {
    run_limited(renderer, workdir, input, cols, TIMEOUT, MAX_BYTES)
}

/// [`run_renderer`] with its caps as parameters, so tests can shorten them.
/// Repo content reaches the renderer on stdin only, never in argv or env.
fn run_limited(
    renderer: &ExternalRenderer,
    workdir: &Path,
    input: &[u8],
    cols: u16,
    timeout: Duration,
    max_output: usize,
) -> Result<Vec<u8>, String> {
    let label = &renderer.label;
    let (program, args) = renderer
        .argv
        .split_first()
        .ok_or_else(|| format!("{label} has no command"))?;
    let program = resolve(program).ok_or_else(|| format!("{label} not found"))?;
    let mut command = Command::new(program);
    command
        .args(args)
        .env("COLUMNS", cols.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // A runner with no directory has nothing to offer; the program is
    // already absolute, so the current one does no harm.
    if !workdir.as_os_str().is_empty() {
        command.current_dir(workdir);
    }
    // Its own process group, so a kill reaches whatever it starts.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command
        .spawn()
        .map_err(|e| terminal_safe(format!("{label}: {e}").as_bytes()))?;

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
    let mut status = None;
    loop {
        if output.is_none() {
            match stdout.try_recv() {
                Ok(Ok(bytes)) => output = Some(bytes),
                Ok(Err(Overflow)) => {
                    kill(&mut child);
                    return Err(format!("Output of {label} is too large"));
                }
                Err(_) => {}
            }
        }
        if status.is_none() {
            status = child.try_wait().map_err(|e| e.to_string())?;
        }
        if let Some(status) = status
            && let Some(bytes) = output.take()
        {
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
            kill(&mut child);
            return Err(format!("{label} timed out"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
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

/// Stop the renderer and everything it started, then reap it.
fn kill(child: &mut Child) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .arg("-KILL")
            .arg("--")
            .arg(format!("-{}", child.id()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
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
            let content = squeeze(&sanitize(text));
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
    use crate::ansi::parse;
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
        }
    }

    #[test]
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
    fn discover_puts_the_env_first_then_pager_diff_then_core_pager() {
        let runner = FakeRunner::new()
            .expect(&PAGER, ok(b"delta --dark\n"))
            .expect(&CORE, ok(b"/opt/bin/diff-so-fancy\n"));
        let found = discover(&runner, Some("riff".into()));
        assert_eq!(labels(&found), ["riff", "delta", "diff-so-fancy"]);
        assert_eq!(found[1].argv, os(&["delta", "--dark"]));
        runner.assert_done();
    }

    #[test]
    fn discover_reads_only_global_config() {
        let runner = FakeRunner::new()
            .expect(&PAGER, fail(""))
            .expect(&CORE, ok(b"delta\n"));
        assert_eq!(labels(&discover(&runner, None)), ["delta"]);
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
            assert!(discover(&runner, None).is_empty(), "{value:?}");
        }
    }

    #[test]
    fn discover_keeps_one_renderer_per_distinct_argv() {
        let runner = FakeRunner::new()
            .expect(&PAGER, ok(b"delta"))
            .expect(&CORE, ok(b"delta"));
        assert_eq!(labels(&discover(&runner, Some("delta".into()))), ["delta"]);
    }

    #[test]
    fn resolve_never_searches_relative_path_entries() {
        let found = resolve(OsStr::new("sh")).expect("sh is on PATH");
        assert!(found.is_absolute());
        assert_eq!(resolve(OsStr::new("/bin/x")), Some(PathBuf::from("/bin/x")));
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
        let result = run_limited(&r, Path::new("/"), b"", 80, Duration::from_millis(200), 100);
        assert_eq!(result, Err("sh timed out".into()));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn too_much_output_is_refused() {
        let r = renderer(&["sh", "-c", "yes"]);
        let result = run_limited(&r, Path::new("/"), b"", 80, Duration::from_secs(10), 1000);
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
    fn read_diff_feeds_the_plain_diff_and_refuses_a_failure() {
        let request = DiffRequest {
            path: "a.rs".into(),
            orig_path: None,
            side: DiffSide::Worktree,
            options: Default::default(),
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
