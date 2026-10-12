//! The URL of the remote whose forge repository the branches belong to:
//! `origin`, or the only remote when there is no `origin`.

use super::path::os_string;
use super::remotes::RemoteName;
use super::runner::{GitCommand, GitRunner};

/// The raw URL bytes, or `None` when no remote qualifies or git fails.
pub fn read_remote_url(runner: &dyn GitRunner) -> Option<Vec<u8>> {
    let name = remote_name(runner)?;
    let output = runner.run(&url_command(&name, "--get")).ok()?;
    if !output.success {
        return None;
    }
    let url = output.stdout.strip_suffix(b"\0").unwrap_or(&output.stdout);
    (!url.is_empty()).then(|| url.to_vec())
}

/// The remote's name and its one URL, for a fetch that must reach exactly
/// the repository the URL names. `None` when the remote has several URLs:
/// `--get` reads the last while a fetch uses the first.
pub fn read_fetch_remote(runner: &dyn GitRunner) -> Option<(RemoteName, Vec<u8>)> {
    let name = remote_name(runner)?;
    let output = runner.run(&url_command(&name, "--get-all")).ok()?;
    if !output.success {
        return None;
    }
    let body = output.stdout.strip_suffix(b"\0").unwrap_or(&output.stdout);
    let mut urls = body.split(|&b| b == 0);
    match (urls.next(), urls.next()) {
        (Some(url), None) if !url.is_empty() => Some((RemoteName::from_bytes(&name), url.to_vec())),
        _ => None,
    }
}

/// `origin`, or the only remote there is.
fn remote_name(runner: &dyn GitRunner) -> Option<Vec<u8>> {
    let listed = runner.run(&GitCommand::new("remote")).ok()?;
    if !listed.success {
        return None;
    }
    let names: Vec<&[u8]> = listed
        .stdout
        .split(|&b| b == b'\n')
        .filter(|name| !name.is_empty())
        .collect();
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::fake::{FakeRunner, fail, ok};

    #[test]
    fn origin_is_used_when_present() {
        let fake = FakeRunner::new()
            .expect(&["remote"], ok(b"fork\norigin\n"))
            .expect(
                &["config", "-z", "--get", "remote.origin.url"],
                ok(b"git@github.com:o/r.git\0"),
            );
        assert_eq!(
            read_remote_url(&fake),
            Some(b"git@github.com:o/r.git".to_vec())
        );
        fake.assert_done();
    }

    #[test]
    fn a_fetch_remote_comes_with_its_name_and_only_one_url() {
        let get_all = ["config", "-z", "--get-all", "remote.upstream.url"];
        let fake = FakeRunner::new()
            .expect(&["remote"], ok(b"upstream\n"))
            .expect(&get_all, ok(b"https://github.com/o/r\0"));
        assert_eq!(
            read_fetch_remote(&fake),
            Some((
                RemoteName::from("upstream"),
                b"https://github.com/o/r".to_vec()
            ))
        );
        // Two URLs: the one checked need not be the one fetched from.
        let fake = FakeRunner::new()
            .expect(&["remote"], ok(b"upstream\n"))
            .expect(
                &get_all,
                ok(b"https://github.com/o/r\0https://evil.example/o/r\0"),
            );
        assert_eq!(read_fetch_remote(&fake), None);
        let fake = FakeRunner::new()
            .expect(&["remote"], ok(b"upstream\n"))
            .expect(&get_all, fail(""));
        assert_eq!(read_fetch_remote(&fake), None);
    }

    #[test]
    fn the_only_remote_stands_in_for_origin() {
        let fake = FakeRunner::new()
            .expect(&["remote"], ok(b"upstream\n"))
            .expect(
                &["config", "-z", "--get", "remote.upstream.url"],
                ok(b"https://github.com/o/r\0"),
            );
        assert_eq!(
            read_remote_url(&fake),
            Some(b"https://github.com/o/r".to_vec())
        );
    }

    #[test]
    fn several_remotes_without_origin_or_none_give_nothing() {
        let fake = FakeRunner::new().expect(&["remote"], ok(b"a\nb\n"));
        assert_eq!(read_remote_url(&fake), None);
        let fake = FakeRunner::new().expect(&["remote"], ok(b""));
        assert_eq!(read_remote_url(&fake), None);
        let fake = FakeRunner::new().expect(&["remote"], fail("not a repo"));
        assert_eq!(read_remote_url(&fake), None);
        let fake = FakeRunner::new()
            .expect(&["remote"], ok(b"origin\n"))
            .expect(&["config", "-z", "--get", "remote.origin.url"], fail(""));
        assert_eq!(read_remote_url(&fake), None);
    }
}
