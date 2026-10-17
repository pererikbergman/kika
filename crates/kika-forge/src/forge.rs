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
use super::cli::CliStatus;
use super::discussion::PrDiscussion;
use super::discussion_write::{DiscussionWrite, ReviewEvent, Written};
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

    /// Whether the adapter's CLI is installed and usable, from its version
    /// command alone: local, with no host, no network and no token.
    fn check(&self) -> CliStatus;

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

/// How a forge kind names its CLI, its requests and their numbers in
/// user text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Words {
    /// The forge's own name.
    pub name: &'static str,
    /// The CLI that holds the login, as the user runs it.
    pub cli: &'static str,
    /// What one of its pull requests is called.
    pub request: &'static str,
    /// What its pull requests are called, plural.
    pub requests: &'static str,
    /// The short name of one, as in `PR #7`.
    pub short: &'static str,
    /// What goes before a request's number.
    pub sign: char,
    /// The `[forge]` key that names one of its hosts, as the config takes it.
    pub host_key: &'static str,
}

/// `kind`'s words, without building an adapter, so a view can ask. Each
/// adapter's own list answers, as [`supports`] does.
pub fn words(kind: ForgeKind) -> &'static Words {
    match kind {
        ForgeKind::GitHub => &GitHubForge::WORDS,
        ForgeKind::GitLab => &GitLabForge::WORDS,
    }
}

/// The kinds of review a `kind` forge takes, in the order they are
/// offered; GitHub's while no kind is known yet, as [`words_or_default`].
pub fn review_events(kind: Option<ForgeKind>) -> &'static [ReviewEvent] {
    match kind {
        None | Some(ForgeKind::GitHub) => GitHubForge::EVENTS,
        Some(ForgeKind::GitLab) => GitLabForge::EVENTS,
    }
}

/// Whether `id` is fit to send back to a `kind` forge as a thread's or a
/// request's id: a GitHub node id, or a GitLab global id. GitHub's check
/// while no kind is known yet, as [`words_or_default`].
pub fn write_id_ok(kind: Option<ForgeKind>, id: &str) -> bool {
    match kind {
        None | Some(ForgeKind::GitHub) => super::review::node_id_ok(id),
        Some(ForgeKind::GitLab) => super::gitlab_discussion::gitlab_id_ok(id),
    }
}

/// The most lines GitLab lets one suggestion take above the line its
/// note is on.
const GITLAB_MAX_ABOVE: u32 = 100;

/// The info string of a suggestion fence for a `kind` forge, for a pick
/// that reaches `above` lines above the line the comment is on. GitHub
/// takes the whole pick from the comment's range; GitLab counts the lines
/// above the last one, where its note is anchored, and takes at most
/// 101 lines, so a longer pick has `None`.
pub fn suggestion_info(kind: Option<ForgeKind>, above: u32) -> Option<String> {
    match kind {
        None | Some(ForgeKind::GitHub) => Some("suggestion".into()),
        Some(ForgeKind::GitLab) => {
            (above <= GITLAB_MAX_ABOVE).then(|| format!("suggestion:-{above}+0"))
        }
    }
}

/// How `method` is named on a `kind` forge. GitLab's project fixes how a
/// merge lands, so its plain merge is "merge", not a merge commit.
/// GitHub's words while no kind is known yet, as [`words_or_default`].
pub fn merge_label(kind: Option<ForgeKind>, method: MergeMethod) -> &'static str {
    match (kind, method) {
        (None | Some(ForgeKind::GitHub), _) => method.label(),
        (Some(ForgeKind::GitLab), MergeMethod::Merge) => "merge",
        (Some(ForgeKind::GitLab), MergeMethod::Squash) => "squash",
        (Some(ForgeKind::GitLab), MergeMethod::Rebase) => "rebase",
    }
}

/// The ref a `kind` forge keeps request `number`'s head under, to fetch
/// when the forge will not serve the head commit by id: GitLab's
/// `refs/merge-requests/<n>/head`. GitHub's heads are fetched by id alone,
/// so it has none, and nor has a kind not known yet.
pub fn head_ref(kind: Option<ForgeKind>, number: u64) -> Option<String> {
    match kind {
        None | Some(ForgeKind::GitHub) => None,
        Some(ForgeKind::GitLab) => Some(format!("refs/merge-requests/{number}/head")),
    }
}

