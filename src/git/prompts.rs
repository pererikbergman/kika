//! Whether a git command may ask the user something on the terminal: a
//! signing passphrase, a hook that reads a reply or git's editor. Such a
//! command runs with the terminal handed over; the rest run detached.

use std::path::Path;

use super::diff::terminal_safe;
use super::ops::{GitOp, MergeStrategy};
use super::runner::{GitCommand, GitRunner};

/// The hooks a commit can run. `pre-auto-gc` is left out: it runs only when
/// a commit triggers a gc, which is rare and not worth a screen switch.
pub const COMMIT_HOOKS: [&str; 6] = [
    "pre-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "post-rewrite",
    "reference-transaction",
];

/// What the repository's config and hooks say about a commit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptFacts {
    /// `commit.gpgsign` is on.
    pub signs: bool,
    /// The names from [`COMMIT_HOOKS`] that exist and are executable.
    pub hooks: Vec<&'static str>,
}

/// Whether `op` should run on the real terminal. Only commits ever do.
/// `None` facts mean the probe failed: any doubt hands over, since handing
/// over always works and a detached prompt hangs. `hooks_detached` is the
/// user's word that their hooks never ask anything, so hooks alone keep a
/// commit detached; signing still hands over, as it may ask for a passphrase.
pub fn needs_terminal(op: &GitOp, facts: Option<&PromptFacts>, hooks_detached: bool) -> bool {
    let GitOp::Commit {
        amend, no_verify, ..
    } = op
    else {
        return false;
    };
    let Some(facts) = facts else {
        return true;
    };
    // Amending and `--no-edit` sign too, and `--no-verify` does not stop it.
    if facts.signs {
        return true;
    }
    if hooks_detached {
        return false;
    }
    facts.hooks.iter().any(|&hook| match hook {
        "pre-commit" | "commit-msg" => !no_verify,
        "post-rewrite" => *amend,
        _ => true,
    })
}

/// Whether `op` always runs with the terminal handed over: it may make
/// commits, so hooks, signing and git's editor for the message must work
/// as on the command line; the merge tool is interactive. A fast-forward,
/// a squash (which stops before committing), an abort, a checkout and a
/// reset make none, so they run detached.
pub fn runs_attached(op: &GitOp) -> bool {
    match op {
        GitOp::Merge { strategy, .. } => *strategy == MergeStrategy::NoFastForward,
        GitOp::Rebase { .. }
        | GitOp::InteractiveRebase(_)
        | GitOp::Revert(_)
        | GitOp::CherryPick(_)
        | GitOp::Continue(_)
        | GitOp::Skip(_) => true,
        // The merge tool is the user's own program on the terminal.
        GitOp::MergeTool(_) => true,
        _ => false,
    }
}

/// Read whether commits sign and which commit hooks are installed.
/// Blocking; run it from a task.
pub fn read_prompt_facts(runner: &dyn GitRunner) -> Result<PromptFacts, String> {
    let signs = read_gpgsign(runner)?;
    let hooks_dir = read_hooks_dir(runner)?;
    let hooks = COMMIT_HOOKS
        .into_iter()
        .filter(|name| is_executable(&hooks_dir.join(name)))
        .collect();
    Ok(PromptFacts { signs, hooks })
}

/// A read that takes no lock and whose messages are in English.
fn read_command(subcommand: &str) -> GitCommand {
    GitCommand::new(subcommand)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
}

fn read_gpgsign(runner: &dyn GitRunner) -> Result<bool, String> {
    let cmd = read_command("config")
        .arg("--type=bool")
        .arg("--get")
        .arg("commit.gpgsign");
    let output = runner.run(&cmd).map_err(|e| e.to_string())?;
    if output.success {
        return Ok(output.stdout.trim_ascii() == b"true");
    }
    // Unset is a failure with nothing on stderr.
    if output.stderr.is_empty() {
        return Ok(false);
    }
    Err(terminal_safe(&output.stderr))
}

/// Where git looks for hooks, `core.hooksPath` included, as an absolute path.
fn read_hooks_dir(runner: &dyn GitRunner) -> Result<std::path::PathBuf, String> {
    let cmd = read_command("rev-parse").arg("--git-path").arg("hooks");
    let output = runner.run(&cmd).map_err(|e| e.to_string())?;
    if !output.success {
        let message = terminal_safe(&output.stderr);
        return Err(if message.is_empty() {
            "git rev-parse --git-path hooks failed".into()
        } else {
            message
        });
    }
    let dir = path_from_bytes(output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout));
    // Relative paths are relative to where git ran.
    Ok(runner.workdir().join(dir))
}

#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::OsStr::from_bytes(bytes).into()
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: &[u8]) -> std::path::PathBuf {
    String::from_utf8_lossy(bytes).into_owned().into()
}

