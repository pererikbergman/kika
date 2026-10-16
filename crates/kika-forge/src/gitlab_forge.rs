//! The GitLab forge adapter: badges through `glab`, and nothing else yet.

use crate::cache::Cached;
use crate::cli::{CliStatus, CliVersion, judge};
use crate::discussion::PrDiscussion;
use crate::discussion_write::{DiscussionWrite, Written};
use crate::forge::{Badges, Capability, Forge, Unsupported, Words};
use crate::github::Refusal;
use crate::gitlab::Glab;
use crate::head::Head;
use crate::review::{MergeMethod, PrDetail, Queues};
use crate::url::ForgeKind;
use crate::{CreateError, ForgeRepo};
use kika_git::commit_files::CommitId;

/// GitLab behind [`Forge`]. What it cannot do yet answers
/// [`Unsupported`] without a call.
pub struct GitLabForge<'a>(pub(crate) Glab<'a>);

impl GitLabForge<'_> {
    /// What GitLab can be asked so far.
    pub const SUPPORTS: &'static [Capability] = &[Capability::Badges];

    /// How GitLab is named to the user: merge requests, numbered `!N`.
    pub const WORDS: Words = Words {
        cli: "glab",
        requests: "merge requests",
        sign: '!',
    };

    /// The oldest `glab` accepted; none yet, as no flag needs a newer one.
    pub const MIN_CLI: Option<CliVersion> = None;

    fn lacks(capability: Capability) -> Unsupported {
        Unsupported {
            kind: ForgeKind::GitLab,
            capability,
        }
    }
}

impl Forge for GitLabForge<'_> {
    fn kind(&self) -> ForgeKind {
        ForgeKind::GitLab
    }

    fn check(&self) -> CliStatus {
        judge(self.0.run_version(), Self::MIN_CLI)
    }

    /// GitLab has no probe: each refresh asks GraphQL, and nothing is
    /// reused or kept with an ETag.
    fn badges(&self, repo: &ForgeRepo, heads: &[Head], _cached: Option<&Cached>) -> Badges {
        Badges::Fresh {
            found: self.0.fetch_badges(repo, heads),
            etag: None,
        }
    }

    fn create_pull(
        &self,
        _repo: &ForgeRepo,
        _head: &str,
        _base: &str,
        _title: &str,
    ) -> Result<(u64, String), CreateError> {
        Err(CreateError::Unsupported(Self::lacks(
            Capability::CreatePull,
        )))
    }

    fn merge_pull(
        &self,
        _repo: &ForgeRepo,
        _number: u64,
        _method: MergeMethod,
        _head_oid: &CommitId,
    ) -> Result<(), CreateError> {
        Err(CreateError::Unsupported(Self::lacks(Capability::Merge)))
    }

    fn set_file_viewed(
        &self,
        _node_id: &str,
        _path: &str,
        _viewed: bool,
    ) -> Result<(), CreateError> {
        Err(CreateError::Unsupported(Self::lacks(Capability::Viewed)))
    }

    fn write_discussion(&self, _write: &DiscussionWrite) -> Result<Written, CreateError> {
        Err(CreateError::Unsupported(Self::lacks(
            Capability::DiscussionWrite,
        )))
    }

    fn fetch_queues(&self, _repo: &ForgeRepo) -> Result<Queues, Refusal> {
        Err(Refusal::Unsupported(Self::lacks(Capability::Review)))
    }

    fn fetch_detail(&self, _repo: &ForgeRepo, _number: u64) -> Result<Option<PrDetail>, Refusal> {
        Err(Refusal::Unsupported(Self::lacks(Capability::Review)))
    }

    fn fetch_discussion(
        &self,
        _repo: &ForgeRepo,
        _number: u64,
    ) -> Result<Option<PrDiscussion>, Refusal> {
        Err(Refusal::Unsupported(Self::lacks(Capability::Review)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ForgeHost;
    use crate::gitlab::FakeGlab;
    use crate::review::MergeMethod;
    use kika_git::recording::CommandSink;

    fn repo() -> ForgeRepo {
        ForgeRepo {
            host: "gitlab.example.com".into(),
            owner: "acme".into(),
            name: "widgets".into(),
        }
    }

    #[test]
    fn every_method_but_badges_is_unsupported_without_a_call() {
        let runner = FakeGlab::new();
        let sink = CommandSink::new();
        let host = ForgeHost::parse("gitlab.example.com")
            .unwrap()
            .with_kind(ForgeKind::GitLab);
        let forge = GitLabForge(Glab {
            runner: &runner,
            sink: &sink,
            host: &host,
            cwd: "/cache".into(),
            now: 0,
        });
        let lacks = |capability| Unsupported {
            kind: ForgeKind::GitLab,
            capability,
        };
        let created = |capability| CreateError::Unsupported(lacks(capability));
        let refused = |capability| Refusal::Unsupported(lacks(capability));
        let head = CommitId::parse(&"a".repeat(40)).unwrap();
        let write = DiscussionWrite::Comment {
            subject: "MR_1".into(),
            body: "Thanks".into(),
        };

        assert_eq!(forge.kind(), ForgeKind::GitLab);
        assert_eq!(
            forge
                .create_pull(&repo(), "topic", "main", "Add login")
                .unwrap_err(),
            created(Capability::CreatePull)
        );
        assert_eq!(
            forge
                .merge_pull(&repo(), 7, MergeMethod::Merge, &head)
                .unwrap_err(),
            created(Capability::Merge)
        );
        assert_eq!(
            forge.set_file_viewed("MR_1", "src/a.rs", true).unwrap_err(),
            created(Capability::Viewed)
        );
        assert_eq!(
            forge.write_discussion(&write).unwrap_err(),
            created(Capability::DiscussionWrite)
        );
        assert_eq!(
            forge.fetch_queues(&repo()).unwrap_err(),
            refused(Capability::Review)
        );
        assert_eq!(
            forge.fetch_detail(&repo(), 7).unwrap_err(),
            refused(Capability::Review)
        );
        assert_eq!(
            forge.fetch_discussion(&repo(), 7).unwrap_err(),
            refused(Capability::Review)
        );
        assert!(runner.calls().is_empty());
        // Each one is a capability the adapter does not list.
        for capability in Capability::ALL {
            assert_eq!(
                GitLabForge::SUPPORTS.contains(&capability),
                capability == Capability::Badges
            );
        }
    }

    #[test]
    fn check_runs_glab_version_once_with_no_host_and_no_stdin() {
        let sink = CommandSink::new();
        let host = ForgeHost::parse_gitlab("gitlab.example.com").unwrap();
        let check = |runner: &FakeGlab| {
            GitLabForge(Glab {
                runner,
                sink: &sink,
                host: &host,
                cwd: "/cache".into(),
                now: 0,
            })
            .check()
        };
        let runner = FakeGlab::new().reply(crate::github::answer("glab 1.36.0 (f8b8d1b2)\n"));
        assert_eq!(check(&runner), CliStatus::Ready);
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].kind, crate::gitlab::GlabCall::Version);
        assert_eq!(calls[0].args(), ["--version"]);
        assert!(calls[0].stdin().is_empty());
        assert_eq!(calls[0].display_line(), "glab --version");
        assert_eq!(check(&FakeGlab::new().missing()), CliStatus::NotInstalled);
        assert_eq!(GitLabForge::MIN_CLI, None);
    }
}
