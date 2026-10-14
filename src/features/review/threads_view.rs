//! Review threads drawn into the files screen's diff: a block above the
//! first row for outdated, file-level, other-view and missing-line
//! threads, and each line thread right under its row. Reads state only;
//! every string was made terminal-safe when GitHub's answer was parsed,
//! and every row is cut to the pane.

use std::collections::BTreeMap;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::markdown;
use super::state::{ReviewFetch, ReviewState};
use super::threads::{FileThreads, ThreadScope, first_rows};
use super::view::problem_text;
use crate::diff_pane::DiffPane;
use crate::diff_pane_view::{diff_pane_body, marks_of, styled_diff_line};
use crate::view::width_of;
use kika_forge::anchor::{GhSide, Placement, RowNumber, thread_span};
use kika_forge::discussion::{PrDiscussion, ReviewThread};
use kika_git::diff::{DiffLine, LineKind};

/// Body rows an unresolved comment shows inline before it is cut.
pub const INLINE_BODY: usize = 6;
/// Comments of one thread drawn inline.
const INLINE_COMMENTS: usize = 20;
/// The longest time shown when GitHub's is not RFC 3339.
const TIME_COLS: usize = 20;

/// `YYYY-MM-DD HH:MM` from an RFC 3339 time; anything else cut short.
pub fn short_time(created: &str) -> String {
    let b = created.as_bytes();
    let digits = |r: std::ops::Range<usize>| r.into_iter().all(|i| b[i].is_ascii_digit());
    let rfc = b.len() >= 16
        && digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && matches!(b[10], b'T' | b't' | b' ')
        && digits(11..13)
        && b[13] == b':'
        && digits(14..16);
    if rfc {
        format!("{} {}", &created[..10], &created[11..16])
    } else {
        created.chars().take(TIME_COLS).collect()
    }
}

/// The detail screen's thread line, when the threads were asked for.
pub fn thread_summary(review: &ReviewState) -> Option<String> {
    let ReviewFetch { loading, error, .. } = &review.discussion_fetch;
    if *loading {
        return Some("Threads: loading…".into());
    }
    if let Some(problem) = error {
        return Some(format!(
            "Threads: could not be read ({})",
            problem_text(*problem).0
        ));
    }
    let discussion = review.discussion.as_ref()?;
    let unresolved = discussion.threads.iter().filter(|t| !t.resolved).count();
    Some(format!(
        "Threads: {unresolved} unresolved of {} · {} comments",
        discussion.threads_total,
        discussion.conversation.len()
    ))
}

/// `line` cut to `width` columns, ending in `…` when anything is cut.
fn clip_line(line: Line<'static>, width: usize) -> Line<'static> {
    if line.width() <= width {
        return line;
    }
    let style = line.style;
    let mut spans = Vec::new();
    let mut used = 0;
    for span in line.spans {
        let w = span.width();
        if used + w < width {
            used += w;
            spans.push(span);
            continue;
        }
        // Something is cut from here on: leave a column for the `…`.
        let room = width - used;
        let mut kept = String::new();
        let mut cols = 0;
        for c in span.content.chars() {
            let cw = width_of(c.encode_utf8(&mut [0; 4]));
            if cols + cw + 1 > room {
                break;
            }
            cols += cw;
            kept.push(c);
        }
        if room > 0 {
            kept.push('…');
        }
        spans.push(Span::styled(kept, span.style));
        break;
    }
    Line::from(spans).style(style)
}

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

/// What the header of a thread above the diff says before its author.
fn top_label(thread: &ReviewThread, placement: Placement) -> String {
    match placement {
        Placement::File => "file comment".into(),
        Placement::OtherView => "on the whole pull request (c)".into(),
        Placement::Line { end, .. } => format!("line {} · not in this diff", end.line),
        Placement::Outdated => {
            let side = match thread.side {
                Some(GhSide::Left) => " (old)",
                Some(GhSide::Right) => " (new)",
                None => "",
            };
            match thread.original_line {
                Some(n) => format!("outdated · was line {n}{side}"),
                None => "outdated".into(),
            }
        }
    }
}

/// Where a line thread sits: `line N` or `lines A–B`.
fn span_label(thread: &ReviewThread) -> Option<String> {
    let (start, end) = thread_span(thread)?;
    Some(match start {
        Some(start) if start != end => format!("lines {}–{}", start.line, end.line),
        _ => format!("line {}", end.line),
    })
}

/// A thread's header: who started it, when, its state, where and how
/// many comments. A thread above the diff (`top` is where it was placed)
/// leads with why it is there; its lines show only when they are real
/// lines of another view.
fn header(thread: &ReviewThread, top: Option<Placement>, selected: bool) -> Line<'static> {
    let outdated = top == Some(Placement::Outdated);
    let first = thread.comments.first();
    let mut parts: Vec<String> = top.map(|p| top_label(thread, p)).into_iter().collect();
    parts.push(first.map_or_else(|| "ghost".to_string(), |c| c.author.clone()));
    if let Some(c) = first {
        parts.push(short_time(&c.created));
    }
    parts.push(
        if thread.resolved {
            "resolved"
        } else {
            "unresolved"
        }
        .into(),
    );
    let span = matches!(top, None | Some(Placement::OtherView));
    if span && let Some(span) = span_label(thread) {
        parts.push(span);
    }
    let n = thread.comments_total.max(thread.comments.len() as u64);
    parts.push(if n == 1 {
        "1 comment".into()
    } else {
        format!("{n} comments")
    });
    let mut style = if thread.resolved || outdated {
        dim()
    } else {
        Style::new().fg(Color::Yellow)
    };
    if selected {
        style = style.add_modifier(Modifier::BOLD);
    }
    let glyph = if selected { "▶ " } else { "┌ " };
    Line::styled(format!("{glyph}{}", parts.join(" · ")), style)
}

