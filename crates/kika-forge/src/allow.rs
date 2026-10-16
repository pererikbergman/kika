//! Which repositories on a host Kika may ask `gh` or `glab` about, from the global
//! `[forge.allow]` table. Naming a host opts it in; an allow-list narrows
//! that to the owners and repositories the user trusts, so opening some
//! other clone on the same host never reaches the network.

use super::ForgeRepo;
use super::remote::{MAX_PARTS, segment_ok};

/// One `[forge.allow]` entry: every repository of `owner`, or only
/// `owner/name`. On a GitLab host the entry may be a group path: then it
/// names that project, or every project under that group at any depth.
/// Whole names, no patterns, so it allows no more than it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowRule {
    /// One part or more, none holding a `/`.
    path: Vec<String>,
}

impl AllowRule {
    /// `text` as `owner` or `owner/repo`, or why it is neither. Each part
    /// follows the rules a remote URL's path does, and a repository is
    /// named without `.git`, as the forge names it.
    pub fn parse(text: &str) -> Result<Self, String> {
        let shape = "one / at most";
        Self::parse_up_to(text, 2, "is not an owner or owner/repo", shape, true)
    }

    /// `text` as a GitLab group or project path (`group`, `group/sub`,
    /// `group/sub/project`, up to [`MAX_PARTS`] parts), or why it is not
    /// one. The parts follow [`Self::parse`]'s rules, and none is `-`,
    /// which GitLab keeps for its own pages.
    pub fn parse_nested(text: &str) -> Result<Self, String> {
        let shape = format!("at most {MAX_PARTS} parts, no part that is only -");
        let what = "is not a GitLab group or project path";
        Self::parse_up_to(text, MAX_PARTS, what, &shape, false)
    }

    /// `text` split into 1 to `most` parts; `dash_ok` lets a part be `-`.
    fn parse_up_to(
        text: &str,
        most: usize,
        what: &str,
        shape: &str,
        dash_ok: bool,
    ) -> Result<Self, String> {
        let parts: Vec<&str> = text.split('/').collect();
        let part_ok = |part: &&str| {
            segment_ok(part)
                && !part.to_ascii_lowercase().ends_with(".git")
                && (dash_ok || *part != "-")
        };
        if parts.len() > most || !parts.iter().all(part_ok) {
            return Err(format!(
                "{text:?} {what}: use letters, digits, ., _ and - only, {shape}, no .git"
            ));
        }
        Ok(Self {
            path: parts.into_iter().map(String::from).collect(),
        })
    }

    /// Whether `repo` is the repository this names, or sits under the
    /// owner or group it names: its path starts with this one's, whole
    /// part by whole part, so `acme` never covers `acmecorp/x`. Case is
    /// ignored, as the forges ignore it. A GitHub path has two parts, so
    /// there this is the owner, or that one repository.
    pub fn allows(&self, repo: &ForgeRepo) -> bool {
        let full: Vec<&str> = repo.owner.split('/').chain([repo.name.as_str()]).collect();
        self.path.len() <= full.len()
            && self
                .path
                .iter()
                .zip(&full)
                .all(|(rule, part)| rule.eq_ignore_ascii_case(part))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ForgeHost;

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

    fn gitlab(owner: &str, name: &str) -> ForgeRepo {
        ForgeRepo {
            host: "gitlab.example.com".into(),
            ..repo(owner, name)
        }
    }

    fn nested(rule: &str, owner: &str, name: &str) -> bool {
        AllowRule::parse_nested(rule)
            .unwrap()
            .allows(&gitlab(owner, name))
    }

    #[test]
    fn a_group_allows_every_project_under_it_at_any_depth() {
        assert!(nested("acme", "acme", "x"));
        assert!(nested("acme", "acme/sub", "x"));
        assert!(nested("acme", "acme/a/b/c", "x"));
        assert!(nested("ACME", "Acme/Sub", "x"));
        // Whole parts, never a string prefix.
        assert!(!nested("acme", "acmecorp", "x"));
        assert!(!nested("acme", "acmecorp/sub", "x"));
        assert!(!nested("acme", "other/acme", "x"));
    }

    #[test]
    fn a_subgroup_narrows_to_its_own_projects_and_subgroups() {
        // `acme/sub` the project, and everything under the group.
        assert!(nested("acme/sub", "acme", "sub"));
        assert!(nested("acme/sub", "acme/sub", "x"));
        assert!(nested("acme/sub", "acme/sub/deep", "y"));
        assert!(nested("ACME/Sub", "acme/SUB", "x"));
        assert!(!nested("acme/sub", "acme/other", "x"));
        assert!(!nested("acme/sub", "acme", "x"));
        assert!(!nested("acme/sub", "acme", "subx"));
        assert!(!nested("acme/sub", "acme/subx", "y"));
        // A project path: that project (and anything a group of that
        // path would hold; GitLab never lets the two share a path).
        assert!(nested("acme/sub/x", "acme/sub", "x"));
        assert!(!nested("acme/sub/x", "acme/sub", "y"));
        assert!(!nested("acme/sub/x", "acme", "x"));
    }

    #[test]
    fn a_nested_path_is_refused_on_github_and_checked_on_gitlab() {
        assert!(AllowRule::parse("a/b/c").is_err());
        let deepest = vec!["g"; MAX_PARTS].join("/");
        for good in ["acme", "acme/sub", "a/b/c", deepest.as_str()] {
            assert!(AllowRule::parse_nested(good).is_ok(), "{good:?}");
        }
        let too_deep = vec!["g"; MAX_PARTS + 1].join("/");
        for bad in [
            too_deep.as_str(),
            "a//b",
            "x.git",
            "a/b/x.GIT",
            "-",
            "a/-/b",
            "",
            "a/",
            "/a",
            "a/*",
            "a/..",
        ] {
            let err = AllowRule::parse_nested(bad).unwrap_err();
            assert!(err.starts_with(&format!("{bad:?} ")), "{bad:?}: {err}");
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
