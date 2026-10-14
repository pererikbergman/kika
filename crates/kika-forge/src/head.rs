//! Which branch on the forge a local branch's pull request comes from: its
//! "head", an owner and a ref name. Worked out from local git config only,
//! so a branch that tracks a fork, or tracks a branch of another name,
//! still finds its pull request.

use std::collections::HashMap;

use super::ForgeRepo;
use super::remote::parse_remote;
use kika_git::branches::BranchName;
use kika_git::forge_remote::Upstream;

/// The forge branch a local branch's pull request is looked up by. The
/// owner comes from a local remote URL, never from the forge's answer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Head {
    pub branch: BranchName,
    pub owner: String,
    pub name: String,
}

/// The head of each of `branches`, in order. A branch's upstream names it
/// when the upstream is a branch on a remote of `base`'s host; otherwise
/// the head is the local name in `base`'s owner.
pub fn heads(
    branches: &[BranchName],
    upstreams: &HashMap<BranchName, Upstream>,
    base: &ForgeRepo,
) -> Vec<Head> {
    branches
        .iter()
        .map(|branch| {
            let (owner, name) = upstreams
                .get(branch)
                .and_then(|upstream| upstream_head(upstream, base))
                .unwrap_or_else(|| {
                    // Only UTF-8 names reach here; see `ForgeState::set_branches`.
                    let name = String::from_utf8_lossy(&branch.0).into_owned();
                    (base.owner.clone(), name)
                });
            Head {
                branch: branch.clone(),
                owner,
                name,
            }
        })
        .collect()
}

/// The (owner, ref name) `upstream` names, or `None` when it is not a
/// branch on a repository of `base`'s host.
fn upstream_head(upstream: &Upstream, base: &ForgeRepo) -> Option<(String, String)> {
    let name = upstream.merge.strip_prefix(b"refs/heads/")?;
    let name = std::str::from_utf8(name).ok().filter(|n| !n.is_empty())?;
    let repo = parse_remote(upstream.url.as_deref()?)?;
    repo.host
        .eq_ignore_ascii_case(&base.host)
        .then(|| (repo.owner, name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> ForgeRepo {
        ForgeRepo {
            host: "GitHub.com".into(),
            owner: "o".into(),
            name: "r".into(),
        }
    }

    fn upstream(remote: &str, merge: &[u8], url: Option<&str>) -> Upstream {
        Upstream {
            remote: remote.into(),
            merge: merge.to_vec(),
            url: url.map(Into::into),
        }
    }

    fn head(branch: &str, owner: &str, name: &str) -> Head {
        Head {
            branch: branch.into(),
            owner: owner.into(),
            name: name.into(),
        }
    }

    /// The head of the one branch `fix` with `upstream`.
    fn head_of(upstream: Upstream) -> Head {
        let upstreams = HashMap::from([(BranchName::from("fix"), upstream)]);
        let [head] = heads(&["fix".into()], &upstreams, &base())
            .try_into()
            .unwrap();
        head
    }

    #[test]
    fn a_branch_with_no_upstream_is_its_own_name_in_the_base_owner() {
        assert_eq!(
            heads(&["main".into(), "wip".into()], &HashMap::new(), &base()),
            [head("main", "o", "main"), head("wip", "o", "wip")]
        );
    }

    #[test]
    fn an_upstream_names_the_owner_and_ref() {
        // Renamed: `fix` tracks `origin/feature-x`.
        let renamed = upstream(
            "origin",
            b"refs/heads/feature-x",
            Some("git@github.com:o/r.git"),
        );
        assert_eq!(head_of(renamed), head("fix", "o", "feature-x"));
        // A fork, on the same host in another case.
        let fork = upstream(
            "fork",
            b"refs/heads/fix",
            Some("https://GITHUB.com/me/r-fork"),
        );
        assert_eq!(head_of(fork), head("fix", "me", "fix"));
    }

    #[test]
    fn an_upstream_that_does_not_qualify_falls_back_to_the_default() {
        let default = head("fix", "o", "fix");
        let url = Some("https://github.com/me/r");
        for upstream in [
            // Another host.
            upstream("gl", b"refs/heads/fix", Some("https://gitlab.com/me/r")),
            // A local upstream.
            upstream(".", b"refs/heads/main", None),
            // Not a branch.
            upstream("fork", b"refs/tags/v1", url),
            upstream("fork", b"refs/heads/", url),
            upstream("fork", b"refs/heads/\xff", url),
            // A URL that is not a forge repository, or none.
            upstream("fork", b"refs/heads/x", Some("/srv/git/r.git")),
            upstream("fork", b"refs/heads/x", None),
        ] {
            assert_eq!(head_of(upstream.clone()), default, "{upstream:?}");
        }
    }
}
