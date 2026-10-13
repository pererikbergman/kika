//! `HEAD`'s reflog: every place `HEAD` has been, newest first, with what
//! moved it. The Reflog tab lists it, and the undo planner reads it to
//! find the last action to reverse.

use crate::features::commits::git::detached_checkout;
use crate::git::branches::run;
use crate::git::commit_files::CommitId;
use crate::git::diff::terminal_safe;
use crate::git::runner::{GitCommand, GitRunner};
use crate::op::checkout_command;

use super::undo::{Direction, Step};

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
    /// The branch `HEAD` is on, as the full ref `symbolic-ref` names;
    /// `None` when detached.
    pub head_ref: Option<Vec<u8>>,
    /// Newest first.
    pub entries: Vec<ReflogEntry>,
    /// That branch's own reflog, newest first. Only it says where a
    /// rebased branch was before: `HEAD`'s may have been elsewhere, as
    /// after `git rebase <upstream> <branch>` run from another branch.
    pub branch_entries: Vec<ReflogEntry>,
}

/// Where `HEAD` is: its commit, and its branch as a full ref.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeadState {
    pub oid: Option<CommitId>,
    pub head_ref: Option<Vec<u8>>,
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
    let HeadState { oid, head_ref } = read_head(runner);
    // A missing branch reflog only leaves a rebase without an undo.
    let branch_entries = head_ref
        .as_ref()
        .filter(|r| r.starts_with(b"refs/heads/"))
        .and_then(|r| {
            run(
                runner,
                GitCommand::new("reflog")
                    .arg("show")
                    .arg("-z")
                    .arg("-n")
                    .arg(DEPTH)
                    .arg("--format=%H %T %gs")
                    .arg(crate::git::path::os_string(r))
                    .arg("--"),
            )
            .ok()
        })
        .map(|out| parse_records(&out))
        .unwrap_or_default();
    Ok(Reflog {
        head: oid,
        head_ref,
        entries,
        branch_entries,
    })
}

/// `HEAD`'s commit and branch, each `None` when git cannot say: an unborn
/// or broken `HEAD`, or a detached one.
pub fn read_head(runner: &dyn GitRunner) -> HeadState {
    let line = |cmd: GitCommand| {
        run(runner, cmd).ok().map(|mut out| {
            while out.last().is_some_and(|b| b.is_ascii_whitespace()) {
                out.pop();
            }
            out
        })
    };
    let oid = line(
        GitCommand::new("rev-parse")
            .arg("-q")
            .arg("--verify")
            .arg("HEAD"),
    )
    .and_then(|out| String::from_utf8(out).ok())
    .and_then(|s| CommitId::parse(&s));
    let head_ref =
        line(GitCommand::new("symbolic-ref").arg("-q").arg("HEAD")).filter(|r| !r.is_empty());
    HeadState { oid, head_ref }
}

/// Whether `entry`, from the reflog of the branch `full_ref`, is a rebase
/// putting that branch at its new tip: `rebase (finish): <ref> onto …`,
/// the `-i` and `pull --rebase` forms, or git 2.15's `rebase finished:`.
pub fn finishes_rebase_of(entry: &ReflogEntry, full_ref: &[u8]) -> bool {
    let message = &entry.message;
    let Some(colon) = message.iter().position(|&b| b == b':') else {
        return false;
    };
    let head = &message[..colon];
    let finish = (word(message, b"rebase") || word(message, b"pull"))
        && (head.ends_with(b"(finish)") || head == b"rebase finished");
    let mut wanted = b": ".to_vec();
    wanted.extend_from_slice(full_ref);
    wanted.extend_from_slice(b" onto ");
    finish && message[colon..].starts_with(&wanted)
}

/// NUL-terminated records, the empty end left out.
pub fn parse_records(out: &[u8]) -> Vec<ReflogEntry> {
    out.split(|&b| b == 0)
        .filter(|record| !record.is_empty())
        .map(ReflogEntry::parse)
        .collect()
}

