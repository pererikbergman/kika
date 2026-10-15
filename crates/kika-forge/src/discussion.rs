//! A pull request's discussion on GitHub: its review threads, with their
//! comments and the lines they were written on, and its conversation of
//! issue comments and review bodies, in one GraphQL call. Every string is
//! made terminal-safe while it is parsed, so no type here holds raw
//! server text. The repository and number travel as variables on stdin;
//! argv is the fixed `gh api graphql` line.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::anchor::GhSide;
use super::github::{Gh, GhCall, Refusal};
use super::review::{
    Login, MAX_PATH, ReviewVerdict, author_of, capped_body, data_of, full_oid, line, node_id_ok,
    outcome, pull_request_answer, verdict_of,
};
use super::{ForgeRepo, ForgeStatus};
use kika_git::commit_files::CommitId;
use kika_git::diff::{EmojiZwj, sanitize_body};

/// The most review threads kept, and asked for; there is no paging past it.
pub const MAX_THREADS: usize = 100;
/// The most comments kept of one thread.
pub const MAX_THREAD_COMMENTS: usize = 50;
/// The most conversation items kept: 100 issue comments and 100 reviews.
pub const MAX_CONVERSATION: usize = 200;
/// The most of one comment read, in bytes.
pub const MAX_COMMENT: usize = 16 * 1024;
/// The lines of a thread's hunk kept, from its end: the lines it was
/// written on, shown when it is outdated.
pub const MAX_CONTEXT: usize = 4;
/// The most of one context line kept, in characters.
const MAX_CONTEXT_LINE: usize = 512;

/// What a thread is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadSubject {
    Line,
    File,
}

/// One comment of a thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadComment {
    pub author: String,
    /// When it was written, as GitHub wrote it.
    pub created: String,
    /// Cut to [`MAX_COMMENT`], made terminal-safe a line at a time.
    pub body: String,
}

/// One review thread, as GitHub placed it on the whole pull request's
/// diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewThread {
    /// GitHub's id, kept only when [`node_id_ok`]; never drawn.
    pub node_id: Option<String>,
    /// At most [`MAX_PATH`] bytes; only compared with local paths, never
    /// drawn.
    pub path: String,
    pub resolved: bool,
    pub outdated: bool,
    pub subject: ThreadSubject,
    pub side: Option<GhSide>,
    pub line: Option<u32>,
    pub start_side: Option<GhSide>,
    pub start_line: Option<u32>,
    pub original_line: Option<u32>,
    pub original_start_line: Option<u32>,
    /// The last [`MAX_CONTEXT`] lines of the first comment's hunk, each
    /// with its `+`, `-` or space.
    pub context: Vec<String>,
    pub comments: Vec<ThreadComment>,
    /// How many comments GitHub says the thread has.
    pub comments_total: u64,
}

/// What a conversation item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationKind {
    Comment,
    Review(ReviewVerdict),
}

/// A comment on the pull request itself, or a review's body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationItem {
    pub author: String,
    pub created: String,
    pub body: String,
    pub kind: ConversationKind,
}

/// Everything said on a pull request that review mode shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrDiscussion {
    /// The head GitHub had when it answered; `None` when it sent no hex id.
    pub head_oid: Option<CommitId>,
    pub threads: Vec<ReviewThread>,
    /// How many threads GitHub says there are.
    pub threads_total: u64,
    /// Issue comments and review bodies, oldest first.
    pub conversation: Vec<ConversationItem>,
    /// How many issue comments and reviews GitHub says there are, the
    /// reviews with no body included.
    pub conversation_total: u64,
    /// GitHub has more issue comments or reviews than were read, so the
    /// conversation shown is not all of it. Unlike `conversation_total`,
    /// empty reviews do not set it.
    pub conversation_capped: bool,
}

impl Gh<'_> {
    /// The discussion of pull request `number` of `repo`; `None` when
    /// GitHub has no such pull request.
    pub fn fetch_discussion(
        &self,
        repo: &ForgeRepo,
        number: u64,
    ) -> Result<Option<PrDiscussion>, Refusal> {
        // GraphQL numbers are 32-bit; a larger one names no pull request.
        if number > i32::MAX as u64 {
            return Ok(None);
        }
        let kind = GhCall::Graphql {
            body: discussion_body(repo, number),
        };
        let now = self.now;
        self.run_logged(
            kind,
            |output| pull_request_answer(output, now, read_discussion),
            outcome,
        )
    }
}

