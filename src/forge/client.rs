//! The one forge client. Every `gh` call goes through [`ForgeContext`]:
//! it applies the host's `[forge.allow]` gate, runs `gh` in the cache
//! directory and reads the clock, so no caller can reach the network for
//! a repository the user never opted in.

use std::path::PathBuf;

use super::cache;
use super::github::Gh;
use super::merge::MergeMethod;
use super::task::ForgeContext;
use super::{CreateError, ForgeHost, ForgeRepo, ForgeStatus};
use crate::git::commit_files::CommitId;

impl ForgeContext {
    /// Seconds since the epoch, for rate limits and the cache.
    pub fn now(&self) -> i64 {
        crate::effect::unix_now()
    }

    /// Where `gh` runs: the cache directory, never the repository, so
    /// `gh` reads no repository's git config.
    fn cwd(&self) -> PathBuf {
        self.cache_dir
            .clone()
            .filter(|dir| cache::ensure_dir(dir).is_ok())
            .unwrap_or_else(std::env::temp_dir)
    }

    /// A `gh` for `repo` on `host`, or `None` when the host's allow-list
    /// leaves `repo` out: then nothing may reach the network.
    pub fn gh<'a>(&'a self, host: &'a ForgeHost, repo: &ForgeRepo, now: i64) -> Option<Gh<'a>> {
        host.allows(repo).then(|| Gh {
            runner: self.gh.as_ref(),
            sink: &self.sink,
            host,
            cwd: self.cwd(),
            now,
        })
    }

    /// Open a pull request on `repo`; see [`Gh::create_pull`]. A
    /// repository the allow-list leaves out fails without a call.
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
    /// the allow-list leaves out fails without a call.
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
    use crate::forge::allow::AllowRule;
    use crate::forge::github::fixtures::CREATE_201;
    use crate::forge::github::{FakeGh, answer};
    use crate::git::fake::TempDir;
    use crate::git::recording::CommandSink;

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
