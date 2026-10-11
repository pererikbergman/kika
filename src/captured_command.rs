//! A custom command run in the background with its output read back, for
//! the `popup` and `none` output modes. Kika keeps the terminal; the
//! program gets no input and a bounded share of memory.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::time::Instant;

use crate::git::diff::terminal_safe;
use crate::handoff::start_error;

/// The most output kept. A program that prints more is stopped, so a
/// runaway one cannot fill memory.
pub const MAX_OUTPUT: usize = 256 * 1024;

/// Added after output cut at [`MAX_OUTPUT`].
pub const TRUNCATED: &str = "[output truncated]";

/// How long to keep reading after the program exits. A process it left
/// in the background may hold the pipes open for good.
const DRAIN_AFTER_EXIT: Duration = Duration::from_millis(500);

/// Run `argv` directly in `workdir`, stdin closed, and return what it
/// printed on stdout and stderr together, made terminal-safe. A failure to
/// start, or a non-zero exit with the output after the status, is `Err`.
/// Dropping the future kills the program, so quitting never waits on it.
pub async fn run_captured(argv: Vec<OsString>, workdir: PathBuf) -> Result<String, String> {
    let Some((program, args)) = argv.split_first() else {
        return Err("The command is empty".into());
    };
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .current_dir(&workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| start_error(program, &e))?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let (mut out_buf, mut err_buf) = (vec![0; 8192], vec![0; 8192]);
    let mut output = Vec::new();
    let mut status = None;
    let mut deadline = None;
    let mut truncated = false;
    while stdout.is_some() || stderr.is_some() {
        let read = tokio::select! {
            read = read_some(&mut stdout, &mut out_buf) => read.map(|n| (n, true)),
            read = read_some(&mut stderr, &mut err_buf) => read.map(|n| (n, false)),
            exited = child.wait(), if status.is_none() => {
                status = Some(exited);
                deadline = Some(Instant::now() + DRAIN_AFTER_EXIT);
                continue;
            }
            () = sleep_until(deadline) => break,
        };
        let Some((n, from_stdout)) = read else {
            continue;
        };
        let buf = if from_stdout { &out_buf } else { &err_buf };
        let room = MAX_OUTPUT - output.len();
        output.extend_from_slice(&buf[..n.min(room)]);
        if n > room {
            truncated = true;
            break;
        }
    }
    let mut text = terminal_safe(&output);
    if truncated {
        // Stopped by Kika, not failed: show what there is.
        let _ = child.start_kill();
        let _ = child.wait().await;
        text.push_str(if text.is_empty() { "" } else { "\n" });
        text.push_str(TRUNCATED);
        return Ok(text);
    }
    let status = match status {
        Some(status) => status,
        None => child.wait().await,
    }
    .map_err(|e| terminal_safe(format!("Could not wait for the command: {e}").as_bytes()))?;
    if status.success() {
        Ok(text)
    } else if text.is_empty() {
        Err(terminal_safe(format!("exited with {status}").as_bytes()))
    } else {
        Err(terminal_safe(
            format!("exited with {status}\n{text}").as_bytes(),
        ))
    }
}

/// Read once from `pipe`, closing it on end or error. `None` means nothing
/// was read this time. A closed pipe never answers.
async fn read_some<R: AsyncRead + Unpin>(pipe: &mut Option<R>, buf: &mut [u8]) -> Option<usize> {
    let Some(reader) = pipe else {
        return std::future::pending().await;
    };
    match reader.read(buf).await {
        Ok(0) | Err(_) => {
            *pipe = None;
            None
        }
        Ok(n) => Some(n),
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::git::fake::TempDir;

    fn argv(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    #[tokio::test]
    async fn output_from_both_streams_comes_back_terminal_safe() {
        let tmp = TempDir::new("captured");
        let script = "printf 'out\\033[2J\\n'; printf 'err\\n' >&2; pwd";
        let text = run_captured(argv(&["sh", "-c", script]), tmp.path().into())
            .await
            .unwrap();
        assert!(text.contains("out\u{FFFD}[2J"), "{text:?}");
        assert!(text.contains("err"), "{text:?}");
        assert!(!text.contains('\x1b'), "{text:?}");
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        // The streams are read as they come, so the order between them is not fixed.
        let in_workdir = text
            .lines()
            .any(|line| std::fs::canonicalize(line).is_ok_and(|p| p == dir));
        assert!(in_workdir, "{text:?}");
    }

    #[tokio::test]
    async fn stdin_is_closed() {
        let tmp = TempDir::new("captured-stdin");
        let text = run_captured(argv(&["sh", "-c", "cat; echo done"]), tmp.path().into())
            .await
            .unwrap();
        assert_eq!(text, "done");
    }

    #[tokio::test]
    async fn endless_output_is_cut_and_the_program_stopped() {
        let tmp = TempDir::new("captured-endless");
        let text = run_captured(
            argv(&["sh", "-c", "while :; do echo y; done"]),
            tmp.path().into(),
        )
        .await
        .unwrap();
        assert!(text.ends_with(TRUNCATED), "{}", &text[text.len() - 40..]);
        assert!(text.len() <= MAX_OUTPUT + TRUNCATED.len() + 1);
    }

    #[tokio::test]
    async fn a_program_left_in_the_background_does_not_hold_up_the_result() {
        let tmp = TempDir::new("captured-background");
        let started = Instant::now();
        let text = run_captured(argv(&["sh", "-c", "sleep 30 & echo hi"]), tmp.path().into())
            .await
            .unwrap();
        assert_eq!(text, "hi");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn a_failure_to_start_says_which_program() {
        let tmp = TempDir::new("captured-missing");
        let e = run_captured(argv(&["kika-no-such-program"]), tmp.path().into())
            .await
            .unwrap_err();
        assert!(
            e.starts_with("Could not start kika-no-such-program: "),
            "{e}"
        );
    }
}