/// Whether git would run `path` as a hook: a file, executable on unix.
fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::repo_state::Operation;

    #[test]
    fn only_ops_that_can_commit_run_attached() {
        use crate::git::commit_files::CommitId;
        use crate::git::ops::ResetMode;
        let oid = || CommitId::parse("abc").unwrap();
        let stash = || crate::git::stash::StashRef {
            index: 0,
            oid: oid(),
        };
        let merge = |strategy| GitOp::Merge {
            branch: "b".into(),
            strategy,
        };
        let attached = [
            merge(MergeStrategy::NoFastForward),
            GitOp::Rebase { onto: "b".into() },
            GitOp::Continue(Operation::Merge),
            GitOp::Continue(Operation::Rebase),
            GitOp::Skip(Operation::CherryPick),
            GitOp::MergeTool("a.txt".into()),
            GitOp::Revert(oid()),
            GitOp::CherryPick(vec![oid()]),
            GitOp::Continue(Operation::Revert),
            GitOp::InteractiveRebase(crate::git::rebase_plan::sample()),
        ];
        for op in attached {
            assert!(runs_attached(&op), "{op:?}");
        }
        let detached = [
            merge(MergeStrategy::FastForwardOnly),
            merge(MergeStrategy::Squash),
            GitOp::Abort(Operation::Merge),
            GitOp::Abort(Operation::Rebase),
            GitOp::Checkout("b".into()),
            GitOp::CheckoutCommit(oid()),
            GitOp::Reset {
                to: oid(),
                mode: ResetMode::Hard,
            },
            GitOp::Abort(Operation::Revert),
            GitOp::StageAll,
            // A stash makes commits, but no hooks run for them.
            GitOp::StashPush {
                mode: crate::git::ops::StashMode::All,
                message: None,
                paths: vec![],
            },
            GitOp::StashApply(stash()),
            GitOp::StashPop(stash()),
            GitOp::StashDrop(stash()),
            GitOp::StashRename {
                stash: stash(),
                message: "m".into(),
            },
        ];
        for op in detached {
            assert!(!runs_attached(&op), "{op:?}");
        }
    }
    use crate::git::path::RepoPath;

    fn commit(amend: bool, no_verify: bool) -> GitOp {
        GitOp::Commit {
            message: Some("m\n".into()),
            stage_all: false,
            amend,
            no_verify,
        }
    }

    fn hooks(names: &[&'static str]) -> PromptFacts {
        PromptFacts {
            signs: false,
            hooks: names.to_vec(),
        }
    }

    fn all_commits() -> [GitOp; 4] {
        [
            commit(false, false),
            commit(true, false),
            commit(false, true),
            commit(true, true),
        ]
    }

    #[test]
    fn only_commits_ever_need_the_terminal() {
        let signing = PromptFacts {
            signs: true,
            hooks: COMMIT_HOOKS.to_vec(),
        };
        for op in [
            GitOp::StageAll,
            GitOp::UnstageAll,
            GitOp::Stage(vec![RepoPath::from("a")]),
            GitOp::SubmoduleUpdate(RepoPath::from("sub")),
            GitOp::SubmoduleInit(RepoPath::from("sub")),
        ] {
            assert!(!needs_terminal(&op, None, false), "{op:?}");
            assert!(!needs_terminal(&op, Some(&signing), false), "{op:?}");
        }
    }

    #[test]
    fn a_failed_probe_hands_over() {
        for op in all_commits() {
            assert!(needs_terminal(&op, None, false), "{op:?}");
        }
    }

    #[test]
    fn signing_hands_over_whatever_the_options() {
        let facts = PromptFacts {
            signs: true,
            hooks: vec![],
        };
        for op in all_commits() {
            assert!(needs_terminal(&op, Some(&facts), false), "{op:?}");
        }
    }

    #[test]
    fn pre_commit_and_commit_msg_hand_over_unless_hooks_are_skipped() {
        for hook in ["pre-commit", "commit-msg"] {
            let facts = hooks(&[hook]);
            assert!(
                needs_terminal(&commit(false, false), Some(&facts), false),
                "{hook}"
            );
            assert!(
                needs_terminal(&commit(true, false), Some(&facts), false),
                "{hook}"
            );
            assert!(
                !needs_terminal(&commit(false, true), Some(&facts), false),
                "{hook}"
            );
            assert!(
                !needs_terminal(&commit(true, true), Some(&facts), false),
                "{hook}"
            );
        }
    }

    #[test]
    fn hooks_that_no_verify_does_not_skip_always_hand_over() {
        for hook in ["prepare-commit-msg", "post-commit", "reference-transaction"] {
            let facts = hooks(&[hook]);
            for op in all_commits() {
                assert!(needs_terminal(&op, Some(&facts), false), "{hook} {op:?}");
            }
        }
    }

    #[test]
    fn post_rewrite_hands_over_only_when_amending() {
        let facts = hooks(&["post-rewrite"]);
        assert!(needs_terminal(&commit(true, false), Some(&facts), false));
        assert!(needs_terminal(&commit(true, true), Some(&facts), false));
        assert!(!needs_terminal(&commit(false, false), Some(&facts), false));
        assert!(!needs_terminal(&commit(false, true), Some(&facts), false));
    }

    #[test]
    fn no_signing_and_no_hooks_stays_detached() {
        for op in all_commits() {
            assert!(
                !needs_terminal(&op, Some(&PromptFacts::default()), false),
                "{op:?}"
            );
        }
    }

    #[test]
    fn hooks_detached_keeps_a_hook_only_commit_detached() {
        let facts = hooks(&["pre-commit", "commit-msg", "post-commit"]);
        let op = commit(false, false);
        assert!(!needs_terminal(&op, Some(&facts), true));
        assert!(needs_terminal(&op, Some(&facts), false));
        for op in all_commits() {
            assert!(
                !needs_terminal(&op, Some(&hooks(&COMMIT_HOOKS)), true),
                "{op:?}"
            );
        }
    }

    #[test]
    fn hooks_detached_still_hands_signing_over() {
        for facts in [
            PromptFacts {
                signs: true,
                hooks: vec![],
            },
            PromptFacts {
                signs: true,
                hooks: COMMIT_HOOKS.to_vec(),
            },
        ] {
            for op in all_commits() {
                assert!(needs_terminal(&op, Some(&facts), true), "{facts:?} {op:?}");
            }
        }
    }

    #[test]
    fn hooks_detached_still_hands_a_failed_probe_over_and_never_other_ops() {
        for op in all_commits() {
            assert!(needs_terminal(&op, None, true), "{op:?}");
        }
        assert!(!needs_terminal(&GitOp::StageAll, None, true));
        assert!(!needs_terminal(
            &GitOp::StageAll,
            Some(&hooks(&COMMIT_HOOKS)),
            true
        ));
    }

    use crate::git::CliRunner;
    use crate::git::fake::{FakeRunner, TempDir, fail, git, ok};

    fn repo(label: &str) -> TempDir {
        let tmp = TempDir::new(label);
        git(tmp.path(), &["init", "-q"]);
        tmp
    }

    #[cfg(unix)]
    fn write_hook(dir: &Path, name: &str, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let file = dir.join(name);
        std::fs::write(&file, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn unset_gpgsign_is_false_and_set_is_true_in_real_git() {
        let tmp = repo("prompts-sign");
        let runner = CliRunner::isolated(tmp.path());
        assert_eq!(read_prompt_facts(&runner), Ok(PromptFacts::default()));

        git(tmp.path(), &["config", "commit.gpgsign", "true"]);
        assert!(read_prompt_facts(&runner).unwrap().signs);
    }

    #[cfg(unix)]
    #[test]
    fn an_executable_hook_is_found_and_a_plain_file_is_not_in_real_git() {
        let tmp = repo("prompts-hooks");
        let hooks = tmp.path().join(".git/hooks");
        write_hook(&hooks, "pre-commit", 0o755);
        write_hook(&hooks, "commit-msg", 0o644);
        let runner = CliRunner::isolated(tmp.path());
        assert_eq!(read_prompt_facts(&runner).unwrap().hooks, ["pre-commit"]);
    }

    #[cfg(unix)]
    #[test]
    fn core_hooks_path_is_honoured_in_real_git() {
        let tmp = repo("prompts-hookspath");
        write_hook(&tmp.path().join(".git/hooks"), "pre-commit", 0o755);
        write_hook(&tmp.path().join("my-hooks"), "post-commit", 0o755);
        git(tmp.path(), &["config", "core.hooksPath", "my-hooks"]);
        let runner = CliRunner::isolated(tmp.path());
        assert_eq!(read_prompt_facts(&runner).unwrap().hooks, ["post-commit"]);
    }

    #[cfg(unix)]
    #[test]
    fn the_probe_reads_the_repository_the_runner_was_moved_to() {
        let first = repo("prompts-first");
        let second = repo("prompts-second");
        write_hook(&second.path().join(".git/hooks"), "commit-msg", 0o755);
        git(second.path(), &["config", "commit.gpgsign", "true"]);
        let runner = CliRunner::isolated(first.path());
        assert_eq!(read_prompt_facts(&runner), Ok(PromptFacts::default()));

        runner.set_workdir(second.path().into());

        assert_eq!(
            read_prompt_facts(&runner),
            Ok(PromptFacts {
                signs: true,
                hooks: vec!["commit-msg"],
            })
        );
    }

    #[test]
    fn both_reads_take_no_lock_and_any_other_config_failure_is_an_error() {
        let config = ["config", "--type=bool", "--get", "commit.gpgsign"];
        let runner = FakeRunner::new().expect(&config, fail("")).expect(
            &["rev-parse", "--git-path", "hooks"],
            ok(b"/nowhere/hooks\n"),
        );
        assert_eq!(read_prompt_facts(&runner), Ok(PromptFacts::default()));
        runner.assert_done();
        for env in runner.envs() {
            assert!(
                env.contains(&("GIT_OPTIONAL_LOCKS".into(), "0".into())),
                "{env:?}"
            );
            assert!(env.contains(&("LC_ALL".into(), "C".into())), "{env:?}");
        }

        let runner = FakeRunner::new().expect(&config, fail("fatal: bad boolean \x1b[2J\n"));
        assert_eq!(
            read_prompt_facts(&runner),
            Err("fatal: bad boolean \u{FFFD}[2J".into())
        );
    }
}