fn bar() -> Span<'static> {
    Span::styled("│ ", dim())
}

/// A context line of an outdated thread, coloured by its `+`/`-`.
fn context_line(text: &str, width: usize) -> Line<'static> {
    let kind = match text.chars().next() {
        Some('+') => LineKind::Added,
        Some('-') => LineKind::Removed,
        Some('@') => LineKind::Hunk,
        _ => LineKind::Context,
    };
    let line = DiffLine {
        kind,
        text: text.to_string(),
    };
    let mut spans = vec![bar()];
    spans.extend(styled_diff_line(&line, width.saturating_sub(2), &[]).spans);
    Line::from(spans)
}

/// Every row of one thread at `width`, stopping at `room` rows; `top` as
/// for [`header`].
fn thread_rows(
    thread: &ReviewThread,
    top: Option<Placement>,
    selected: bool,
    width: usize,
    room: usize,
) -> Vec<Line<'static>> {
    let outdated = top == Some(Placement::Outdated);
    let mut out = vec![header(thread, top, selected)];
    // Folded to its header: the reader shows the rest.
    if thread.resolved {
        return out;
    }
    if outdated {
        out.extend(thread.context.iter().map(|c| context_line(c, width)));
    }
    let inner = width.saturating_sub(2).max(1);
    for comment in thread.comments.iter().take(INLINE_COMMENTS) {
        if out.len() >= room {
            return out;
        }
        out.push(Line::from(vec![
            bar(),
            Span::styled(
                comment.author.clone(),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" · {}", short_time(&comment.created)), dim()),
        ]));
        let body = markdown::render(&comment.body, inner);
        let more = body.len().saturating_sub(INLINE_BODY);
        for line in body.into_iter().take(INLINE_BODY) {
            let mut spans = vec![bar()];
            spans.extend(line.spans);
            out.push(Line::from(spans));
        }
        if more > 0 {
            out.push(Line::from(vec![
                bar(),
                Span::styled(format!("… {more} more lines"), dim()),
            ]));
        }
    }
    let shown = thread.comments.len().min(INLINE_COMMENTS) as u64;
    let total = thread.comments_total.max(thread.comments.len() as u64);
    if total > shown {
        out.push(Line::from(vec![
            bar(),
            Span::styled(format!("… {} more comments", total - shown), dim()),
        ]));
    }
    out.push(Line::styled("└", dim()));
    out
}

