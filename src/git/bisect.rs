//! The bisect in progress, read from the files git keeps for it in the git
//! dir. Nothing read here ever reaches argv: marks take a [`CommitId`] the
//! model picked, and the reset takes no argument.

use std::io::Read;
use std::path::Path;

use super::commit_files::CommitId;
use super::safe_text::{sanitize, terminal_safe};

/// The most of any one bisect file that is read. A real log is a few
/// kilobytes; this keeps a hostile one from filling memory.
const MAX_FILE: u64 = 1 << 20;

/// The longest found subject kept, in characters; a common message width.
const SUBJECT_CHARS: usize = 72;

/// How the user marks a commit. Kika uses only the default terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BisectMark {
    Good,
    Bad,
    Skip,
}

impl BisectMark {
    /// The `git bisect` subcommand that records it.
    pub fn word(self) -> &'static str {
        match self {
            BisectMark::Good => "good",
            BisectMark::Bad => "bad",
            BisectMark::Skip => "skip",
        }
    }
}

/// The words a bisect marks commits with, bad (new) first, as
/// `BISECT_TERMS` lists them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terms {
    pub bad: String,
    pub good: String,
}

impl Default for Terms {
    fn default() -> Self {
        Self {
            bad: "bad".into(),
            good: "good".into(),
        }
    }
}

impl Terms {
    /// Kika marks only a bisect that uses `bad` and `good`.
    pub fn is_default(&self) -> bool {
        self.bad == "bad" && self.good == "good"
    }
}

/// The commit git named as the first bad one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub commit: CommitId,
    /// Terminal-safe and at most 72 characters.
    pub subject: String,
}

/// What the Status panel shows of a bisect: where it started, its marks
/// and its outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BisectState {
    /// The first line of `BISECT_START`, terminal-safe: the branch or
    /// commit a reset goes back to. For display only.
    pub start: String,
    pub terms: Terms,
    pub good: usize,
    pub bad: usize,
    pub skip: usize,
    pub first_bad: Option<Found>,
    /// Only skipped commits are left, so git cannot name the first bad one.
    pub only_skipped: bool,
}

/// The bisect in `git_dir`, or `None` when there is no `BISECT_LOG`.
/// `BISECT_TERMS` is missing before the first mark, and in any git before
/// 2.7; the terms are then `bad` and `good`. Any of the three that is not
/// a regular file, such as a FIFO or a symlink, means no bisect state is
/// shown at all.
pub fn read_bisect(git_dir: &Path) -> Option<BisectState> {
    let log = read_capped(&git_dir.join("BISECT_LOG")).ok()??;
    let terms = read_capped(&git_dir.join("BISECT_TERMS"))
        .ok()?
        .and_then(|bytes| parse_terms(&bytes))
        .unwrap_or_default();
    let start = read_capped(&git_dir.join("BISECT_START"))
        .ok()?
        .map(|bytes| terminal_safe(first_line(&bytes)))
        .unwrap_or_default();
    Some(BisectState {
        start,
        ..parse_log(&log, &terms)
    })
}

/// Walk `BISECT_LOG`, counting the marks and noting the outcome. Lines
/// that are not a mark or an outcome, or that do not parse, are skipped:
/// a log is never an error. `start` is left empty.
pub fn parse_log(bytes: &[u8], terms: &Terms) -> BisectState {
    let mut state = BisectState {
        terms: terms.clone(),
        ..BisectState::default()
    };
    for line in bytes.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if let Some(mark) = line.strip_prefix(b"git bisect ") {
            count_mark(&mut state, mark);
        } else if line == b"# only skipped commits left to test" {
            state.only_skipped = true;
        } else if let Some(found) = found_line(line, &terms.bad) {
            state.first_bad = Some(found);
        }
    }
    state
}

/// `<word> <hex>`, where the word is a term or `skip`.
fn count_mark(state: &mut BisectState, mark: &[u8]) {
    let Ok(mark) = std::str::from_utf8(mark) else {
        return;
    };
    let Some((word, rev)) = mark.split_once(' ') else {
        return;
    };
    if CommitId::parse(rev).is_none() {
        return;
    }
    let word = sanitize(word.as_bytes());
    if word == state.terms.good {
        state.good += 1;
    } else if word == state.terms.bad {
        state.bad += 1;
    } else if word == "skip" {
        state.skip += 1;
    }
}

