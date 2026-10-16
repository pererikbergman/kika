//! The GitHub forge adapter: every capability, through `gh`.

use std::collections::HashSet;

use crate::cache::Cached;
use crate::discussion::PrDiscussion;
use crate::discussion_write::{DiscussionWrite, Written};
use crate::forge::{Badges, Capability, Forge};
use crate::github::{Answer, Gh, Refusal};
use crate::head::Head;
use crate::review::{MergeMethod, PrDetail, Queues};
use crate::url::ForgeKind;
use crate::{CiState, CreateError, ForgeRepo, PrState};
use kika_git::commit_files::CommitId;

/// GitHub behind [`Forge`]. Each method is [`Gh`]'s own.
pub struct GitHubForge<'a>(pub(crate) Gh<'a>);

impl GitHubForge<'_> {
    /// What GitHub can be asked: everything.
    pub const SUPPORTS: &'static [Capability] = &Capability::ALL;
}

impl Forge for GitHubForge<'_> {
    fn kind(&self) -> ForgeKind {
        ForgeKind::GitHub
    }

    /// A refresh first probes, for free, whether the pull requests
    /// changed; when they did not and the cache is still good, no GraphQL
    /// query runs. A fork skips the probe: its own list says nothing of
    /// its parent's pull requests.
    fn badges(&self, repo: &ForgeRepo, heads: &[Head], cached: Option<&Cached>) -> Badges {
        let gh = &self.0;
        let sent = cached.and_then(|c| c.etag.clone());
        let forked = cached.is_some_and(|c| c.forked);
        let etag = if forked {
            sent
        } else {
            match gh.probe(repo, sent.as_deref()) {
                Answer::Limited { until } => {
                    return Badges::Fresh {
                        found: Err(Refusal::Limited { until }),
                        etag: None,
                    };
                }
                // The cache is left as it is: its `fetched_at` stays the
                // time of the last GraphQL answer, which bounds how long
                // 304s may reuse it.
                Answer::NotModified => match cached.filter(|c| reusable(c, heads, gh.now)) {
                    Some(cached) => return Badges::Reuse(cached.badges.clone()),
                    None => sent,
                },
                Answer::Body { etag, .. } => etag,
                // No worse than without a probe: ask anyway.
                Answer::Failed(_) => None,
            }
        };
        Badges::Fresh {
            found: gh.fetch_badges(repo, heads),
            etag,
        }
    }

    fn create_pull(
        &self,
        repo: &ForgeRepo,
        head: &str,
        base: &str,
        title: &str,
    ) -> Result<(u64, String), CreateError> {
        self.0.create_pull(repo, head, base, title)
    }

    fn merge_pull(
        &self,
        repo: &ForgeRepo,
        number: u64,
        method: MergeMethod,
        head_oid: &CommitId,
    ) -> Result<(), CreateError> {
        self.0.merge_pull(repo, number, method, head_oid)
    }

    fn set_file_viewed(&self, node_id: &str, path: &str, viewed: bool) -> Result<(), CreateError> {
        self.0.set_file_viewed(node_id, path, viewed)
    }

    fn write_discussion(&self, write: &DiscussionWrite) -> Result<Written, CreateError> {
        self.0.write_discussion(write)
    }

    fn fetch_queues(&self, repo: &ForgeRepo) -> Result<Queues, Refusal> {
        self.0.fetch_queues(repo)
    }

    fn fetch_detail(&self, repo: &ForgeRepo, number: u64) -> Result<Option<PrDetail>, Refusal> {
        self.0.fetch_detail(repo, number)
    }

    fn fetch_discussion(
        &self,
        repo: &ForgeRepo,
        number: u64,
    ) -> Result<Option<PrDiscussion>, Refusal> {
        self.0.fetch_discussion(repo, number)
    }
}

/// How long, in seconds, a GraphQL answer may be reused on 304s alone.
/// CI can finish or re-run without changing the pull-request list, so a
/// green badge is asked about again at least this often.
const MAX_REUSE: i64 = 15 * 60;

/// Whether `cached` still answers for `heads` at `now` when the pull
/// requests did not change: the same heads were asked, less than
/// [`MAX_REUSE`] ago, and no open PR waits on CI, which can finish
/// without changing the pull-request list.
pub(crate) fn reusable(cached: &Cached, heads: &[Head], now: i64) -> bool {
    let waiting = cached.badges.values().any(|badge| {
        matches!(badge.state, PrState::Open | PrState::Draft)
            && matches!(badge.ci, Some(CiState::Pending | CiState::Failure))
    });
    let set = |heads: &[Head]| heads.iter().cloned().collect::<HashSet<_>>();
    // A time ahead of the clock is not trusted either.
    let young = (0..MAX_REUSE).contains(&(now - cached.fetched_at));
    set(&cached.queried) == set(heads) && young && !waiting
}
