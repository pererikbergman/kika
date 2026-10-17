//! The URL of the remote whose forge repository the branches belong to:
//! `origin`, or the only remote when there is no `origin`. Also each local
//! branch's upstream and the URL of its remote, so a badge can follow the
//! branch a pull request was opened from.

use std::collections::HashMap;

use super::branches::BranchName;
use super::path::os_string;
use super::remotes::RemoteName;
use super::runner::{GitCommand, GitRunner};

/// The forge remote as Kika may use it: its one URL, or only its name
/// when it has several. A remote with several URLs is used for no forge
/// lookup or fetch, since git fetches from the first while a plain
/// `--get` reads the last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeRemote {
    One { name: RemoteName, url: Vec<u8> },
    Several(RemoteName),
}

/// The forge remote and every URL it has, read once with `--get-all`.
/// `None` when no remote qualifies, git fails, or the URL is empty.
pub fn read_forge_remote(runner: &dyn GitRunner) -> Option<ForgeRemote> {
    let name = remote_name(runner)?;
    let output = runner.run(&url_command(&name, "--get-all")).ok()?;
    if !output.success {
        return None;
    }
    let body = output.stdout.strip_suffix(b"\0").unwrap_or(&output.stdout);
    let mut urls = body.split(|&b| b == 0);
    let name = RemoteName::from_bytes(&name);
    match (urls.next(), urls.next()) {
        (Some(url), None) if !url.is_empty() => Some(ForgeRemote::One {
            name,
            url: url.to_vec(),
        }),
        (Some(_), Some(_)) => Some(ForgeRemote::Several(name)),
        _ => None,
    }
}

/// Where a local branch's upstream lives: the remote's name, the ref on
/// that remote (`refs/heads/...` for a branch), and the remote's URL when
/// `git remote` lists it and it has one. Raw bytes, as git gave them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    pub remote: Vec<u8>,
    pub merge: Vec<u8>,
    pub url: Option<Vec<u8>>,
}

/// The upstream of each local branch that tracks a remote one. A branch
/// with no upstream, or one on the local repository (`.`), is left out.
/// Each remote's URL is read once, and only for a remote `git remote`
/// lists, so no other text reaches a config key. Empty when git fails.
pub fn read_upstreams(runner: &dyn GitRunner) -> HashMap<BranchName, Upstream> {
    let cmd = upstreams_command();
    let Some(output) = runner.run(&cmd).ok().filter(|o| o.success) else {
        return HashMap::new();
    };
    let rows: Vec<(BranchName, &[u8], &[u8])> = output
        .stdout
        .split(|&b| b == b'\n')
        .filter_map(|row| {
            let mut fields = row.split(|&b| b == 0);
            let refname = fields.next()?;
            let (remote, merge) = (fields.next()?, fields.next()?);
            let name = refname.strip_prefix(b"refs/heads/")?;
            let tracked = !name.is_empty() && !remote.is_empty() && remote != b".";
            tracked.then(|| (BranchName::from_bytes(name), remote, merge))
        })
        .collect();
    if rows.is_empty() {
        return HashMap::new();
    }
    let listed = list_remotes(runner).unwrap_or_default();
    let mut urls: HashMap<&[u8], Option<Vec<u8>>> = HashMap::new();
    for (_, remote, _) in &rows {
        if !urls.contains_key(remote) {
            let url = listed
                .iter()
                .any(|name| name == remote)
                .then(|| read_url(runner, remote))
                .flatten();
            urls.insert(remote, url);
        }
    }
    rows.into_iter()
        .map(|(branch, remote, merge)| {
            let upstream = Upstream {
                remote: remote.to_vec(),
                merge: merge.to_vec(),
                url: urls.get(remote).cloned().flatten(),
            };
            (branch, upstream)
        })
        .collect()
}

/// Every local branch with its upstream. Sets none of the standard vars,
/// as it always has.
fn upstreams_command() -> GitCommand {
    GitCommand::new("for-each-ref")
        .arg(UPSTREAM_FORMAT)
        .arg("refs/heads")
        .user_env()
}

/// Each branch's ref, its upstream's remote and the upstream's name there.
const UPSTREAM_FORMAT: &str =
    "--format=%(refname)%00%(upstream:remotename)%00%(upstream:remoteref)";

