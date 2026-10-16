//! Review anchoring: where a GitHub review thread sits in Kika's diff.
//! GitHub names a commented line by side (`LEFT` old, `RIGHT` new) and
//! line number on the whole pull request's diff; Kika names it by
//! [`LineId`]. This maps one to the other, numbers the rows of a streamed
//! diff, and decides when a thread no longer fits the diff shown. Pure:
//! no I/O, nothing read from the network here.

use super::discussion::{ReviewThread, ThreadSubject};
use kika_git::commit_files::CommitId;
use kika_git::diff::{DiffLine, LineKind};
use kika_git::patch::{LineId, LineSide};

/// The side of the diff GitHub put a comment on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GhSide {
    /// The base: removed and context lines, by old line number.
    Left,
    /// The head: added and context lines, by new line number.
    Right,
}

/// A line as GitHub's review API names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GhLine {
    pub side: GhSide,
    pub line: u32,
}

/// GitHub's line as Kika's. For a context line too, the id names "line N
/// of that side".
pub fn to_line_id(gh: GhLine) -> LineId {
    let side = match gh.side {
        GhSide::Left => LineSide::Old,
        GhSide::Right => LineSide::New,
    };
    LineId {
        side,
        line: gh.line,
    }
}

/// The exact inverse of [`to_line_id`].
pub fn from_line_id(id: LineId) -> GhLine {
    let side = match id.side {
        LineSide::Old => GhSide::Left,
        LineSide::New => GhSide::Right,
    };
    GhLine {
        side,
        line: id.line,
    }
}

/// A thread's first and last line, from `start_side/start_line` and
/// `side/line`. `None` when GitHub gave no line or side for its end: the
/// thread then has no place on the current diff.
pub fn thread_span(thread: &ReviewThread) -> Option<(Option<LineId>, LineId)> {
    let side = thread.side?;
    let end = to_line_id(GhLine {
        side,
        line: thread.line?,
    });
    // A start with no side of its own is on the end's.
    let start = thread.start_line.map(|line| {
        to_line_id(GhLine {
            side: thread.start_side.unwrap_or(side),
            line,
        })
    });
    Some((start, end))
}

/// A row's old and new line numbers; `None` on a side it is not on.
pub type RowNumber = (Option<u32>, Option<u32>);

/// The (old, new) line number of each streamed row. Numbering starts at
/// each good `@@ -a[,b] +c[,d] @@` header: a context row moves both sides
/// on, a removed row the old, an added row the new. File headers, hunk
/// headers and git's `\ No newline` marker have none, and so has every
/// row after a malformed header until the next good one.
pub fn row_numbers(lines: &[DiffLine]) -> Vec<RowNumber> {
    walk(lines)
        .into_iter()
        .map(|row| match row {
            None => (None, None),
            Some((LineKind::Added, _, new)) => (None, Some(new)),
            Some((LineKind::Removed, old, _)) => (Some(old), None),
            Some((_, old, new)) => (Some(old), Some(new)),
        })
        .collect()
}

/// The old and new line counters at each numbered row, before that row
/// moves them on; `None` where [`row_numbers`] has none. An added row's
/// old counter is the next old line, a removed row's new counter the
/// next new line: what a forge that names a line by both sides needs.
pub fn row_positions(lines: &[DiffLine]) -> Vec<Option<(u32, u32)>> {
    walk(lines)
        .into_iter()
        .map(|row| row.map(|(_, old, new)| (old, new)))
        .collect()
}

/// Each row's kind and the (old, new) counters before it, for the rows
/// that are numbered; the one walk both numberings share.
fn walk(lines: &[DiffLine]) -> Vec<Option<(LineKind, u32, u32)>> {
    let mut at: Option<(u32, u32)> = None;
    lines
        .iter()
        .map(|row| match row.kind {
            LineKind::Header => {
                at = None;
                None
            }
            LineKind::Hunk => {
                at = hunk_start(&row.text);
                None
            }
            LineKind::Context if row.text.starts_with('\\') => None,
            kind => {
                let (old, new) = at.as_mut()?;
                let before = (kind, *old, *new);
                match kind {
                    LineKind::Added => *new = new.saturating_add(1),
                    LineKind::Removed => *old = old.saturating_add(1),
                    _ => {
                        *old = old.saturating_add(1);
                        *new = new.saturating_add(1);
                    }
                }
                Some(before)
            }
        })
        .collect()
}

