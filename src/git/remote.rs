//! Fetch, pull and push the user asked for. Plain git commands, so their
//! upstream, `push.default`, `pull.rebase`, credential helpers, ssh-agent
//! and ssh config apply exactly as on the command line. Questions git or
//! ssh still have go to Kika's askpass bridge (see `git::askpass`), never
//! to the screen Kika owns.

use std::path::PathBuf;

use super::askpass::SOCKET_ENV;
use super::diff::terminal_safe;
use super::remotes::RemoteName;
use super::runner::{GitCommand, GitRunner};
use super::tags::TagName;

/// A network operation the user started with a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteOp {
    /// Every remote, like auto-fetch.
    Fetch,
    Pull,
    /// With `force_with_lease`, replace the remote's commits, but only if
    /// nobody pushed since the last fetch.
    Push {
        force_with_lease: bool,
    },
    /// One tag to one remote, under the same name. Never forced.
    PushTag {
        remote: RemoteName,
        tag: TagName,
    },
}

impl RemoteOp {
    /// What it is doing, for the status bar.
    pub fn doing(&self) -> &'static str {
        match self {
            RemoteOp::Fetch => "fetching…",
            RemoteOp::Pull => "pulling…",
            RemoteOp::Push { .. } => "pushing…",
            RemoteOp::PushTag { .. } => "pushing tag…",
        }
    }

    /// What the error popup calls its failure.
    pub fn failure_title(&self) -> &'static str {
        match self {
            RemoteOp::Fetch => "Fetch failed",
            RemoteOp::Pull => "Pull failed",
            RemoteOp::Push { .. } => "Push failed",
            RemoteOp::PushTag { .. } => "Push tag failed",
        }
    }
}

/// Where git and ssh find the askpass bridge.
// Only the unix bridge makes one.
#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskpassEnv {
    /// Kika's own binary, which acts as the askpass program.
    pub program: PathBuf,
    pub socket: PathBuf,
    /// Whether to set `GIT_ASKPASS`; off when the user has their own.
    pub git_askpass: bool,
}

/// Why a remote operation failed. Messages are terminal-safe with any
/// credentials in URLs taken out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteFailure {
    /// A plain push the remote refused because it has commits this branch
    /// does not; a push with `--force-with-lease` may replace them.
    Rejected(String),
    Failed(String),
}

/// The command for `op`. Git never prompts on the terminal; with
/// `askpass`, git and ssh ask through the bridge, and without it (no
/// bridge on this platform, or it failed to start) they fail instead of
/// asking. Runs away from the terminal, so an ssh too old for
/// `SSH_ASKPASS_REQUIRE` cannot read it either.
///
/// A tag goes as `refs/tags/<name>:refs/tags/<name>`, which starts with
/// `refs/` and so is never an option. The remote's name comes from the
/// config, which can be edited by hand: [`run_remote`] refuses one that
/// starts with `-` before this is built.
pub fn remote_command(op: &RemoteOp, askpass: Option<&AskpassEnv>) -> GitCommand {
    let cmd = match op {
        RemoteOp::Fetch => GitCommand::new("fetch").arg("--all"),
        RemoteOp::Pull => GitCommand::new("pull").env("GIT_MERGE_AUTOEDIT", "no"),
        RemoteOp::Push {
            force_with_lease: false,
        } => GitCommand::new("push"),
        RemoteOp::Push {
            force_with_lease: true,
        } => GitCommand::new("push").arg("--force-with-lease"),
        RemoteOp::PushTag { remote, tag } => {
            let full = tag.full_ref();
            let mut refspec = full.clone();
            refspec.push(":");
            refspec.push(&full);
            GitCommand::new("push")
                .arg(remote.as_os_string())
                .arg(refspec)
        }
    };
    with_prompts(cmd, askpass)
}

/// `cmd` set up to talk to a remote as [`remote_command`] does: never on
/// the terminal, and with git's and ssh's questions going to `askpass`
/// when there is one.
pub(crate) fn with_prompts(cmd: GitCommand, askpass: Option<&AskpassEnv>) -> GitCommand {
    let cmd = cmd
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .without_terminal();
    let Some(askpass) = askpass else {
        // As auto-fetch: ssh's askpass is `false`, so ssh gives up at once.
        return cmd
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("SSH_ASKPASS", "false");
    };
    let cmd = if askpass.git_askpass {
        cmd.env("GIT_ASKPASS", &askpass.program)
    } else {
        cmd
    };
    cmd.env("SSH_ASKPASS", &askpass.program)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env(SOCKET_ENV, &askpass.socket)
}