/// The Branches panel's row about a `kind` CLI's `status`: what is wrong,
/// and what to do, in that forge's own words.
pub fn cli_text(kind: ForgeKind, status: CliStatus) -> (String, Option<String>) {
    let name = kind.name();
    let Words { cli, requests, .. } = words(kind);
    match status {
        CliStatus::Ready => (format!("{name}: {cli} ready"), None),
        CliStatus::NotInstalled => (
            format!("{name}: {cli} not installed"),
            Some(format!("install the {name} CLI to see {requests}")),
        ),
        CliStatus::Unusable => (
            format!("{name}: {cli} not working"),
            Some(format!("check that {cli} --version runs")),
        ),
        CliStatus::TooOld { found, needed } => (
            format!("{name}: {cli} {found} is too old"),
            Some(format!("update to {cli} {needed} or newer")),
        ),
    }
}

/// The words of `kind`, or GitHub's while no kind is known yet: a host is
/// GitHub unless the config says otherwise.
pub fn words_or_default(kind: Option<ForgeKind>) -> &'static Words {
    kind.map_or(&GitHubForge::WORDS, words)
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
    fn github_supports_every_capability_and_gitlab_all_but_creating_and_viewed_marks() {
        for capability in Capability::ALL {
            assert_eq!(supports(ForgeKind::GitHub, capability), Ok(()));
            let gitlab = supports(ForgeKind::GitLab, capability);
            match capability {
                Capability::Badges
                | Capability::Review
                | Capability::DiscussionWrite
                | Capability::Merge => assert_eq!(gitlab, Ok(())),
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
    fn each_kind_has_its_own_words() {
        assert_eq!(
            *words(ForgeKind::GitHub),
            Words {
                name: "GitHub",
                cli: "gh",
                request: "pull request",
                requests: "pull requests",
                short: "PR",
                sign: '#',
                host_key: "github",
            }
        );
        assert_eq!(
            *words(ForgeKind::GitLab),
            Words {
                name: "GitLab",
                cli: "glab",
                request: "merge request",
                requests: "merge requests",
                short: "MR",
                sign: '!',
                host_key: "gitlab_ask",
            }
        );
        assert_eq!(words_or_default(None), words(ForgeKind::GitHub));
        assert_eq!(
            words_or_default(Some(ForgeKind::GitLab)),
            words(ForgeKind::GitLab)
        );
    }

    #[test]
    fn each_kind_offers_its_own_review_events() {
        use ReviewEvent::{Approve, Comment, RequestChanges};
        assert_eq!(
            review_events(Some(ForgeKind::GitHub)),
            [Approve, RequestChanges, Comment]
        );
        assert_eq!(review_events(None), review_events(Some(ForgeKind::GitHub)));
        assert_eq!(review_events(Some(ForgeKind::GitLab)), [Approve, Comment]);
    }

    #[test]
    fn each_kind_checks_its_own_ids() {
        let discussion = "gid://gitlab/Discussion/1111111111111111111111111111111111111111";
        assert!(write_id_ok(Some(ForgeKind::GitLab), discussion));
        assert!(write_id_ok(
            Some(ForgeKind::GitLab),
            "gid://gitlab/MergeRequest/42"
        ));
        assert!(!write_id_ok(Some(ForgeKind::GitLab), "PRRT_kwDOA1"));
        assert!(!write_id_ok(Some(ForgeKind::GitHub), discussion));
        assert!(write_id_ok(Some(ForgeKind::GitHub), "PRRT_kwDOA1"));
        assert!(write_id_ok(None, "PRRT_kwDOA1"));
        assert!(!write_id_ok(None, discussion));
    }

    #[test]
    fn each_kind_writes_its_own_suggestion_fence() {
        let github = Some(ForgeKind::GitHub);
        let gitlab = Some(ForgeKind::GitLab);
        // GitHub: single line or range.
        assert_eq!(suggestion_info(github, 0).as_deref(), Some("suggestion"));
        assert_eq!(suggestion_info(github, 4).as_deref(), Some("suggestion"));
        assert_eq!(suggestion_info(None, 200).as_deref(), Some("suggestion"));
        // GitLab: a single line, a 5-line range, the longest it takes.
        assert_eq!(
            suggestion_info(gitlab, 0).as_deref(),
            Some("suggestion:-0+0")
        );
        assert_eq!(
            suggestion_info(gitlab, 4).as_deref(),
            Some("suggestion:-4+0")
        );
        assert_eq!(
            suggestion_info(gitlab, 100).as_deref(),
            Some("suggestion:-100+0")
        );
        // More than 101 lines.
        assert_eq!(suggestion_info(gitlab, 101), None);
    }

    #[test]
    fn each_kind_names_its_merge_methods() {
        use MergeMethod::{Merge, Rebase, Squash};
        let gitlab = Some(ForgeKind::GitLab);
        for kind in [None, Some(ForgeKind::GitHub)] {
            for method in [Merge, Squash, Rebase] {
                assert_eq!(merge_label(kind, method), method.label());
            }
        }
        assert_eq!(merge_label(None, Merge), "merge commit");
        assert_eq!(merge_label(gitlab, Merge), "merge");
        assert_eq!(merge_label(gitlab, Squash), "squash");
    }

    #[test]
    fn only_gitlab_has_a_head_ref_to_fall_back_on() {
        assert_eq!(
            head_ref(Some(ForgeKind::GitLab), 12).as_deref(),
            Some("refs/merge-requests/12/head")
        );
        assert_eq!(head_ref(Some(ForgeKind::GitHub), 12), None);
        assert_eq!(head_ref(None, 12), None);
    }

    #[test]
    fn unsupported_says_what_is_missing_and_where() {
        let unsupported = supports(ForgeKind::GitLab, Capability::CreatePull).unwrap_err();
        assert_eq!(
            unsupported.message(),
            "Creating a pull request is not available on GitLab yet"
        );
    }

    #[test]
    fn the_cli_row_is_worded_per_forge() {
        use crate::cli::CliVersion;
        let v = |major, minor| CliVersion {
            major,
            minor,
            patch: 0,
        };
        let row = |kind, status| {
            let (what, hint) = cli_text(kind, status);
            (what, hint.unwrap_or_default())
        };
        let own = |what: &str, hint: &str| (what.to_string(), hint.to_string());
        assert_eq!(
            row(ForgeKind::GitLab, CliStatus::NotInstalled),
            own(
                "GitLab: glab not installed",
                "install the GitLab CLI to see merge requests"
            )
        );
        assert_eq!(
            row(ForgeKind::GitHub, CliStatus::NotInstalled),
            own(
                "GitHub: gh not installed",
                "install the GitHub CLI to see pull requests"
            )
        );
        assert_eq!(
            row(ForgeKind::GitLab, CliStatus::Unusable),
            own("GitLab: glab not working", "check that glab --version runs")
        );
        assert_eq!(
            row(
                ForgeKind::GitLab,
                CliStatus::TooOld {
                    found: v(1, 2),
                    needed: v(1, 5)
                }
            ),
            own(
                "GitLab: glab 1.2.0 is too old",
                "update to glab 1.5.0 or newer"
            )
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

    /// The lines of `text` outside `#[cfg(test)]` items, numbered from 1.
    /// Only the test items are dropped, so production code after a mid-file
    /// test module is still scanned. The brace count ignores strings.
    fn non_test_lines(text: &str) -> Vec<(usize, &str)> {
        let mut kept = Vec::new();
        let mut lines = text.lines().enumerate();
        while let Some((n, line)) = lines.next() {
            let Some(rest) = line.trim_start().strip_prefix("#[cfg(test)]") else {
                kept.push((n + 1, line));
                continue;
            };
            // Skip the item: its other attributes, then up to a `;` at
            // depth 0 or the brace that closes its body.
            let mut depth = 0usize;
            let mut opened = false;
            let item = std::iter::once(rest).chain(lines.by_ref().map(|(_, l)| l));
            'item: for part in item {
                if depth == 0 && !opened && part.trim_start().starts_with("#[") {
                    continue;
                }
                for c in part.chars() {
                    match c {
                        '{' => {
                            depth += 1;
                            opened = true;
                        }
                        '}' => depth = depth.saturating_sub(1),
                        ';' if depth == 0 => break 'item,
                        _ => {}
                    }
                    if opened && depth == 0 {
                        break 'item;
                    }
                }
            }
        }
        kept
    }

    #[test]
    fn the_gate_scan_skips_only_test_items() {
        let text = "\
use a;
#[cfg(test)] use b;
fn live() {}
#[cfg(test)]
#[allow(dead_code)]
fn helper() {
    if x { y(); }
}
mid();
#[cfg(test)]
mod tests {
    fn t() {
        match x { _ => {} }
    }
}
after();
#[cfg(any(test, feature = \"test-support\"))]
fn fixture() {}
";
        let kept: Vec<usize> = non_test_lines(text).iter().map(|(n, _)| *n).collect();
        assert_eq!(kept, [1, 3, 9, 16, 17, 18]);
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
            for (n, line) in non_test_lines(&text) {
                if line.contains("ForgeKind::GitHub") || line.contains("ForgeKind::GitLab") {
                    found.push(format!("{}:{n}: {}", path.display(), line.trim()));
                }
            }
        }
        assert!(
            found.is_empty(),
            "a forge kind decides behaviour outside the adapters:\n{}",
            found.join("\n")
        );
    }

    /// One way to reach a forge CLI, and the files allowed to use it.
    struct Rule {
        pattern: &'static str,
        matches: fn(&str) -> bool,
        /// Whether the app crate's sources are scanned too, not just kika-forge's.
        app_too: bool,
        allowed: &'static [&'static str],
    }

    /// Whether `needle` is in `line` with no identifier character before it.
    fn bare(line: &str, needle: &str) -> bool {
        line.match_indices(needle).any(|(i, _)| {
            !line[..i]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        })
    }

    #[test]
    fn forge_cli_calls_go_through_the_gate() {
        let forge_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let app_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src");
        let mut files = Vec::new();
        sources(&forge_src, &mut files);
        let forge_files = files.len();
        sources(&app_src, &mut files);
        assert!(files.len() > 20, "the scan found the sources: {files:?}");
        // Only `adapter` builds a CLI's calls, and only `ForgeContext::forge`
        // and `check_cli` reach it, past the host, allow and CLI gates.
        let rules: &[Rule] = &[
            Rule {
                pattern: "`.gh(`, `.glab(`, `.gh.run(` or `.glab.run(`",
                matches: |line| {
                    [".gh(", ".glab(", ".gh.run(", ".glab.run("]
                        .iter()
                        .any(|call| line.contains(call))
                },
                app_too: true,
                // `adapter` hands each forge its CLI here.
                allowed: &["forge.rs"],
            },
            Rule {
                pattern: "`adapter(`",
                matches: |line| bare(line, "adapter("),
                app_too: false,
                // `ForgeContext::forge` and `check_cli` call it after the gates.
                allowed: &["client.rs"],
            },
            Rule {
                pattern: "`.gh.as_ref()` or `.glab.as_ref()`",
                matches: |line| line.contains(".gh.as_ref()") || line.contains(".glab.as_ref()"),
                app_too: false,
                // `ForgeContext` lends its runners to the gated clients only.
                allowed: &["client.rs"],
            },
            Rule {
                pattern: "a `Gh {` or `Glab {` struct literal",
                matches: |line| bare(line, "Gh {") || bare(line, "Glab {"),
                app_too: false,
                // The gated clients are built in `ForgeContext` alone.
                allowed: &["client.rs"],
            },
            Rule {
                pattern: "`.call(`",
                matches: |line| line.contains(".call("),
                app_too: false,
                // A scan cannot tell a `Glab` from a `Gh`, so only the files
                // holding the clients' own `impl` blocks may call.
                allowed: &[
                    "gitlab.rs",
                    "gitlab_review.rs",
                    "gitlab_discussion.rs",
                    "gitlab_submit.rs",
                    "gitlab_merge.rs",
                    "github.rs",
                    "review.rs",
                ],
            },
        ];
        let mut found = Vec::new();
        for (i, path) in files.iter().enumerate() {
            let in_forge = i < forge_files;
            let text = std::fs::read_to_string(path).unwrap();
            for (n, line) in non_test_lines(&text) {
                for rule in rules {
                    let scanned = in_forge || rule.app_too;
                    let allowed = rule
                        .allowed
                        .iter()
                        .any(|name| *path == forge_src.join(name));
                    if scanned && !allowed && (rule.matches)(line) {
                        found.push(format!(
                            "{}:{n}: {}: {}",
                            path.display(),
                            rule.pattern,
                            line.trim()
                        ));
                    }
                }
            }
        }
        assert!(
            found.is_empty(),
            "a forge CLI is reached around the gate:\n{}",
            found.join("\n")
        );
    }
}
