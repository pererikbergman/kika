//! The I/O behind the diff's own effects: the timer that marks a slow
//! load, and reading the focused view's patch for staging. `LoadDiff`,
//! `DiffDemand` and `CancelDiff` go straight to the `DiffStreamer` that
//! `Runtime` holds. The staging ops themselves stay core `GitOp`s, run by
//! `op.rs`.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::background;
use crate::effect::{DIFF_LOADING_DELAY, Tag};
use crate::git::GitRunner;
use crate::git::diff::DiffRequest;
use crate::intent::AppIntent;

/// Report `DiffSlow` after [`DIFF_LOADING_DELAY`], so fast loads never
/// flash "Loading…".
pub(crate) fn loading_delay(tx: UnboundedSender<AppIntent>, tag: Tag) {
    tokio::spawn(async move {
        tokio::time::sleep(DIFF_LOADING_DELAY).await;
        let _ = tx.send(AppIntent::DiffSlow { tag });
    });
}

/// Read the patch `request` names off the event loop and report it as
/// `PatchLoaded`.
pub(crate) fn load_patch(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    request: DiffRequest,
) {
    background::run(
        tx,
        move || crate::git::diff::load_patch(runner.as_ref(), &request),
        // A panic still reports back, or the busy counter would stay raised.
        background::crashed("git diff"),
        move |result| AppIntent::PatchLoaded { tag, result },
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use crate::effect::{Effect, Runtime, Tag};
    use crate::git::diff::{DiffRequest, DiffSide};
    use crate::git::fake::{FakeRunner, ok};
    use crate::intent::AppIntent;
    use crate::op::{GitOp, run_op};
    use crate::test_support::HostilePanic;

    fn patch_request() -> DiffRequest {
        DiffRequest {
            path: "a.rs".into(),
            orig_path: None,
            side: DiffSide::Index,
            options: Default::default(),
            commit: None,
        }
    }

    #[tokio::test]
    async fn load_patch_reports_back_with_the_same_tag() {
        let runner = Arc::new(FakeRunner::new().expect(
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "-U3",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                "--cached",
                "--",
                "a.rs",
            ],
            ok(b"diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-a\n+b\n"),
        ));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 6,
            user: true,
        };

        runtime.spawn(Effect::LoadPatch {
            tag,
            request: patch_request(),
        });

        match rx.recv().await {
            Some(AppIntent::PatchLoaded {
                tag: got,
                result: Ok(patch),
            }) => {
                assert_eq!(got, tag);
                assert_eq!(patch.hunks.len(), 1);
            }
            other => panic!("expected a loaded patch, got {other:?}"),
        }
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_panicking_patch_load_still_reports_back() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(HostilePanic), tx);
        let tag = Tag {
            generation: 0,
            seq: 7,
            user: true,
        };

        runtime.spawn(Effect::LoadPatch {
            tag,
            request: patch_request(),
        });

        match rx.recv().await {
            Some(AppIntent::PatchLoaded {
                tag: got,
                result: Err(message),
            }) => {
                assert_eq!(got, tag);
                assert!(message.starts_with("git diff crashed: "), "{message:?}");
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed PatchLoaded, got {other:?}"),
        }
    }

    #[test]
    fn apply_cached_sends_the_patch_on_stdin() {
        for (reverse, argv) in [
            (
                false,
                &["apply", "--cached", "--whitespace=nowarn", "-"][..],
            ),
            (
                true,
                &["apply", "--cached", "--reverse", "--whitespace=nowarn", "-"][..],
            ),
        ] {
            let runner = FakeRunner::new().expect(argv, ok(b""));
            let op = GitOp::ApplyCached {
                patch: b"the patch\n".to_vec(),
                reverse,
            };
            assert_eq!(run_op(&runner, &op), Ok(()));
            runner.assert_done();
            assert_eq!(runner.inputs(), [Some(b"the patch\n".to_vec())]);
            assert_eq!(
                runner.envs(),
                [vec![
                    ("GIT_LITERAL_PATHSPECS".into(), "1".into()),
                    ("LC_ALL".into(), "C".into())
                ]]
            );
        }
    }

    /// Stage hunk 1 of a two-hunk file through the real patch engine and
    /// `git apply`, then unstage it again from the staged diff.
    fn stage_and_unstage_one_hunk(name: &str) {
        use crate::git::diff::{DiffRequest, DiffSide, load_patch};
        use crate::git::fake::{TempDir, git};
        use crate::git::patch::{Direction, PatchSelection, build};

        let tmp = TempDir::new("apply");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        let lines: Vec<String> = (1..=20).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.join(name), lines.concat()).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        let mut changed = lines.clone();
        changed[0] = "line 1a\nline 1b\n".into();
        changed[19] = "line twenty\n".into();
        std::fs::write(dir.join(name), changed.concat()).unwrap();
        let runner = crate::git::CliRunner::isolated(dir);
        let request = |side| DiffRequest {
            path: name.into(),
            orig_path: None,
            side,
            options: Default::default(),
            commit: None,
        };
        let cached = || String::from_utf8(git(dir, &["diff", "--cached"])).unwrap();
        let worktree = || String::from_utf8(git(dir, &["diff"])).unwrap();

        let patch = load_patch(&runner, &request(DiffSide::Worktree)).unwrap();
        let hunk = build(&patch, &PatchSelection::Hunk(1), Direction::Stage).unwrap();
        let stage = GitOp::ApplyCached {
            patch: hunk,
            reverse: false,
        };
        run_op(&runner, &stage).unwrap();
        let (index, work) = (cached(), worktree());
        assert!(
            index.contains("+line twenty") && !index.contains("+line 1a"),
            "{index}"
        );
        assert!(
            work.contains("+line 1a") && !work.contains("line twenty"),
            "{work}"
        );

        // Stage the rest too, then take only hunk 0 back out of the index.
        git(dir, &["add", "--", name]);
        let patch = load_patch(&runner, &request(DiffSide::Index)).unwrap();
        assert_eq!(patch.hunks.len(), 2);
        let hunk = build(&patch, &PatchSelection::Hunk(0), Direction::Unstage).unwrap();
        let unstage = GitOp::ApplyCached {
            patch: hunk,
            reverse: true,
        };
        run_op(&runner, &unstage).unwrap();
        let (index, work) = (cached(), worktree());
        assert!(
            index.contains("+line twenty") && !index.contains("+line 1a"),
            "{index}"
        );
        assert!(
            work.contains("+line 1a") && !work.contains("line twenty"),
            "{work}"
        );
    }

    #[test]
    fn a_built_hunk_stages_and_unstages_only_itself_in_real_git() {
        stage_and_unstage_one_hunk("a.txt");
    }

    /// git quotes this name in the diff header; the patch keeps it quoted.
    #[cfg(unix)]
    #[test]
    fn a_hunk_of_a_file_whose_name_needs_quoting_stages_in_real_git() {
        stage_and_unstage_one_hunk("tab\there \"q\".txt");
    }

    /// Stage one added line of a later hunk, then unstage one line from the
    /// staged view, each time through the real engine and `git apply`.
    #[test]
    fn single_lines_stage_and_unstage_alone_in_real_git() {
        use crate::git::diff::{DiffRequest, DiffSide, load_patch};
        use crate::git::fake::{TempDir, git};
        use crate::git::patch::{Direction, FilePatch, LineId, PatchSelection, build};

        /// The id of the one changed line whose text is `text`.
        fn id_of(patch: &FilePatch, text: &str) -> LineId {
            let wanted = format!("{text}\n");
            patch
                .hunks
                .iter()
                .flat_map(|h| h.lines.iter().zip(h.line_ids()))
                .find_map(|(line, id)| id.filter(|_| line.text == wanted.as_bytes()))
                .unwrap()
        }

        let tmp = TempDir::new("lines");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        let lines: Vec<String> = (1..=20).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.join("l.txt"), lines.concat()).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        let mut changed = lines.clone();
        changed[0] = "line 1a\nline 1b\n".into();
        changed[14] = "fifteen a\nfifteen b\n".into();
        std::fs::write(dir.join("l.txt"), changed.concat()).unwrap();
        let runner = crate::git::CliRunner::isolated(dir);
        let request = |side| DiffRequest {
            path: "l.txt".into(),
            orig_path: None,
            side,
            options: Default::default(),
            commit: None,
        };
        let apply = |patch: &FilePatch, text: &str, direction| {
            let selection = PatchSelection::Lines([id_of(patch, text)].into());
            let op = GitOp::ApplyCached {
                patch: build(patch, &selection, direction).unwrap(),
                reverse: direction == Direction::Unstage,
            };
            run_op(&runner, &op).unwrap();
        };
        let index = || String::from_utf8(git(dir, &["show", ":l.txt"])).unwrap();
        let with = |at: usize, text: &str| {
            let mut out = lines.clone();
            out[at] = text.into();
            out
        };

        let patch = load_patch(&runner, &request(DiffSide::Worktree)).unwrap();
        apply(&patch, "fifteen a", Direction::Stage);
        assert_eq!(index(), with(14, "line 15\nfifteen a\n").concat());
        let cached = String::from_utf8(git(dir, &["diff", "--cached"])).unwrap();
        let added: Vec<&str> = cached.lines().filter(|l| l.starts_with('+')).collect();
        assert_eq!(added, ["+++ b/l.txt", "+fifteen a"], "{cached}");
        assert!(!cached.lines().any(|l| l.starts_with("-l")), "{cached}");

        // Everything staged; take only "fifteen b" back out, past hunk 0.
        git(dir, &["add", "--", "l.txt"]);
        let patch = load_patch(&runner, &request(DiffSide::Index)).unwrap();
        assert_eq!(patch.hunks.len(), 2);
        apply(&patch, "fifteen b", Direction::Unstage);
        let mut expected = with(14, "fifteen a\n");
        expected[0] = "line 1a\nline 1b\n".into();
        assert_eq!(index(), expected.concat());
        let work = String::from_utf8(git(dir, &["diff"])).unwrap();
        let changes: Vec<&str> = work
            .lines()
            .filter(|l| l.starts_with(['+', '-']) && !l.starts_with("+++") && !l.starts_with("---"))
            .collect();
        assert_eq!(changes, ["+fifteen b"], "{work}");
    }

    /// Regression: a reverse patch must carry the index-side start of its
    /// hunk. The file holds identical blocks, and the first staged hunk adds
    /// exactly one block's worth of lines, so a start shifted as if for a
    /// forward apply lands on an identical copy and unstages that instead.
    #[test]
    fn unstaging_one_of_two_staged_hunks_next_to_repeated_blocks_takes_out_only_it() {
        use crate::git::diff::{DiffRequest, DiffSide, load_patch};
        use crate::git::fake::{TempDir, git};
        use crate::git::patch::{Direction, PatchSelection, build};

        let tmp = TempDir::new("reverse");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        let block = |middle: &str| format!("a\nb\nc\n{middle}\nd\ne\nf\n");
        let filler: String = (1..=8).map(|i| format!("filler {i}\n")).collect();
        let head = format!(
            "top\n{filler}{}{}{}{filler}",
            block("Y"),
            block("X"),
            block("Y")
        );
        std::fs::write(dir.join("r.txt"), &head).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        let inserted: String = (1..=7).map(|i| format!("new {i}\n")).collect();
        let changed = format!(
            "top\n{inserted}{filler}{}{}{}{filler}",
            block("Y"),
            block("Y"),
            block("Y")
        );
        std::fs::write(dir.join("r.txt"), &changed).unwrap();
        git(dir, &["add", "--", "r.txt"]);

        let runner = crate::git::CliRunner::isolated(dir);
        let request = DiffRequest {
            path: "r.txt".into(),
            orig_path: None,
            side: DiffSide::Index,
            options: Default::default(),
            commit: None,
        };
        let patch = load_patch(&runner, &request).unwrap();
        assert_eq!(patch.hunks.len(), 2);
        let hunk = build(&patch, &PatchSelection::Hunk(1), Direction::Unstage).unwrap();
        let unstage = GitOp::ApplyCached {
            patch: hunk,
            reverse: true,
        };
        run_op(&runner, &unstage).unwrap();

        let index = git(dir, &["show", ":r.txt"]);
        let only_first = format!(
            "top\n{inserted}{filler}{}{}{}{filler}",
            block("Y"),
            block("X"),
            block("Y")
        );
        assert_eq!(String::from_utf8(index).unwrap(), only_first);
        let work = String::from_utf8(git(dir, &["diff"])).unwrap();
        assert!(
            work.contains("-X\n+Y\n") && !work.contains("new 1"),
            "{work}"
        );
        let cached = String::from_utf8(git(dir, &["diff", "--cached"])).unwrap();
        assert!(
            cached.contains("+new 7") && !cached.contains("+Y") && !cached.contains("-Y"),
            "{cached}"
        );
    }

    #[test]
    fn apply_edited_recounts_and_sends_the_patch_on_stdin() {
        let runner = FakeRunner::new().expect(
            &["apply", "--cached", "--recount", "--whitespace=nowarn", "-"],
            ok(b""),
        );
        let op = GitOp::ApplyEdited {
            patch: b"the patch\n".to_vec(),
        };
        assert_eq!(run_op(&runner, &op), Ok(()));
        runner.assert_done();
        assert_eq!(runner.inputs(), [Some(b"the patch\n".to_vec())]);
        assert_eq!(
            runner.envs(),
            [vec![
                ("GIT_LITERAL_PATHSPECS".into(), "1".into()),
                ("LC_ALL".into(), "C".into())
            ]]
        );
    }

    /// `e.txt` committed as `one`, then `two` and `three` added after it.
    fn edit_repo() -> crate::git::fake::TempDir {
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("edited");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        std::fs::write(dir.join("e.txt"), "one\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        std::fs::write(dir.join("e.txt"), "one\ntwo\nthree\n").unwrap();
        tmp
    }

    #[test]
    fn an_edited_hunk_with_stale_counts_stages_exactly_what_is_left_in_real_git() {
        use crate::git::fake::git;

        let tmp = edit_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        // `+three` was deleted in the editor; the counts still say 3.
        let patch = b"diff --git a/e.txt b/e.txt\n--- a/e.txt\n+++ b/e.txt\n\
            @@ -1 +1,3 @@\n one\n+two\n"
            .to_vec();
        run_op(&runner, &GitOp::ApplyEdited { patch }).unwrap();
        assert_eq!(git(dir, &["show", ":e.txt"]), b"one\ntwo\n");
        assert_eq!(
            std::fs::read_to_string(dir.join("e.txt")).unwrap(),
            "one\ntwo\nthree\n"
        );
    }

    #[test]
    fn a_garbage_edit_fails_and_leaves_the_index_alone_in_real_git() {
        use crate::git::fake::git;

        let tmp = edit_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        let header = "diff --git a/e.txt b/e.txt\n--- a/e.txt\n+++ b/e.txt\n";
        for body in ["garbage\n", "@@ -1 +1,2 @@\n nope\n+two\n"] {
            let patch = format!("{header}{body}").into_bytes();
            let result = run_op(&runner, &GitOp::ApplyEdited { patch });
            assert!(result.is_err(), "{body:?}: {result:?}");
            assert_eq!(git(dir, &["diff", "--cached"]), b"", "{body:?}");
        }
    }
}
