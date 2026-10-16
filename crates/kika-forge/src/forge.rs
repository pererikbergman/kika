//! The one **Forge** interface. Each forge kind has one forge adapter
//! ([`GitHubForge`], [`GitLabForge`]) behind [`Forge`], and only
//! [`ForgeContext::forge`](crate::task::ForgeContext::forge) hands one
//! out, after the host and allow-list checks. The rest of Kika never asks
//! which forge kind it talks to in order to decide what to do: a
//! capability an adapter lacks answers [`Unsupported`].

use std::collections::HashMap;

pub use super::github_forge::GitHubForge;
#[cfg(test)]
pub(crate) use super::github_forge::reusable;
pub use super::gitlab_forge::GitLabForge;

use super::cache::Cached;
use super::discussion::PrDiscussion;
use super::discussion_write::{DiscussionWrite, Written};
use super::github::{Fetched, Refusal};
use super::head::Head;
use super::review::{MergeMethod, PrDetail, Queues};
use super::task::ForgeContext;
use super::url::ForgeKind;
use super::{CreateError, ForgeHost, ForgeRepo, PrBadge};
use kika_git::branches::BranchName;
use kika_git::commit_files::CommitId;

/// What one forge can be asked. Every method is the adapter's own, with
/// no default, so a new adapter has to answer each one, if only with
/// [`Unsupported`].
pub trait Forge {
    /// Which forge kind the adapter talks to, for display.
    fn kind(&self) -> ForgeKind;

    /// The pull request of each of `heads` in `repo`. `cached` is the
    /// last answer kept, which an adapter may reuse when the forge says
    /// nothing changed.
    fn badges(&self, repo: &ForgeRepo, heads: &[Head], cached: Option<&Cached>) -> Badges;

    /// Open a pull request on `repo` from `head` into `base`: its number
    /// and page.
    fn create_pull(
        &self,
        repo: &ForgeRepo,
        head: &str,
        base: &str,
        title: &str,
    ) -> Result<(u64, String), CreateError>;

    /// Merge pull request `number` by `method`, only while its head is
    /// still `head_oid`.
    fn merge_pull(
        &self,
        repo: &ForgeRepo,
        number: u64,
        method: MergeMethod,
        head_oid: &CommitId,
    ) -> Result<(), CreateError>;

    /// Mark `path` of the pull request the forge knows as `node_id`
    /// viewed, or not.
    fn set_file_viewed(&self, node_id: &str, path: &str, viewed: bool) -> Result<(), CreateError>;

    /// Reply to, resolve or unresolve a thread, comment, or submit a
    /// review.
    fn write_discussion(&self, write: &DiscussionWrite) -> Result<Written, CreateError>;

    /// The review queues of `repo`.
    fn fetch_queues(&self, repo: &ForgeRepo) -> Result<Queues, Refusal>;

    /// Pull request `number` of `repo`; `None` when the forge has none.
    fn fetch_detail(&self, repo: &ForgeRepo, number: u64) -> Result<Option<PrDetail>, Refusal>;

    /// The discussion of pull request `number`; `None` when the forge
    /// has no such pull request.
    fn fetch_discussion(
        &self,
        repo: &ForgeRepo,
        number: u64,
    ) -> Result<Option<PrDiscussion>, Refusal>;
}

/// What a badge load came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Badges {
    /// Nothing changed and the cached badges still answer: the cache is
    /// left as it is.
    Reuse(HashMap<BranchName, PrBadge>),
    /// The forge's answer, and the ETag to keep with it.
    Fresh {
        found: Result<Fetched, Refusal>,
        etag: Option<String>,
    },
}

/// One thing a forge may be asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Badges,
    CreatePull,
    Merge,
    Review,
    Viewed,
    DiscussionWrite,
}

impl Capability {
    pub const ALL: [Capability; 6] = [
        Capability::Badges,
        Capability::CreatePull,
        Capability::Merge,
        Capability::Review,
        Capability::Viewed,
        Capability::DiscussionWrite,
    ];