/// The argv of `GitOp::Travel`: the planned `step`, logged under
/// `direction`'s marker.
// `--keep` makes git itself refuse to move when a local change, or
// an untracked file, is in the way. The marker in
// `GIT_REFLOG_ACTION` is all `checkout` logs, and `reset` logs it as
// `<marker>: updating HEAD`, as far back as git 2.15.
pub(crate) fn travel_command(direction: Direction, step: &Step) -> GitCommand {
    match step {
        Step::MoveTo(oid) => GitCommand::new("reset")
            .arg("-q")
            .arg("--keep")
            .arg(oid.as_str())
            .arg("--"),
        Step::CheckoutBranch(name) => checkout_command(name),
        Step::CheckoutDetached(oid) => detached_checkout(oid),
    }
    .write_env()
    .env("GIT_REFLOG_ACTION", direction.marker())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::features::commits::reflog::undo::Expected;
    use crate::git::fake::{FakeRunner, fail, ok};
    use crate::git::runner::EnvProfile;
    use crate::op::{GitOp, HEAD_MOVED, op_command, run_op};
    use crate::test_support::{argv, cid as oid, rev_parse, two_commit_repo};

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
    const SYMBOLIC: &[&str] = &["symbolic-ref", "-q", "HEAD"];
    const BRANCH: &[&str] = &[
        "reflog",
        "show",
        "-z",
        "-n",
        "1000",
        "--format=%H %T %gs",
        "refs/heads/main",
        "--",
    ];
    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const T: &str = "1111111111111111111111111111111111111111";

    fn action(message: &str) -> Action {
        Action::parse(message.as_bytes())
    }

    #[test]
    fn the_read_runs_exactly_these_commands_without_locks_and_in_the_c_locale() {
        let hostile = include_bytes!("../../../git/fixtures/reflog/hostile.bin");
        let branch = format!("{A} {T} commit: x\0");
        let runner = FakeRunner::new()
            .expect(SHOW, ok(hostile))
            .expect(VERIFY, ok(format!("{A}\n").as_bytes()))
            .expect(SYMBOLIC, ok(b"refs/heads/main\n"))
            .expect(BRANCH, ok(branch.as_bytes()));

        let reflog = read_reflog(&runner).unwrap();

        runner.assert_done();
        let env: Vec<(OsString, OsString)> = vec![
            ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
            ("LC_ALL".into(), "C".into()),
        ];
        assert_eq!(runner.envs(), vec![env; 4]);
        assert_eq!(reflog.head, CommitId::parse(A));
        assert_eq!(reflog.head_ref.as_deref(), Some(&b"refs/heads/main"[..]));
        assert_eq!(reflog.entries.len(), 5);
        assert_eq!(reflog.branch_entries.len(), 1);
    }

    #[test]
    fn hostile_messages_draw_with_no_control_characters() {
        let entries = parse_records(include_bytes!("../../../git/fixtures/reflog/hostile.bin"));
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
        let entries = parse_records(include_bytes!("../../../git/fixtures/reflog/hostile.bin"));
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
            .expect(VERIFY, fail(""))
            .expect(SYMBOLIC, fail(""));
        assert_eq!(read_reflog(&runner), Ok(Reflog::default()));
    }

    #[test]
    fn a_branch_reflog_that_cannot_be_read_leaves_it_empty() {
        let runner = FakeRunner::new()
            .expect(SHOW, ok(b""))
            .expect(VERIFY, ok(format!("{A}\n").as_bytes()))
            .expect(SYMBOLIC, ok(b"refs/heads/main\n"))
            .expect(BRANCH, fail("fatal: no reflog"));
        let reflog = read_reflog(&runner).unwrap();
        assert_eq!(reflog.branch_entries, []);
        assert_eq!(reflog.head_ref.as_deref(), Some(&b"refs/heads/main"[..]));
    }

    #[test]
    fn only_a_rebase_finish_naming_the_branch_counts_as_its_new_tip() {
        let entry = |message: &str| ReflogEntry::parse(format!("{A} {T} {message}").as_bytes());
        let topic = b"refs/heads/topic";
        for yes in [
            "rebase (finish): refs/heads/topic onto abc",
            "rebase -i (finish): refs/heads/topic onto abc",
            "rebase finished: refs/heads/topic onto abc",
            "pull --rebase (finish): refs/heads/topic onto abc",
        ] {
            assert!(finishes_rebase_of(&entry(yes), topic), "{yes}");
        }
        for no in [
            "rebase (finish): refs/heads/topical onto abc",
            "rebase (finish): returning to refs/heads/topic",
            "rebase (pick): refs/heads/topic onto abc",
            "commit: rebase (finish): refs/heads/topic onto abc",
            "kika undo: updating HEAD",
        ] {
            assert!(!finishes_rebase_of(&entry(no), topic), "{no}");
        }
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
        assert_eq!(reflog.head_ref.as_deref(), Some(&b"refs/heads/side"[..]));
        assert_eq!(reflog.branch_entries.len(), 1, "side was just made");
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

    /// A travel op expecting a detached `HEAD` at `abc`; only its argv
    /// is looked at.
    fn travel(direction: Direction, step: Step) -> GitOp {
        GitOp::Travel {
            direction,
            step,
            expect: Expected {
                head_ref: None,
                oid: oid("abc"),
            },
        }
    }

    /// A travel op expecting `HEAD` where it is now in `runner`'s repo.
    fn travel_here(runner: &dyn GitRunner, direction: Direction, step: Step) -> GitOp {
        let now = super::read_head(runner);
        GitOp::Travel {
            direction,
            step,
            expect: Expected {
                head_ref: now.head_ref,
                oid: now.oid.unwrap(),
            },
        }
    }

    fn planned(plan: crate::features::commits::reflog::undo::Plan) -> GitOp {
        GitOp::Travel {
            direction: plan.direction,
            step: plan.step,
            expect: plan.expect,
        }
    }

    #[test]
    fn travel_names_only_ids_and_loaded_branches_and_marks_the_reflog() {
        let cases: [(GitOp, &[&str], &str); 4] = [
            (
                travel(Direction::Undo, Step::MoveTo(oid("abc123"))),
                &["reset", "-q", "--keep", "abc123", "--"],
                "kika undo",
            ),
            (
                travel(Direction::Redo, Step::MoveTo(oid("abc123"))),
                &["reset", "-q", "--keep", "abc123", "--"],
                "kika redo",
            ),
            (
                travel(Direction::Undo, Step::CheckoutBranch("main".into())),
                &["checkout", "-q", "main", "--"],
                "kika undo",
            ),
            (
                travel(Direction::Redo, Step::CheckoutDetached(oid("abc123"))),
                &["checkout", "-q", "--detach", "abc123", "--"],
                "kika redo",
            ),
        ];
        for (op, expected, marker) in cases {
            let cmd = op_command(&op);
            assert_eq!(argv(&cmd), expected, "{op:?}");
            assert_eq!(cmd.env_profile(), EnvProfile::Write, "{op:?}");
            let envs: Vec<_> = cmd
                .process_env()
                .iter()
                .map(|(k, v)| {
                    (
                        k.to_str().unwrap().to_owned(),
                        v.to_str().unwrap().to_owned(),
                    )
                })
                .collect();
            assert_eq!(
                envs,
                [
                    ("LC_ALL", "C"),
                    ("GIT_LITERAL_PATHSPECS", "1"),
                    ("GIT_REFLOG_ACTION", marker),
                ]
                .map(|(k, v)| (k.to_owned(), v.to_owned())),
                "{op:?}"
            );
        }
        let checkout = op_command(&GitOp::Checkout("main".into()));
        assert_eq!(argv(&checkout), ["checkout", "-q", "main", "--"]);
    }

    #[test]
    fn an_undo_leaves_a_marker_the_next_plan_reads_back_in_real_git() {
        use crate::features::commits::reflog::git::{Action, read_reflog};
        use crate::features::commits::reflog::undo::{Context, plan};

        let (tmp, first) = two_commit_repo();
        let dir = tmp.path();
        let second = rev_parse(dir, "HEAD");
        let runner = crate::git::CliRunner::isolated(dir);
        let ctx = Context {
            branch: Some("main"),
            branches: &[],
            dirty: false,
        };

        let undo = plan(Direction::Undo, &read_reflog(&runner).unwrap(), &ctx).unwrap();
        assert_eq!(undo.step, Step::MoveTo(first.clone()));
        run_op(&runner, &planned(undo)).unwrap();

        assert_eq!(rev_parse(dir, "HEAD"), first);
        let reflog = read_reflog(&runner).unwrap();
        assert_eq!(reflog.entries[0].action, Action::Undo);
        assert_eq!(reflog.entries[0].message, b"kika undo: updating HEAD");
        let redo = plan(Direction::Redo, &reflog, &ctx).unwrap();
        assert_eq!(redo.step, Step::MoveTo(second));
    }

    #[test]
    fn reset_keep_refuses_to_clobber_a_local_change_or_an_untracked_file_in_real_git() {
        use crate::git::fake::git;

        let (tmp, first) = two_commit_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        std::fs::write(dir.join("a.txt"), "mine\n").unwrap();
        let undo = travel_here(&runner, Direction::Undo, Step::MoveTo(first.clone()));
        let refused = run_op(&runner, &undo).unwrap_err();
        assert_ne!(refused, HEAD_MOVED);
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "mine\n"
        );

        // The commit after `first` adds `b.txt`; an untracked one is in
        // the way of going back to it.
        git(dir, &["checkout", "-q", "--", "a.txt"]);
        std::fs::write(dir.join("b.txt"), "theirs\n").unwrap();
        git(dir, &["add", "b.txt"]);
        git(dir, &["commit", "-q", "-m", "third"]);
        let third = rev_parse(dir, "HEAD");
        let undo = travel_here(&runner, Direction::Undo, Step::MoveTo(first));
        run_op(&runner, &undo).unwrap();
        std::fs::write(dir.join("b.txt"), "untracked\n").unwrap();
        let redo = travel_here(&runner, Direction::Redo, Step::MoveTo(third));
        let refused = run_op(&runner, &redo).unwrap_err();
        assert_ne!(refused, HEAD_MOVED);
        assert_eq!(
            std::fs::read_to_string(dir.join("b.txt")).unwrap(),
            "untracked\n"
        );
    }

    #[test]
    fn a_travel_whose_head_moved_since_the_question_changes_nothing_in_real_git() {
        use crate::git::fake::git;

        let (tmp, first) = two_commit_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        let undo = travel_here(&runner, Direction::Undo, Step::MoveTo(first.clone()));

        // Another commit lands while the question is open.
        std::fs::write(dir.join("a.txt"), "three\n").unwrap();
        git(dir, &["commit", "-q", "-am", "third"]);
        let third = rev_parse(dir, "HEAD");
        assert_eq!(run_op(&runner, &undo), Err(HEAD_MOVED.into()));
        assert_eq!(rev_parse(dir, "HEAD"), third);

        // Same commit, another branch: still not what was asked about.
        let undo = travel_here(&runner, Direction::Undo, Step::MoveTo(first));
        git(dir, &["checkout", "-q", "-b", "other"]);
        assert_eq!(run_op(&runner, &undo), Err(HEAD_MOVED.into()));
        assert_eq!(rev_parse(dir, "refs/heads/other"), third);
    }

    #[test]
    fn undoing_a_rebase_started_from_another_branch_restores_the_rebased_branch_in_real_git() {
        use crate::features::commits::reflog::git::read_reflog;
        use crate::features::commits::reflog::undo::{Context, plan};
        use crate::git::fake::git;

        let (tmp, base) = two_commit_repo();
        let dir = tmp.path();
        let main = rev_parse(dir, "HEAD");
        git(dir, &["checkout", "-q", "-b", "topic", base.as_str()]);
        std::fs::write(dir.join("t.txt"), "topic\n").unwrap();
        git(dir, &["add", "t.txt"]);
        git(dir, &["commit", "-q", "-m", "topic"]);
        let old_topic = rev_parse(dir, "HEAD");
        git(dir, &["checkout", "-q", "-b", "other", main.as_str()]);
        std::fs::write(dir.join("o.txt"), "other\n").unwrap();
        git(dir, &["add", "o.txt"]);
        git(dir, &["commit", "-q", "-m", "other"]);
        let other = rev_parse(dir, "HEAD");
        // Run from `other`: HEAD's reflog never held topic's old tip.
        git(dir, &["rebase", "-q", main.as_str(), "topic"]);
        let new_topic = rev_parse(dir, "HEAD");

        let runner = crate::git::CliRunner::isolated(dir);
        let reflog = read_reflog(&runner).unwrap();
        let branches = crate::git::branches::list_branches(&runner).unwrap();
        let ctx = Context {
            branch: Some("topic"),
            branches: &branches,
            dirty: false,
        };
        let undo = plan(Direction::Undo, &reflog, &ctx).unwrap();
        assert_eq!(undo.step, Step::MoveTo(old_topic.clone()));
        assert_ne!(undo.step, Step::MoveTo(other.clone()));
        run_op(&runner, &planned(undo)).unwrap();
        assert_eq!(rev_parse(dir, "refs/heads/topic"), old_topic);
        assert_eq!(rev_parse(dir, "refs/heads/other"), other);

        // Without the branch's own reflog there is no safe target.
        let redo = plan(Direction::Redo, &read_reflog(&runner).unwrap(), &ctx).unwrap();
        run_op(&runner, &planned(redo)).unwrap();
        assert_eq!(rev_parse(dir, "HEAD"), new_topic);
        std::fs::remove_file(dir.join(".git/logs/refs/heads/topic")).unwrap();
        assert_eq!(
            plan(Direction::Undo, &read_reflog(&runner).unwrap(), &ctx),
            Err(
                "Cannot undo rebase: the branch's own reflog does not say where it was before"
                    .into()
            )
        );
    }

    #[test]
    fn a_head_pointing_outside_refs_heads_plans_no_undo_in_real_git() {
        use crate::features::commits::reflog::git::read_reflog;
        use crate::features::commits::reflog::undo::{Context, plan};
        use crate::git::fake::git;

        let (tmp, _) = two_commit_repo();
        let dir = tmp.path();
        let head = rev_parse(dir, "HEAD");
        git(
            dir,
            &["update-ref", "refs/remotes/origin/main", head.as_str()],
        );
        git(dir, &["symbolic-ref", "HEAD", "refs/remotes/origin/main"]);
        std::fs::write(dir.join("a.txt"), "three\n").unwrap();
        git(dir, &["commit", "-q", "-am", "on a remote ref"]);
        let moved = rev_parse(dir, "refs/remotes/origin/main");

        let runner = crate::git::CliRunner::isolated(dir);
        let reflog = read_reflog(&runner).unwrap();
        let ctx = Context {
            branch: None,
            branches: &[],
            dirty: false,
        };
        assert_eq!(
            plan(Direction::Undo, &reflog, &ctx),
            Err("HEAD is on refs/remotes/origin/main, not a branch; nothing is undone".into())
        );
        assert_eq!(rev_parse(dir, "refs/remotes/origin/main"), moved);
    }
}