/// The files screen's diff with `threads` drawn in, at `inner`'s size.
/// `numbers` are [`kika_forge::anchor::row_numbers`] of the shown diff;
/// `selected` is the thread `]`/`[` went to. An error or a slow load
/// shows as the plain diff pane does.
pub fn thread_diff_body(
    diff: &DiffPane,
    numbers: &[RowNumber],
    threads: &FileThreads,
    scope: &ThreadScope,
    selected: Option<usize>,
    inner: Rect,
) -> Paragraph<'static> {
    if diff.error.is_some() || diff.show_loading {
        return diff_pane_body(diff, inner);
    }
    let (rows, width) = (usize::from(inner.height), usize::from(inner.width));
    let discussion: &PrDiscussion = scope.discussion;
    let mut out: Vec<Line<'static>> = Vec::new();
    let start = diff.scroll.min(diff.lines.len());
    // The block above the diff belongs to its top.
    if start == 0 {
        for &t in &threads.top {
            if out.len() >= rows {
                break;
            }
            let (Some(thread), Some(placement)) = (discussion.threads.get(t), scope.placement(t))
            else {
                continue;
            };
            let room = rows - out.len();
            out.extend(thread_rows(
                thread,
                Some(placement),
                selected == Some(t),
                width,
                room,
            ));
        }
    }
    if diff.done && diff.lines.is_empty() && out.len() < rows {
        out.push(Line::raw("No diff"));
    }
    let found = first_rows(numbers, threads.lines.iter().map(|(id, _)| *id));
    let mut under: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (id, t) in &threads.lines {
        if let Some(&row) = found.get(id) {
            under.entry(row).or_default().push(*t);
        }
    }
    for (at, line) in diff.lines.iter().enumerate().skip(start) {
        if out.len() >= rows {
            break;
        }
        out.push(styled_diff_line(line, width, &marks_of(&diff.search, at)));
        for &t in under.get(&at).into_iter().flatten() {
            if out.len() >= rows {
                break;
            }
            let Some(thread) = discussion.threads.get(t) else {
                continue;
            };
            let room = rows - out.len();
            out.extend(thread_rows(thread, None, selected == Some(t), width, room));
        }
    }
    out.truncate(rows);
    Paragraph::new(
        out.into_iter()
            .map(|line| clip_line(line, width))
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::features::review::files::FilesFocus;
    use crate::features::review::threads::tests::{
        cache_diff, discussion_from, files_with, fixture_discussion,
    };
    use crate::model::AppState;
    use crate::view::buffer_rows;
    use kika_forge::discussion::fixtures::hostile_discussion;

    fn draw(state: &AppState, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::view::render(state, frame))
            .unwrap();
        buffer_rows(terminal.backend().buffer())
    }

    /// The cache.rs diff streamed in whole on the files screen.
    fn cache_shown(discussion: PrDiscussion) -> AppState {
        let mut state = files_with(&["src/cache.rs", "src/lib.rs"], discussion);
        state.size = (100, 30);
        load(&mut state, cache_diff());
        state
    }

    fn load(state: &mut AppState, lines: Vec<DiffLine>) {
        let tag = crate::effect::Tag {
            generation: state.generation,
            seq: state.diff.seq.unwrap(),
            user: false,
        };
        state.update(crate::test_support::diff_chunk(
            tag,
            kika_git::diff::DiffChunk { lines, done: true },
        ));
    }

    /// The row index holding `text`, failing with the screen.
    fn row_with(rows: &[String], text: &str) -> usize {
        rows.iter()
            .position(|r| r.contains(text))
            .unwrap_or_else(|| panic!("{text:?} not in {rows:#?}"))
    }

    #[test]
    fn short_time_reads_rfc3339_and_cuts_anything_else() {
        for (created, want) in [
            ("2023-11-14T21:00:00Z", "2023-11-14 21:00"),
            ("2023-11-14t21:05:59+02:00", "2023-11-14 21:05"),
            ("2023-11-14 08:30", "2023-11-14 08:30"),
            ("yesterday", "yesterday"),
            ("2023-11-14", "2023-11-14"),
            ("2023-1x-14T21:00:00Z", "2023-1x-14T21:00:00Z"),
            ("an hour or so ago, give or take", "an hour or so ago, g"),
            ("", ""),
            (
                "日本語日本語日本語日本語日本語日本語日本語",
                "日本語日本語日本語日本語日本語日本語日本",
            ),
        ] {
            assert_eq!(short_time(created), want, "{created:?}");
        }
    }

    #[test]
    fn line_threads_draw_under_their_rows_on_the_right_side() {
        let state = cache_shown(fixture_discussion());
        let rows = draw(&state, 100, 30);
        // RIGHT new line 3 is the context row ` let cache = 1;`.
        let line3 = row_with(&rows, " let cache = 1;");
        let right = row_with(
            &rows,
            "┌ alice · 2023-11-14 21:00 · unresolved · line 3 · 2 comments",
        );
        assert_eq!(right, line3 + 1, "{rows:#?}");
        // LEFT old line 2 is the removed row.
        let removed = row_with(&rows, "-fn old() {}");
        let left = row_with(
            &rows,
            "┌ carol · 2023-11-14 21:05 · resolved · line 2 · 1 comment",
        );
        assert_eq!(left, removed + 1, "{rows:#?}");
        // Resolved: its header only, then the next diff row.
        assert!(rows[left + 1].contains("+fn new() {}"), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("Why remove this?")));
        // Unresolved: comments with bodies, the long one cut at six rows.
        assert!(rows[right + 1].contains("│ alice · 2023-11-14 21:00"));
        assert!(
            rows[right + 2].contains("│ Please cache this."),
            "{rows:#?}"
        );
        let more = row_with(&rows, "… ");
        assert_eq!(more, right + 2 + INLINE_BODY, "{rows:#?}");
        assert!(rows[more].contains("more lines"), "{rows:#?}");
        let bob = row_with(&rows, "│ bob · 2023-11-14 21:30");
        assert!(rows[bob + 1].contains("│ Agreed."), "{rows:#?}");
        assert!(rows[bob + 2].contains('└'), "{rows:#?}");
        assert!(rows[bob + 3].contains(" fn tail() {}"), "{rows:#?}");
    }

    #[test]
    fn the_cut_says_how_many_lines_are_hidden() {
        let discussion = fixture_discussion();
        let body = &discussion.threads[0].comments[0].body;
        let rendered = markdown::render(body, 60).len();
        assert!(rendered > INLINE_BODY, "{rendered}");
        let state = cache_shown(discussion);
        let rows = draw(&state, 100, 30);
        let want = format!("│ … {} more lines", rendered - INLINE_BODY);
        assert!(
            rows.iter().any(|r| r.contains(&want)),
            "{want:?} in {rows:#?}"
        );
    }

    #[test]
    fn an_outdated_thread_draws_on_top_with_the_lines_it_was_written_on() {
        let mut state = files_with(&["src/cache.rs", "src/lib.rs"], fixture_discussion());
        state.size = (100, 30);
        type_char_j(&mut state);
        let mut classifier = kika_git::diff::Classifier::new(kika_git::diff::EmojiZwj::Escape);
        let lines = [
            "diff --git a/src/lib.rs b/src/lib.rs",
            "--- a/src/lib.rs",
            "+++ b/src/lib.rs",
            "@@ -1,7 +1,7 @@",
            " a",
            " b",
            " c",
            " d",
            "+e",
            " f",
            " g",
        ]
        .iter()
        .map(|t| classifier.classify(t.as_bytes()))
        .collect();
        load(&mut state, lines);
        let rows = draw(&state, 100, 30);
        let top = row_with(
            &rows,
            "outdated · was line 7 (new) · erin · 2023-11-13 10:00",
        );
        let diff_header = row_with(&rows, "diff --git a/src/lib.rs");
        assert!(top < diff_header, "{rows:#?}");
        for (i, context) in [" context5", "-old6", "+new6", "+new7"].iter().enumerate() {
            assert!(
                rows[top + 1 + i].contains(&format!("│ {context}")),
                "{rows:#?}"
            );
        }
        assert!(rows[top + 5].contains("│ erin"), "{rows:#?}");
        // Only on top: no row of the diff carries it.
        assert_eq!(rows.iter().filter(|r| r.contains("erin ·")).count(), 2);
        // The multi-line one is resolved and on its lines.
        let multi = row_with(&rows, "dave · 2023-11-14 21:10 · resolved · lines 3–5");
        assert!(rows[multi - 1].contains("+e"), "{rows:#?}");
        // Scrolled off the top, the block goes with it.
        state.diff.scroll = 1;
        let rows = draw(&state, 100, 30);
        assert!(!rows.iter().any(|r| r.contains("outdated")), "{rows:#?}");
    }

    fn type_char_j(state: &mut AppState) {
        crate::features::review::state::tests::type_char(state, 'j');
    }

    #[test]
    fn in_a_commit_view_threads_show_only_on_top_for_the_whole_pull_request() {
        let mut state = cache_shown(fixture_discussion());
        state.review.files.as_mut().unwrap().view =
            crate::features::review::files::ReviewView::Commit(0);
        let rows = draw(&state, 100, 30);
        let first = row_with(&rows, "on the whole pull request (c) · alice");
        // Its lines are real ones of the whole pull request: they show.
        let wide = draw(&state, 160, 30);
        let header = row_with(&wide, "on the whole pull request (c) · alice");
        assert!(wide[header].contains("unresolved · line 3"), "{wide:#?}");
        let second = row_with(&rows, "on the whole pull request (c) · carol");
        let header = row_with(&rows, "diff --git a/src/cache.rs");
        assert!(first < second && second < header, "{rows:#?}");
        let line3 = row_with(&rows, " let cache = 1;");
        assert!(!rows[line3 + 1].contains('┌'), "{rows:#?}");
    }

    #[test]
    fn a_line_missing_from_the_finished_diff_says_so_on_top() {
        let mut discussion = fixture_discussion();
        discussion.threads[0].line = Some(90);
        let state = cache_shown(discussion);
        let rows = draw(&state, 100, 30);
        let top = row_with(&rows, "line 90 · not in this diff · alice");
        assert!(top < row_with(&rows, "diff --git"), "{rows:#?}");
        // Said once, not again as the thread's lines.
        assert_eq!(rows[top].matches("line 90").count(), 1, "{rows:#?}");
    }

    #[test]
    fn the_selected_thread_is_marked_and_bold() {
        let mut state = cache_shown(fixture_discussion());
        state.review.files.as_mut().unwrap().thread = Some(0);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| crate::view::render(&state, frame))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rows = buffer_rows(&buffer);
        let y = row_with(&rows, "▶ alice · 2023-11-14 21:00 · unresolved");
        let x = rows[y][..rows[y].find('▶').unwrap()].chars().count();
        let style = buffer[(x as u16, y as u16)].style();
        assert!(style.add_modifier.contains(Modifier::BOLD), "{style:?}");
        assert_eq!(style.fg, Some(Color::Yellow));
        let resolved = row_with(&rows, "┌ carol");
        let x = rows[resolved][..rows[resolved].find('┌').unwrap()]
            .chars()
            .count();
        let style = buffer[(x as u16, resolved as u16)].style();
        assert!(style.add_modifier.contains(Modifier::DIM), "{style:?}");
    }

    #[test]
    fn hostile_text_draws_escaped_and_no_size_overruns_the_pane() {
        let mut state = cache_shown(discussion_from(&hostile_discussion()));
        state.review.files.as_mut().unwrap().focus = FilesFocus::Diff;
        let rows = draw(&state, 100, 30);
        assert!(rows.iter().any(|r| r.contains("<U+202E>evil")), "{rows:#?}");
        for row in &rows {
            assert!(!row.chars().any(|c| c.is_control()), "{row:?}");
        }
        for (w, h) in [(80, 24), (40, 10), (20, 6), (100, 30)] {
            state.size = (w, h);
            let rows = draw(&state, w, h);
            assert_eq!(rows.len(), usize::from(h));
            for row in &rows {
                assert!(crate::view::width_of(row) <= usize::from(w), "{row:?}");
                assert!(!row.chars().any(|c| c.is_control()), "{row:?}");
            }
            // The diff pane's right border is the last column of each
            // row inside it: nothing writes past it.
            let inner: Vec<&String> = rows[2..rows.len() - 2].iter().collect();
            for row in inner {
                let last = row.chars().last().unwrap_or(' ');
                assert!(matches!(last, '│' | '┃' | ' '), "{w}x{h}: {row:?}");
            }
        }
    }

    #[test]
    fn rows_are_cut_with_an_ellipsis() {
        let line = Line::from(vec![Span::raw("│ "), Span::raw("abcdefghij")]);
        let cut = clip_line(line, 6);
        let text: String = cut.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "│ abc…");
        assert_eq!(cut.width(), 6);
        let exact = clip_line(Line::from(vec![Span::raw("ab"), Span::raw("cd")]), 2);
        let text: String = exact.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "a…");
        let wide = clip_line(Line::raw("日本語"), 4);
        assert!(wide.width() <= 4, "{wide:?}");
        let short = clip_line(Line::raw("short"), 10);
        assert_eq!(short, Line::raw("short"));
    }

    #[test]
    fn the_detail_counts_the_threads_or_says_it_waits_or_failed() {
        use crate::features::review::state::ReviewProblem;
        let mut review = ReviewState::default();
        assert_eq!(thread_summary(&review), None);
        review.discussion_fetch.loading = true;
        assert_eq!(
            thread_summary(&review).as_deref(),
            Some("Threads: loading…")
        );
        review.discussion_fetch.loading = false;
        review.discussion_fetch.error =
            Some(ReviewProblem::Status(kika_forge::ForgeStatus::Offline));
        assert_eq!(
            thread_summary(&review).as_deref(),
            Some("Threads: could not be read (GitHub could not be reached)")
        );
        review.discussion_fetch.error = None;
        review.discussion = Some(fixture_discussion());
        assert_eq!(
            thread_summary(&review).as_deref(),
            Some("Threads: 2 unresolved of 5 · 3 comments")
        );
    }
}
