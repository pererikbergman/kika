//! The one forge client. Every `gh` and `glab` call goes through
//! [`ForgeContext`]: it checks the repository is on the host and that the
//! host's `[forge.allow]` lets it through, and only then hands out the
//! host's forge adapter ([`GitHubForge`](crate::forge::GitHubForge) or
//! [`GitLabForge`](crate::forge::GitLabForge)) as a [`Forge`]. The adapter
//! runs its CLI in the cache directory and reads the clock, so no caller
//! can reach the network for a repository the user never opted in.

use std::path::PathBuf;

use super::cache;
use super::discussion_write::{DiscussionWrite, Written};
use super::forge::{self, Forge};
use super::github::Gh;
use super::gitlab::Glab;
use super::merge::MergeMethod;
use super::task::ForgeContext;
use super::{CreateError, ForgeHost, ForgeRepo, ForgeStatus};
use kika_git::commit_files::CommitId;

impl ForgeContext {
    /// Seconds since the epoch, for rate limits and the cache.
    pub fn now(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64)
    }

    /// Where `gh` and `glab` run: the cache directory, never the
    /// repository, so neither reads any repository's git config.
    fn cwd(&self) -> PathBuf {
        self.cache_dir
            .clone()
            .filter(|dir| cache::ensure_dir(dir).is_ok())
            .unwrap_or_else(std::env::temp_dir)
    }

    /// Whether `repo` may be asked about on `host`: it is on the host, and
    /// the host's allow-list lets the repository through.
    fn opens(host: &ForgeHost, repo: &ForgeRepo) -> bool {
        repo.host.eq_ignore_ascii_case(host.as_str()) && host.allows(repo)
    }

    /// The forge adapter of `host`'s kind for `repo`, or `None` when
    /// `repo` is on another host or the host's allow-list leaves it out:
    /// then nothing may reach the network.
    pub fn forge<'a>(
        &'a self,
        host: &'a ForgeHost,
        repo: &ForgeRepo,
        now: i64,
    ) -> Option<Box<dyn Forge + 'a>> {
        Self::opens(host, repo).then(|| forge::adapter(self, host, now))
    }

    /// The `gh` calls for `host`. Only [`forge::adapter`] builds one,
    /// after [`Self::forge`]'s checks.
    pub(crate) fn gh<'a>(&'a self, host: &'a ForgeHost, now: i64) -> Gh<'a> {
        Gh {
            runner: self.gh.as_ref(),
            sink: &self.sink,
            host,
            cwd: self.cwd(),
            now,
        }
    }

    /// The `glab` calls for `host`. Only [`forge::adapter`] builds one,
    /// after [`Self::forge`]'s checks.
    pub(crate) fn glab<'a>(&'a self, host: &'a ForgeHost, now: i64) -> Glab<'a> {
        Glab {
            runner: self.glab.as_ref(),
            sink: &self.sink,
            host,
            cwd: self.cwd(),
            now,
        }
    }

    /// Open a pull request on `repo`; see [`Forge::create_pull`]. A
    /// repository [`Self::forge`] refuses fails without a call.
    pub fn create_pull(
        &self,
        host: &ForgeHost,
        repo: &ForgeRepo,
        head: &str,
        base: &str,
        title: &str,
    ) -> Result<(u64, String), CreateError> {
        self.forge(host, repo, self.now())
            .ok_or(CreateError::Status(ForgeStatus::Failed))?
            .create_pull(repo, head, base, title)
    }

    /// Merge pull request `number`; see [`Forge::merge_pull`]. A
    /// repository [`Self::forge`] refuses fails without a call.
    pub fn merge_pull(
        &self,
        host: &ForgeHost,
        repo: &ForgeRepo,
        number: u64,
        method: MergeMethod,
        head_oid: &CommitId,
    ) -> Result<(), CreateError> {
        self.forge(host, repo, self.now())
            .ok_or(CreateError::Status(ForgeStatus::Failed))?
            .merge_pull(repo, number, method, head_oid)
    }

    /// Mark `path` viewed or not; see [`Forge::set_file_viewed`]. A
    /// repository [`Self::forge`] refuses fails without a call.
    pub fn set_file_viewed(
        &self,
        host: &ForgeHost,
        repo: &ForgeRepo,
        node_id: &str,
        path: &str,
        viewed: bool,
    ) -> Result<(), CreateError> {
        self.forge(host, repo, self.now())
            .ok_or(CreateError::Status(ForgeStatus::Failed))?
            .set_file_viewed(node_id, path, viewed)
    }

    /// Reply to, resolve or unresolve a review thread, comment on a pull
    /// request or submit a review; see [`Forge::write_discussion`]. A
    /// repository [`Self::forge`] refuses fails without a call.
    pub fn write_discussion(
        &self,
        host: &ForgeHost,
        repo: &ForgeRepo,
        write: &DiscussionWrite,
    ) -> Result<Written, CreateError> {
        self.forge(host, repo, self.now())
            .ok_or(CreateError::Status(ForgeStatus::Failed))?
            .write_discussion(write)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::allow::AllowRule;
    use crate::forge::{Capability, Unsupported};
    use crate::github::fixtures::CREATE_201;
    use crate::github::{FakeGh, answer};
    use crate::gitlab::FakeGlab;
    use crate::url::ForgeKind;
    use kika_git::fake::TempDir;
    use kika_git::recording::CommandSink;

    fn repo() -> ForgeRepo {
        ForgeRepo {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "widgets".into(),
        }
    }

    fn context(gh: Arc<FakeGh>, cache_dir: Option<PathBuf>) -> ForgeContext {
        ForgeContext {
            gh,
            glab: Arc::new(FakeGlab::new()),
            sink: CommandSink::new(),
            cache_dir,
        }
    }

    #[test]
    fn a_repository_the_allow_list_leaves_out_never_reaches_gh() {
        let runner = Arc::new(FakeGh::new());
        let ctx = context(runner.clone(), None);
        let host = ForgeHost::parse("github.com")
            .unwrap()
            .with_allow(vec![AllowRule::parse("someone-else").unwrap()]);
        let failed = Err(CreateError::Status(ForgeStatus::Failed));

        assert!(ctx.forge(&host, &repo(), 0).is_none());
        assert_eq!(
            ctx.create_pull(&host, &repo(), "topic", "main", "Add login")
                .map(|_| ()),
            failed
        );
        let head = CommitId::parse(&"a".repeat(40)).unwrap();
        assert_eq!(
            ctx.merge_pull(&host, &repo(), 7, MergeMethod::Merge, &head),
            failed
        );
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn a_repository_on_another_host_never_reaches_gh() {
        let runner = Arc::new(FakeGh::new());
        let ctx = context(runner.clone(), None);
        let host = ForgeHost::parse("gitlab.example").unwrap();
        let failed = Err(CreateError::Status(ForgeStatus::Failed));

        assert!(ctx.forge(&host, &repo(), 0).is_none());
        assert_eq!(
            ctx.create_pull(&host, &repo(), "topic", "main", "Add login")
                .map(|_| ()),
            failed
        );
        let head = CommitId::parse(&"a".repeat(40)).unwrap();
        assert_eq!(
            ctx.merge_pull(&host, &repo(), 7, MergeMethod::Merge, &head),
            failed
        );
        assert!(runner.calls().is_empty());

        // The host is compared without regard to case.
        let github = ForgeHost::parse("github.com").unwrap();
        let shown = ForgeRepo {
            host: "GitHub.com".into(),
            ..repo()
        };
        assert!(ctx.forge(&github, &shown, 0).is_some());
    }

    fn gitlab_repo() -> ForgeRepo {
        ForgeRepo {
            host: "gitlab.example.com".into(),
            ..repo()
        }
    }

    fn gitlab(name: &str) -> ForgeHost {
        ForgeHost::parse(name).unwrap().with_kind(ForgeKind::GitLab)
    }

    #[test]
    fn each_cli_is_only_asked_about_its_own_kind_of_host() {
        let gh = Arc::new(FakeGh::new());
        let glab = Arc::new(FakeGlab::new());
        let ctx = ForgeContext {
            gh: gh.clone(),
            glab: glab.clone(),
            sink: CommandSink::new(),
            cache_dir: None,
        };
        let failed = Err(CreateError::Status(ForgeStatus::Failed));
        let lab = gitlab("gitlab.example.com");

        let kind = |host: &ForgeHost, repo: &ForgeRepo| ctx.forge(host, repo, 0).map(|f| f.kind());

        // A GitLab-kind host gets the GitLab adapter, which never reaches
        // `gh`, not even to create or merge.
        assert_eq!(kind(&lab, &gitlab_repo()), Some(ForgeKind::GitLab));
        let lacks = |capability| {
            Err(CreateError::Unsupported(Unsupported {
                kind: ForgeKind::GitLab,
                capability,
            }))
        };
        assert_eq!(
            ctx.create_pull(&lab, &gitlab_repo(), "topic", "main", "Add login")
                .map(|_| ()),
            lacks(Capability::CreatePull)
        );
        let head = CommitId::parse(&"a".repeat(40)).unwrap();
        assert_eq!(
            ctx.merge_pull(&lab, &gitlab_repo(), 7, MergeMethod::Merge, &head),
            lacks(Capability::Merge)
        );

        // A GitHub-kind host gets the GitHub adapter, never `glab`.
        let hub = ForgeHost::parse("github.com").unwrap();
        assert_eq!(kind(&hub, &repo()), Some(ForgeKind::GitHub));

        // Another host, or a repository the allow-list leaves out.
        assert_eq!(kind(&gitlab("gitlab.com"), &gitlab_repo()), None);
        let narrowed = lab.with_allow(vec![AllowRule::parse("someone-else").unwrap()]);
        assert_eq!(kind(&narrowed, &gitlab_repo()), None);
        assert_eq!(
            ctx.create_pull(&narrowed, &gitlab_repo(), "topic", "main", "Add login")
                .map(|_| ()),
            failed
        );
        // The host is compared without regard to case.
        let shown = ForgeRepo {
            host: "GitLab.Example.com".into(),
            ..gitlab_repo()
        };
        assert_eq!(
            kind(&gitlab("gitlab.example.com"), &shown),
            Some(ForgeKind::GitLab)
        );

        assert!(gh.calls().is_empty());
        assert!(glab.calls().is_empty());
    }

    #[test]
    fn glab_runs_in_the_cache_dir_or_else_the_temp_dir() {
        let tmp = TempDir::new("forge-client-glab");
        let dir = tmp.path().join("forge");
        let host = gitlab("gitlab.example.com");
        let ctx = ForgeContext {
            cache_dir: Some(dir.clone()),
            ..context(Arc::new(FakeGh::new()), None)
        };
        assert_eq!(ctx.glab(&host, 5).cwd, dir);
        assert!(dir.is_dir());
        let ctx = context(Arc::new(FakeGh::new()), None);
        let glab = ctx.glab(&host, 5);
        assert_eq!((glab.cwd, glab.now), (std::env::temp_dir(), 5));
    }

    #[test]
    fn a_viewed_mark_reaches_gh_only_for_an_allowed_repository_on_the_host() {
        let runner = Arc::new(FakeGh::new());
        let ctx = context(runner.clone(), None);
        let failed = Err(CreateError::Status(ForgeStatus::Failed));
        let left_out = ForgeHost::parse("github.com")
            .unwrap()
            .with_allow(vec![AllowRule::parse("someone-else").unwrap()]);
        let elsewhere = ForgeHost::parse("gitlab.example").unwrap();
        for host in [&left_out, &elsewhere] {
            for viewed in [true, false] {
                assert_eq!(
                    ctx.set_file_viewed(host, &repo(), "PR_kwDOA1", "src/a.rs", viewed),
                    failed
                );
            }
        }
        assert!(runner.calls().is_empty());

        let marked = "HTTP/2.0 200 OK\r\n\r\n{\"data\":{\"markFileAsViewed\":{}}}";
        let runner = Arc::new(FakeGh::new().reply(answer(marked)));
        let ctx = context(runner.clone(), None);
        let github = ForgeHost::parse("github.com").unwrap();
        assert_eq!(
            ctx.set_file_viewed(&github, &repo(), "PR_kwDOA1", "src/a.rs", true),
            Ok(())
        );
        assert_eq!(runner.calls().len(), 1);
    }

    #[test]
    fn a_discussion_write_reaches_gh_only_for_an_allowed_repository_on_the_host() {
        let write = DiscussionWrite::Comment {
            subject: "PR_kwDOA1".into(),
            body: "Thanks".into(),
        };
        let runner = Arc::new(FakeGh::new());
        let ctx = context(runner.clone(), None);
        let failed = Err(CreateError::Status(ForgeStatus::Failed));
        let left_out = ForgeHost::parse("github.com")
            .unwrap()
            .with_allow(vec![AllowRule::parse("someone-else").unwrap()]);
        let elsewhere = ForgeHost::parse("gitlab.example").unwrap();
        for host in [&left_out, &elsewhere] {
            assert_eq!(ctx.write_discussion(host, &repo(), &write), failed);
        }
        assert!(runner.calls().is_empty());

        let posted = "HTTP/2.0 200 OK\r\n\r\n{\"data\":{\"addComment\":{\"commentEdge\":{\"node\":{\"id\":\"IC_1\"}}}}}";
        let runner = Arc::new(FakeGh::new().reply(answer(posted)));
        let ctx = context(runner.clone(), None);
        let github = ForgeHost::parse("github.com").unwrap();
        assert_eq!(
            ctx.write_discussion(&github, &repo(), &write),
            Ok(Written::Posted)
        );
        assert_eq!(runner.calls().len(), 1);
    }

    #[test]
    fn gh_runs_in_the_cache_dir_or_else_the_temp_dir() {
        let host = ForgeHost::parse("github.com").unwrap();
        let tmp = TempDir::new("forge-client");
        let dir = tmp.path().join("forge");

        let runner = Arc::new(FakeGh::new().reply(answer(CREATE_201)));
        let ctx = context(runner.clone(), Some(dir.clone()));
        assert!(
            ctx.create_pull(&host, &repo(), "topic", "main", "Add login")
                .is_ok()
        );
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].cwd, dir);

        let runner = Arc::new(FakeGh::new().reply(answer(CREATE_201)));
        let ctx = context(runner.clone(), None);
        assert!(
            ctx.create_pull(&host, &repo(), "topic", "main", "Add login")
                .is_ok()
        );
        assert_eq!(runner.calls()[0].cwd, std::env::temp_dir());
    }
}
