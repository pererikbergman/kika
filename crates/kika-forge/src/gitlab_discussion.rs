//! A merge request's discussion on GitLab, in one GraphQL call: the
//! discussions on its diff become review threads, the rest its
//! conversation. A GitLab position (old and new line numbers) is mapped
//! to the side and line a thread has on GitHub, so anchoring works the
//! same for both. Every string is made terminal-safe while it is parsed.
//! The project path and iid travel as variables on stdin; argv is the
//! fixed `glab api graphql` line.

use serde::Deserialize;

use super::anchor::GhSide;
use super::discussion::{
    ConversationItem, ConversationKind, MAX_COMMENT, MAX_CONVERSATION, MAX_THREAD_COMMENTS,
    MAX_THREADS, PrDiscussion, ReviewThread, ThreadComment, ThreadSubject, line_number,
};
use super::github::Refusal;
use super::gitlab::{Glab, GlabCall};
use super::gitlab_review::{DiffRefs, User, mr_request, username};
use super::review::{MAX_PATH, Nodes, capped_body, data_of, full_oid, line, read_with};
use super::{ForgeRepo, ForgeStatus};

impl Glab<'_> {
    /// The discussion of merge request `number` of `repo`; `None` when
    /// GitLab has no such merge request.
    pub fn fetch_discussion(
        &self,
        repo: &ForgeRepo,
        number: u64,
    ) -> Result<Option<PrDiscussion>, Refusal> {
        let kind = GlabCall::Graphql {
            body: mr_request(DISCUSSION_QUERY, repo, number),
        };
        self.call(kind, |answer| read_with(answer, read_discussion))
    }
}

/// The discussion query: fixed text; the project path and iid are
/// variables. `first:100` is [`MAX_THREADS`], `first:50`
/// [`MAX_THREAD_COMMENTS`].
const DISCUSSION_QUERY: &str = "query($path:ID!,$iid:String!){project(fullPath:$path)\
{mergeRequest(iid:$iid){diffRefs{headSha startSha} \
discussions(first:100){pageInfo{hasNextPage} nodes{id resolved \
notes(first:50){pageInfo{hasNextPage} nodes{body system createdAt author{username} \
position{diffRefs{headSha startSha} filePath oldPath newPath oldLine newLine positionType \
lineRange{start{oldLine newLine} end{oldLine newLine}}}}}}}}}}";

