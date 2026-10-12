//! Which repositories on a host Kika may ask `gh` about, from the global
//! `[forge.allow]` table. Naming a host opts it in; an allow-list narrows
//! that to the owners and repositories the user trusts, so opening some
//! other clone on the same host never reaches the network.

use super::ForgeRepo;
use super::remote::segment_ok;

/// One `[forge.allow]` entry: every repository of `owner`, or only
/// `owner/name`. Exact names, no patterns, so it allows no more than it
/// says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowRule {
    owner: String,
    name: Option<String>,
}

impl AllowRule {
    /// `text` as `owner` or `owner/repo`, or why it is neither. Each part
    /// follows the rules a remote URL's path does, and a repository is
    /// named without `.git`, as the forge names it.
    pub fn parse(text: &str) -> Result<Self, String> {
        let bad = |why: &str| Err(format!("{text:?} is not an owner or owner/repo: {why}"));
        let (owner, name) = match text.split_once('/') {
            Some((owner, name)) => (owner, Some(name)),
            None => (text, None),
        };
        let part_ok = |part: &str| segment_ok(part) && !part.to_ascii_lowercase().ends_with(".git");
        if !part_ok(owner) || !name.is_none_or(part_ok) {
            return bad("use letters, digits, ., _ and - only, one / at most, no .git");
        }
        Ok(Self {
            owner: owner.to_string(),
            name: name.map(String::from),
        })
    }

    /// Whether `repo` is the owner, or the repository, this names. Case is
    /// ignored, as GitHub ignores it in both.
    pub fn allows(&self, repo: &ForgeRepo) -> bool {
        self.owner.eq_ignore_ascii_case(&repo.owner)
            && self
                .name
                .as_ref()
                .is_none_or(|name| name.eq_ignore_ascii_case(&repo.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::ForgeHost;

    fn repo(owner: &str, name: &str) -> ForgeRepo {
        ForgeRepo {
            host: "github.com".into(),
            owner: owner.into(),
            name: name.into(),
        }
    }

    fn allows(rule: &str, owner: &str, name: &str) -> bool {
        AllowRule::parse(rule).unwrap().allows(&repo(owner, name))
    }

    #[test]
    fn an_owner_allows_all_its_repositories_in_any_case() {
        assert!(allows("acme", "acme", "x"));
        assert!(allows("acme", "ACME", "x"));
        assert!(!allows("acme", "acme2", "x"));
        assert!(!allows("acme", "other", "acme"));
    }

    #[test]
    fn an_owner_and_repository_allow_only_that_one() {
        assert!(allows("acme/kika", "acme", "kika"));
        assert!(allows("acme/kika", "Acme", "Kika"));
        assert!(!allows("acme/kika", "acme", "kika2"));
        assert!(!allows("acme/kika", "acme", "other"));
        assert!(!allows("acme/kika", "other", "kika"));
    }

    #[test]
    fn patterns_extra_parts_and_dot_git_are_refused() {
        for bad in [
            "*",
            "acme/*",
            "a/b/c",
            "",
            "acme/",
            "/kika",
            "acme/kika.git",
            "acme/kika.GIT",
            "acme.git",
            "..",
            "acme/..",
            "ac me",
            "acme\u{1b}",
        ] {
            let err = AllowRule::parse(bad).unwrap_err();
            assert!(err.starts_with(&format!("{bad:?} ")), "{bad:?}: {err}");
        }
        for good in ["acme", "acme/kika", "a.b_c-d/k.i_k-a"] {
            assert!(AllowRule::parse(good).is_ok(), "{good:?}");
        }
    }

    #[test]
    fn a_host_allows_all_without_a_list_and_nothing_with_an_empty_one() {
        let host = ForgeHost::parse("github.com").unwrap();
        assert!(host.allows(&repo("anyone", "anything")));
        let none = host.clone().with_allow(vec![]);
        assert!(!none.allows(&repo("acme", "x")));
        let acme = host.with_allow(vec![AllowRule::parse("acme").unwrap()]);
        assert!(acme.allows(&repo("acme", "x")));
        assert!(!acme.allows(&repo("other", "x")));
    }
}