/// The discussion query: fixed text; owner, name and number are variables.
const DISCUSSION_QUERY: &str = "query($owner:String!,$name:String!,$number:Int!)\
{repository(owner:$owner,name:$name){pullRequest(number:$number)\
{headRefOid reviewThreads(first:100){totalCount nodes{id isResolved isOutdated path \
diffSide startDiffSide line startLine originalLine originalStartLine subjectType \
comments(first:50){totalCount nodes{author{login} body createdAt diffHunk originalCommit{oid}}}}} \
comments(first:100){totalCount nodes{author{login} body createdAt}} \
reviews(first:100){totalCount nodes{author{login} body state submittedAt}}}}}";

fn discussion_body(repo: &ForgeRepo, number: u64) -> Vec<u8> {
    let variables = json!({ "owner": repo.owner, "name": repo.name, "number": number });
    serde_json::to_vec(&json!({ "query": DISCUSSION_QUERY, "variables": variables }))
        .unwrap_or_default()
}

#[derive(Deserialize)]
struct DiscussionData {
    repository: Option<DiscussionRepo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiscussionRepo {
    pull_request: Option<PrNode>,
}

/// The connections stay loose JSON, so one malformed node is skipped
/// rather than failing the whole answer.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrNode {
    head_ref_oid: Option<String>,
    review_threads: Option<Value>,
    comments: Option<Value>,
    reviews: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadNode {
    id: Option<String>,
    is_resolved: Option<bool>,
    is_outdated: Option<bool>,
    path: Option<String>,
    diff_side: Option<String>,
    start_diff_side: Option<String>,
    line: Option<i64>,
    start_line: Option<i64>,
    original_line: Option<i64>,
    original_start_line: Option<i64>,
    subject_type: Option<String>,
    comments: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommentNode {
    author: Option<Login>,
    body: Option<String>,
    created_at: Option<String>,
    diff_hunk: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewBodyNode {
    author: Option<Login>,
    body: Option<String>,
    state: Option<String>,
    submitted_at: Option<String>,
}

/// A connection's `totalCount` (the nodes read when it has none), and
/// every node that parses as `T`; null and malformed ones are skipped.
fn connection<T: DeserializeOwned>(value: Option<Value>) -> (Option<u64>, Vec<T>) {
    let Some(Value::Object(mut map)) = value else {
        return (None, Vec::new());
    };
    let total = map.get("totalCount").and_then(Value::as_u64);
    let nodes = match map.remove("nodes") {
        Some(Value::Array(nodes)) => nodes
            .into_iter()
            .filter_map(|node| serde_json::from_value::<Option<T>>(node).ok().flatten())
            .collect(),
        _ => Vec::new(),
    };
    (total, nodes)
}

fn read_discussion(body: &[u8]) -> Result<Option<PrDiscussion>, ForgeStatus> {
    let data: DiscussionData = data_of(body)?;
    let repository = data.repository.ok_or(ForgeStatus::NoAccess)?;
    let Some(pr) = repository.pull_request else {
        return Ok(None);
    };
    let (threads_total, nodes) = connection::<ThreadNode>(pr.review_threads);
    let threads: Vec<ReviewThread> = nodes
        .into_iter()
        .filter_map(thread_of)
        .take(MAX_THREADS)
        .collect();
    let (comments_total, comments) = connection::<CommentNode>(pr.comments);
    let (reviews_total, reviews) = connection::<ReviewBodyNode>(pr.reviews);
    let comments_read = comments.len() as u64;
    let reviews_read = reviews.len() as u64;
    let half = (MAX_CONVERSATION / 2) as u64;
    // More than taken: by GitHub's count or by the nodes it sent.
    let past_cap = |total: Option<u64>, read: u64| total.unwrap_or(read).max(read) > read.min(half);
    let mut conversation_capped =
        past_cap(comments_total, comments_read) || past_cap(reviews_total, reviews_read);
    let mut conversation: Vec<ConversationItem> = comments
        .into_iter()
        .take(MAX_CONVERSATION / 2)
        .map(|c| ConversationItem {
            author: author_of(c.author),
            created: line(c.created_at.as_deref().unwrap_or_default()),
            body: capped_body(c.body.as_deref().unwrap_or_default(), MAX_COMMENT),
            kind: ConversationKind::Comment,
        })
        .chain(
            reviews
                .into_iter()
                .take(MAX_CONVERSATION / 2)
                .filter(|r| r.body.as_deref().is_some_and(|b| !b.trim().is_empty()))
                .map(|r| ConversationItem {
                    author: author_of(r.author),
                    created: line(r.submitted_at.as_deref().unwrap_or_default()),
                    body: capped_body(r.body.as_deref().unwrap_or_default(), MAX_COMMENT),
                    // A state Kika does not know still said something.
                    kind: ConversationKind::Review(
                        r.state
                            .as_deref()
                            .and_then(verdict_of)
                            .unwrap_or(ReviewVerdict::Commented),
                    ),
                }),
        )
        .collect();
    // RFC 3339 times in one zone sort as text; the sort is stable, so
    // equal or odd times keep GitHub's order.
    conversation.sort_by(|a, b| a.created.cmp(&b.created));
    conversation_capped |= conversation.len() > MAX_CONVERSATION;
    conversation.truncate(MAX_CONVERSATION);
    Ok(Some(PrDiscussion {
        head_oid: full_oid(pr.head_ref_oid),
        threads_total: threads_total.unwrap_or(threads.len() as u64),
        threads,
        conversation,
        conversation_total: comments_total.unwrap_or(comments_read)
            + reviews_total.unwrap_or(reviews_read),
        conversation_capped,
    }))
}

fn side_of(text: Option<&str>) -> Option<GhSide> {
    match text? {
        "LEFT" => Some(GhSide::Left),
        "RIGHT" => Some(GhSide::Right),
        _ => None,
    }
}

/// A line number GitHub may send: 1 or more, within `u32`.
fn line_number(n: Option<i64>) -> Option<u32> {
    n.and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0)
}

/// A thread with a path Kika can compare; `None` without one.
fn thread_of(node: ThreadNode) -> Option<ReviewThread> {
    let path = node.path.filter(|p| !p.is_empty() && p.len() <= MAX_PATH)?;
    let (total, comments) = connection::<CommentNode>(node.comments);
    let context = comments
        .first()
        .and_then(|c| c.diff_hunk.as_deref())
        .map(context_of)
        .unwrap_or_default();
    let comments: Vec<ThreadComment> = comments
        .into_iter()
        .take(MAX_THREAD_COMMENTS)
        .map(|c| ThreadComment {
            author: author_of(c.author),
            created: line(c.created_at.as_deref().unwrap_or_default()),
            body: capped_body(c.body.as_deref().unwrap_or_default(), MAX_COMMENT),
        })
        .collect();
    let subject = match node.subject_type.as_deref() {
        Some("FILE") => ThreadSubject::File,
        _ => ThreadSubject::Line,
    };
    Some(ReviewThread {
        node_id: node.id.filter(|id| node_id_ok(id)),
        path,
        resolved: node.is_resolved.unwrap_or(false),
        outdated: node.is_outdated.unwrap_or(false),
        subject,
        side: side_of(node.diff_side.as_deref()),
        line: line_number(node.line),
        start_side: side_of(node.start_diff_side.as_deref()),
        start_line: line_number(node.start_line),
        original_line: line_number(node.original_line),
        original_start_line: line_number(node.original_start_line),
        context,
        comments_total: total.unwrap_or(comments.len() as u64),
        comments,
    })
}

/// The last [`MAX_CONTEXT`] lines of `hunk`, each cut short and made
/// terminal-safe, keeping its `+`, `-` or space.
fn context_of(hunk: &str) -> Vec<String> {
    let lines: Vec<&str> = hunk
        .strip_suffix('\n')
        .unwrap_or(hunk)
        .split('\n')
        .filter(|l| !l.is_empty())
        .collect();
    let from = lines.len().saturating_sub(MAX_CONTEXT);
    lines[from..]
        .iter()
        .map(|l| {
            let l = l.strip_suffix('\r').unwrap_or(l);
            let cut = match l.char_indices().nth(MAX_CONTEXT_LINE) {
                Some((at, _)) => &l[..at],
                None => l,
            };
            sanitize_body(cut.as_bytes(), EmojiZwj::Escape)
        })
        .collect()
}

#[cfg(any(test, feature = "test-support"))]
pub mod fixtures {
    use crate::review::fixtures::HOSTILE;

    pub const DISCUSSION: &str = include_str!("fixtures/review_discussion.http");
    pub const DISCUSSION_MISSING: &str = include_str!("fixtures/review_discussion_missing.http");

    /// The discussion answer with `HOSTILE` in an author, comment bodies,
    /// a hunk, a time and the conversation.
    pub fn hostile_discussion() -> String {
        let quoted = serde_json::to_string(HOSTILE).unwrap();
        let bare = quoted.trim_matches('"');
        DISCUSSION
            .replace("\"alice\"", &quoted)
            .replace("\"Agreed.\"", &quoted)
            .replace("Please cache this.", bare)
            .replace("use std::fmt;", bare)
            .replace("context5", bare)
            .replace("\"2023-11-14T21:00:00Z\"", &quoted)
            .replace("\"Thanks for this!\"", &quoted)
            .replace("Looks good overall", bare)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::ForgeHost;
    use crate::github::fixtures::{GRAPHQL_RATE_LIMITED, HTTP_401_STDERR};
    use crate::github::{FakeGh, GhCommand, answer, failure};
    use kika_git::recording::CommandSink;

    const NOW: i64 = 1_700_000_000;

    /// A current line thread with no comments, for anchoring tests.
    pub(crate) fn thread(
        side: Option<GhSide>,
        line: Option<u32>,
        start_side: Option<GhSide>,
        start_line: Option<u32>,
    ) -> ReviewThread {
        ReviewThread {
            node_id: None,
            path: "src/a.rs".into(),
            resolved: false,
            outdated: false,
            subject: ThreadSubject::Line,
            side,
            line,
            start_side,
            start_line,
            original_line: line,
            original_start_line: start_line,
            context: Vec::new(),
            comments: Vec::new(),
            comments_total: 0,
        }
    }

    fn repo() -> ForgeRepo {
        ForgeRepo {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "widgets".into(),
        }
    }

    fn fetch(runner: &FakeGh, number: u64) -> Result<Option<PrDiscussion>, Refusal> {
        let host = ForgeHost::parse("github.com").unwrap();
        let sink = CommandSink::new();
        Gh::fixture(runner, &sink, &host, NOW).fetch_discussion(&repo(), number)
    }

    fn parsed(text: &str) -> PrDiscussion {
        fetch(&FakeGh::new().reply(answer(text)), 42)
            .unwrap()
            .unwrap()
    }

    fn sent(cmd: &GhCommand) -> serde_json::Value {
        serde_json::from_slice(&cmd.stdin()).unwrap()
    }

    #[test]
    fn one_call_carries_the_fixed_query_and_only_the_three_variables() {
        let runner = FakeGh::new().reply(answer(DISCUSSION));
        fetch(&runner, 42).unwrap().unwrap();
        let [cmd] = runner.calls().try_into().unwrap();
        assert_eq!(
            cmd.display_line(),
            "gh api graphql --include --hostname github.com --input -"
        );
        let argv: Vec<String> = cmd
            .args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        for word in ["acme", "widgets", "42"] {
            assert!(argv.iter().all(|a| !a.contains(word)), "{argv:?}");
        }
        let body = sent(&cmd);
        let query = body["query"].as_str().unwrap();
        assert_eq!(query, DISCUSSION_QUERY);
        for word in ["acme", "widgets", "42"] {
            assert!(!query.contains(word), "{word} in {query}");
        }
        let vars = body["variables"].as_object().unwrap();
        let mut names: Vec<&str> = vars.keys().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(names, ["name", "number", "owner"]);
        assert_eq!(body["variables"]["owner"], "acme");
        assert_eq!(body["variables"]["name"], "widgets");
        assert_eq!(body["variables"]["number"], 42);
        for field in [
            "reviewThreads(first:100)",
            "comments(first:50)",
            "comments(first:100)",
            "reviews(first:100)",
            "isResolved",
            "isOutdated",
            "diffSide",
            "startDiffSide",
            "originalLine",
            "subjectType",
            "diffHunk",
            "headRefOid",
        ] {
            assert!(query.contains(field), "{field} not asked: {query}");
        }
    }

    #[test]
    fn the_fixture_parses_into_its_threads() {
        let d = parsed(DISCUSSION);
        assert_eq!(d.head_oid, CommitId::parse(&"1".repeat(40)));
        assert_eq!(d.threads_total, 5);
        assert_eq!(d.threads.len(), 5);
        let [right, left, multi, outdated, file] = d.threads.as_slice() else {
            panic!("{:?}", d.threads);
        };
        assert_eq!(right.node_id.as_deref(), Some("PRRT_kwDOA1"));
        assert_eq!(right.path, "src/cache.rs");
        assert_eq!(
            (right.side, right.line, right.start_side, right.start_line),
            (Some(GhSide::Right), Some(3), None, None)
        );
        assert_eq!(
            (right.resolved, right.outdated, right.subject),
            (false, false, ThreadSubject::Line)
        );
        assert_eq!(right.comments_total, 2);
        assert_eq!(
            right.comments[1],
            ThreadComment {
                author: "bob".into(),
                created: "2023-11-14T21:30:00Z".into(),
                body: "Agreed.".into(),
            }
        );
        assert_eq!(right.comments[0].author, "alice");
        assert!(
            right.comments[0]
                .body
                .starts_with("Please cache this.\n\n- one\n"),
            "{:?}",
            right.comments[0].body
        );
        assert!(!right.comments[0].body.contains('\r'));
        assert_eq!(
            right.context,
            [
                " use std::fmt;",
                "-fn old() {}",
                "+fn new() {}",
                " let cache = 1;"
            ]
        );

        assert_eq!((left.side, left.line), (Some(GhSide::Left), Some(2)));
        assert!(left.resolved);
        assert_eq!(
            (multi.side, multi.line, multi.start_side, multi.start_line),
            (Some(GhSide::Right), Some(5), Some(GhSide::Right), Some(3))
        );
        assert_eq!(multi.path, "src/lib.rs");

        assert!(outdated.outdated && !outdated.resolved);
        assert_eq!((outdated.line, outdated.original_line), (None, Some(7)));
        assert_eq!(outdated.context, [" context5", "-old6", "+new6", "+new7"]);

        assert_eq!(file.subject, ThreadSubject::File);
        assert_eq!(file.path, "docs/café.md");
        assert_eq!(file.context, Vec::<String>::new());
    }

    #[test]
    fn the_conversation_is_merged_by_time_without_empty_reviews() {
        let d = parsed(DISCUSSION);
        let items: Vec<(&str, &str, ConversationKind)> = d
            .conversation
            .iter()
            .map(|c| (c.author.as_str(), c.body.as_str(), c.kind))
            .collect();
        assert_eq!(
            items,
            [
                ("frank", "Thanks for this!", ConversationKind::Comment),
                (
                    "alice",
                    "Looks good overall",
                    ConversationKind::Review(ReviewVerdict::Commented)
                ),
                ("grace", "Ping", ConversationKind::Comment),
            ]
        );
        assert_eq!(d.conversation[1].created, "2023-11-14T22:00:00Z");
        assert_eq!(d.conversation_total, 4);
        // The empty review counts in the total but cuts nothing.
        assert!(!d.conversation_capped);
    }

    #[test]
    fn a_missing_pull_request_is_none_and_refusals_map_as_for_the_detail() {
        assert_eq!(
            fetch(&FakeGh::new().reply(answer(DISCUSSION_MISSING)), 999),
            Ok(None)
        );
        let runner = FakeGh::new();
        assert_eq!(fetch(&runner, u64::from(u32::MAX)), Ok(None));
        assert_eq!(runner.calls(), []);
        let gone = DISCUSSION_MISSING.replace("{\"pullRequest\":null}", "null");
        assert_eq!(
            fetch(&FakeGh::new().reply(answer(&gone)), 999),
            Err(Refusal::Status(ForgeStatus::NoAccess))
        );
        assert_eq!(
            fetch(&FakeGh::new().reply(answer(GRAPHQL_RATE_LIMITED)), 42),
            Err(Refusal::Limited {
                until: 1_700_003_600
            })
        );
        assert_eq!(
            fetch(&FakeGh::new().reply(failure(1, HTTP_401_STDERR)), 42),
            Err(Refusal::Status(ForgeStatus::NotLoggedIn))
        );
        assert_eq!(
            fetch(&FakeGh::new().missing(), 42),
            Err(Refusal::Status(ForgeStatus::NoGh))
        );
        assert_eq!(
            fetch(&FakeGh::new().reply(answer("{\"data\":null}")), 42),
            Err(Refusal::Status(ForgeStatus::Failed))
        );
    }

    fn no_controls(text: &str) {
        assert!(
            !text
                .chars()
                .any(|c| c.is_control() || kika_git::safe_text::is_invisible_or_bidi(c)),
            "{text:?}"
        );
    }

    #[test]
    fn hostile_text_leaves_no_control_or_bidi_character_in_any_field() {
        let d = parsed(&hostile_discussion());
        let mut seen = 0;
        for thread in &d.threads {
            no_controls(&thread.path);
            for line in &thread.context {
                no_controls(line);
                seen += usize::from(line.contains("red"));
            }
            for comment in &thread.comments {
                no_controls(&comment.author);
                no_controls(&comment.created);
                for line in comment.body.split('\n') {
                    no_controls(line);
                }
                seen += usize::from(comment.author.contains("red"));
                seen += usize::from(comment.body.contains("red"));
                seen += usize::from(comment.created.contains("red"));
            }
        }
        for item in &d.conversation {
            no_controls(&item.author);
            no_controls(&item.created);
            for line in item.body.split('\n') {
                no_controls(line);
            }
            seen += usize::from(item.body.contains("red"));
        }
        // Every replaced field came through, escaped rather than dropped.
        assert!(seen >= 8, "{seen}: {d:#?}");
        let alice = &d.threads[0].comments[0];
        assert!(alice.author.contains("<U+202E>evil"), "{alice:?}");
    }

    #[test]
    fn an_over_long_body_or_context_line_is_cut() {
        let big = DISCUSSION.replace("Agreed.", &"é".repeat(MAX_COMMENT));
        let d = parsed(&big);
        let body = &d.threads[0].comments[1].body;
        assert!(body.len() <= MAX_COMMENT, "{}", body.len());
        assert!(body.len() > MAX_COMMENT - 2);
        let wide = DISCUSSION.replace("context5", &"c".repeat(2000));
        let d = parsed(&wide);
        assert_eq!(d.threads[3].context[0].chars().count(), MAX_CONTEXT_LINE);
    }

    /// The fixture with its threads replaced by `nodes`.
    fn with_threads(nodes: &[String]) -> String {
        let start = DISCUSSION.find("\"reviewThreads\":").unwrap();
        let end = DISCUSSION.rfind(",\"comments\":{\"totalCount\":2").unwrap();
        format!(
            "{}\"reviewThreads\":{{\"totalCount\":{},\"nodes\":[{}]}}{}",
            &DISCUSSION[..start],
            nodes.len(),
            nodes.join(","),
            &DISCUSSION[end..]
        )
    }

    fn node(i: usize, comments: usize) -> String {
        let comments: Vec<String> = (0..comments)
            .map(|c| {
                format!("{{\"author\":{{\"login\":\"u{c}\"}},\"body\":\"b\",\"createdAt\":\"t\"}}")
            })
            .collect();
        format!(
            "{{\"path\":\"f{i}\",\"diffSide\":\"RIGHT\",\"line\":{},\"subjectType\":\"LINE\",\
             \"comments\":{{\"totalCount\":{},\"nodes\":[{}]}}}}",
            i + 1,
            comments.len(),
            comments.join(",")
        )
    }

    #[test]
    fn threads_and_comments_past_their_caps_are_cut() {
        let nodes: Vec<String> = (0..MAX_THREADS + 7)
            .map(|i| node(i, if i == 0 { MAX_THREAD_COMMENTS + 9 } else { 1 }))
            .collect();
        let d = parsed(&with_threads(&nodes));
        assert_eq!(d.threads.len(), MAX_THREADS);
        assert_eq!(d.threads_total, (MAX_THREADS + 7) as u64);
        assert_eq!(d.threads[0].comments.len(), MAX_THREAD_COMMENTS);
        assert_eq!(
            d.threads[0].comments_total,
            (MAX_THREAD_COMMENTS + 9) as u64
        );
    }

    /// The fixture with its issue comments, or its reviews, replaced by
    /// `n` nodes and a `totalCount` of `total`.
    fn with_conversation(reviews: bool, n: usize, total: usize) -> String {
        let (start, end) = if reviews {
            // Up to the braces that close the pull request and above it.
            (
                DISCUSSION.rfind("\"reviews\":").unwrap(),
                DISCUSSION.rfind("}}}}").unwrap(),
            )
        } else {
            (
                DISCUSSION.rfind("\"comments\":{\"totalCount\":2").unwrap(),
                DISCUSSION.rfind(",\"reviews\":").unwrap(),
            )
        };
        let nodes: Vec<String> = (0..n)
            .map(|i| {
                format!(
                    "{{\"author\":{{\"login\":\"u{i}\"}},\"body\":\"b{i}\",\"state\":\"COMMENTED\",\
                     \"createdAt\":\"2023-11-14T20:00:00Z\",\"submittedAt\":\"2023-11-14T20:00:00Z\"}}"
                )
            })
            .collect();
        let name = if reviews { "reviews" } else { "comments" };
        format!(
            "{}\"{name}\":{{\"totalCount\":{total},\"nodes\":[{}]}}{}",
            &DISCUSSION[..start],
            nodes.join(","),
            &DISCUSSION[end..]
        )
    }

    #[test]
    fn the_conversation_says_when_comments_or_reviews_pass_their_caps() {
        let half = MAX_CONVERSATION / 2;
        for reviews in [false, true] {
            // All read: not capped, even at the cap.
            let d = parsed(&with_conversation(reviews, half, half));
            assert!(!d.conversation_capped, "reviews {reviews}");
            // GitHub has more than it sent.
            let d = parsed(&with_conversation(reviews, half, half + 1));
            assert!(d.conversation_capped, "reviews {reviews}");
            assert_eq!(
                d.conversation.len(),
                MAX_CONVERSATION / 2 + if reviews { 2 } else { 1 }
            );
            // It sent more than are taken.
            let d = parsed(&with_conversation(reviews, half + 3, half + 3));
            assert!(d.conversation_capped, "reviews {reviews}");
        }
    }

    #[test]
    fn null_and_malformed_nodes_are_skipped_without_a_panic() {
        let nodes = [
            "null".to_string(),
            "42".to_string(),
            "{\"path\":\"a\",\"line\":\"three\"}".to_string(),
            "{\"path\":\"b\",\"line\":-4,\"diffSide\":\"UP\",\"isResolved\":null}".to_string(),
            "{\"line\":3}".to_string(),
            format!("{{\"path\":\"{}\"}}", "p".repeat(MAX_PATH + 1)),
            "{\"path\":\"c\",\"comments\":{\"nodes\":[null,7,{\"body\":3}]}}".to_string(),
            "{\"path\":\"d\",\"comments\":{\"nodes\":\"x\"}}".to_string(),
            node(9, 1),
        ];
        let d = parsed(&with_threads(&nodes));
        let paths: Vec<&str> = d.threads.iter().map(|t| t.path.as_str()).collect();
        assert_eq!(paths, ["b", "c", "d", "f9"]);
        // An odd side or number is none, not a guess.
        assert_eq!((d.threads[0].side, d.threads[0].line), (None, None));
        assert!(!d.threads[0].resolved);
        assert_eq!(d.threads[1].comments, []);
        // A whole connection that is not one leaves an empty discussion.
        let odd = DISCUSSION.replace("\"reviewThreads\":{", "\"reviewThreads\":7,\"x\":{");
        let d = parsed(&odd);
        assert_eq!((d.threads.len(), d.threads_total), (0, 0));
        assert_eq!(d.conversation.len(), 3);
    }

    #[test]
    fn an_unknown_review_state_still_shows_its_body() {
        let odd = DISCUSSION.replace("\"COMMENTED\"", "\"SOMETHING_NEW\"");
        let d = parsed(&odd);
        assert_eq!(
            d.conversation[1].kind,
            ConversationKind::Review(ReviewVerdict::Commented)
        );
        // A review body of only spaces is empty too.
        let blank = DISCUSSION.replace("Looks good overall", "  \\r\\n ");
        assert_eq!(parsed(&blank).conversation.len(), 2);
    }
}
