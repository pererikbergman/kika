//! The one forge client. Every `gh` and `glab` call goes through
//! [`ForgeContext`]: it checks the repository is on the host, that the
//! host is the CLI's kind of forge and that the host's `[forge.allow]`
//! lets it through, runs the CLI in the cache directory and reads the
//! clock, so no caller can reach the network for a repository the user
//! never opted in.

use std::path::PathBuf;

use super::cache;
use super::github::Gh;
use super::gitlab::Glab;
use super::merge::MergeMethod;
use super::task::ForgeContext;
use super::url::ForgeKind;
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

    /// Whether `repo` may be asked about on `host` as a `kind` forge: it
    /// is on the host, the host is of that kind, and its allow-list lets
    /// the repository through.
    fn opens(host: &ForgeHost, repo: &ForgeRepo, kind: ForgeKind) -> bool {
        repo.host.eq_ignore_ascii_case(host.as_str()) && host.kind() == kind && host.allows(repo)
    }

    /// A `gh` for `repo` on `host`, or `None` when `repo` is on another
    /// host, the host is not GitHub, or the host's allow-list leaves it
    /// out: then nothing may reach the network.
    pub fn gh<'a>(&'a self, host: &'a ForgeHost, repo: &ForgeRepo, now: i64) -> Option<Gh<'a>> {
        Self::opens(host, repo, ForgeKind::GitHub).then(|| Gh {
            runner: self.gh.as_ref(),
            sink: &self.sink,
            host,
            cwd: self.cwd(),
            now,
        })
    }

    /// A `glab` for `repo` on `host`, or `None` when `repo` is on another
    /// host, the host is not GitLab, or the host's allow-list leaves it
    /// out: then nothing may reach the network.
    pub fn glab<'a>(&'a self, host: &'a ForgeHost, repo: &ForgeRepo, now: i64) -> Option<Glab<'a>> {
        Self::opens(host, repo, ForgeKind::GitLab).then(|| Glab {
            runner: self.glab.as_ref(),
            sink: &self.sink,
            host,
            cwd: self.cwd(),
            now,
        })
    }

    /// Open a pull request on `repo`; see [`Gh::create_pull`]. A
    /// repository [`Self::gh`] refuses fails without a call.
    pub fn create_pull(
        &self,
        host: &ForgeHost,
        repo: &ForgeRepo,
        head: &str,
        base: &str,
        title: &str,
    ) -> Result<(u64, String), CreateError> {
        self.gh(host, repo, self.now())
            .ok_or(CreateError::Status(ForgeStatus::Failed))?
            .create_pull(repo, head, base, title)
    }

    /// Merge pull request `number`; see [`Gh::merge_pull`]. A repository
    /// [`Self::gh`] refuses fails without a call.
    pub fn merge_pull(
        &self,
        host: &ForgeHost,
        repo: &ForgeRepo,
        number: u64,
        method: MergeMethod,
        head_oid: &CommitId,
    ) -> Result<(), CreateError> {
        self.gh(host, repo, self.now())
            .ok_or(CreateError::Status(ForgeStatus::Failed))?
            .merge_pull(repo, number, method, head_oid)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::allow::AllowRule;
    use crate::github::fixtures::CREATE_201;
    use crate::github::{FakeGh, answer};
    use crate::gitlab::FakeGlab;
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

        assert!(ctx.gh(&host, &repo(), 0).is_none());
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

        assert!(ctx.gh(&host, &repo(), 0).is_none());
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
        assert!(ctx.gh(&github, &shown, 0).is_some());
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

        // A GitLab-kind host never reaches `gh`, not even to create or merge.
        assert!(ctx.gh(&lab, &gitlab_repo(), 0).is_none());
        assert_eq!(
            ctx.create_pull(&lab, &gitlab_repo(), "topic", "main", "Add login")
                .map(|_| ()),
            failed
        );
        let head = CommitId::parse(&"a".repeat(40)).unwrap();
        assert_eq!(
            ctx.merge_pull(&lab, &gitlab_repo(), 7, MergeMethod::Merge, &head),
            failed
        );
        assert!(ctx.glab(&lab, &gitlab_repo(), 0).is_some());

        // A GitHub-kind host never reaches `glab`.
        let hub = ForgeHost::parse("github.com").unwrap();
        assert!(ctx.glab(&hub, &repo(), 0).is_none());
        assert!(ctx.gh(&hub, &repo(), 0).is_some());

        // Another host, or a repository the allow-list leaves out.
        assert!(ctx.glab(&gitlab("gitlab.com"), &gitlab_repo(), 0).is_none());
        let narrowed = lab.with_allow(vec![AllowRule::parse("someone-else").unwrap()]);
        assert!(ctx.glab(&narrowed, &gitlab_repo(), 0).is_none());
        // The host is compared without regard to case.
        let shown = ForgeRepo {
            host: "GitLab.Example.com".into(),
            ..gitlab_repo()
        };
        assert!(ctx.glab(&gitlab("gitlab.example.com"), &shown, 0).is_some());

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
        assert_eq!(ctx.glab(&host, &gitlab_repo(), 5).unwrap().cwd, dir);
        assert!(dir.is_dir());
        let ctx = context(Arc::new(FakeGh::new()), None);
        let glab = ctx.glab(&host, &gitlab_repo(), 5).unwrap();
        assert_eq!((glab.cwd, glab.now), (std::env::temp_dir(), 5));
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