/// The argv [`read_upstreams`] runs first, for tests that script it.
#[cfg(any(test, feature = "test-support"))]
pub const UPSTREAMS_ARGV: [&str; 3] = ["for-each-ref", UPSTREAM_FORMAT, "refs/heads"];

/// `remote.<name>.url`, or `None` when unset or git fails.
fn read_url(runner: &dyn GitRunner, name: &[u8]) -> Option<Vec<u8>> {
    let output = runner.run(&url_command(name, "--get")).ok()?;
    if !output.success {
        return None;
    }
    let url = output.stdout.strip_suffix(b"\0").unwrap_or(&output.stdout);
    (!url.is_empty()).then(|| url.to_vec())
}

/// The names `git remote` lists.
fn list_remotes(runner: &dyn GitRunner) -> Option<Vec<Vec<u8>>> {
    let listed = runner.run(&remotes_command()).ok()?;
    if !listed.success {
        return None;
    }
    Some(
        listed
            .stdout
            .split(|&b| b == b'\n')
            .filter(|name| !name.is_empty())
            .map(<[u8]>::to_vec)
            .collect(),
    )
}

/// `git remote`, in the user's own env like the other reads here.
fn remotes_command() -> GitCommand {
    GitCommand::new("remote").user_env()
}

/// `origin`, or the only remote there is.
fn remote_name(runner: &dyn GitRunner) -> Option<Vec<u8>> {
    let names = list_remotes(runner)?;
    let names: Vec<&[u8]> = names.iter().map(Vec::as_slice).collect();
    match names.as_slice() {
        _ if names.contains(&&b"origin"[..]) => Some(b"origin".to_vec()),
        [only] => Some(only.to_vec()),
        _ => None,
    }
}