/// `# first <bad> commit: [<hex>] <subject>`, which git appends once it
/// names the first bad commit; newer git writes `'<bad>'`.
fn found_line(line: &[u8], bad: &str) -> Option<Found> {
    let line = std::str::from_utf8(line).ok()?;
    let rest = line.strip_prefix("# first ")?;
    let rest = sanitized_prefix(rest, bad)?;
    let rest = rest.strip_prefix(" commit: [")?;
    let (hex, subject) = rest.split_once(']')?;
    let commit = CommitId::parse(hex)?;
    let subject = terminal_safe(subject.trim_start_matches(' ').as_bytes());
    Some(Found {
        commit,
        subject: elide(&subject, SUBJECT_CHARS),
    })
}

/// `text` after the term `term`, compared as the terms were read: through
/// the sanitiser. Git 2.55 and newer quote the term (`'bad'`); older git
/// writes it bare.
fn sanitized_prefix<'a>(text: &'a str, term: &str) -> Option<&'a str> {
    let end = text.find(" commit: [")?;
    let word = &text.as_bytes()[..end];
    let word = word
        .strip_prefix(b"'")
        .and_then(|w| w.strip_suffix(b"'"))
        .unwrap_or(word);
    (sanitize(word) == term).then(|| &text[end..])
}

/// The two lines of `BISECT_TERMS`, bad first. Anything else is not a set
/// of terms, so the defaults stand.
fn parse_terms(bytes: &[u8]) -> Option<Terms> {
    let mut lines = bytes
        .split(|&b| b == b'\n')
        .map(|line| sanitize(line.strip_suffix(b"\r").unwrap_or(line)));
    let bad = lines.next().filter(|t| !t.is_empty())?;
    let good = lines.next().filter(|t| !t.is_empty())?;
    Some(Terms { bad, good })
}

fn first_line(bytes: &[u8]) -> &[u8] {
    bytes.split(|&b| b == b'\n').next().unwrap_or_default()
}

/// The path is there but is not a regular file.
#[derive(Debug, PartialEq, Eq)]
struct NotRegular;

/// Read at most [`MAX_FILE`] bytes of `path`: `Ok(None)` when it is missing
/// or cannot be read. Only a regular file is opened, never through a
/// symlink: a FIFO would block the status load and a link would show
/// another file's text. On unix the open itself refuses a link and does
/// not wait on a FIFO swapped in after the check.
fn read_capped(path: &Path) -> Result<Option<Vec<u8>>, NotRegular> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(None);
    };
    if !meta.is_file() {
        return Err(NotRegular);
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let Ok(file) = options.open(path) else {
        return Ok(None);
    };
    // It may have been swapped since the check; ask the open handle.
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return Err(NotRegular);
    }
    let mut bytes = Vec::new();
    match file.take(MAX_FILE).read_to_end(&mut bytes) {
        Ok(_) => Ok(Some(bytes)),
        Err(_) => Ok(None),
    }
}

