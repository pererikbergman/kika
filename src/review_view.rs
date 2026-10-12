//! Review mode on screen: the queue tabs and rows, or one pull request's
//! header, checks and description. Reads state only; every string in it
//! was made terminal-safe when GitHub's answer was parsed.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::branches_view::clock;
use crate::forge::review::{
    CheckStatus, Mergeability, PrDetail, PrSummary, QueueKind, ReviewVerdict,
};
use crate::forge::{ForgeStatus, PrState};
use crate::markdown;
use crate::model::AppState;
use crate::model::review::{NO_FORGE, ReviewProblem, ReviewScreen, ReviewState};
use crate::view::elide_right;

pub fn render(state: &AppState, frame: &mut Frame, area: Rect) {
    let review = &state.review;
    let repo = review
        .repo
        .as_ref()
        .map(|r| format!(" · {}/{}", r.owner, r.name))
        .unwrap_or_default();
    let number = match review.screen {
        ReviewScreen::Detail(number) => format!(" #{number}"),
        ReviewScreen::Queue => String::new(),
    };
    let title = format!(" Review{repo}{number} ");
    let block = Block::bordered().title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines = match review.screen {
        ReviewScreen::Queue => queue_lines(review, inner),
        ReviewScreen::Detail(number) => detail_screen(review, number, inner),
    };
    frame.render_widget(Paragraph::new(lines), inner);
}

/// What a problem says, and what to do about it when there is a next step.
pub fn problem_text(problem: ReviewProblem) -> (String, Option<&'static str>) {
    match problem {
        ReviewProblem::NoForgeHost => (
            NO_FORGE.to_string(),
            Some("Add its host to [forge] github."),
        ),
        ReviewProblem::Limited(until) => (
            format!("GitHub is rate limited until {} UTC", clock(until)),
            None,
        ),
        ReviewProblem::Missing(number) => (format!("No pull request #{number}"), None),
        ReviewProblem::Status(status) => {
            let (what, next) = match status {
                ForgeStatus::NoGh => (
                    "The gh command is not installed",
                    Some("Install the GitHub CLI."),
                ),
                ForgeStatus::NotLoggedIn => (
                    "gh is not logged in to this host",
                    Some("Run gh auth login."),
                ),
                ForgeStatus::Offline => (
                    "GitHub could not be reached",
                    Some("Check the network, then press r."),
                ),
                ForgeStatus::NoAccess => (
                    "No access to the repository with gh's login",
                    Some("Check gh auth status."),
                ),
                ForgeStatus::RateLimited => ("GitHub is rate limiting this account", None),
                ForgeStatus::Failed => (
                    "GitHub gave no usable answer",
                    Some("Press r to try again."),
                ),
            };
            (what.to_string(), next)
        }
    }
}

/// The problem in red and its next step below it, each wrapped to
/// `width` so neither is cut off.
fn problem_lines(problem: ReviewProblem, width: usize) -> Vec<Line<'static>> {
    let (what, next) = problem_text(problem);
    let mut lines = markdown::wrap_text(&what, Style::new().fg(Color::Red), width);
    if let Some(next) = next {
        lines.extend(markdown::wrap_text(next, Style::new(), width));
    }
    lines
}

/// The tab line, then the rows the filter keeps, scrolled to the cursor.
fn queue_lines(review: &ReviewState, inner: Rect) -> Vec<Line<'static>> {
    let mut tabs: Vec<Span<'static>> = Vec::new();
    for (i, kind) in QueueKind::ALL.into_iter().enumerate() {
        if i > 0 {
            tabs.push(Span::styled(
                " │ ",
                Style::new().add_modifier(Modifier::DIM),
            ));
        }
        let count = review
            .queues
            .as_ref()
            .map(|q| format!(" ({})", q.get(kind).len()))
            .unwrap_or_default();
        let style = if kind == review.queue {
            Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::new()
        };
        tabs.push(Span::styled(format!("{}{count}", kind.title()), style));
    }
    if !review.filter.is_empty() {
        tabs.push(Span::styled(
            format!("  /{}", review.filter),
            Style::new().add_modifier(Modifier::DIM),
        ));
    }
    let mut lines = vec![Line::from(tabs)];
    if let Some(problem) = review.queues_fetch.error {
        lines.extend(problem_lines(problem, usize::from(inner.width)));
        return lines;
    }
    if review.queues_fetch.loading && review.queues.is_none() {
        lines.push(Line::raw("Loading pull requests…"));
        return lines;
    }
    let rows = review.rows();
    let visible = review.visible();
    if visible.is_empty() {
        lines.push(Line::raw(if rows.is_empty() {
            "No pull requests in this queue"
        } else {
            "No pull request matches the filter"
        }));
        return lines;
    }
    let room = usize::from(inner.height).saturating_sub(1).max(1);
    let at = visible
        .iter()
        .position(|&i| i == review.cursor)
        .unwrap_or(0);
    let skip = at.saturating_sub(room - 1);
    let width = usize::from(inner.width);
    let digits = visible
        .iter()
        .map(|&i| rows[i].number.to_string().len())
        .max()
        .unwrap_or(1);
    for &i in visible.iter().skip(skip).take(room) {
        lines.push(queue_row(&rows[i], i == review.cursor, digits, width));
    }
    lines
}