/// Run `op`. A plain push the remote rejects as behind is `Rejected`, so
/// the user can be offered `--force-with-lease`; a rejected tag push is
/// only ever `Failed`.
pub fn run_remote(
    runner: &dyn GitRunner,
    op: &RemoteOp,
    askpass: Option<&AskpassEnv>,
) -> Result<(), RemoteFailure> {
    run_remote_with(runner, op, askpass, &|key| std::env::var_os(key).is_some())
}

/// [`run_remote`], with `env_set` saying which variables Kika's own
/// environment sets, so tests do not depend on the host's.
fn run_remote_with(
    runner: &dyn GitRunner,
    op: &RemoteOp,
    askpass: Option<&AskpassEnv>,
    env_set: &dyn Fn(&str) -> bool,
) -> Result<(), RemoteFailure> {
    if let RemoteOp::PushTag { remote, .. } = op
        && remote.0.starts_with(b"-")
    {
        return Err(RemoteFailure::Failed(
            "Push to a remote named like an option with git directly".into(),
        ));
    }
    let askpass = askpass_for(runner, askpass, env_set);
    let cmd = remote_command(op, askpass.as_ref());
    let output = runner
        .run(&cmd)
        .map_err(|e| RemoteFailure::Failed(safe(e.to_string().as_bytes())))?;
    if output.success {
        return Ok(());
    }
    let message = safe(&output.stderr);
    let message = if message.is_empty() {
        let subcommand = cmd.args()[0].to_string_lossy();
        format!("git {subcommand} failed")
    } else {
        message
    };
    let behind = message.contains("[rejected]")
        && (message.contains("(non-fast-forward)") || message.contains("(fetch first)"));
    if behind
        && *op
            == (RemoteOp::Push {
                force_with_lease: false,
            })
    {
        Err(RemoteFailure::Rejected(message))
    } else {
        Err(RemoteFailure::Failed(message))
    }
}

/// The bridge as a remote command uses it: without `GIT_ASKPASS` when the
/// user has their own askpass program for git.
pub(crate) fn askpass_for(
    runner: &dyn GitRunner,
    askpass: Option<&AskpassEnv>,
    env_set: &dyn Fn(&str) -> bool,
) -> Option<AskpassEnv> {
    askpass.map(|env| AskpassEnv {
        git_askpass: env.git_askpass && !user_askpass(runner, env_set),
        ..env.clone()
    })
}

/// Whether the user chose their own askpass program for git, which
/// [`remote_command`] then leaves in place. Any doubt, such as a failed
/// config read, keeps theirs.
fn user_askpass(runner: &dyn GitRunner, env_set: &dyn Fn(&str) -> bool) -> bool {
    if env_set("GIT_ASKPASS") {
        return true;
    }
    let cmd = GitCommand::new("config")
        .arg("--get")
        .arg("core.askPass")
        .env("LC_ALL", "C");
    // Unset is exit 1 with nothing on stderr.
    !matches!(runner.run(&cmd), Ok(out) if !out.success && out.stderr.is_empty())
}

/// Git's words, safe to draw and with no credentials in them.
pub(crate) fn safe(bytes: &[u8]) -> String {
    redact_userinfo(&terminal_safe(bytes))
}