/// `text` cut to `max` characters, the last one an `…` when cut.
pub(crate) fn elide(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::fake::TempDir;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";
    const C: &str = "3333333333333333333333333333333333333333";

    fn parse(log: &str) -> BisectState {
        parse_log(log.as_bytes(), &Terms::default())
    }

    #[test]
    fn counts_good_bad_and_skip_marks_from_the_command_lines() {
        let log = format!(
            "git bisect start\n\
             # status: waiting for both good and bad commits\n\
             # bad: [{A}] eight\n\
             git bisect bad {A}\n\
             # good: [{B}] one\n\
             git bisect good {B}\n\
             git bisect good {C}\n\
             # skip: [{C}] four\n\
             git bisect skip {C}\n"
        );
        let state = parse(&log);
        assert_eq!((state.good, state.bad, state.skip), (2, 1, 1));
        assert_eq!(state.first_bad, None);
        assert!(!state.only_skipped);
        assert_eq!(state.start, "");
        assert!(state.terms.is_default());
    }

    #[test]
    fn the_first_bad_line_names_the_commit_and_its_subject() {
        let log = format!(
            "git bisect bad {A}\ngit bisect good {B}\n# first bad commit: [{C}] commit 5\n"
        );
        let found = parse(&log).first_bad.unwrap();
        assert_eq!(found.commit.as_str(), C);
        assert_eq!(found.subject, "commit 5");
    }

    #[test]
    fn newer_gits_quoted_term_still_names_the_found_commit() {
        let log = format!(
            "# status: waiting for 'good' commit(s), 'bad' commit known\n\
             git bisect bad {A}\n\
             # first 'bad' commit: [{C}] commit 5\n"
        );
        let state = parse(&log);
        assert_eq!(state.bad, 1);
        let found = state.first_bad.unwrap();
        assert_eq!(
            (found.commit.as_str(), found.subject.as_str()),
            (C, "commit 5")
        );
        let custom = Terms {
            bad: "broken".into(),
            good: "fixed".into(),
        };
        let log = format!("# first 'broken' commit: [{A}] x\n# first 'bad' commit: [{C}] y\n");
        assert_eq!(
            parse_log(log.as_bytes(), &custom)
                .first_bad
                .unwrap()
                .subject,
            "x"
        );
    }

    #[test]
    fn a_long_subject_is_cut_to_72_characters() {
        let log = format!("# first bad commit: [{C}] {}\n", "é".repeat(100));
        let subject = parse(&log).first_bad.unwrap().subject;
        assert_eq!(subject.chars().count(), 72);
        assert!(subject.ends_with('…'));
    }

    #[test]
    fn custom_terms_count_their_own_words() {
        let terms = Terms {
            bad: "broken".into(),
            good: "fixed".into(),
        };
        assert!(!terms.is_default());
        let log = format!(
            "git bisect start '--term-new=broken' '--term-old=fixed'\n\
             git bisect broken {A}\n\
             git bisect fixed {B}\n\
             git bisect bad {C}\n\
             # first bad commit: [{C}] not this term\n\
             # first broken commit: [{A}] this one\n"
        );
        let state = parse_log(log.as_bytes(), &terms);
        assert_eq!((state.good, state.bad, state.skip), (1, 1, 0));
        assert_eq!(state.first_bad.unwrap().subject, "this one");
        assert_eq!(state.terms, terms);
    }

    #[test]
    fn only_skipped_commits_left_is_noted() {
        let log = format!(
            "git bisect skip {A}\n# only skipped commits left to test\n\
             # possible first bad commit: [{A}] a\n"
        );
        let state = parse(&log);
        assert!(state.only_skipped);
        assert_eq!(state.first_bad, None);
    }

    #[test]
    fn hostile_and_broken_lines_are_skipped_or_made_safe() {
        let mut log = Vec::new();
        log.extend_from_slice(format!("git bisect good {A}\r\n").as_bytes());
        log.extend_from_slice(b"git bisect start '--term-new=x' 'HEAD' '--' 'a b'\n");
        log.extend_from_slice(b"git bisect bad HEAD~1\n");
        log.extend_from_slice(b"git bisect bad -x\n");
        log.extend_from_slice(b"git bisect bad\n");
        log.extend_from_slice(b"git bisect \xff\xfe 1234\n");
        log.extend_from_slice(b"\xff\xfe garbage\n");
        log.extend_from_slice(b"# first bad commit: [nothex] x\n");
        log.extend_from_slice(
            format!("# first bad commit: [{B}] evil\x1b[2J\u{202E}gnp\r\n").as_bytes(),
        );
        let state = parse_log(&log, &Terms::default());
        assert_eq!((state.good, state.bad, state.skip), (1, 0, 0));
        let found = state.first_bad.unwrap();
        assert_eq!(found.commit.as_str(), B);
        assert_eq!(found.subject, "evil\u{FFFD}[2J<U+202E>gnp");
        assert!(!found.subject.chars().any(char::is_control));
    }

    #[test]
    fn an_empty_or_truncated_log_is_no_marks() {
        for log in ["", "\n\n", "git bisect", "# first bad commit: ["] {
            let state = parse(log);
            assert_eq!((state.good, state.bad, state.skip), (0, 0, 0), "{log:?}");
            assert_eq!(state.first_bad, None);
        }
    }

    #[test]
    fn no_log_is_no_bisect() {
        let tmp = TempDir::new("bisect-none");
        std::fs::write(tmp.path().join("BISECT_START"), "main\n").unwrap();
        assert_eq!(read_bisect(tmp.path()), None);
    }

    #[test]
    fn without_a_terms_file_the_terms_are_bad_and_good() {
        let tmp = TempDir::new("bisect-read");
        let dir = tmp.path();
        std::fs::write(dir.join("BISECT_LOG"), format!("git bisect bad {A}\n")).unwrap();
        std::fs::write(dir.join("BISECT_START"), "main\nignored\n").unwrap();
        let state = read_bisect(dir).unwrap();
        assert_eq!(state.terms, Terms::default());
        assert_eq!(state.start, "main");
        assert_eq!(state.bad, 1);
    }

    #[test]
    fn the_terms_file_and_a_hostile_start_are_read_safely() {
        let tmp = TempDir::new("bisect-terms");
        let dir = tmp.path();
        std::fs::write(dir.join("BISECT_LOG"), format!("git bisect new {A}\n")).unwrap();
        std::fs::write(dir.join("BISECT_TERMS"), "new\r\nold\n").unwrap();
        std::fs::write(dir.join("BISECT_START"), "evil\x1b]0;x\x07\u{202E}\n").unwrap();
        let state = read_bisect(dir).unwrap();
        assert_eq!(
            state.terms,
            Terms {
                bad: "new".into(),
                good: "old".into()
            }
        );
        assert_eq!(state.bad, 1);
        assert_eq!(state.start, "evil\u{FFFD}]0;x\u{FFFD}<U+202E>");
    }

    #[test]
    fn a_terms_file_short_of_two_lines_keeps_the_defaults() {
        assert_eq!(parse_terms(b""), None);
        assert_eq!(parse_terms(b"bad\n"), None);
        assert_eq!(parse_terms(b"\ngood\n"), None);
    }

    #[cfg(unix)]
    fn mkfifo(path: &Path) {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c` is a valid NUL-terminated path for the call.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_or_a_symlink_among_the_files_is_no_bisect_and_never_blocks() {
        use std::os::unix::fs::symlink;
        for (name, kind) in [
            ("BISECT_LOG", "fifo"),
            ("BISECT_START", "fifo"),
            ("BISECT_TERMS", "fifo"),
            ("BISECT_LOG", "link"),
            ("BISECT_START", "link"),
            ("BISECT_TERMS", "link"),
        ] {
            let tmp = TempDir::new("bisect-special");
            let dir = tmp.path();
            let elsewhere = dir.join("elsewhere");
            std::fs::write(&elsewhere, "secret\n").unwrap();
            for file in ["BISECT_LOG", "BISECT_START", "BISECT_TERMS"] {
                if file == name {
                    continue;
                }
                let text = match file {
                    "BISECT_LOG" => format!("git bisect bad {A}\n"),
                    "BISECT_TERMS" => "bad\ngood\n".into(),
                    _ => "main\n".into(),
                };
                std::fs::write(dir.join(file), text).unwrap();
            }
            match kind {
                "fifo" => mkfifo(&dir.join(name)),
                _ => symlink(&elsewhere, dir.join(name)).unwrap(),
            }
            assert_eq!(read_bisect(dir), None, "{name} as a {kind}");
        }
    }

    #[test]
    fn a_directory_in_place_of_a_file_is_no_bisect() {
        let tmp = TempDir::new("bisect-dir");
        let dir = tmp.path();
        std::fs::write(dir.join("BISECT_LOG"), format!("git bisect bad {A}\n")).unwrap();
        std::fs::create_dir(dir.join("BISECT_START")).unwrap();
        assert_eq!(read_bisect(dir), None);
        assert_eq!(read_capped(&dir.join("BISECT_START")), Err(NotRegular));
        assert_eq!(read_capped(&dir.join("missing")), Ok(None));
    }

    #[test]
    fn a_huge_log_is_read_only_up_to_the_cap() {
        let tmp = TempDir::new("bisect-cap");
        let dir = tmp.path();
        let mut log = vec![b'#'; MAX_FILE as usize];
        log.extend_from_slice(format!("\ngit bisect bad {A}\n").as_bytes());
        std::fs::write(dir.join("BISECT_LOG"), log).unwrap();
        assert_eq!(read_bisect(dir).unwrap().bad, 0);
    }
}
