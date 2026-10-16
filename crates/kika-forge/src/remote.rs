//! Which forge repository a remote URL names. Strict on purpose: a URL
//! that could be read two ways, or that names an unexpected port or
//! scheme, gives nothing, so Kika never asks a forge about a repository
//! the user did not mean.

use super::{ForgeHost, ForgeRepo};

/// The most `/`-parts a repository path may have: GitLab's 20 nested
/// group levels plus the project.
pub const MAX_PARTS: usize = 21;

/// The host, owner and name of `url`, from one of these forms only:
///
/// - `https://[userinfo@]host[:443]/owner/repo[.git][/]`
/// - `ssh://[user@]host[:22]/owner/repo[.git]`
/// - `user@host:owner/repo[.git]`
///
/// `owner` may itself be a GitLab group path (`group/sub`, up to
/// [`MAX_PARTS`] parts in all); `name` is always the last part. Only a
/// GitLab host is ever matched with such a nested owner.
///
/// Userinfo ends at the last `@` of the authority and is dropped.
pub fn parse_remote(url: &[u8]) -> Option<ForgeRepo> {
    let url = std::str::from_utf8(url).ok()?;
    if let Some(rest) = url.strip_prefix("https://") {
        let (authority, path) = rest.split_once('/')?;
        let path = path.strip_suffix('/').unwrap_or(path);
        from_parts(authority, Some("443"), path)
    } else if let Some(rest) = url.strip_prefix("ssh://") {
        let (authority, path) = rest.split_once('/')?;
        from_parts(authority, Some("22"), path)
    } else if url.contains("://") {
        None
    } else {
        // scp-like: what comes before the first `:` is the authority,
        // unless it holds a `/`, which makes the whole a local path.
        let (authority, path) = url.split_once(':')?;
        if authority.contains('/') || !authority.contains('@') {
            return None;
        }
        from_parts(authority, None, path)
    }
}

/// The configured host `repo` is on: whole-name equality, ignoring
/// ASCII case, and nothing looser. A GitHub host only ever matches an
/// `owner/repo` path: a nested owner is GitLab's alone.
pub fn matches<'a>(repo: &ForgeRepo, hosts: &'a [ForgeHost]) -> Option<&'a ForgeHost> {
    let host = repo.host.to_ascii_lowercase();
    let nested = repo.owner.contains('/');
    hosts
        .iter()
        .find(|h| h.as_str() == host && (h.nests() || !nested))
}

/// The configured host `repo` is on, when its `[forge.allow]` lets Kika
/// ask about `repo`: the gate, as a pure answer for the model to decide
/// what to offer.
pub fn allowed<'a>(repo: &ForgeRepo, hosts: &'a [ForgeHost]) -> Option<&'a ForgeHost> {
    matches(repo, hosts).filter(|host| host.allows(repo))
}