/// Whether `id` is fit to keep as a GitLab global id:
/// `gid://gitlab/<Type>/<id>`, the type ASCII letters and `::`, the id
/// ASCII letters and digits, 128 bytes at most.
pub fn gitlab_id_ok(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("gid://gitlab/") else {
        return false;
    };
    let Some((kind, key)) = rest.split_once('/') else {
        return false;
    };
    id.len() <= 128
        && !kind.is_empty()
        && kind.bytes().all(|b| b.is_ascii_alphabetic() || b == b':')
        && !key.is_empty()
        && key.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Whether `id` is a merge request's GitLab global id, as a write
/// names it: `gid://gitlab/MergeRequest/<n>`, `n` decimal.
pub fn merge_request_id_ok(id: &str) -> bool {
    gitlab_id_ok(id)
        && id
            .strip_prefix("gid://gitlab/MergeRequest/")
            .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
}

#[derive(Deserialize)]
struct DiscussionData {
    project: Option<DiscussionProject>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiscussionProject {
    merge_request: Option<MrNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MrNode {
    diff_refs: Option<DiffRefs>,
    discussions: Option<Page<DiscussionNode>>,
}

/// A connection with whether more follow.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page<T> {
    page_info: Option<PageInfo>,
    #[serde(flatten)]
    nodes: Nodes<T>,
}

impl<T> Page<T> {
    fn more(&self) -> bool {
        self.page_info.as_ref().is_some_and(|i| i.has_next_page)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    #[serde(default)]
    has_next_page: bool,
}

#[derive(Deserialize)]
struct DiscussionNode {
    id: Option<String>,
    #[serde(default)]
    resolved: bool,
    notes: Option<Page<NoteNode>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NoteNode {
    body: Option<String>,
    #[serde(default)]
    system: bool,
    created_at: Option<String>,
    author: Option<User>,
    position: Option<Position>,
}

/// Where a diff note was written, as GitLab places it.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Position {
    diff_refs: Option<DiffRefs>,
    file_path: Option<String>,
    old_path: Option<String>,
    new_path: Option<String>,
    old_line: Option<i64>,
    new_line: Option<i64>,
    position_type: Option<String>,
    line_range: Option<LineRange>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct LineRange {
    start: Option<RangeEnd>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RangeEnd {
    old_line: Option<i64>,
    new_line: Option<i64>,
}

/// A thread's place, in the fields [`ReviewThread`] has for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadLines {
    pub subject: ThreadSubject,
    pub side: Option<GhSide>,
    pub line: Option<u32>,
    pub start_side: Option<GhSide>,
    pub start_line: Option<u32>,
}

/// One end of a position as a side and line: a new line only is added
/// (`Right`), an old line only removed (`Left`), and both a context line,
/// placed by its new number as GitHub places one.
fn side_line(old: Option<i64>, new: Option<i64>) -> Option<(GhSide, u32)> {
    match (line_number(old), line_number(new)) {
        (_, Some(new)) => Some((GhSide::Right, new)),
        (Some(old), None) => Some((GhSide::Left, old)),
        (None, None) => None,
    }
}

/// Map a GitLab position to the side and line fields of a thread. A
/// `lineRange` whose start is not its end sets the start; a `file`
/// position is about the whole file and has no line.
pub(crate) fn thread_lines(position: &Position) -> ThreadLines {
    if position.position_type.as_deref() == Some("file") {
        return ThreadLines {
            subject: ThreadSubject::File,
            side: None,
            line: None,
            start_side: None,
            start_line: None,
        };
    }
    let end = side_line(position.old_line, position.new_line);
    let start = position
        .line_range
        .as_ref()
        .and_then(|range| range.start.as_ref())
        .and_then(|start| side_line(start.old_line, start.new_line))
        .filter(|start| Some(*start) != end && end.is_some());
    ThreadLines {
        subject: ThreadSubject::Line,
        side: end.map(|(side, _)| side),
        line: end.map(|(_, line)| line),
        start_side: start.map(|(side, _)| side),
        start_line: start.map(|(_, line)| line),
    }
}

fn read_discussion(body: &[u8]) -> Result<Option<PrDiscussion>, ForgeStatus> {
    let data: DiscussionData = data_of(body)?;
    let project = data.project.ok_or(ForgeStatus::NoAccess)?;
    let Some(mr) = project.merge_request else {
        return Ok(None);
    };
    let head = mr.diff_refs.and_then(|r| full_oid(r.head_sha));
    let (more, discussions) = match mr.discussions {
        Some(page) => (page.more(), page.nodes.items().collect()),
        None => (false, Vec::new()),
    };
    let mut threads = Vec::new();
    let mut conversation = Vec::new();
    for discussion in discussions {
        let notes: Vec<NoteNode> = discussion
            .notes
            .map(|page| page.nodes.items().filter(|n| !n.system).collect())
            .unwrap_or_default();
        let Some(first) = notes.first() else {
            continue;
        };
        if first.position.is_some() {
            threads.extend(thread_of(
                discussion.id,
                discussion.resolved,
                notes,
                head.as_ref(),
            ));
        } else {
            conversation.extend(notes.into_iter().map(|note| ConversationItem {
                author: username(note.author),
                created: line(note.created_at.as_deref().unwrap_or_default()),
                body: capped_body(note.body.as_deref().unwrap_or_default(), MAX_COMMENT),
                kind: ConversationKind::Comment,
            }));
        }
    }
    let threads_total = threads.len() as u64;
    threads.truncate(MAX_THREADS);
    let conversation_total = conversation.len() as u64;
    // RFC 3339 times in one zone sort as text; the sort is stable, so
    // equal or odd times keep GitLab's order.
    conversation.sort_by(|a, b| a.created.cmp(&b.created));
    let conversation_capped = more || conversation.len() > MAX_CONVERSATION;
    conversation.truncate(MAX_CONVERSATION);
    Ok(Some(PrDiscussion {
        head_oid: head,
        threads,
        threads_total,
        conversation,
        conversation_total,
        conversation_capped,
    }))
}

/// A diff discussion as a thread, placed by its first note; `None`
/// without a path Kika can compare. It is outdated when it was written
/// on another head than `head`.
fn thread_of(
    id: Option<String>,
    resolved: bool,
    notes: Vec<NoteNode>,
    head: Option<&kika_git::commit_files::CommitId>,
) -> Option<ReviewThread> {
    let position = notes.first()?.position.as_ref()?;
    let path = [&position.new_path, &position.file_path, &position.old_path]
        .into_iter()
        .flatten()
        .find(|p| !p.is_empty())?
        .clone();
    if path.len() > MAX_PATH {
        return None;
    }
    let written_on = position
        .diff_refs
        .as_ref()
        .and_then(|r| full_oid(r.head_sha.clone()));
    let outdated = written_on.is_none() || written_on.as_ref() != head;
    let lines = thread_lines(position);
    let comments_total = notes.len() as u64;
    let comments = notes
        .into_iter()
        .take(MAX_THREAD_COMMENTS)
        .map(|note| ThreadComment {
            author: username(note.author),
            created: line(note.created_at.as_deref().unwrap_or_default()),
            body: capped_body(note.body.as_deref().unwrap_or_default(), MAX_COMMENT),
        })
        .collect();
    // As GitHub answers an outdated thread: its lines are the ones it
    // was written on, and it has none on the current diff.
    let current = |n: Option<u32>| if outdated { None } else { n };
    Some(ReviewThread {
        node_id: id.filter(|id| gitlab_id_ok(id)),
        path,
        resolved,
        outdated,
        subject: lines.subject,
        side: lines.side,
        line: current(lines.line),
        start_side: lines.start_side,
        start_line: current(lines.start_line),
        original_line: lines.line,
        original_start_line: lines.start_line,
        context: Vec::new(),
        comments,
        comments_total,
    })
}

#[cfg(any(test, feature = "test-support"))]
pub mod fixtures {
    use crate::review::fixtures::HOSTILE;

    pub const DISCUSSION: &str = include_str!("fixtures/gitlab_review_discussion.http");
    pub const DISCUSSION_MISSING: &str =
        include_str!("fixtures/gitlab_review_discussion_missing.http");

    /// The discussion answer with `HOSTILE` in an author, note bodies, a
    /// time and the conversation.
    pub fn hostile_discussion() -> String {
        let quoted = serde_json::to_string(HOSTILE).unwrap();
        let bare = quoted.trim_matches('"');
        DISCUSSION
            .replace("\"alice\"", &quoted)
            .replace("\"Agreed.\"", &quoted)
            .replace("Please cache this.", bare)
            .replace("\"2023-11-14T21:00:00Z\"", &quoted)
            .replace("\"Thanks for this!\"", &quoted)
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::ForgeHost;
    use crate::anchor::{GhLine, from_line_id, thread_span, to_line_id};
    use crate::github::answer;
    use crate::gitlab::fixtures::{PROJECT_NULL, RATE_LIMITED_429, UNAUTHORIZED_401};
    use crate::gitlab::{FakeGlab, GlabCommand};
    use crate::url::ForgeKind;
    use kika_git::commit_files::CommitId;
    use kika_git::patch::LineId;
    use kika_git::recording::CommandSink;

    fn repo() -> ForgeRepo {
        ForgeRepo {
            host: "gitlab.example.com".into(),
            owner: "acme".into(),
            name: "widgets".into(),
        }
    }

    fn fetch(runner: &FakeGlab, number: u64) -> Result<Option<PrDiscussion>, Refusal> {
        let host = ForgeHost::parse("gitlab.example.com")
            .unwrap()
            .with_kind(ForgeKind::GitLab);
        let sink = CommandSink::new();
        let glab = Glab {
            runner,
            sink: &sink,
            host: &host,
            cwd: "/cache".into(),
            now: 1_700_000_000,
        };
        glab.fetch_discussion(&repo(), number)
    }

    fn parsed(text: &str) -> PrDiscussion {
        fetch(&FakeGlab::new().reply(answer(text)), 42)
            .unwrap()
            .unwrap()
    }

    fn sent(cmd: &GlabCommand) -> serde_json::Value {
        serde_json::from_slice(&cmd.stdin()).unwrap()
    }

    fn position(old: Option<i64>, new: Option<i64>) -> Position {
        Position {
            old_line: old,
            new_line: new,
            position_type: Some("text".into()),
            ..Position::default()
        }
    }

    fn ranged(start: (Option<i64>, Option<i64>), end: (Option<i64>, Option<i64>)) -> Position {
        Position {
            line_range: Some(LineRange {
                start: Some(RangeEnd {
                    old_line: start.0,
                    new_line: start.1,
                }),
            }),
            ..position(end.0, end.1)
        }
    }

    /// A current line thread in `lines`' place.
    fn placed(lines: ThreadLines) -> ReviewThread {
        let mut thread = crate::discussion::tests::thread(
            lines.side,
            lines.line,
            lines.start_side,
            lines.start_line,
        );
        thread.subject = lines.subject;
        thread
    }

    #[test]
    fn each_kind_of_position_maps_to_its_side_and_line() {
        let line = |side, line| ThreadLines {
            subject: ThreadSubject::Line,
            side: Some(side),
            line: Some(line),
            start_side: None,
            start_line: None,
        };
        // Added, removed, and a context line by its new number.
        assert_eq!(
            thread_lines(&position(None, Some(3))),
            line(GhSide::Right, 3)
        );
        assert_eq!(
            thread_lines(&position(Some(2), None)),
            line(GhSide::Left, 2)
        );
        assert_eq!(
            thread_lines(&position(Some(4), Some(6))),
            line(GhSide::Right, 6)
        );
        // Multi-line: the range's start, on its own side.
        assert_eq!(
            thread_lines(&ranged((None, Some(3)), (None, Some(5)))),
            ThreadLines {
                start_side: Some(GhSide::Right),
                start_line: Some(3),
                ..line(GhSide::Right, 5)
            }
        );
        assert_eq!(
            thread_lines(&ranged((Some(2), None), (None, Some(5)))),
            ThreadLines {
                start_side: Some(GhSide::Left),
                start_line: Some(2),
                ..line(GhSide::Right, 5)
            }
        );
        // A range of one line has no start of its own.
        assert_eq!(
            thread_lines(&ranged((None, Some(5)), (None, Some(5)))),
            line(GhSide::Right, 5)
        );
        // A whole file.
        let file = Position {
            position_type: Some("file".into()),
            ..position(Some(1), Some(1))
        };
        assert_eq!(
            thread_lines(&file),
            ThreadLines {
                subject: ThreadSubject::File,
                side: None,
                line: None,
                start_side: None,
                start_line: None,
            }
        );
        // No line, or one that is no line number: none, not a guess.
        let none = thread_lines(&position(Some(0), Some(-3)));
        assert_eq!((none.side, none.line), (None, None));
    }

    #[test]
    fn a_mapped_thread_anchors_back_on_the_gitlab_line() {
        for (old, new, side, want) in [
            (None, Some(3), GhSide::Right, 3),
            (Some(2), None, GhSide::Left, 2),
            (Some(4), Some(6), GhSide::Right, 6),
        ] {
            let thread = placed(thread_lines(&position(old, new)));
            let (start, end) = thread_span(&thread).unwrap();
            assert_eq!(start, None);
            assert_eq!(from_line_id(end), GhLine { side, line: want });
            assert_eq!(to_line_id(GhLine { side, line: want }), end);
            let id = match side {
                GhSide::Right => LineId::added(want),
                GhSide::Left => LineId::removed(want),
            };
            assert_eq!(end, id, "{old:?} {new:?}");
        }
        let thread = placed(thread_lines(&ranged((Some(2), None), (None, Some(5)))));
        assert_eq!(
            thread_span(&thread),
            Some((Some(LineId::removed(2)), LineId::added(5)))
        );
        let file = Position {
            position_type: Some("file".into()),
            ..Position::default()
        };
        assert_eq!(thread_span(&placed(thread_lines(&file))), None);
    }

    #[test]
    fn one_call_carries_the_fixed_query_and_the_path_and_iid_only() {
        let runner = FakeGlab::new().reply(answer(DISCUSSION));
        fetch(&runner, 42).unwrap().unwrap();
        let [cmd] = runner.calls().try_into().unwrap();
        assert_eq!(
            cmd.display_line(),
            "glab api graphql --include --hostname gitlab.example.com --input -"
        );
        let body = sent(&cmd);
        assert_eq!(body["query"], DISCUSSION_QUERY);
        assert_eq!(
            body["variables"],
            serde_json::json!({ "path": "acme/widgets", "iid": "42" })
        );
        assert!(DISCUSSION_QUERY.contains(&format!("discussions(first:{MAX_THREADS})")));
        assert!(DISCUSSION_QUERY.contains(&format!("notes(first:{MAX_THREAD_COMMENTS})")));
    }

    #[test]
    fn the_fixture_parses_into_threads_and_a_conversation_without_system_notes() {
        let d = parsed(DISCUSSION);
        assert_eq!(d.head_oid, CommitId::parse(&"1".repeat(40)));
        assert_eq!(d.threads_total, 6);
        let [right, left, multi, outdated, file, context] = d.threads.as_slice() else {
            panic!("{:#?}", d.threads);
        };
        assert_eq!(
            right.node_id.as_deref(),
            Some("gid://gitlab/Discussion/1111111111111111111111111111111111111111")
        );
        assert_eq!(right.path, "src/cache.rs");
        assert_eq!(
            (right.side, right.line, right.start_side, right.start_line),
            (Some(GhSide::Right), Some(3), None, None)
        );
        assert_eq!(
            (right.resolved, right.outdated, right.subject),
            (false, false, ThreadSubject::Line)
        );
        // The system note in it is not a comment.
        assert_eq!(right.comments_total, 2);
        assert_eq!(
            right.comments[1],
            ThreadComment {
                author: "bob".into(),
                created: "2023-11-14T21:30:00Z".into(),
                body: "Agreed.".into(),
            }
        );
        assert_eq!(right.comments[0].body, "Please cache this.\n\n- one\n- two");

        assert_eq!((left.side, left.line), (Some(GhSide::Left), Some(2)));
        assert!(left.resolved);
        assert_eq!(
            (multi.side, multi.line, multi.start_side, multi.start_line),
            (Some(GhSide::Right), Some(5), Some(GhSide::Right), Some(3))
        );
        assert_eq!(multi.path, "src/lib.rs");
        // Written on another head: no line now, the old one kept.
        assert!(outdated.outdated && !outdated.resolved);
        assert_eq!((outdated.line, outdated.original_line), (None, Some(7)));
        assert_eq!(file.subject, ThreadSubject::File);
        assert_eq!(file.path, "docs/café.md");
        assert_eq!((context.side, context.line), (Some(GhSide::Right), Some(6)));
        assert!(d.threads.iter().all(|t| t.context.is_empty()));

        let items: Vec<(&str, &str)> = d
            .conversation
            .iter()
            .map(|c| (c.author.as_str(), c.body.as_str()))
            .collect();
        assert_eq!(
            items,
            [
                ("frank", "Thanks for this!"),
                ("grace", "Ping"),
                ("alice", "Pong")
            ]
        );
        assert!(
            d.conversation
                .iter()
                .all(|c| c.kind == ConversationKind::Comment)
        );
        assert_eq!(d.conversation_total, 3);
        assert!(!d.conversation_capped);
    }

    #[test]
    fn only_a_gitlab_global_id_is_kept() {
        let good = "gid://gitlab/Discussion/1111111111111111111111111111111111111111";
        assert!(gitlab_id_ok(good));
        assert!(gitlab_id_ok("gid://gitlab/MergeRequests::Discussion/7"));
        let too_long = format!("gid://gitlab/Discussion/{}", "a".repeat(110));
        for bad in [
            "",
            "PRRT_kwDOA1",
            "gid://gitlab/Discussion/",
            "gid://gitlab//1",
            "gid://gitlab/Discussion/../x",
            "gid://gitlab/Discussion/1/2",
            "gid://other/Discussion/1",
            "gid://gitlab/Discussion/1\u{1b}[31m",
            too_long.as_str(),
        ] {
            assert!(!gitlab_id_ok(bad), "{bad:?}");
        }
        let odd = DISCUSSION.replacen(good, "gid://gitlab/Discussion/../x", 1);
        assert_eq!(parsed(&odd).threads[0].node_id, None);
    }

    /// The fixture with its discussions replaced by `nodes`.
    fn with_discussions(nodes: &[String], more: bool) -> String {
        let start = DISCUSSION.find("\"discussions\":").unwrap();
        let end = DISCUSSION.rfind("}}}}").unwrap();
        format!(
            "{}\"discussions\":{{\"pageInfo\":{{\"hasNextPage\":{more}}},\"nodes\":[{}]}}{}",
            &DISCUSSION[..start],
            nodes.join(","),
            &DISCUSSION[end..]
        )
    }

    fn diff_discussion(i: usize, notes: usize) -> String {
        let head = "1".repeat(40);
        let notes: Vec<String> = (0..notes)
            .map(|n| {
                format!(
                    "{{\"body\":\"b\",\"system\":false,\"createdAt\":\"t\",\
                     \"author\":{{\"username\":\"u{n}\"}},\"position\":{{\"diffRefs\":\
                     {{\"headSha\":\"{head}\"}},\"filePath\":\"f{i}\",\"newLine\":{},\
                     \"positionType\":\"text\"}}}}",
                    i + 1
                )
            })
            .collect();
        format!(
            "{{\"id\":\"gid://gitlab/Discussion/{i}\",\"resolved\":false,\
             \"notes\":{{\"nodes\":[{}]}}}}",
            notes.join(",")
        )
    }

    #[test]
    fn threads_and_comments_past_their_caps_are_cut() {
        let nodes: Vec<String> = (0..MAX_THREADS + 7)
            .map(|i| diff_discussion(i, if i == 0 { MAX_THREAD_COMMENTS + 9 } else { 1 }))
            .collect();
        let d = parsed(&with_discussions(&nodes, false));
        assert_eq!(d.threads.len(), MAX_THREADS);
        assert_eq!(d.threads_total, (MAX_THREADS + 7) as u64);
        assert_eq!(d.threads[0].comments.len(), MAX_THREAD_COMMENTS);
        assert_eq!(
            d.threads[0].comments_total,
            (MAX_THREAD_COMMENTS + 9) as u64
        );
        assert_eq!(d.threads[5].line, Some(6));
        // More discussions than were read: the conversation says so.
        let d = parsed(&with_discussions(&nodes[..1], true));
        assert!(d.conversation_capped);
    }

    #[test]
    fn null_and_malformed_nodes_are_skipped_without_a_panic() {
        let nodes = [
            "null".to_string(),
            "{\"id\":\"x\",\"notes\":null}".to_string(),
            "{\"notes\":{\"nodes\":[null]}}".to_string(),
            // Only system notes: nothing to show.
            "{\"notes\":{\"nodes\":[{\"body\":\"merged\",\"system\":true}]}}".to_string(),
            // A diff note with no path.
            "{\"notes\":{\"nodes\":[{\"body\":\"b\",\"position\":{\"newLine\":1}}]}}".to_string(),
            format!(
                "{{\"notes\":{{\"nodes\":[{{\"position\":{{\"filePath\":\"{}\"}}}}]}}}}",
                "p".repeat(MAX_PATH + 1)
            ),
            diff_discussion(9, 1),
        ];
        let d = parsed(&with_discussions(&nodes, false));
        let paths: Vec<&str> = d.threads.iter().map(|t| t.path.as_str()).collect();
        assert_eq!(paths, ["f9"]);
        assert!(d.conversation.is_empty());
        // A note without a head it was written on is outdated.
        let unplaced = with_discussions(
            &[diff_discussion(9, 1).replace(&"1".repeat(40), "x")],
            false,
        );
        assert!(parsed(&unplaced).threads[0].outdated);
    }

    #[test]
    fn a_missing_merge_request_is_none_and_refusals_map_as_for_the_detail() {
        assert_eq!(
            fetch(&FakeGlab::new().reply(answer(DISCUSSION_MISSING)), 999),
            Ok(None)
        );
        for (fixture, want) in [
            (PROJECT_NULL, Refusal::Status(ForgeStatus::NoAccess)),
            (UNAUTHORIZED_401, Refusal::Status(ForgeStatus::NotLoggedIn)),
            (
                RATE_LIMITED_429,
                Refusal::Limited {
                    until: 1_700_001_200,
                },
            ),
        ] {
            assert_eq!(
                fetch(&FakeGlab::new().reply(answer(fixture)), 42),
                Err(want)
            );
        }
        assert_eq!(
            fetch(&FakeGlab::new().missing(), 42),
            Err(Refusal::Status(ForgeStatus::Cli(
                crate::cli::CliStatus::NotInstalled
            )))
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
            seen += usize::from(item.author.contains("red"));
            seen += usize::from(item.body.contains("red"));
        }
        assert!(seen >= 5, "{seen}: {d:#?}");
    }
}