/// The longest author a queue row shows, in columns.
const AUTHOR_COLS: usize = 20;

/// One queue row, exactly `width` wide: a cursor mark, the number, the
/// title cut with `…` to leave room for the author and the draft mark,
/// then those. The cursor row is a full-width bar.
fn queue_row(pr: &PrSummary, cursor: bool, digits: usize, width: usize) -> Line<'static> {
    let style = if cursor {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new()
    };
    let mark = if cursor { "▸ " } else { "  " };
    let number = format!("#{:<digits$}  ", pr.number);
    let author = format!("  {}", elide_right(&pr.author, AUTHOR_COLS));
    let draft = if pr.draft { "  draft" } else { "" };
    let fixed = width_of(mark) + width_of(&number) + width_of(&author) + width_of(draft);
    let room = width.saturating_sub(fixed);
    let title = elide_right(&pr.title, room);
    let title = format!(
        "{title}{}",
        " ".repeat(room.saturating_sub(width_of(&title)))
    );
    let mut spans = vec![
        Span::styled(mark, style.add_modifier(Modifier::BOLD)),
        Span::styled(number, style),
        Span::styled(title, style),
        Span::styled(author, style.add_modifier(Modifier::BOLD)),
        Span::styled(draft, style.add_modifier(Modifier::DIM)),
    ];
    let used: usize = spans.iter().map(Span::width).sum();
    if used < width {
        spans.push(Span::styled(" ".repeat(width - used), style));
    }
    Line::from(spans)
}

fn width_of(text: &str) -> usize {
    Span::raw(text).width()
}

/// The detail screen: loading, a problem, or the pull request scrolled.
fn detail_screen(review: &ReviewState, number: u64, inner: Rect) -> Vec<Line<'static>> {
    if let Some(problem) = review.detail_fetch.error {
        return problem_lines(problem, usize::from(inner.width));
    }
    match &review.detail {
        Some(detail) => {
            let width = usize::from(inner.width);
            let fresh;
            let body = match &review.body {
                Some(body) if body.width == width => &body.lines,
                // Not rendered for this width yet: only before the first
                // resize, so it is drawn here this once.
                _ => {
                    fresh = markdown::render(&detail.body, width);
                    &fresh
                }
            };
            let lines = detail_lines(detail, body, width);
            let max = lines.len().saturating_sub(usize::from(inner.height));
            lines
                .into_iter()
                .skip(review.detail_scroll.min(max))
                .collect()
        }
        None => vec![Line::raw(format!("Loading pull request #{number}…"))],
    }
}

/// The review panel's inside, in `main`: where the detail is drawn.
pub fn detail_area(main: Rect) -> Rect {
    Block::bordered().inner(main)
}