/// The first old and new line of a hunk header, or `None` when it is not
/// one git writes for a two-sided diff.
fn hunk_start(text: &str) -> Option<(u32, u32)> {
    let rest = text.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, rest) = rest.split_once(' ')?;
    if !rest.starts_with("@@") {
        return None;
    }
    let start = |range: &str| -> Option<u32> {
        let (first, len) = match range.split_once(',') {
            Some((first, len)) => (first, Some(len)),
            None => (range, None),
        };
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        if !digits(first) || len.is_some_and(|l| !digits(l)) {
            return None;
        }
        first.parse().ok()
    };
    Some((start(old)?, start(new)?))
}

/// The first row numbered `id` on its side, from [`row_numbers`]. A new
/// id can only match an added or context row, an old id a removed or
/// context row, since only those carry a number on that side.
pub fn row_in(numbers: &[RowNumber], id: LineId) -> Option<usize> {
    numbers.iter().position(|&(old, new)| match id.side {
        LineSide::Old => old == Some(id.line),
        LineSide::New => new == Some(id.line),
    })
}

/// The first streamed row that is line `id`.
pub fn row_of(lines: &[DiffLine], id: LineId) -> Option<usize> {
    row_in(&row_numbers(lines), id)
}

/// Which diff the reviewer has open: GitHub's positions are on the whole
/// pull request's, so a single commit's diff cannot hold them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorView {
    WholePr,
    Commit,
}

/// Where a thread is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// On its lines of the diff shown.
    Line { start: Option<LineId>, end: LineId },
    /// Its lines changed since it was written: above the diff, with the
    /// lines it was written against.
    Outdated,
    /// About the whole file, not a line.
    File,
    /// On the whole pull request's diff, which is not the one shown.
    OtherView,
}