/// `git config -z <get> remote.<name>.url`.
fn url_command(name: &[u8], get: &str) -> GitCommand {
    let mut key = b"remote.".to_vec();
    key.extend_from_slice(name);
    key.extend_from_slice(b".url");
    GitCommand::new("config")
        .arg("-z")
        .arg(get)
        .arg(os_string(&key))
        .user_env()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeRunner, fail, ok};
    use crate::runner::EnvProfile;

    const GET_ALL_ORIGIN: [&str; 4] = ["config", "-z", "--get-all", "remote.origin.url"];

    fn one(name: &str, url: &str) -> Option<ForgeRemote> {
        Some(ForgeRemote::One {
            name: RemoteName::from(name),
            url: url.into(),
        })
    }

    #[test]
    fn origin_is_used_when_present() {
        let fake = FakeRunner::new()
            .expect(&["remote"], ok(b"fork\norigin\n"))
            .expect(&GET_ALL_ORIGIN, ok(b"git@github.com:o/r.git\0"));
        assert_eq!(
            read_forge_remote(&fake),
            one("origin", "git@github.com:o/r.git")
        );
        fake.assert_done();
        // These reads always ran in the user's own env.
        assert_eq!(fake.process_envs(), [vec![], vec![]]);
        for cmd in [
            remotes_command(),
            upstreams_command(),
            url_command(b"origin", "--get-all"),
            url_command(b"origin", "--get"),
        ] {
            assert_eq!(cmd.env_profile(), Some(EnvProfile::UserEnv), "{cmd:?}");
        }
    }

    #[test]
    fn a_remote_with_several_urls_is_several_in_either_order() {
        // The FakeRunner fails on any other command, so only `--get-all`
        // is read: never `--get`, which gives the last URL.
        for urls in [
            &b"https://github.com/me/mirror.git\0https://github.com/o/r.git\0"[..],
            &b"https://github.com/o/r.git\0https://github.com/me/mirror.git\0"[..],
        ] {
            let fake = FakeRunner::new()
                .expect(&["remote"], ok(b"origin\n"))
                .expect(&GET_ALL_ORIGIN, ok(urls));
            assert_eq!(
                read_forge_remote(&fake),
                Some(ForgeRemote::Several(RemoteName::from("origin")))
            );
            fake.assert_done();
        }
    }

    #[test]
    fn the_only_remote_stands_in_for_origin() {
        let fake = FakeRunner::new()
            .expect(&["remote"], ok(b"upstream\n"))
            .expect(
                &["config", "-z", "--get-all", "remote.upstream.url"],
                ok(b"https://github.com/o/r\0"),
            );
        assert_eq!(
            read_forge_remote(&fake),
            one("upstream", "https://github.com/o/r")
        );
    }

    #[test]
    fn several_remotes_without_origin_or_none_give_nothing() {
        let fake = FakeRunner::new().expect(&["remote"], ok(b"a\nb\n"));
        assert_eq!(read_forge_remote(&fake), None);
        let fake = FakeRunner::new().expect(&["remote"], ok(b""));
        assert_eq!(read_forge_remote(&fake), None);
        let fake = FakeRunner::new().expect(&["remote"], fail("not a repo"));
        assert_eq!(read_forge_remote(&fake), None);
        let fake = FakeRunner::new()
            .expect(&["remote"], ok(b"origin\n"))
            .expect(&GET_ALL_ORIGIN, fail(""));
        assert_eq!(read_forge_remote(&fake), None);
        // An empty URL is no URL.
        let fake = FakeRunner::new()
            .expect(&["remote"], ok(b"origin\n"))
            .expect(&GET_ALL_ORIGIN, ok(b"\0"));
        assert_eq!(read_forge_remote(&fake), None);
    }

    const UPSTREAMS: [&str; 3] = UPSTREAMS_ARGV;

    fn upstream(remote: &str, merge: &str, url: Option<&str>) -> Upstream {
        Upstream {
            remote: remote.into(),
            merge: merge.into(),
            url: url.map(Into::into),
        }
    }

    #[test]
    fn upstreams_are_read_from_nul_separated_rows_with_each_url_once() {
        let rows = b"refs/heads/wip\0origin\0refs/heads/feature-x\n\
            refs/heads/fix\0fork\0refs/heads/fix\n\
            refs/heads/main\0origin\0refs/heads/main\n\
            refs/heads/local\0.\0refs/heads/main\n\
            refs/heads/none\0\0\n";
        let fake = FakeRunner::new()
            .expect(&UPSTREAMS, ok(rows))
            .expect(&["remote"], ok(b"fork\norigin\n"))
            .expect(
                &["config", "-z", "--get", "remote.origin.url"],
                ok(b"git@github.com:o/r.git\0"),
            )
            .expect(
                &["config", "-z", "--get", "remote.fork.url"],
                ok(b"https://github.com/me/r\0"),
            );
        let upstreams = read_upstreams(&fake);
        fake.assert_done();
        let origin = Some("git@github.com:o/r.git");
        assert_eq!(
            upstreams,
            HashMap::from([
                (
                    BranchName::from("wip"),
                    upstream("origin", "refs/heads/feature-x", origin)
                ),
                (
                    BranchName::from("fix"),
                    upstream("fork", "refs/heads/fix", Some("https://github.com/me/r"))
                ),
                (
                    BranchName::from("main"),
                    upstream("origin", "refs/heads/main", origin)
                ),
            ])
        );
    }

    #[test]
    fn a_remote_git_does_not_list_gets_no_config_read() {
        // A name git does not list never reaches a config key.
        let fake = FakeRunner::new()
            .expect(&UPSTREAMS, ok(b"refs/heads/a\0gone\0refs/heads/a\n"))
            .expect(&["remote"], ok(b"origin\n"));
        assert_eq!(
            read_upstreams(&fake),
            HashMap::from([(
                BranchName::from("a"),
                upstream("gone", "refs/heads/a", None)
            )])
        );
        fake.assert_done();
        // A URL that cannot be read is no URL.
        let fake = FakeRunner::new()
            .expect(&UPSTREAMS, ok(b"refs/heads/a\0origin\0refs/heads/a\n"))
            .expect(&["remote"], ok(b"origin\n"))
            .expect(&["config", "-z", "--get", "remote.origin.url"], fail(""));
        assert_eq!(read_upstreams(&fake)[&BranchName::from("a")].url, None);
        fake.assert_done();
    }

    #[test]
    fn no_tracked_branch_or_a_git_failure_reads_nothing_more() {
        let fake = FakeRunner::new().expect(&UPSTREAMS, ok(b"refs/heads/a\0\0\n"));
        assert!(read_upstreams(&fake).is_empty());
        fake.assert_done();
        let fake = FakeRunner::new().expect(&UPSTREAMS, fail("not a repo"));
        assert!(read_upstreams(&fake).is_empty());
        fake.assert_done();
    }
}