/// `authority` (`[userinfo@]host[:port]`) and `owner/repo[.git]`, where
/// `owner` may hold more `/`-parts.
/// `port` is the one port allowed; `None` allows none.
fn from_parts(authority: &str, port: Option<&str>, path: &str) -> Option<ForgeRepo> {
    let host_port = match authority.rfind('@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    };
    let host = match (host_port.split_once(':'), port) {
        (None, _) => host_port,
        (Some((host, given)), Some(allowed)) if given == allowed => host,
        _ => return None,
    };
    if !host_ok(host) {
        return None;
    }
    let path = path.strip_suffix(".git").unwrap_or(path);
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 2 || !path_ok(&parts) {
        return None;
    }
    let (owner, name) = path.rsplit_once('/')?;
    Some(ForgeRepo {
        host: host.to_string(),
        owner: owner.to_string(),
        name: name.to_string(),
    })
}

/// ASCII letters, digits and `-` in non-empty labels: no percent escapes,
/// no look-alike letters, no trailing dot.
fn host_ok(host: &str) -> bool {
    !host.is_empty()
        && host.split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// Whether `parts` can name a repository or group: 1 to [`MAX_PARTS`]
/// plain segments, and in a nested path (3 parts or more) no `-`, which
/// GitLab keeps as the marker before its own pages (`/-/`).
fn path_ok(parts: &[&str]) -> bool {
    (1..=MAX_PARTS).contains(&parts.len())
        && parts.iter().all(|part| segment_ok(part))
        && (parts.len() < 3 || !parts.contains(&"-"))
}

/// One path segment of `[A-Za-z0-9._-]+`, not made of dots only.
pub(super) fn segment_ok(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && segment.bytes().any(|b| b != b'.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(host: &str, owner: &str, name: &str) -> Option<ForgeRepo> {
        Some(ForgeRepo {
            host: host.into(),
            owner: owner.into(),
            name: name.into(),
        })
    }

    fn github() -> Vec<ForgeHost> {
        vec![ForgeHost::parse("github.com").unwrap()]
    }

    #[test]
    fn allowed_is_the_matching_host_only_when_it_lets_the_repo_in() {
        use super::super::allow::AllowRule;
        let widgets = repo("github.com", "acme", "widgets").unwrap();
        let other = ForgeHost::parse("gitlab.example").unwrap();
        assert_eq!(allowed(&widgets, std::slice::from_ref(&other)), None);
        let rule = |r: &str| vec![AllowRule::parse(r).unwrap()];
        let shut = ForgeHost::parse("github.com")
            .unwrap()
            .with_allow(rule("someone"));
        assert_eq!(allowed(&widgets, &[other.clone(), shut]), None);
        let open = ForgeHost::parse("github.com")
            .unwrap()
            .with_allow(rule("acme"));
        let hosts = [other, open.clone()];
        assert_eq!(allowed(&widgets, &hosts), Some(&open));
    }

    /// Whether `url` parses to a repository on github.com.
    fn on_github(url: &str) -> bool {
        parse_remote(url.as_bytes()).is_some_and(|r| matches(&r, &github()).is_some())
    }

    #[test]
    fn https_ssh_and_scp_forms_parse() {
        let want = repo("github.com", "octo", "kika");
        for url in [
            "https://github.com/octo/kika",
            "https://github.com/octo/kika.git",
            "https://github.com/octo/kika/",
            "https://github.com:443/octo/kika.git",
            "ssh://git@github.com/octo/kika.git",
            "ssh://git@github.com:22/octo/kika",
            "ssh://github.com/octo/kika",
            "git@github.com:octo/kika.git",
            "git@github.com:octo/kika",
        ] {
            assert_eq!(parse_remote(url.as_bytes()), want, "{url}");
        }
        assert_eq!(
            parse_remote(b"https://GitHub.com/o.x/r_y-z"),
            repo("GitHub.com", "o.x", "r_y-z")
        );
        assert!(on_github("https://GitHub.com/o/r"));
    }

    #[test]
    fn look_alikes_and_other_forms_never_match_github() {
        for url in [
            "https://github.com@evil.io/o/r",
            "https://evil.io/github.com/o/r",
            "https://github.com.evil.io/o/r",
            "git@github.com.evil.io:o/r",
            "https://evilgithub.com/o/r",
            "https://github.com:8443/o/r",
            "ssh://git@github.com:2222/o/r",
            "git@github.com:22:o/r",
            "http://github.com/o/r",
            "git://github.com/o/r",
            "file:///github.com/o/r",
            "https://gіthub.com/o/r",
            "https://github%2ecom/o/r",
            "https://github.com./o/r",
            "https://github.com/o/r/x",
            "https://github.com/o",
            "https://github.com//r",
            "https://github.com/o/r?x=1",
            "https://github.com/../r",
            "github.com:o/r",
            "/srv/github.com:o/r",
            "./x@github.com:o/r",
        ] {
            assert!(!on_github(url), "{url}");
        }
        assert_eq!(parse_remote(b"https://github.com/o/\xff"), None);
    }

    #[test]
    fn a_gitlab_path_in_nested_groups_parses_to_its_namespace_and_name() {
        let want = repo("gitlab.example.com", "acme/sub", "x");
        for url in [
            "https://gitlab.example.com/acme/sub/x",
            "https://gitlab.example.com/acme/sub/x.git",
            "https://gitlab.example.com/acme/sub/x/",
            "ssh://git@gitlab.example.com/acme/sub/x.git",
            "git@gitlab.example.com:acme/sub/x.git",
        ] {
            assert_eq!(parse_remote(url.as_bytes()), want, "{url}");
        }
        assert_eq!(
            parse_remote(b"git@gitlab.example.com:a/b/c/d/e/f.git"),
            repo("gitlab.example.com", "a/b/c/d/e", "f")
        );
        let deepest = vec!["g"; MAX_PARTS].join("/");
        let parsed = parse_remote(format!("https://h.io/{deepest}").as_bytes()).unwrap();
        assert_eq!(parsed.owner, vec!["g"; MAX_PARTS - 1].join("/"));
        assert_eq!(parsed.name, "g");
    }

    #[test]
    fn a_bad_nested_path_gives_none() {
        let too_deep = vec!["g"; MAX_PARTS + 1].join("/");
        for path in [
            "a//x",
            "/a/x",
            too_deep.as_str(),
            "acme/../x",
            "acme/s b/x",
            "acme/sub/x?y=1",
            "acme/-/x",
            "-/sub/x",
            "acme/sub/-",
            "acme/sub/x/-/tree",
        ] {
            let url = format!("https://gitlab.example.com/{path}");
            assert_eq!(parse_remote(url.as_bytes()), None, "{url}");
            let scp = format!("git@gitlab.example.com:{path}");
            assert_eq!(parse_remote(scp.as_bytes()), None, "{scp}");
        }
    }

    #[test]
    fn only_a_gitlab_host_matches_a_nested_owner() {
        let nested_hub = parse_remote(b"https://github.com/acme/sub/x").unwrap();
        assert_eq!(matches(&nested_hub, &github()), None);
        assert_eq!(allowed(&nested_hub, &github()), None);
        let lab = vec![ForgeHost::parse_gitlab("gitlab.example.com").unwrap()];
        let nested_lab = parse_remote(b"https://gitlab.example.com/acme/sub/x").unwrap();
        assert_eq!(matches(&nested_lab, &lab), Some(&lab[0]));
        assert_eq!(allowed(&nested_lab, &lab), Some(&lab[0]));
        // The same host as a GitHub one does not match it.
        let as_hub = vec![ForgeHost::parse("gitlab.example.com").unwrap()];
        assert_eq!(matches(&nested_lab, &as_hub), None);
        // Two parts match either kind, as before.
        let flat = parse_remote(b"https://gitlab.example.com/acme/x").unwrap();
        assert!(matches(&flat, &lab).is_some() && matches(&flat, &as_hub).is_some());
    }

    #[test]
    fn userinfo_is_dropped_and_never_kept() {
        let parsed = parse_remote(b"https://x:tok@github.com/o/r.git").unwrap();
        assert_eq!(parsed, repo("github.com", "o", "r").unwrap());
        assert!(matches(&parsed, &github()).is_some());
        assert!(!format!("{parsed:?}").contains("tok"));
        // The last `@` ends the userinfo, even with one inside it.
        assert_eq!(
            parse_remote(b"https://a@b:tok@github.com/o/r"),
            repo("github.com", "o", "r")
        );
    }

    #[test]
    fn matching_is_exact_on_the_whole_host() {
        let hosts = vec![
            ForgeHost::parse("github.com").unwrap(),
            ForgeHost::parse("github.example.com").unwrap(),
        ];
        let on = |host: &str| matches(&repo(host, "o", "r").unwrap(), &hosts).is_some();
        assert!(on("github.com"));
        assert!(on("GITHUB.example.COM"));
        assert!(!on("example.com"));
        assert!(!on("hub.com"));
        assert!(!on("github.co"));
        assert!(matches(&repo("github.com", "o", "r").unwrap(), &[]).is_none());
    }
}
