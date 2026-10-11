//! `HEAD`'s reflog: every place `HEAD` has been, newest first, with what
//! moved it. The Reflog tab lists it, and the undo planner reads it to
//! find the last action to reverse.

use super::branches::run;
use super::commit_files::CommitId;
use super::diff::terminal_safe;
use super::runner::{GitCommand, GitRunner};

/// How many entries are read. An action older than this cannot be undone.
const DEPTH: &str = "1000";

/// Characters of a message the tab shows; the rest is never needed.
const DISPLAY_MAX: usize = 200;

/// What Kika writes as `GIT_REFLOG_ACTION` when it undoes or redoes, so
/// the planner can find its own steps again.
pub const UNDO_MARKER: &str = "kika undo";
pub const REDO_MARKER: &str = "kika redo";

/// The reflog and where `HEAD` is now, read together so a plan can tell
/// whether `HEAD` moved since the action it reverses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reflog {
    pub head: Option<CommitId>,
    /// Newest first.
    pub entries: Vec<ReflogEntry>,
}

/// One reflog entry. The message is kept raw: it can hold anything but
/// NUL, so it is only drawn through [`ReflogEntry::display`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflogEntry {
    /// `None` when git printed something that is not hex; such an entry is
    /// listed, but never planned from.
    pub oid: Option<CommitId>,
    pub tree: Option<CommitId>,
    pub message: Vec<u8>,
    pub action: Action,
}

/// What an entry's message says moved `HEAD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// `checkout: moving from <from> to <to>`, both raw names as typed:
    /// a branch, a full hex id for a detached `HEAD`, or anything else.
    Checkout {
        from: Vec<u8>,
        to: Vec<u8>,
    },
    /// The current branch, or a detached `HEAD`, moved to another commit.
    Move(MoveKind),
    /// The first commit, which has nothing before it to go back to.
    CommitInitial,
    /// A rebase detached `HEAD` at its base.
    RebaseStart,
    /// A commit a rebase replayed, or any other rebase message.
    RebaseStep,
    /// A rebase put `HEAD` back on `branch`, its short name.
    RebaseFinish {
        branch: Vec<u8>,
    },
    /// Kika's own undo or redo.
    Undo,
    Redo,
    /// Anything else, an aborted rebase included.
    Other,
}

/// What moved the branch, for a [`Action::Move`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveKind {
    Commit,
    Amend,
    Merge,
    Pull,
    CherryPick,
    Revert,
    Reset,
}

impl ReflogEntry {
    /// One record of `reflog show --format=%H %T %gs`: split at the first
    /// two spaces only, since the message may hold any.
    pub fn parse(record: &[u8]) -> Self {
        let mut fields = record.splitn(3, |&b| b == b' ');
        let mut id = || {
            fields
                .next()
                .and_then(|f| std::str::from_utf8(f).ok())
                .and_then(CommitId::parse)
        };
        let oid = id();
        let tree = id();
        let message = fields.next().unwrap_or_default().to_vec();
        let action = Action::parse(&message);
        Self {
            oid,
            tree,
            message,
            action,
        }
    }

    /// The message as one line safe to draw, cut to a length no row needs
    /// more of.
    pub fn display(&self) -> String {
        one_line(&self.message).chars().take(DISPLAY_MAX).collect()
    }

    /// The short action name: the message up to its first `:`.
    pub fn label(&self) -> String {
        let end = self
            .message
            .iter()
            .position(|&b| b == b':')
            .unwrap_or(self.message.len());
        one_line(&self.message[..end])
    }
}

/// `bytes` through the terminal sanitiser, its lines joined by spaces.
fn one_line(bytes: &[u8]) -> String {
    terminal_safe(bytes).replace('\n', " ")
}

