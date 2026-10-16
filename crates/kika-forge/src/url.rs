//! Web addresses on a forge, built locally from a remote's parsed parts.
//! Nothing here reaches the network. Only the host, owner and name of a
//! [`ForgeRepo`] go in, so a remote's userinfo or port can never appear,
//! and each part is checked again here, so a URL is never built from
//! anything a parser upstream let slip.

use super::anchor::{GhLine, GhSide};
use super::remote::{matches, parse_remote};
use super::{ForgeHost, ForgeRepo};
use kika_git::branches::BranchName;
use kika_git::commit_files::CommitId;
use kika_git::remotes::Remote;

/// Which forge a repository is on, which decides the shape of its URLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeKind {
    GitHub,
    GitLab,
}

impl ForgeKind {
    /// The forge's name, for user text.
    pub fn name(self) -> &'static str {
        match self {
            ForgeKind::GitHub => "GitHub",
            ForgeKind::GitLab => "GitLab",
        }
    }
}

/// `https://<host>/<owner>/<name>`, or `None` when a part is not plain:
/// the host must be ASCII letters, digits, `-` and dots, and the owner and
/// name `[A-Za-z0-9._-]`, not dots only.
pub fn https_base(repo: &ForgeRepo) -> Option<String> {
    let host_ok = !repo.host.is_empty()
        && repo.host.split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    if !host_ok || !segment_ok(&repo.owner) || !segment_ok(&repo.name) {
        return None;
    }
    Some(format!(
        "https://{}/{}/{}",
        repo.host.to_ascii_lowercase(),
        repo.owner,
        repo.name
    ))
}

/// One path segment of `[A-Za-z0-9._-]+`, not made of dots only.
fn segment_ok(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && segment.bytes().any(|b| b != b'.')
}

/// The page of commit `id`. GitLab keeps its pages under `/-/`.
pub fn commit_url(kind: ForgeKind, repo: &ForgeRepo, id: &CommitId) -> Option<String> {
    let base = https_base(repo)?;
    let hex = commit_hex(id.as_str())?;
    Some(match kind {
        ForgeKind::GitHub => format!("{base}/commit/{hex}"),
        ForgeKind::GitLab => format!("{base}/-/commit/{hex}"),
    })
}

/// GitHub pull request `number`'s page.
pub fn pull_url(repo: &ForgeRepo, number: u64) -> Option<String> {
    Some(format!("{}/pull/{number}", https_base(repo)?))
}

/// Line `line` of `path` on pull request `number`'s page: its files view,
/// or with `commit`, that commit's view. GitHub names a file's block by
/// the SHA-256 of its name, and a line by side (`L` old, `R` new) and
/// number. `None` when a part is not plain or the line is 0.
pub fn pr_line_url(
    repo: &ForgeRepo,
    number: u64,
    commit: Option<&CommitId>,
    path: &str,
    line: GhLine,
) -> Option<String> {
    if line.line == 0 {
        return None;
    }
    let pull = pull_url(repo, number)?;
    let page = match commit {
        None => format!("{pull}/files"),
        Some(id) => format!("{pull}/commits/{}", commit_hex(id.as_str())?),
    };
    let side = match line.side {
        GhSide::Left => 'L',
        GhSide::Right => 'R',
    };
    Some(format!(
        "{page}#diff-{}{side}{}",
        sha256_hex(path.as_bytes()),
        line.line
    ))
}

/// A commit id, when it is hex: `CommitId` is already, and it is checked
/// again here, as every part is.
fn commit_hex(hex: &str) -> Option<&str> {
    (!hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hex)
}

/// SHA-256 of `bytes`, lowercase hex.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// GitLab's merge requests whose source is `branch`, in any state: with
/// no transport to GitLab, no request number is known, so the list
/// filtered to the branch stands in for it.
pub fn gitlab_mrs_url(repo: &ForgeRepo, branch: &BranchName) -> Option<String> {
    Some(format!(
        "{}/-/merge_requests?scope=all&source_branch={}",
        https_base(repo)?,
        percent_encode(&branch.0)
    ))
}

/// Why a branch has no page to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoBranchPage {
    /// The page is the pull request's, and none is known for the branch.
    NoPull,
    /// A part of the repository is not plain enough to build a URL from.
    Unbuildable,
}

/// The page for `branch`: on GitHub its pull request `pr`, on GitLab its
/// merge requests, which need no number.
pub fn branch_page_url(
    kind: ForgeKind,
    repo: &ForgeRepo,
    branch: &BranchName,
    pr: Option<u64>,
) -> Result<String, NoBranchPage> {
    let url = match kind {
        ForgeKind::GitHub => pull_url(repo, pr.ok_or(NoBranchPage::NoPull)?),
        ForgeKind::GitLab => gitlab_mrs_url(repo, branch),
    };
    url.ok_or(NoBranchPage::Unbuildable)
}

