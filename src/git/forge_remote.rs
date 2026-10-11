//! The URL of the remote whose forge repository the branches belong to:
//! `origin`, or the only remote when there is no `origin`.

use super::path::os_string;
use super::runner::{GitCommand, GitRunner};

/// The raw URL bytes, or `None` when no remote qualifies or git fails.
pub fn read_remote_url(runner: &dyn GitRunner) -> Option<Vec<u8>> {
    let listed = runner.run(&GitCommand::new("remote")).ok()?;
    if !listed.success {
        return None;
    }
    let names: Vec<&[u8]> = listed
        .stdout
        .split(|&b| b == b'\n')
        .filter(|name| !name.is_empty())
        .collect();
    let name = match names.as_slice() {
        _ if names.contains(&&b"origin"[..]) => &b"origin"[..],
        [only] => only,
        _ => return None,
    };
    let mut key = b"remote.".to_vec();
    key.extend_from_slice(name);
    key.extend_from_slice(b".url");
    let cmd = GitCommand::new("config")
        .arg("-z")
        .arg("--get")
        .arg(os_string(&key));
    let output = runner.run(&cmd).ok()?;
    if !output.success {
        return None;
    }
    let url = output.stdout.strip_suffix(b"\0").unwrap_or(&output.stdout);
    (!url.is_empty()).then(|| url.to_vec())
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