impl Action {
    /// Match the message's leading bytes against what git writes.
    pub fn parse(message: &[u8]) -> Self {
        if is_marker(message, UNDO_MARKER) {
            return Action::Undo;
        }
        if is_marker(message, REDO_MARKER) {
            return Action::Redo;
        }
        if let Some(moves) = message.strip_prefix(b"checkout: moving from ") {
            return checkout(moves);
        }
        if message.starts_with(b"commit (initial):") {
            return Action::CommitInitial;
        }
        let head = &message[..message
            .iter()
            .position(|&b| b == b':')
            .unwrap_or(message.len())];
        // `pull --rebase` names its rebase steps after itself.
        if word(message, b"rebase") || (word(message, b"pull") && contains(head, b" (")) {
            return rebase(message, head);
        }
        const MOVES: &[(&[u8], MoveKind)] = &[
            (b"commit:", MoveKind::Commit),
            (b"commit (amend):", MoveKind::Amend),
            (b"commit (merge):", MoveKind::Merge),
            (b"merge ", MoveKind::Merge),
            (b"reset: moving to ", MoveKind::Reset),
        ];
        if let Some((_, kind)) = MOVES.iter().find(|(p, _)| message.starts_with(p)) {
            return Action::Move(*kind);
        }
        const WORDS: &[(&[u8], MoveKind)] = &[
            (b"pull", MoveKind::Pull),
            (b"cherry-pick", MoveKind::CherryPick),
            (b"revert", MoveKind::Revert),
        ];
        match WORDS.iter().find(|(w, _)| word(message, w)) {
            Some((_, kind)) => Action::Move(*kind),
            None => Action::Other,
        }
    }
}

/// `marker` alone, or followed by `:` as `reset` writes it.
fn is_marker(message: &[u8], marker: &str) -> bool {
    message
        .strip_prefix(marker.as_bytes())
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(b":"))
}