/// Place `thread` in `view` of the pull request whose head is `head`;
/// `discussion_head` is the head GitHub had when the threads were read.
pub fn place(
    thread: &ReviewThread,
    view: AnchorView,
    head: &CommitId,
    discussion_head: Option<&CommitId>,
) -> Placement {
    if thread.subject == ThreadSubject::File {
        return Placement::File;
    }
    if view == AnchorView::Commit {
        return Placement::OtherView;
    }
    // The pull request moved between the detail read and this one: its
    // line numbers may be for a diff Kika does not show.
    if discussion_head != Some(head) {
        return Placement::Outdated;
    }
    match thread_span(thread) {
        Some((start, end)) if !thread.outdated => Placement::Line { start, end },
        _ => Placement::Outdated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discussion::tests::thread;

    fn rows(texts: &[&str]) -> Vec<DiffLine> {
        let mut classifier = kika_git::diff::Classifier::new(kika_git::diff::EmojiZwj::Escape);
        texts
            .iter()
            .map(|t| classifier.classify(t.as_bytes()))
            .collect()
    }

    const HEADER: [&str; 4] = [
        "diff --git a/f b/f",
        "index 1111111..2222222 100644",
        "--- a/f",
        "+++ b/f",
    ];

    fn with_header(body: &[&'static str]) -> Vec<&'static str> {
        HEADER.iter().copied().chain(body.iter().copied()).collect()
    }

    #[test]
    fn github_lines_and_line_ids_round_trip_on_both_sides() {
        for (gh, id) in [
            (
                GhLine {
                    side: GhSide::Left,
                    line: 7,
                },
                LineId::removed(7),
            ),
            (
                GhLine {
                    side: GhSide::Right,
                    line: 12,
                },
                LineId::added(12),
            ),
        ] {
            assert_eq!(to_line_id(gh), id);
            assert_eq!(from_line_id(id), gh);
            assert_eq!(from_line_id(to_line_id(gh)), gh);
            assert_eq!(to_line_id(from_line_id(id)), id);
        }
    }

    #[test]
    fn a_span_covers_single_multi_line_and_missing_line_threads() {
        let single = thread(Some(GhSide::Right), Some(4), None, None);
        assert_eq!(thread_span(&single), Some((None, LineId::added(4))));
        let multi = thread(Some(GhSide::Right), Some(9), Some(GhSide::Left), Some(6));
        assert_eq!(
            thread_span(&multi),
            Some((Some(LineId::removed(6)), LineId::added(9)))
        );
        // A start with no side of its own is on the end's side.
        let same = thread(Some(GhSide::Left), Some(9), None, Some(6));
        assert_eq!(
            thread_span(&same),
            Some((Some(LineId::removed(6)), LineId::removed(9)))
        );
        for missing in [
            thread(None, Some(4), None, None),
            thread(Some(GhSide::Right), None, None, Some(2)),
        ] {
            assert_eq!(thread_span(&missing), None);
        }
    }

    type Case = (&'static str, Vec<&'static str>, Vec<RowNumber>);

    fn numbering_cases() -> Vec<Case> {
        let none = (None, None);
        vec![
            (
                "two hunks",
                with_header(&[
                    "@@ -1,3 +1,3 @@",
                    " a",
                    "-b",
                    "+B",
                    " c",
                    "@@ -10,2 +10,3 @@ fn x() {",
                    " j",
                    "+k",
                    " l",
                ]),
                [none; 4]
                    .into_iter()
                    .chain([
                        none,
                        (Some(1), Some(1)),
                        (Some(2), None),
                        (None, Some(2)),
                        (Some(3), Some(3)),
                        none,
                        (Some(10), Some(10)),
                        (None, Some(11)),
                        (Some(11), Some(12)),
                    ])
                    .collect(),
            ),
            (
                "a new file",
                with_header(&["@@ -0,0 +1,3 @@", "+x", "+y", "+z"]),
                [none; 4]
                    .into_iter()
                    .chain([none, (None, Some(1)), (None, Some(2)), (None, Some(3))])
                    .collect(),
            ),
            (
                "a deleted file",
                with_header(&["@@ -1,2 +0,0 @@", "-x", "-y"]),
                [none; 4]
                    .into_iter()
                    .chain([none, (Some(1), None), (Some(2), None)])
                    .collect(),
            ),
            (
                "a no-newline marker",
                with_header(&[
                    "@@ -1 +1 @@",
                    "-x",
                    "\\ No newline at end of file",
                    "+y",
                    "\\ No newline at end of file",
                ]),
                [none; 4]
                    .into_iter()
                    .chain([none, (Some(1), None), none, (None, Some(1)), none])
                    .collect(),
            ),
            (
                "a header without lengths",
                with_header(&["@@ -5 +5 @@", "-x", "+y"]),
                [none; 4]
                    .into_iter()
                    .chain([none, (Some(5), None), (None, Some(5))])
                    .collect(),
            ),
            (
                "a malformed header, then a good one",
                with_header(&[
                    "@@ -x,1 +1,1 @@",
                    " a",
                    "+b",
                    "@@@ -1,1 -1,1 +1,1 @@@",
                    " c",
                    "@@ -3,1 +3,1",
                    " d",
                    "@@ -8,1 +9,1 @@",
                    " e",
                ]),
                [none; 4]
                    .into_iter()
                    .chain([
                        none,
                        none,
                        none,
                        none,
                        none,
                        none,
                        none,
                        none,
                        (Some(8), Some(9)),
                    ])
                    .collect(),
            ),
        ]
    }

    #[test]
    fn rows_are_numbered_from_each_hunk_header() {
        for (name, texts, want) in numbering_cases() {
            let lines = rows(&texts);
            assert_eq!(row_numbers(&lines), want, "{name}");
        }
    }

    #[test]
    fn positions_hold_both_counters_before_each_numbered_row() {
        let lines = rows(&with_header(&[
            "@@ -1,3 +1,3 @@",
            " a",
            "-b",
            "+B",
            " c",
            "\\ No newline at end of file",
            "@@ -10,2 +10,3 @@ fn x() {",
            " j",
            "+k",
            " l",
        ]));
        let want: Vec<Option<(u32, u32)>> = [None; 4]
            .into_iter()
            .chain([
                None,
                Some((1, 1)),
                // A removed row: the new counter is the next new line.
                Some((2, 2)),
                // An added row: the old counter is the next old line.
                Some((3, 2)),
                Some((3, 3)),
                None,
                None,
                Some((10, 10)),
                Some((11, 11)),
                Some((11, 12)),
            ])
            .collect();
        assert_eq!(row_positions(&lines), want);
        // Numbered where `row_numbers` numbers, and on the same lines.
        for (pos, number) in row_positions(&lines).iter().zip(row_numbers(&lines)) {
            match (pos, number) {
                (None, (None, None)) => {}
                (Some((old, new)), (o, n)) => {
                    assert!(o.is_none_or(|o| o == *old) && n.is_none_or(|n| n == *new));
                    assert!(o.is_some() || n.is_some());
                }
                other => panic!("{other:?}"),
            }
        }
        for (name, texts, _) in numbering_cases() {
            let lines = rows(&texts);
            let numbered = |n: &RowNumber| n.0.is_some() || n.1.is_some();
            let a: Vec<bool> = row_numbers(&lines).iter().map(numbered).collect();
            let b: Vec<bool> = row_positions(&lines).iter().map(Option::is_some).collect();
            assert_eq!(a, b, "{name}");
        }
    }

    #[test]
    fn an_old_id_finds_a_removed_or_context_row_and_a_new_id_an_added_or_context_row() {
        let lines = rows(&with_header(&[
            "@@ -1,3 +1,3 @@",
            " a",
            "-b",
            "+B",
            " c",
            "@@ -10,2 +10,3 @@",
            " j",
            "+k",
            " l",
        ]));
        for (id, want) in [
            (LineId::removed(1), Some(5)),
            (LineId::added(1), Some(5)),
            (LineId::removed(2), Some(6)),
            (LineId::added(2), Some(7)),
            (LineId::removed(3), Some(8)),
            (LineId::added(3), Some(8)),
            (LineId::added(11), Some(11)),
            (LineId::removed(11), Some(12)),
            (LineId::added(12), Some(12)),
            // Not in any hunk.
            (LineId::added(5), None),
            (LineId::removed(12), None),
        ] {
            assert_eq!(row_of(&lines, id), want, "{id:?}");
            assert_eq!(row_in(&row_numbers(&lines), id), want, "{id:?} by numbers");
        }
        for (name, texts, _) in numbering_cases() {
            let lines = rows(&texts);
            for (row, &(old, new)) in row_numbers(&lines).iter().enumerate() {
                if let Some(old) = old {
                    let found = row_of(&lines, LineId::removed(old)).unwrap();
                    assert!(found <= row, "{name}: old {old}");
                    assert!(matches!(
                        lines[found].kind,
                        LineKind::Removed | LineKind::Context
                    ));
                }
                if let Some(new) = new {
                    let found = row_of(&lines, LineId::added(new)).unwrap();
                    assert!(found <= row, "{name}: new {new}");
                    assert!(matches!(
                        lines[found].kind,
                        LineKind::Added | LineKind::Context
                    ));
                }
            }
        }
    }

    fn head(c: char) -> CommitId {
        CommitId::parse(&c.to_string().repeat(40)).unwrap()
    }

    #[test]
    fn placement_follows_the_five_rules_in_order() {
        let current = thread(Some(GhSide::Right), Some(4), None, None);
        let multi = thread(Some(GhSide::Right), Some(9), Some(GhSide::Right), Some(7));
        let outdated = ReviewThread {
            outdated: true,
            ..current.clone()
        };
        let no_line = thread(Some(GhSide::Right), None, None, None);
        let file = ReviewThread {
            subject: ThreadSubject::File,
            ..current.clone()
        };
        let (a, b) = (head('a'), head('b'));
        let line = |start, end| Placement::Line { start, end };
        let cases: Vec<(
            &str,
            &ReviewThread,
            AnchorView,
            Option<&CommitId>,
            Placement,
        )> = vec![
            (
                "file",
                &file,
                AnchorView::WholePr,
                Some(&a),
                Placement::File,
            ),
            (
                "file in a commit",
                &file,
                AnchorView::Commit,
                Some(&b),
                Placement::File,
            ),
            (
                "commit view",
                &current,
                AnchorView::Commit,
                Some(&a),
                Placement::OtherView,
            ),
            (
                "outdated in a commit",
                &outdated,
                AnchorView::Commit,
                Some(&a),
                Placement::OtherView,
            ),
            (
                "moved head",
                &current,
                AnchorView::WholePr,
                Some(&b),
                Placement::Outdated,
            ),
            (
                "unknown head",
                &current,
                AnchorView::WholePr,
                None,
                Placement::Outdated,
            ),
            (
                "isOutdated",
                &outdated,
                AnchorView::WholePr,
                Some(&a),
                Placement::Outdated,
            ),
            (
                "line null",
                &no_line,
                AnchorView::WholePr,
                Some(&a),
                Placement::Outdated,
            ),
            (
                "current",
                &current,
                AnchorView::WholePr,
                Some(&a),
                line(None, LineId::added(4)),
            ),
            (
                "multi-line",
                &multi,
                AnchorView::WholePr,
                Some(&a),
                line(Some(LineId::added(7)), LineId::added(9)),
            ),
        ];
        for (name, thread, view, discussion_head, want) in cases {
            assert_eq!(place(thread, view, &a, discussion_head), want, "{name}");
        }
    }
}