/// `bytes` with everything but RFC 3986's unreserved characters as `%XX`.
pub fn percent_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The forge repository the branches belong to, and its kind: the remote
/// `origin`, else the only remote, as the pull-request load reads it. Its
/// URL must parse and its host be one the user listed, GitHub's first.
pub fn forge_for(
    remotes: &[Remote],
    github: &[ForgeHost],
    gitlab: &[ForgeHost],
) -> Option<(ForgeKind, ForgeRepo)> {
    let configured: Vec<&Remote> = remotes.iter().filter(|r| r.configured).collect();
    let remote = match configured.as_slice() {
        _ if configured.iter().any(|r| r.name.0 == b"origin") => {
            configured.into_iter().find(|r| r.name.0 == b"origin")?
        }
        [only] => only,
        _ => return None,
    };
    repo_of(remote, github, gitlab)
}

/// `remote`'s forge repository and kind, when its URL is on a listed host.
pub fn repo_of(
    remote: &Remote,
    github: &[ForgeHost],
    gitlab: &[ForgeHost],
) -> Option<(ForgeKind, ForgeRepo)> {
    let repo = parse_remote(remote.url.as_deref()?.as_bytes())?;
    if matches(&repo, github).is_some() {
        Some((ForgeKind::GitHub, repo))
    } else if matches(&repo, gitlab).is_some() {
        Some((ForgeKind::GitLab, repo))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kika_git::remotes::RemoteName;

    const OID: &str = "0123456789abcdef0123456789abcdef01234567";

    fn hosts(names: &[&str]) -> Vec<ForgeHost> {
        names.iter().map(|h| ForgeHost::parse(h).unwrap()).collect()
    }

    fn remote(name: &str, url: &str) -> Remote {
        Remote {
            name: RemoteName::from(name),
            url: Some(url.into()),
            configured: true,
            branches: vec![],
        }
    }

    fn repo(host: &str, owner: &str, name: &str) -> ForgeRepo {
        ForgeRepo {
            host: host.into(),
            owner: owner.into(),
            name: name.into(),
        }
    }

    #[test]
    fn every_remote_form_gives_a_plain_https_url_without_userinfo_or_port() {
        let id = CommitId::parse(OID).unwrap();
        for url in [
            "https://github.com/o/r.git",
            "https://user:token@github.com:443/o/r",
            "ssh://git@github.com:22/o/r.git",
            "git@github.com:o/r.git",
            "https://GitHub.com/o/r/",
        ] {
            let parsed = parse_remote(url.as_bytes()).unwrap();
            assert_eq!(
                pull_url(&parsed, 7).as_deref(),
                Some("https://github.com/o/r/pull/7"),
                "{url}"
            );
            let commit = commit_url(ForgeKind::GitHub, &parsed, &id).unwrap();
            assert_eq!(commit, format!("https://github.com/o/r/commit/{OID}"));
            for leak in ["user", "token", "@", ":443", ":22", "git@"] {
                assert!(!commit.contains(leak), "{url}: {commit}");
            }
        }
        assert_eq!(
            commit_url(ForgeKind::GitLab, &repo("gitlab.com", "g", "p"), &id).unwrap(),
            format!("https://gitlab.com/g/p/-/commit/{OID}")
        );
    }

    #[test]
    fn a_gitlab_branch_is_percent_encoded_in_the_merge_request_list() {
        let url = gitlab_mrs_url(&repo("gitlab.com", "g", "p"), &"feat/a b#".into());
        assert_eq!(
            url.as_deref(),
            Some("https://gitlab.com/g/p/-/merge_requests?scope=all&source_branch=feat%2Fa%20b%23")
        );
        let odd = BranchName::from_bytes(b"x~y_z.-\xff&?=");
        assert_eq!(percent_encode(&odd.0), "x~y_z.-%FF%26%3F%3D");
    }

    #[test]
    fn a_bad_owner_name_or_host_gives_none() {
        for bad in [
            repo("github.com", "o/x", "r"),
            repo("github.com", "o", "r?x"),
            repo("github.com", "..", "r"),
            repo("github.com", "", "r"),
            repo("github.com", "o", "r#x"),
            repo("github.com", "ö", "r"),
            repo("evil.io/x", "o", "r"),
            repo("github.com:8443", "o", "r"),
            repo("u@github.com", "o", "r"),
            repo("", "o", "r"),
        ] {
            assert_eq!(pull_url(&bad, 1), None, "{bad:?}");
            assert_eq!(gitlab_mrs_url(&bad, &"b".into()), None, "{bad:?}");
        }
    }

    #[test]
    fn a_pr_line_url_points_at_the_files_view_or_the_commit_at_the_side_and_line() {
        let r = repo("github.com", "acme", "widgets");
        let new = GhLine {
            side: GhSide::Right,
            line: 12,
        };
        let old = GhLine {
            side: GhSide::Left,
            line: 3,
        };
        let hash = "42cb6807ad74b3e201c5a7ca98b911c5fa08380e942be6e4ac5807f8377f87fc";
        assert_eq!(
            pr_line_url(&r, 7, None, "src/main.rs", new).as_deref(),
            Some(format!("https://github.com/acme/widgets/pull/7/files#diff-{hash}R12").as_str())
        );
        assert_eq!(
            pr_line_url(&r, 7, None, "src/main.rs", old).as_deref(),
            Some(format!("https://github.com/acme/widgets/pull/7/files#diff-{hash}L3").as_str())
        );
        let id = CommitId::parse(OID).unwrap();
        assert_eq!(
            pr_line_url(&r, 7, Some(&id), "src/main.rs", new).as_deref(),
            Some(
                format!("https://github.com/acme/widgets/pull/7/commits/{OID}#diff-{hash}R12")
                    .as_str()
            )
        );
    }

    #[test]
    fn a_pr_line_url_is_none_for_a_bad_part_or_line_zero() {
        let line = GhLine {
            side: GhSide::Right,
            line: 1,
        };
        for bad in [
            repo("github.com:8443", "o", "r"),
            repo("github.com", "o/x", "r"),
            repo("github.com", "..", "r"),
        ] {
            assert_eq!(pr_line_url(&bad, 1, None, "a", line), None, "{bad:?}");
        }
        let r = repo("github.com", "o", "r");
        let zero = GhLine { line: 0, ..line };
        assert_eq!(pr_line_url(&r, 1, None, "a", zero), None);
        // `CommitId` cannot hold one, but the id is checked again here.
        for bad in ["", "g123", "12 34", "ab/cd"] {
            assert_eq!(commit_hex(bad), None, "{bad:?}");
        }
        assert_eq!(commit_hex(OID), Some(OID));
    }

    #[test]
    fn sha256_matches_the_standard_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn forge_for_picks_origin_else_the_only_remote_else_none() {
        let github = hosts(&["github.com"]);
        let gitlab = hosts(&["gitlab.example.com"]);
        let origin = remote("origin", "git@github.com:o/r.git");
        let fork = remote("fork", "https://github.com/me/r");
        assert_eq!(
            forge_for(&[fork.clone(), origin.clone()], &github, &gitlab),
            Some((ForgeKind::GitHub, repo("github.com", "o", "r")))
        );
        assert_eq!(
            forge_for(std::slice::from_ref(&fork), &github, &gitlab),
            Some((ForgeKind::GitHub, repo("github.com", "me", "r")))
        );
        let other = remote("upstream", "https://github.com/u/r");
        assert_eq!(forge_for(&[fork.clone(), other], &github, &gitlab), None);
        assert_eq!(forge_for(&[], &github, &gitlab), None);
        // A group of refs no configured remote owns is not a remote.
        let stray = Remote {
            configured: false,
            ..remote("origin", "https://github.com/x/y")
        };
        assert_eq!(
            forge_for(&[stray, fork], &github, &gitlab),
            Some((ForgeKind::GitHub, repo("github.com", "me", "r")))
        );
    }

    #[test]
    fn forge_for_knows_github_and_gitlab_hosts_and_nothing_else() {
        let github = hosts(&["github.com"]);
        let gitlab = hosts(&["gitlab.example.com"]);
        let on = |url: &str| forge_for(&[remote("origin", url)], &github, &gitlab);
        assert_eq!(
            on("git@gitlab.example.com:grp/proj.git"),
            Some((ForgeKind::GitLab, repo("gitlab.example.com", "grp", "proj")))
        );
        assert_eq!(on("https://gitlab.com/grp/proj"), None);
        assert_eq!(on("https://github.com.evil.io/o/r"), None);
        // Nested groups stay unrecognised.
        assert_eq!(on("https://gitlab.example.com/a/b/c"), None);
        // With no hosts listed, nothing is a forge.
        let origin = remote("origin", "https://github.com/o/r");
        assert_eq!(forge_for(&[origin], &[], &[]), None);
        let no_url = Remote {
            url: None,
            ..remote("origin", "")
        };
        assert_eq!(forge_for(&[no_url], &github, &gitlab), None);
    }

    #[test]
    fn a_branch_page_is_its_pull_on_github_and_its_merge_requests_on_gitlab() {
        let hub = repo("github.com", "o", "r");
        let branch: BranchName = "topic/a b".into();
        assert_eq!(
            branch_page_url(ForgeKind::GitHub, &hub, &branch, Some(7)),
            Ok("https://github.com/o/r/pull/7".into())
        );
        assert_eq!(
            branch_page_url(ForgeKind::GitHub, &hub, &branch, None),
            Err(NoBranchPage::NoPull)
        );
        let lab = repo("gitlab.com", "g", "p");
        assert_eq!(
            branch_page_url(ForgeKind::GitLab, &lab, &branch, None),
            Ok(
                "https://gitlab.com/g/p/-/merge_requests?scope=all&source_branch=topic%2Fa%20b"
                    .into()
            )
        );
        let odd = repo("github.com", "..", "r");
        assert_eq!(
            branch_page_url(ForgeKind::GitHub, &odd, &branch, Some(7)),
            Err(NoBranchPage::Unbuildable)
        );
        assert_eq!(ForgeKind::GitHub.name(), "GitHub");
        assert_eq!(ForgeKind::GitLab.name(), "GitLab");
    }
}