/// `message` starts with the word `w`, not merely its letters.
fn word(message: &[u8], w: &[u8]) -> bool {
    message
        .strip_prefix(w)
        .is_some_and(|rest| matches!(rest.first(), None | Some(b' ' | b':')))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// `<from> to <to>`. A ` to ` that occurs twice leaves no way to tell the
/// names apart, so nothing is guessed.
fn checkout(moves: &[u8]) -> Action {
    let at: Vec<usize> = moves
        .windows(4)
        .enumerate()
        .filter(|(_, w)| *w == b" to ")
        .map(|(i, _)| i)
        .collect();
    match at.as_slice() {
        [at] => Action::Checkout {
            from: moves[..*at].to_vec(),
            to: moves[at + 4..].to_vec(),
        },
        _ => Action::Other,
    }
}

/// A message of the rebase family. `head` is the part before the first
/// `:`, such as `rebase -i (finish)`.
fn rebase(message: &[u8], head: &[u8]) -> Action {
    if head.ends_with(b"(abort)") || message == b"rebase: aborting" {
        return Action::Other;
    }
    if head.ends_with(b"(start)") || message.starts_with(b"rebase: checkout ") {
        return Action::RebaseStart;
    }
    if head.ends_with(b"(finish)") || head == b"rebase finished" {
        let rest = &message[head.len()..];
        if let Some(branch) = rest.strip_prefix(b": returning to refs/heads/") {
            return Action::RebaseFinish {
                branch: branch.to_vec(),
            };
        }
    }
    Action::RebaseStep
}

/// Every entry of `HEAD`'s reflog, newest first, and `HEAD` itself.
pub fn read_reflog(runner: &dyn GitRunner) -> Result<Reflog, String> {
    let out = run(
        runner,
        GitCommand::new("reflog")
            .arg("show")
            .arg("-z")
            .arg("-n")
            .arg(DEPTH)
            .arg("--format=%H %T %gs")
            .arg("HEAD"),
    )?;
    let entries = parse_records(&out);
    let head = run(
        runner,
        GitCommand::new("rev-parse")
            .arg("-q")
            .arg("--verify")
            .arg("HEAD"),
    )
    .ok()
    .and_then(|out| {
        std::str::from_utf8(&out)
            .ok()
            .and_then(|s| CommitId::parse(s.trim()))
    });
    Ok(Reflog { head, entries })
}

/// NUL-terminated records, the empty end left out.
pub fn parse_records(out: &[u8]) -> Vec<ReflogEntry> {
    out.split(|&b| b == 0)
        .filter(|record| !record.is_empty())
        .map(ReflogEntry::parse)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::git::fake::{FakeRunner, fail, ok};

    const SHOW: &[&str] = &[
        "reflog",
        "show",
        "-z",
        "-n",
        "1000",
        "--format=%H %T %gs",
        "HEAD",
    ];
    const VERIFY: &[&str] = &["rev-parse", "-q", "--verify", "HEAD"];
    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const T: &str = "1111111111111111111111111111111111111111";

    fn action(message: &str) -> Action {
        Action::parse(message.as_bytes())
    }

    #[test]
    fn the_read_runs_exactly_these_commands_without_locks_and_in_the_c_locale() {
        let hostile = include_bytes!("fixtures/reflog/hostile.bin");
        let runner = FakeRunner::new()
            .expect(SHOW, ok(hostile))
            .expect(VERIFY, ok(format!("{A}\n").as_bytes()));

        let reflog = read_reflog(&runner).unwrap();

        runner.assert_done();
        let env: Vec<(OsString, OsString)> = vec![
            ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
            ("LC_ALL".into(), "C".into()),
        ];
        assert_eq!(runner.envs(), [env.clone(), env]);
        assert_eq!(reflog.head, CommitId::parse(A));
        assert_eq!(reflog.entries.len(), 5);
    }

    #[test]
    fn hostile_messages_draw_with_no_control_characters() {
        let entries = parse_records(include_bytes!("fixtures/reflog/hostile.bin"));
        for entry in &entries {
            for text in [entry.display(), entry.label()] {
                assert!(
                    !text.chars().any(|c| c.is_control() || c == '\u{202E}'),
                    "{text:?}"
                );
            }
        }
        assert_eq!(entries[0].display(), "commit: \u{FFFD}[2J clear screen");
        assert_eq!(entries[1].display(), "commit: carriage return");
    }

    #[test]
    fn a_checkout_naming_to_twice_is_other_and_a_plain_one_parses() {
        let entries = parse_records(include_bytes!("fixtures/reflog/hostile.bin"));
        assert_eq!(entries[3].action, Action::Other, "{:?}", entries[3]);
        assert_eq!(
            entries[4].action,
            Action::Checkout {
                from: b"main".to_vec(),
                to: b"side".to_vec()
            }
        );
        assert_eq!(entries[4].oid, CommitId::parse(A));
        assert_eq!(entries[4].tree, CommitId::parse(T));
    }

    #[test]
    fn a_field_that_is_not_hex_becomes_none_and_the_entry_stays() {
        let entry = ReflogEntry::parse(b"zz 1111 commit: x y");
        assert_eq!(entry.oid, None);
        assert_eq!(entry.tree, CommitId::parse("1111"));
        assert_eq!(entry.message, b"commit: x y");
        assert_eq!(entry.action, Action::Move(MoveKind::Commit));
        assert_eq!(entry.label(), "commit");
    }

    #[test]
    fn the_display_is_capped() {
        let long = format!("{A} {T} commit: {}", "x".repeat(500));
        let entry = ReflogEntry::parse(long.as_bytes());
        assert_eq!(entry.display().chars().count(), 200);
    }

    #[test]
    fn moves_are_matched_on_the_leading_bytes() {
        for (message, kind) in [
            ("commit: x", MoveKind::Commit),
            ("commit (amend): x", MoveKind::Amend),
            ("commit (merge): x", MoveKind::Merge),
            ("merge side: Fast-forward", MoveKind::Merge),
            ("pull: Fast-forward", MoveKind::Pull),
            ("pull origin main: Merge made", MoveKind::Pull),
            ("cherry-pick: x", MoveKind::CherryPick),
            ("revert: Revert \"x\"", MoveKind::Revert),
            ("reset: moving to HEAD~1", MoveKind::Reset),
        ] {
            assert_eq!(action(message), Action::Move(kind), "{message}");
        }
        assert_eq!(action("commit (initial): x"), Action::CommitInitial);
        for other in [
            "pullover: x",
            "reset: updating HEAD",
            "branch: Created from HEAD",
            "",
        ] {
            assert_eq!(action(other), Action::Other, "{other}");
        }
    }

    #[test]
    fn rebase_messages_of_git_2_15_and_of_today_parse_alike() {
        for start in [
            "rebase (start): checkout main",
            "rebase -i (start): checkout main",
            "rebase: checkout main",
            "pull --rebase (start): checkout origin/main",
        ] {
            assert_eq!(action(start), Action::RebaseStart, "{start}");
        }
        for step in [
            "rebase (pick): x",
            "rebase -i (squash): x",
            "rebase: replayed subject",
            "rebase",
            "rebase -i (finish): refs/heads/t onto abc",
        ] {
            assert_eq!(action(step), Action::RebaseStep, "{step}");
        }
        for finish in [
            "rebase (finish): returning to refs/heads/topic",
            "rebase -i (finish): returning to refs/heads/topic",
            "rebase finished: returning to refs/heads/topic",
            "pull --rebase (finish): returning to refs/heads/topic",
        ] {
            assert_eq!(
                action(finish),
                Action::RebaseFinish {
                    branch: b"topic".to_vec()
                },
                "{finish}"
            );
        }
        for abort in [
            "rebase (abort): returning to refs/heads/topic",
            "rebase -i (abort): returning to refs/heads/topic",
            "rebase: aborting",
        ] {
            assert_eq!(action(abort), Action::Other, "{abort}");
        }
    }

    #[test]
    fn only_the_exact_markers_count_as_kikas_own_steps() {
        assert_eq!(action("kika undo"), Action::Undo);
        assert_eq!(action("kika undo: updating HEAD"), Action::Undo);
        assert_eq!(action("kika redo"), Action::Redo);
        assert_eq!(action("kika redo: updating HEAD"), Action::Redo);
        assert_eq!(action("kika undone"), Action::Other);
        assert_eq!(action("kika redo it"), Action::Other);
    }

    #[test]
    fn a_failed_read_is_gits_message_made_safe() {
        let runner = FakeRunner::new().expect(SHOW, fail("fatal: bad \x1b[2Jthing\n"));
        assert_eq!(
            read_reflog(&runner),
            Err("fatal: bad \u{FFFD}[2Jthing".into())
        );
    }

    #[test]
    fn a_head_that_does_not_verify_is_none() {
        let runner = FakeRunner::new()
            .expect(SHOW, ok(b""))
            .expect(VERIFY, fail(""));
        assert_eq!(read_reflog(&runner), Ok(Reflog::default()));
    }

    #[test]
    fn a_fresh_repositorys_reflog_is_read_in_real_git() {
        use crate::git::CliRunner;
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("reflog");
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "one"]);
        std::fs::write(dir.join("a.txt"), "b\n").unwrap();
        git(dir, &["commit", "-q", "-am", "two words"]);
        git(dir, &["checkout", "-q", "-b", "side"]);

        let reflog = read_reflog(&CliRunner::isolated(dir)).unwrap();

        let head = String::from_utf8(git(dir, &["rev-parse", "HEAD"])).unwrap();
        assert_eq!(reflog.head, CommitId::parse(head.trim()));
        let actions: Vec<&Action> = reflog.entries.iter().map(|e| &e.action).collect();
        assert_eq!(
            actions,
            [
                &Action::Checkout {
                    from: b"main".to_vec(),
                    to: b"side".to_vec()
                },
                &Action::Move(MoveKind::Commit),
                &Action::CommitInitial,
            ]
        );
        assert_eq!(reflog.entries[1].display(), "commit: two words");
        assert!(reflog.entries.iter().all(|e| e.tree.is_some()));
        assert_ne!(reflog.entries[1].tree, reflog.entries[2].tree);
    }
}