/// How far the detail scrolls in `inner`, with `body` its rendered
/// description.
pub fn detail_max_scroll(detail: &PrDetail, body: &[Line<'static>], inner: Rect) -> usize {
    detail_lines(detail, body, usize::from(inner.width))
        .len()
        .saturating_sub(usize::from(inner.height))
}

fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

/// Every line of the detail at `width`: the header and checks first, so
/// a small screen still shows them, then labels, reviewers and `body`,
/// the description already rendered.
pub fn detail_lines(detail: &PrDetail, body: &[Line<'static>], width: usize) -> Vec<Line<'static>> {
    let mut lines = markdown::wrap_text(&detail.title, bold(), width);
    let by = format!("#{} by ", detail.number);
    lines.push(Line::from(vec![
        Span::raw(by.clone()),
        Span::styled(
            elide_right(&detail.author, width.saturating_sub(width_of(&by))),
            bold(),
        ),
    ]));
    let state = match (detail.state, detail.draft) {
        (PrState::Merged, _) => ("merged", Color::Magenta),
        (PrState::Closed, _) => ("closed", Color::Red),
        (_, true) => ("draft", Color::DarkGray),
        _ => ("open", Color::Green),
    };
    let (word, color) = match detail.mergeable {
        Mergeability::Mergeable => ("mergeable", Color::Green),
        Mergeability::Conflicting => ("conflicts", Color::Red),
        Mergeability::Unknown => ("unknown", Color::DarkGray),
    };
    lines.push(Line::from(vec![
        Span::styled(state.0, Style::new().fg(state.1)),
        Span::styled(" · ", dim()),
        Span::styled(word, Style::new().fg(color)),
    ]));
    let branches = format!("{} ← {}", detail.base, detail.head);
    lines.extend(markdown::wrap_text(&branches, Style::new(), width));
    if !detail.checks.is_empty() {
        lines.push(Line::styled("Checks", bold()));
        for check in &detail.checks {
            let (glyph, word, color) = check_look(check.status);
            let glyph = format!("{glyph} ");
            let word = format!("  {word}");
            // The status word always shows; the name gives way.
            let room = width.saturating_sub(width_of(&glyph) + width_of(&word));
            lines.push(Line::from(vec![
                Span::styled(glyph, Style::new().fg(color)),
                Span::raw(elide_right(&check.name, room)),
                Span::styled(word, Style::new().fg(color)),
            ]));
        }
    }
    lines.extend(packed("Labels: ", &detail.labels, width));
    let reviewers: Vec<String> = detail
        .reviews
        .iter()
        .map(|(who, verdict)| format!("{who} {}", verdict_word(*verdict)))
        .chain(
            detail
                .requested_reviewers
                .iter()
                .map(|who| format!("{who} requested")),
        )
        .collect();
    lines.extend(packed("Reviewers: ", &reviewers, width));
    if !body.is_empty() {
        lines.push(Line::default());
        lines.extend(body.iter().cloned());
    }
    lines
}

/// `items` after `label`, comma separated, as many to a line as fit in
/// `width`; later lines hang under the first item. Nothing when empty.
fn packed(label: &str, items: &[String], width: usize) -> Vec<Line<'static>> {
    let hang = " ".repeat(label.chars().count());
    let mut lines = Vec::new();
    let mut current = vec![Span::styled(label.to_string(), dim())];
    let mut used = label.chars().count();
    let mut empty = true;
    for (i, item) in items.iter().enumerate() {
        let item = elide_right(item, width.saturating_sub(width_of(label) + 1).max(1));
        let text = if i + 1 < items.len() {
            format!("{item},")
        } else {
            item
        };
        let w = Span::raw(text.as_str()).width();
        let space = usize::from(!empty);
        if !empty && used + space + w > width {
            lines.push(Line::from(std::mem::take(&mut current)));
            current = vec![Span::raw(hang.clone())];
            used = hang.len();
            empty = true;
        }
        if !empty {
            current.push(Span::raw(" "));
        }
        used += usize::from(!empty) + w;
        current.push(Span::raw(text));
        empty = false;
    }
    if !items.is_empty() {
        lines.push(Line::from(current));
    }
    lines
}

fn verdict_word(verdict: ReviewVerdict) -> &'static str {
    match verdict {
        ReviewVerdict::Approved => "approved",
        ReviewVerdict::ChangesRequested => "changes requested",
        ReviewVerdict::Commented => "commented",
        ReviewVerdict::Dismissed => "dismissed",
        ReviewVerdict::Pending => "pending",
    }
}

fn check_look(status: CheckStatus) -> (&'static str, &'static str, Color) {
    match status {
        CheckStatus::Success => ("✓", "passed", Color::Green),
        CheckStatus::Failure => ("✗", "failed", Color::Red),
        CheckStatus::Pending => ("●", "pending", Color::Yellow),
        CheckStatus::Neutral => ("○", "neutral", Color::DarkGray),
        CheckStatus::Skipped => ("–", "skipped", Color::DarkGray),
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::forge::ForgeHost;
    use crate::forge::review::tests::hostile_detail;
    use crate::model::Context;
    use crate::model::review::tests::{detail_from, fixture_detail, fixture_queues, repo};
    use crate::view::buffer_rows;

    fn draw(state: &AppState, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::view::render(state, frame))
            .unwrap();
        buffer_rows(terminal.backend().buffer())
    }

    fn reviewing(screen: ReviewScreen) -> AppState {
        let mut state = AppState::new(Context::Review { number: None });
        state.review.screen = screen;
        state.review.repo = Some(repo());
        state
    }

    fn contains(rows: &[String], text: &str) -> bool {
        rows.iter().any(|r| r.contains(text))
    }

    #[test]
    fn the_detail_shows_its_header_checks_and_description() {
        let mut state = reviewing(ReviewScreen::Detail(42));
        state.review.detail = Some(fixture_detail());
        let rows = draw(&state, 80, 30);
        for text in [
            "Review · acme/widgets",
            "Add widget caching",
            "#42 by octocat",
            "main ← feature/cache",
            "enhancement, needs-review",
            "alice approved",
            "bob changes requested",
            "hubot requested",
            "core requested",
            "mergeable",
            "✗ build  failed",
            "✓ ci/lint  passed",
            "● test  pending",
            "Summary",
        ] {
            assert!(contains(&rows, text), "{text:?} missing in {rows:#?}");
        }
        assert!(!contains(&rows, "## Summary"), "{rows:#?}");
    }

    #[test]
    fn hostile_server_text_draws_escaped() {
        let mut state = reviewing(ReviewScreen::Detail(42));
        state.review.detail = Some(detail_from(&hostile_detail("javascript:alert(1)")));
        let rows = draw(&state, 120, 30);
        assert!(contains(&rows, "x�[31mred�]52;c;eA==�"), "{rows:#?}");
        assert!(contains(&rows, "<U+202E>evil"), "{rows:#?}");
        for row in &rows {
            assert!(!row.chars().any(|c| c.is_control()), "{row:?}");
        }
    }

    #[test]
    fn the_queue_shows_tabs_with_counts_and_its_rows() {
        let mut state = reviewing(ReviewScreen::Queue);
        state.review.queues = Some(fixture_queues());
        state.review.queue = QueueKind::AllOpen;
        let rows = draw(&state, 80, 12);
        assert!(
            contains(
                &rows,
                "Assigned (2) │ Review requested (2) │ Authored (0) │ All open (4)"
            ),
            "{rows:#?}"
        );
        // The cursor row is a full-width bar with a mark; the author and
        // draft columns end every row at the right border.
        let row = |n: &str| {
            rows.iter()
                .find(|r| r.contains(n))
                .unwrap_or_else(|| panic!("{n} in {rows:#?}"))
                .clone()
        };
        assert!(
            row("#15").starts_with("│▸ #15  Draft: new gadget API "),
            "{rows:#?}"
        );
        assert!(row("#15").ends_with("  alice  draft│"), "{rows:#?}");
        assert!(
            row("#12").starts_with("│  #12  Cache widget lookups "),
            "{rows:#?}"
        );
        assert!(row("#12").ends_with("  octocat│"), "{rows:#?}");
        assert!(row("#9 ").starts_with("│  #9   Bump serde "), "{rows:#?}");

        state.review.queue = QueueKind::Authored;
        let rows = draw(&state, 80, 12);
        assert!(
            contains(&rows, "No pull requests in this queue"),
            "{rows:#?}"
        );
    }

    #[test]
    fn with_no_forge_host_the_view_says_so_and_a_limit_says_until_when() {
        let mut state = AppState::new(Context::Workspace);
        state.update(crate::intent::AppIntent::EnterReview);
        let rows = draw(&state, 80, 10);
        assert!(contains(&rows, NO_FORGE), "{rows:#?}");

        let mut state = AppState::new(Context::Review { number: Some(42) })
            .with_forge_hosts(vec![ForgeHost::parse("github.com").unwrap()]);
        state.forge.repo = Some(repo());
        state
            .forge
            .limited
            .insert("github.com".into(), 1_700_003_600);
        state.now = 1_700_000_000;
        let effects = state.update(crate::intent::AppIntent::Start);
        assert_eq!(effects, []);
        let rows = draw(&state, 80, 10);
        assert!(
            contains(&rows, "GitHub is rate limited until 23:13 UTC"),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_missing_pull_request_and_loading_are_one_line() {
        let mut state = reviewing(ReviewScreen::Detail(999));
        let rows = draw(&state, 80, 8);
        assert!(contains(&rows, "Loading pull request #999…"), "{rows:#?}");
        state.review.detail_fetch.error = Some(ReviewProblem::Missing(999));
        let rows = draw(&state, 80, 8);
        assert!(contains(&rows, "No pull request #999"), "{rows:#?}");
    }

    #[test]
    fn a_long_wide_title_is_cut_so_the_author_and_draft_mark_show() {
        let mut state = reviewing(ReviewScreen::Queue);
        let mut queues = fixture_queues();
        state.review.queue = QueueKind::AllOpen;
        let long = "日本語のとても長いタイトルが続きます".repeat(4);
        queues = crate::forge::review::tests::with_title(queues, 15, &long);
        state.review.queues = Some(queues);
        let rows = draw(&state, 40, 10);
        let row = rows.iter().find(|r| r.contains("#15")).unwrap();
        assert!(row.contains('…'), "{rows:#?}");
        assert!(row.ends_with("  alice  draft│"), "{rows:#?}");
        for row in &rows {
            assert!(crate::view::width_of(row) <= 40, "{row:?}");
        }
    }

    #[test]
    fn tiny_and_narrow_screens_keep_the_check_statuses() {
        let mut queue = reviewing(ReviewScreen::Queue);
        queue.review.queues = Some(fixture_queues());
        let mut detail = reviewing(ReviewScreen::Detail(42));
        detail.review.detail = Some(fixture_detail());
        for state in [&queue, &detail] {
            for (w, h) in [(0, 0), (1, 1), (10, 3), (20, 6), (30, 12)] {
                let rows = draw(state, w, h);
                for row in &rows {
                    assert!(crate::view::width_of(row) <= usize::from(w), "{row:?}");
                }
            }
        }
        let rows = draw(&detail, 30, 12);
        for status in ["✗ build  failed", "✓ ci/lint  passed", "● test  pending"] {
            assert!(contains(&rows, status), "{status} in {rows:#?}");
        }
        // A long check name gives way to its status.
        let mut long = fixture_detail();
        long.checks[0].name = "a-very-long-check-name-that-runs-on".into();
        detail.review.detail = Some(long);
        let rows = draw(&detail, 30, 12);
        assert!(contains(&rows, "…  failed"), "{rows:#?}");
    }

    #[test]
    fn a_long_title_and_branches_wrap() {
        let mut detail = fixture_detail();
        detail.title = "A title that is far too long for a narrow screen".into();
        detail.head = "feature/a-branch-name-that-goes-on".into();
        let lines = detail_lines(&detail, &[], 28);
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(text[0], "A title that is far too long");
        assert_eq!(text[1], "for a narrow screen");
        assert!(text.iter().any(|t| t == "main ←"), "{text:#?}");
        for line in &lines {
            assert!(line.width() <= 28, "{line:?}");
        }
    }

    #[test]
    fn a_problem_and_its_next_step_wrap_to_a_narrow_panel() {
        let mut state = AppState::new(Context::Workspace);
        state.update(crate::intent::AppIntent::EnterReview);
        let rows = draw(&state, 40, 10);
        assert!(contains(&rows, "[forge] github"), "{rows:#?}");
        assert!(
            contains(&rows, "The remote is on no forge host in the"),
            "{rows:#?}"
        );
        assert!(contains(&rows, "│global config "), "{rows:#?}");
        let problems = [
            ReviewProblem::NoForgeHost,
            ReviewProblem::Limited(1_700_003_600),
            ReviewProblem::Missing(7),
            ReviewProblem::Status(ForgeStatus::NoGh),
            ReviewProblem::Status(ForgeStatus::NotLoggedIn),
            ReviewProblem::Status(ForgeStatus::Offline),
            ReviewProblem::Status(ForgeStatus::NoAccess),
            ReviewProblem::Status(ForgeStatus::RateLimited),
            ReviewProblem::Status(ForgeStatus::Failed),
        ];
        for problem in problems {
            let (what, next) = problem_text(problem);
            for width in [20, 40, 60] {
                let lines = problem_lines(problem, width);
                let text: String = lines
                    .iter()
                    .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
                    .collect::<Vec<_>>()
                    .join(" ");
                let words: Vec<&str> = text.split_whitespace().collect();
                let want: Vec<&str> = what
                    .split_whitespace()
                    .chain(next.into_iter().flat_map(str::split_whitespace))
                    .collect();
                assert_eq!(words, want, "{problem:?} at {width}");
                for line in &lines {
                    assert!(line.width() <= width, "{problem:?} at {width}: {line:?}");
                }
            }
        }
        // In the detail too.
        let mut state = reviewing(ReviewScreen::Detail(42));
        state.review.detail_fetch.error = Some(ReviewProblem::Status(ForgeStatus::Offline));
        let rows = draw(&state, 40, 10);
        assert!(
            contains(&rows, "Check the network, then press r."),
            "{rows:#?}"
        );
    }
}
