//! Review mode on screen: the queue tabs and rows, or one pull request's
//! header, checks and description. Reads state only; every string in it
//! was made terminal-safe when GitHub's answer was parsed.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::branches_view::clock;
use crate::forge::review::{CheckStatus, Mergeability, PrDetail, QueueKind, ReviewVerdict};
use crate::forge::{ForgeStatus, PrState};
use crate::markdown;
use crate::model::AppState;
use crate::model::review::{NO_FORGE, ReviewProblem, ReviewScreen, ReviewState};

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

/// What a problem says, in one line.
pub fn problem_text(problem: ReviewProblem) -> String {
    match problem {
        ReviewProblem::NoForgeHost => NO_FORGE.to_string(),
        ReviewProblem::Limited(until) => {
            format!("GitHub is rate limited until {} UTC", clock(until))
        }
        ReviewProblem::Missing(number) => format!("No pull request #{number}"),
        ReviewProblem::Status(status) => match status {
            ForgeStatus::NoGh => "The gh command is not installed".into(),
            ForgeStatus::NotLoggedIn => {
                "gh is not logged in to this host; run gh auth login".into()
            }
            ForgeStatus::Offline => "GitHub could not be reached".into(),
            ForgeStatus::NoAccess => "No access to the repository with gh's login".into(),
            ForgeStatus::RateLimited => "GitHub is rate limiting this account".into(),
            ForgeStatus::Failed => "GitHub gave no usable answer".into(),
        },
    }
}

fn problem_line(problem: ReviewProblem) -> Line<'static> {
    Line::styled(problem_text(problem), Style::new().fg(Color::Red))
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
        lines.push(problem_line(problem));
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
    let digits = visible
        .iter()
        .map(|&i| rows[i].number.to_string().len())
        .max()
        .unwrap_or(1);
    for &i in visible.iter().skip(skip).take(room) {
        let pr = &rows[i];
        let style = if i == review.cursor {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new()
        };
        let mut spans = vec![
            Span::styled(format!("#{:<digits$}  ", pr.number), style),
            Span::styled(pr.title.clone(), style),
            Span::styled(format!("  {}", pr.author), style.fg(Color::Cyan)),
        ];
        if pr.draft {
            spans.push(Span::styled("  draft", style.add_modifier(Modifier::DIM)));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// The detail screen: loading, a problem, or the pull request scrolled.
fn detail_screen(review: &ReviewState, number: u64, inner: Rect) -> Vec<Line<'static>> {
    if let Some(problem) = review.detail_fetch.error {
        return vec![problem_line(problem)];
    }
    match &review.detail {
        Some(detail) => {
            let lines = detail_lines(detail, usize::from(inner.width));
            let max = lines.len().saturating_sub(usize::from(inner.height));
            lines
                .into_iter()
                .skip(review.detail_scroll.min(max))
                .collect()
        }
        None => vec![Line::raw(format!("Loading pull request #{number}…"))],
    }
}

/// How far the detail scrolls in `area`, the review panel with its border.
pub fn detail_max_scroll(detail: &PrDetail, area: Rect) -> usize {
    let inner = Block::bordered().inner(area);
    detail_lines(detail, usize::from(inner.width))
        .len()
        .saturating_sub(usize::from(inner.height))
}

fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

/// Every line of the detail at `width`: the header, the checks, then
/// the description.
pub fn detail_lines(detail: &PrDetail, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::styled(detail.title.clone(), bold()),
        Line::from(vec![
            Span::raw(format!("#{} by ", detail.number)),
            Span::styled(detail.author.clone(), Style::new().fg(Color::Cyan)),
        ]),
    ];
    let state = match (detail.state, detail.draft) {
        (PrState::Merged, _) => ("merged", Color::Magenta),
        (PrState::Closed, _) => ("closed", Color::Red),
        (_, true) => ("draft", Color::DarkGray),
        _ => ("open", Color::Green),
    };
    lines.push(Line::styled(state.0, Style::new().fg(state.1)));
    lines.push(Line::raw(format!("{} ← {}", detail.base, detail.head)));
    let (word, color) = match detail.mergeable {
        Mergeability::Mergeable => ("mergeable", Color::Green),
        Mergeability::Conflicting => ("conflicts", Color::Red),
        Mergeability::Unknown => ("unknown", Color::DarkGray),
    };
    lines.push(Line::from(vec![
        Span::styled("Merge: ", dim()),
        Span::styled(word, Style::new().fg(color)),
    ]));
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
    if !detail.checks.is_empty() {
        lines.push(Line::default());
        lines.push(Line::styled("Checks", bold()));
        for check in &detail.checks {
            let (glyph, word, color) = check_look(check.status);
            lines.push(Line::from(vec![
                Span::styled(format!("{glyph} "), Style::new().fg(color)),
                Span::raw(check.name.clone()),
                Span::styled(format!("  {word}"), Style::new().fg(color)),
            ]));
        }
    }
    let body = markdown::render(&detail.body, width);
    if !body.is_empty() {
        lines.push(Line::default());
        lines.extend(body);
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
        let text = if i + 1 < items.len() {
            format!("{item},")
        } else {
            item.clone()
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
        assert!(
            contains(&rows, "#15  Draft: new gadget API  alice  draft"),
            "{rows:#?}"
        );
        assert!(
            contains(&rows, "#12  Cache widget lookups  octocat"),
            "{rows:#?}"
        );
        assert!(contains(&rows, "#9   Bump serde  dependabot"), "{rows:#?}");

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
}