    /// The capability as the start of a sentence, for user text.
    pub fn label(self) -> &'static str {
        match self {
            Capability::Badges => "Showing pull request badges",
            Capability::CreatePull => "Creating a pull request",
            Capability::Merge => "Merging a pull request",
            Capability::Review => "Reviewing pull requests",
            Capability::Viewed => "Marking files viewed",
            Capability::DiscussionWrite => "Writing to a discussion",
        }
    }
}

/// A forge kind's adapter has no `capability` yet. One typed answer, so
/// its text is made in one place: [`Unsupported::message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unsupported {
    pub kind: ForgeKind,
    pub capability: Capability,
}

impl Unsupported {
    /// What the user is told.
    pub fn message(&self) -> String {
        format!(
            "{} is not available on {} yet",
            self.capability.label(),
            self.kind.name()
        )
    }
}

/// Whether a `kind` forge has `capability`, without building an adapter,
/// so pure state can ask. Each adapter's own list answers.
pub fn supports(kind: ForgeKind, capability: Capability) -> Result<(), Unsupported> {
    let list = match kind {
        ForgeKind::GitHub => GitHubForge::SUPPORTS,
        ForgeKind::GitLab => GitLabForge::SUPPORTS,
    };
    match list.contains(&capability) {
        true => Ok(()),
        false => Err(Unsupported { kind, capability }),
    }
}

/// The adapter for `host`'s forge kind. The caller has already checked
/// the repository is on `host` and allowed: this is the one place a
/// forge kind picks the code that runs.
pub(crate) fn adapter<'a>(
    ctx: &'a ForgeContext,
    host: &'a ForgeHost,
    now: i64,
) -> Box<dyn Forge + 'a> {
    match host.kind() {
        ForgeKind::GitHub => Box::new(GitHubForge(ctx.gh(host, now))),
        ForgeKind::GitLab => Box::new(GitLabForge(ctx.glab(host, now))),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    #[test]
    fn github_supports_every_capability_and_gitlab_only_badges() {
        for capability in Capability::ALL {
            assert_eq!(supports(ForgeKind::GitHub, capability), Ok(()));
            let gitlab = supports(ForgeKind::GitLab, capability);
            match capability {
                Capability::Badges => assert_eq!(gitlab, Ok(())),
                _ => assert_eq!(
                    gitlab,
                    Err(Unsupported {
                        kind: ForgeKind::GitLab,
                        capability
                    })
                ),
            }
        }
    }

    #[test]
    fn unsupported_says_what_is_missing_and_where() {
        let unsupported = supports(ForgeKind::GitLab, Capability::CreatePull).unwrap_err();
        assert_eq!(
            unsupported.message(),
            "Creating a pull request is not available on GitLab yet"
        );
    }

    /// The `.rs` files under `dir`, recursively.
    fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn forge_kind_matches_stay_in_adapters() {
        let forge_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let app_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src");
        let mut files = Vec::new();
        sources(&forge_src, &mut files);
        sources(&app_src, &mut files);
        assert!(files.len() > 20, "the scan found the sources: {files:?}");
        // URLs are shaped by kind, and a host is GitHub unless the config
        // says otherwise; everywhere else asks the adapter.
        let allowed = |path: &Path| {
            [
                "forge.rs",
                "github_forge.rs",
                "gitlab_forge.rs",
                "url.rs",
                "lib.rs",
            ]
            .iter()
            .any(|name| path == forge_src.join(name))
        };
        let mut found = Vec::new();
        for path in files.iter().filter(|p| !allowed(p)) {
            let text = std::fs::read_to_string(path).unwrap();
            let live = text.split("#[cfg(test)]").next().unwrap_or_default();
            for (n, line) in live.lines().enumerate() {
                if line.contains("ForgeKind::GitHub") || line.contains("ForgeKind::GitLab") {
                    found.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                }
            }
        }
        assert!(
            found.is_empty(),
            "a forge kind decides behaviour outside the adapters:\n{}",
            found.join("\n")
        );
    }
}