/// `text` with the user part of every URL taken out: `scheme://user:pass@host`
/// becomes `scheme://host`. Git quotes remote URLs in its errors, and a URL
/// can carry a token. The authority ends only at the path or a line break,
/// and everything up to its last `@` goes. Not at `?`, `#`, a quote or a
/// space, so none of them inside a password can cut the redaction short;
/// text after a URL with no path (its query included) can lose words up to
/// a later `@`, which is the safe way to be wrong.
pub fn redact_userinfo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (before, after) = rest.split_at(at + 3);
        out.push_str(before);
        let end = after.find(['/', '\n', '\r']).unwrap_or(after.len());
        let authority = &after[..end];
        let host = authority.rfind('@').map_or(0, |i| i + 1);
        rest = &after[host..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::git::fake::{FakeRunner, fail, ok};

    const ASKPASS_UNSET: &[&str] = &["config", "--get", "core.askPass"];

    fn bridge() -> AskpassEnv {
        AskpassEnv {
            program: "/opt/my apps/kika".into(),
            socket: "/tmp/kika-askpass-1-0/s".into(),
            git_askpass: true,
        }
    }

    fn env(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    }

    /// The env of the op's run, the last call made.
    fn op_env(fake: &FakeRunner) -> Vec<(OsString, OsString)> {
        fake.envs().last().cloned().unwrap()
    }

    #[test]
    fn each_op_is_plain_git_with_the_bridge_in_its_env() {
        /// An op, its argv, and the env it adds before the shared part.
        type Case = (
            RemoteOp,
            &'static [&'static str],
            &'static [(&'static str, &'static str)],
        );
        let cases: [Case; 4] = [
            (RemoteOp::Fetch, &["fetch", "--all"], &[]),
            (RemoteOp::Pull, &["pull"], &[("GIT_MERGE_AUTOEDIT", "no")]),
            (
                RemoteOp::Push {
                    force_with_lease: false,
                },
                &["push"],
                &[],
            ),
            (
                RemoteOp::Push {
                    force_with_lease: true,
                },
                &["push", "--force-with-lease"],
                &[],
            ),
        ];
        for (op, argv, extra) in cases {
            let fake = FakeRunner::new()
                .expect(ASKPASS_UNSET, fail(""))
                .expect(argv, ok(b""));

            assert_eq!(
                run_remote_with(&fake, &op, Some(&bridge()), &|_| false),
                Ok(())
            );

            fake.assert_done();
            let mut expected = env(extra);
            expected.extend(env(&[
                ("LC_ALL", "C"),
                ("GIT_TERMINAL_PROMPT", "0"),
                ("GIT_ASKPASS", "/opt/my apps/kika"),
                ("SSH_ASKPASS", "/opt/my apps/kika"),
                ("SSH_ASKPASS_REQUIRE", "force"),
                ("KIKA_ASKPASS_SOCKET", "/tmp/kika-askpass-1-0/s"),
            ]));
            assert_eq!(op_env(&fake), expected, "{op:?}");
            // Nothing is ever typed into git.
            assert_eq!(fake.inputs().last(), Some(&None));
        }
    }

    #[test]
    fn every_op_runs_away_from_the_terminal() {
        for op in [
            RemoteOp::Fetch,
            RemoteOp::Pull,
            RemoteOp::Push {
                force_with_lease: false,
            },
        ] {
            assert!(remote_command(&op, Some(&bridge())).is_without_terminal());
            assert!(remote_command(&op, None).is_without_terminal());
        }
    }

    #[test]
    fn the_users_own_git_askpass_is_kept() {
        let without_git_askpass =
            |fake: &FakeRunner| !op_env(fake).iter().any(|(k, _)| k == "GIT_ASKPASS");

        // Set in the config.
        let fake = FakeRunner::new()
            .expect(ASKPASS_UNSET, ok(b"my-askpass\n"))
            .expect(&["fetch", "--all"], ok(b""));
        assert_eq!(
            run_remote_with(&fake, &RemoteOp::Fetch, Some(&bridge()), &|_| false),
            Ok(())
        );
        fake.assert_done();
        assert!(without_git_askpass(&fake));
        // ssh still asks through the bridge.
        assert!(op_env(&fake).iter().any(|(k, _)| k == "SSH_ASKPASS"));

        // Set in the environment: the config is not even read.
        let fake = FakeRunner::new().expect(&["fetch", "--all"], ok(b""));
        let env_set = |key: &str| key == "GIT_ASKPASS";
        assert_eq!(
            run_remote_with(&fake, &RemoteOp::Fetch, Some(&bridge()), &env_set),
            Ok(())
        );
        fake.assert_done();
        assert!(without_git_askpass(&fake));

        // A config that cannot be read is no reason to override it.
        let fake = FakeRunner::new()
            .expect(ASKPASS_UNSET, fail("error: bad config\n"))
            .expect(&["fetch", "--all"], ok(b""));
        run_remote_with(&fake, &RemoteOp::Fetch, Some(&bridge()), &|_| false).unwrap();
        assert!(without_git_askpass(&fake));
    }

    #[test]
    fn the_core_ask_pass_check_works_on_real_git() {
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("askpass-config");
        git(tmp.path(), &["init", "-q"]);
        let runner = crate::git::CliRunner::isolated(tmp.path());
        assert!(!user_askpass(&runner, &|_| false));
        git(tmp.path(), &["config", "core.askPass", "my-askpass"]);
        assert!(user_askpass(&runner, &|_| false));
    }

    #[test]
    fn without_the_bridge_git_and_ssh_fail_instead_of_asking() {
        let fake = FakeRunner::new().expect(&["push"], ok(b""));
        let push = RemoteOp::Push {
            force_with_lease: false,
        };

        assert_eq!(run_remote_with(&fake, &push, None, &|_| false), Ok(()));

        fake.assert_done();
        assert_eq!(
            op_env(&fake),
            env(&[
                ("LC_ALL", "C"),
                ("GIT_TERMINAL_PROMPT", "0"),
                ("SSH_ASKPASS_REQUIRE", "force"),
                ("SSH_ASKPASS", "false"),
            ])
        );
    }

    fn push(force_with_lease: bool, stderr: &str) -> Result<(), RemoteFailure> {
        let argv: &[&str] = if force_with_lease {
            &["push", "--force-with-lease"]
        } else {
            &["push"]
        };
        let fake = FakeRunner::new().expect(argv, fail(stderr));
        run_remote_with(&fake, &RemoteOp::Push { force_with_lease }, None, &|_| {
            false
        })
    }

    #[test]
    fn a_push_behind_the_remote_is_rejected_so_a_lease_can_be_offered() {
        let fetch_first = "To /r.git\n ! [rejected]        main -> main (fetch first)\n\
                           error: failed to push some refs to '/r.git'\n";
        let non_ff = " ! [rejected]        main -> main (non-fast-forward)\n";
        for stderr in [fetch_first, non_ff] {
            assert!(
                matches!(push(false, stderr), Err(RemoteFailure::Rejected(_))),
                "{stderr}"
            );
            // A lease that is refused again is just a failure.
            assert!(matches!(push(true, stderr), Err(RemoteFailure::Failed(_))));
        }
    }

    #[test]
    fn a_stale_lease_or_a_remote_refusal_is_a_failure_with_no_offer() {
        let stale = " ! [rejected]        main -> main (stale info)\n";
        let remote = " ! [remote rejected] main -> main (pre-receive hook declined)\n";
        for stderr in [stale, remote] {
            assert_eq!(
                push(false, stderr),
                Err(RemoteFailure::Failed(stderr.trim().to_string()))
            );
        }
    }

    #[test]
    fn a_failure_is_safe_to_draw_and_carries_no_credentials() {
        let fake = FakeRunner::new().expect(
            &["fetch", "--all"],
            fail("fatal: unable to access 'https://u:tok@h/r/': \x1b[31mdenied\n"),
        );
        assert_eq!(
            run_remote_with(&fake, &RemoteOp::Fetch, None, &|_| false),
            Err(RemoteFailure::Failed(
                "fatal: unable to access 'https://h/r/': \u{FFFD}[31mdenied".into()
            ))
        );

        let fake = FakeRunner::new().expect(&["pull"], fail(""));
        assert_eq!(
            run_remote_with(&fake, &RemoteOp::Pull, None, &|_| false),
            Err(RemoteFailure::Failed("git pull failed".into()))
        );
    }

    fn push_tag(remote: &str, tag: &[u8]) -> RemoteOp {
        RemoteOp::PushTag {
            remote: remote.into(),
            tag: TagName::from_bytes(tag),
        }
    }

    #[test]
    fn a_tag_push_names_the_remote_and_the_full_ref_on_both_sides() {
        let argv = ["push", "origin", "refs/tags/v1:refs/tags/v1"];
        let fake = FakeRunner::new()
            .expect(ASKPASS_UNSET, fail(""))
            .expect(&argv, ok(b""));
        let op = push_tag("origin", b"v1");

        assert_eq!(
            run_remote_with(&fake, &op, Some(&bridge()), &|_| false),
            Ok(())
        );

        fake.assert_done();
        assert_eq!(
            op_env(&fake),
            env(&[
                ("LC_ALL", "C"),
                ("GIT_TERMINAL_PROMPT", "0"),
                ("GIT_ASKPASS", "/opt/my apps/kika"),
                ("SSH_ASKPASS", "/opt/my apps/kika"),
                ("SSH_ASKPASS_REQUIRE", "force"),
                ("KIKA_ASKPASS_SOCKET", "/tmp/kika-askpass-1-0/s"),
            ])
        );
        assert!(remote_command(&op, None).is_without_terminal());
        // A hand-made tag named like an option stays inside the refspec.
        let odd = remote_command(&push_tag("origin", b"-f"), None);
        assert_eq!(odd.args()[2], "refs/tags/-f:refs/tags/-f");
    }

    #[test]
    fn a_tag_push_to_a_remote_named_like_an_option_never_runs() {
        let fake = FakeRunner::new();
        let result = run_remote_with(&fake, &push_tag("-x", b"v1"), None, &|_| false);
        assert!(
            matches!(result, Err(RemoteFailure::Failed(_))),
            "{result:?}"
        );
        fake.assert_done();
    }

    #[test]
    fn a_rejected_tag_push_is_a_failure_with_no_offer_to_force() {
        let argv = ["push", "origin", "refs/tags/v1:refs/tags/v1"];
        for stderr in [
            " ! [rejected]        v1 -> v1 (already exists)\n",
            " ! [rejected]        v1 -> v1 (non-fast-forward)\n",
            " ! [rejected]        v1 -> v1 (fetch first)\n",
        ] {
            let fake = FakeRunner::new().expect(&argv, fail(stderr));
            let result = run_remote_with(&fake, &push_tag("origin", b"v1"), None, &|_| false);
            assert!(
                matches!(result, Err(RemoteFailure::Failed(_))),
                "{stderr}: {result:?}"
            );
        }
        let op = push_tag("origin", b"v1");
        assert_eq!(op.doing(), "pushing tag…");
        assert_eq!(op.failure_title(), "Push tag failed");
    }

    #[test]
    fn redact_userinfo_takes_the_user_part_out_of_every_url() {
        let redacted = redact_userinfo("fatal: unable to access 'https://u:tok@h/r/'");
        assert!(
            !redacted.contains("u:tok") && !redacted.contains("tok"),
            "{redacted}"
        );
        assert_eq!(redacted, "fatal: unable to access 'https://h/r/'");

        assert_eq!(
            redact_userinfo("from https://ghp_x@github.com/a and http://a:b@c:8080"),
            "from https://github.com/a and http://c:8080"
        );
        // A password with an `@` or a quote in it goes too.
        assert_eq!(redact_userinfo("ssh://u:p@ss@h/r"), "ssh://h/r");
        assert_eq!(
            redact_userinfo("unable to access 'https://u:to'k@h/r/'"),
            "unable to access 'https://h/r/'"
        );
        assert_eq!(redact_userinfo("\"https://u:p\"w@h?x\""), "\"https://h?x\"");
        // Whitespace inside a password, as typed or as a tab drawn as spaces.
        assert_eq!(redact_userinfo("https://u:my pass@h/r"), "https://h/r");
        assert_eq!(redact_userinfo("https://u:a\tb@h/r"), "https://h/r");
        assert_eq!(redact_userinfo("https://u:a    b@h"), "https://h");
        // A URL ends at a line break, so the next line keeps its `@`.
        assert_eq!(
            redact_userinfo("'https://h'\nhint: try git@x"),
            "'https://h'\nhint: try git@x"
        );
        // Nothing to take out.
        for text in ["git@github.com:a/b.git", "https://h/r", "no url", "a://"] {
            assert_eq!(redact_userinfo(text), text);
        }
    }

    #[test]
    fn redact_userinfo_removes_a_password_holding_url_syntax_whole() {
        for (url, password) in [
            ("https://u:p?ss@h/r", "p?ss"),
            ("https://u:p#ss@h/r", "p#ss"),
            ("https://u:my pass@h/r", "my pass"),
            ("https://u:to'k@h/r", "to'k"),
            ("https://u:to\"k@h/r", "to\"k"),
            ("ssh://u:p@ss@h/r", "p@ss"),
        ] {
            let redacted = redact_userinfo(url);
            let (scheme, rest) = redacted.split_once("://").unwrap();
            assert_eq!(redacted, format!("{scheme}://h/r"), "{url}");
            for part in password.split(['?', '#', ' ', '\'', '"', '@']) {
                assert!(!rest.contains(part), "{url} -> {redacted}");
            }
        }
    }

    #[test]
    fn each_op_names_what_it_does_and_how_it_failed() {
        let push = RemoteOp::Push {
            force_with_lease: true,
        };
        assert_eq!(RemoteOp::Fetch.doing(), "fetching…");
        assert_eq!(RemoteOp::Pull.doing(), "pulling…");
        assert_eq!(push.doing(), "pushing…");
        assert_eq!(RemoteOp::Fetch.failure_title(), "Fetch failed");
        assert_eq!(RemoteOp::Pull.failure_title(), "Pull failed");
        assert_eq!(push.failure_title(), "Push failed");
    }
}
