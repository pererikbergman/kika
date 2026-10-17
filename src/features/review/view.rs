//! Review mode on screen: the queue tabs and rows, or one pull request's
//! header, checks and description. Reads state only; every string in it
//! was made terminal-safe when the forge's answer was parsed. Its fixed
//! text names requests in the forge's own [`Words`]: `#N` pull requests
//! on GitHub, `!N` merge requests on GitLab.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use super::files::{FilesFocus, FilesStage, PrFile, PrFiles, ReviewView};
use super::git::FileCounts;
use super::markdown;
use super::pending_view::{comment_marks, has_comment_marks, no_discussion};
use super::state::{NO_FORGE, ReviewProblem, ReviewScreen, ReviewState};
use super::threads::{ThreadScope, thread_note};
use super::threads_view::thread_diff_body;
use crate::diff_pane_view::{diff_pane_body, diff_position, found_note};
use crate::model::AppState;
use crate::rows::{clock, drawn_path, status_colour};
use crate::theme::Palette;
use crate::view::{self, elide_right};
use kika_forge::anchor::row_numbers;
use kika_forge::forge::Words;
use kika_forge::merge::{MergeBlock, merge_options};
use kika_forge::review::{
    CheckStatus, Mergeability, PrDetail, PrSummary, QueueKind, ReviewVerdict, ViewedState,
};
use kika_forge::{ForgeStatus, PrState};

pub fn render(state: &AppState, frame: &mut Frame, area: Rect) {
    let review = &state.review;
    let words = state.review_words();
    let repo = review
        .repo
        .as_ref()
        .map(|r| format!(" · {}/{}", r.owner, r.name))
        .unwrap_or_default();
    let number = match review.screen {
        ReviewScreen::Detail(number)
        | ReviewScreen::Files(number)
        | ReviewScreen::Conversation(number) => format!(" {}{number}", words.sign),
        ReviewScreen::Queue => String::new(),
    };
    let title = format!(" Review{repo}{number} ");
    let block = Block::bordered().title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines = match review.screen {
        ReviewScreen::Queue => queue_lines(&state.palette, review, words, inner),
        ReviewScreen::Detail(number) => detail_screen(&state.palette, review, words, number, inner),
        ReviewScreen::Files(_) => match &review.files {
            Some(files) => return files_screen(state, files, frame, inner),
            None => Vec::new(),
        },
        ReviewScreen::Conversation(_) => {
            let lines = super::conversation_view::conversation_view(review, inner);
            frame.render_widget(Paragraph::new(lines), inner);
            // A general comment is typed over it.
            super::pending_view::render_pending(&state.palette, review, words, frame, inner);
            return;
        }
    };
    frame.render_widget(Paragraph::new(lines), inner);
}

/// What a problem says, and what to do about it when there is a next step.
pub fn problem_text(problem: ReviewProblem, words: &Words) -> (String, Option<String>) {
    let Words { name, cli, .. } = words;
    match problem {
        ReviewProblem::NoForgeHost => (
            NO_FORGE.to_string(),
            Some("Add its host to [forge] github, or its owner to [forge.allow].".into()),
        ),
        ReviewProblem::Limited(until) => (
            format!("{name} is rate limited until {} UTC", clock(until)),
            None,
        ),
        ReviewProblem::Missing(number) => {
            (format!("No {} {}{number}", words.request, words.sign), None)
        }
        ReviewProblem::Unsupported(unsupported) => (unsupported.message(), None),
        ReviewProblem::Status(status) => {
            let (what, next) = match status {
                // No forge kind is at hand here, so the words name none.
                ForgeStatus::Cli(cli) => {
                    let what = kika_forge::cli::problem_text(cli);
                    return (what.trim_end_matches('.').to_string(), None);
                }
                ForgeStatus::NotLoggedIn => (
                    format!("{cli} is not logged in to this host"),
                    Some(format!("Run {cli} auth login.")),
                ),
                ForgeStatus::Offline => (
                    format!("{name} could not be reached"),
                    Some("Check the network, then press r.".into()),
                ),
                ForgeStatus::NoAccess => (
                    format!("No access to the repository with {cli}'s login"),
                    Some(format!("Check {cli} auth status.")),
                ),
                ForgeStatus::RateLimited => (format!("{name} is rate limiting this account"), None),
                ForgeStatus::Failed => (
                    format!("{name} gave no usable answer"),
                    Some("Press r to try again.".into()),
                ),
            };
            (what, next)
        }
    }
}

/// The problem in red and its next step below it, each wrapped to
/// `width` so neither is cut off.
fn problem_lines(
    palette: &Palette,
    problem: ReviewProblem,
    words: &Words,
    width: usize,
) -> Vec<Line<'static>> {
    let (what, next) = problem_text(problem, words);
    let mut lines = markdown::wrap_text(&what, Style::new().fg(palette.error), width);
    if let Some(next) = next {
        lines.extend(markdown::wrap_text(&next, Style::new(), width));
    }
    lines
}

/// The tab line, then the rows the filter keeps, scrolled to the cursor.
fn queue_lines(
    palette: &Palette,
    review: &ReviewState,
    words: &Words,
    inner: Rect,
) -> Vec<Line<'static>> {
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
        lines.extend(problem_lines(
            palette,
            problem,
            words,
            usize::from(inner.width),
        ));
        return lines;
    }
    if review.queues_fetch.loading && review.queues.is_none() {
        lines.push(Line::raw(format!("Loading {}…", words.requests)));
        return lines;
    }
    let rows = review.rows();
    let visible = review.visible();
    if visible.is_empty() {
        lines.push(Line::raw(if rows.is_empty() {
            format!("No {} in this queue", words.requests)
        } else {
            format!("No {} matches the filter", words.request)
        }));
        return lines;
    }
    let QueueWindow { skip, room, .. } = queue_window(&visible, review.cursor, inner.height);
    let width = usize::from(inner.width);
    let digits = visible
        .iter()
        .map(|&i| rows[i].number.to_string().len())
        .max()
        .unwrap_or(1);
    for &i in visible.iter().skip(skip).take(room) {
        lines.push(queue_row(
            &rows[i],
            words,
            i == review.cursor,
            digits,
            width,
        ));
    }
    lines
}

/// Where the queue's rows sit below the tabs line in an inside `height`
/// tall: the cursor's place among the `visible` rows, the first shown and
/// how many fit. Shared with the mouse, so a click lands on the row drawn.
struct QueueWindow {
    at: usize,
    skip: usize,
    room: usize,
}

fn queue_window(visible: &[usize], cursor: usize, height: u16) -> QueueWindow {
    let room = usize::from(height).saturating_sub(1).max(1);
    let at = visible.iter().position(|&i| i == cursor).unwrap_or(0);
    QueueWindow {
        at,
        skip: at.saturating_sub(room - 1),
        room,
    }
}

impl AppState {
    /// The move from the queue's cursor to the row drawn on `line` of the
    /// review panel's inside, `height` lines tall; `None` on the tabs
    /// line, below the last row, or when the queue shows no rows.
    pub(crate) fn review_queue_step_to(&self, line: usize, height: u16) -> Option<isize> {
        let review = &self.review;
        let listed = review.screen == ReviewScreen::Queue
            && review.queues_fetch.error.is_none()
            && !(review.queues_fetch.loading && review.queues.is_none());
        if !listed || line >= usize::from(height) {
            return None;
        }
        let visible = review.visible();
        let QueueWindow { at, skip, room } = queue_window(&visible, review.cursor, height);
        let row = line.checked_sub(1).filter(|&row| row < room)?;
        let pos = skip + row;
        (pos < visible.len()).then(|| pos as isize - at as isize)
    }

    /// The move from the file cursor to the file drawn on `line` of the
    /// files screen's list, `height` lines inside its border; `None` below
    /// the last file or while the list shows no files.
    pub(crate) fn review_files_step_to(&self, line: usize, height: u16) -> Option<isize> {
        let files = self.review.files.as_ref()?;
        let listed = matches!(self.review.screen, ReviewScreen::Files(_))
            && files.stage == FilesStage::Ready
            && !files.rows.is_empty();
        if !listed || line >= usize::from(height) {
            return None;
        }
        let pos = file_start(files.cursor, height) + line;
        (pos < files.rows.len()).then(|| pos as isize - files.cursor as isize)
    }
}

/// The longest author a queue row shows, in columns.
const AUTHOR_COLS: usize = 20;

/// One queue row, exactly `width` wide: a cursor mark, the number, the
/// title cut with `…` to leave room for the author and the draft mark,
/// then those. The cursor row is a full-width bar.
fn queue_row(
    pr: &PrSummary,
    words: &Words,
    cursor: bool,
    digits: usize,
    width: usize,
) -> Line<'static> {
    let style = if cursor {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new()
    };
    let mark = if cursor { "▸ " } else { "  " };
    let number = format!("{}{:<digits$}  ", words.sign, pr.number);
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
fn detail_screen(
    palette: &Palette,
    review: &ReviewState,
    words: &Words,
    number: u64,
    inner: Rect,
) -> Vec<Line<'static>> {
    if let Some(problem) = review.detail_fetch.error {
        return problem_lines(palette, problem, words, usize::from(inner.width));
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
            let threads = super::threads_view::thread_summary(review, words);
            let lines = detail_lines(palette, detail, words, body, threads.as_deref(), width);
            let max = lines.len().saturating_sub(usize::from(inner.height));
            lines
                .into_iter()
                .skip(review.detail_scroll.min(max))
                .collect()
        }
        None => vec![Line::raw(format!(
            "Loading {} {}{number}…",
            words.request, words.sign
        ))],
    }
}

/// The files screen's two panes inside the review panel: the list, then
/// the diff, each with its own border.
fn files_areas(inner: Rect) -> [Rect; 2] {
    let list = (inner.width / 3).clamp(inner.width.min(24), inner.width.min(60));
    Layout::horizontal([Constraint::Length(list), Constraint::Min(0)]).areas(inner)
}

/// The files screen's list and diff in `main`, each with its border:
/// where the mouse finds them.
pub(crate) fn files_screen_areas(main: Rect) -> [Rect; 2] {
    files_areas(detail_area(main))
}

/// Text rows of the files screen's diff in `main`, which the stream's
/// demand is measured in.
pub fn files_diff_rows(main: Rect) -> usize {
    let [_, diff] = files_areas(detail_area(main));
    usize::from(Block::bordered().inner(diff).height)
}

/// The file list and the selected file's diff, the focused one framed
/// as the workspace frames its focused panel.
fn files_screen(state: &AppState, files: &PrFiles, frame: &mut Frame, inner: Rect) {
    let [list, diff] = files_areas(inner);
    let words = state.review_words();
    let pending = state
        .review
        .pending
        .as_ref()
        .and_then(|slot| slot.review())
        .map_or(0, |review| review.comments.len());
    let title = files_title(files, words, pending, usize::from(list.width));
    let block = view::panel_block(&state.palette, title, files.focus == FilesFocus::List);
    let list_inner = block.inner(list);
    frame.render_widget(block, list);
    frame.render_widget(
        Paragraph::new(file_lines(&state.palette, files, words, list_inner)),
        list_inner,
    );

    let stream = &state.diff;
    // Without it, threads still loading or not read look like none.
    let threads = if files.stage == FilesStage::Ready {
        thread_note(state)
    } else {
        String::new()
    };
    let tail = format!(
        "{}{}{threads}",
        diff_position(stream),
        found_note(stream.search.as_ref(), !stream.done)
    );
    let path = files
        .selected()
        .filter(|_| files.stage == FilesStage::Ready)
        .map(|row| row.file.path.display())
        .unwrap_or_default();
    let title = diff_title(path, &tail, usize::from(diff.width));
    let block = view::panel_block(&state.palette, title, files.focus == FilesFocus::Diff);
    let diff_inner = block.inner(diff);
    let commenting = has_comment_marks(state, files);
    let no_threads;
    let body = match &files.stage {
        FilesStage::Ready => match files.selected() {
            Some(row) if state.review.discussion.is_some() || commenting => {
                let discussion = match &state.review.discussion {
                    Some(discussion) => discussion,
                    None => {
                        no_threads = no_discussion();
                        &no_threads
                    }
                };
                let scope = ThreadScope::of(files, discussion);
                let mut threads = scope.file_threads(&row.file.path, None);
                // Numbered once a frame, for the shown file only, and only
                // when it has a thread on a line or a comment to mark.
                let numbers = if threads.lines.is_empty() && !commenting {
                    Vec::new()
                } else {
                    row_numbers(&stream.lines)
                };
                if stream.done && !threads.lines.is_empty() {
                    threads = scope.file_threads(&row.file.path, Some(&numbers));
                }
                let marks = comment_marks(state, files, &numbers);
                thread_diff_body(
                    &state.palette,
                    stream,
                    &numbers,
                    &threads,
                    &scope,
                    files.thread,
                    &marks,
                    diff_inner,
                )
            }
            _ => diff_pane_body(&state.palette, &state.diff, diff_inner),
        },
        FilesStage::Failed(_) => Paragraph::new(Line::styled("No diff to show", dim())),
        _ => Paragraph::new(Line::styled(
            "The diff shows once the files are listed",
            dim(),
        )),
    };
    frame.render_widget(body.block(block), diff);
    if let Some(reader) = &files.reader {
        let discussion = state.review.discussion.as_ref();
        super::reader_view::render_reader(&state.palette, files, discussion, reader, frame, inner);
    }
    super::pending_view::render_pending(&state.palette, &state.review, words, frame, inner);
}

/// What the list shows, long and short: `whole pull request` or
/// `commit k/N <7-hex>`.
fn view_labels(files: &PrFiles, words: &Words) -> (String, String) {
    let commit = match files.view {
        ReviewView::Whole => None,
        ReviewView::Commit(i) => files.commits.as_ref().and_then(|(commits, _)| {
            let commit = commits.get(i)?;
            Some((i + 1, commits.len(), commit.range.oid.as_str()))
        }),
    };
    match commit {
        None => (format!("whole {}", words.request), "whole".into()),
        Some((k, n, oid)) => {
            let hex = view::short_hash(oid);
            (format!("commit {k}/{n} {hex}"), format!("{k}/{n} {hex}"))
        }
    }
}

/// ` PR #N · files (viewed/listed) · K pending · <view> ` (in the forge's
/// words) for a list
/// `width` wide, the pending count only when there is one: the number
/// goes first, then the view's long label, so which commit is shown
/// stays as long as it can.
fn files_title(files: &PrFiles, words: &Words, pending: usize, width: usize) -> String {
    let count = format!("files ({}/{})", files.viewed_count(), files.rows.len());
    // The pending review's count gives way only to the view's short label.
    let pending = if pending > 0 {
        format!(" · {pending} pending")
    } else {
        String::new()
    };
    let (long, short) = view_labels(files, words);
    let room = width.saturating_sub(2);
    let candidates = [
        format!(
            " {} {}{} · {count}{pending} · {long} ",
            words.short, words.sign, files.number
        ),
        format!(" {count}{pending} · {long} "),
        format!(" {count}{pending} · {short} "),
        format!(" {short}{pending} "),
        format!(" {short} "),
    ];
    candidates
        .iter()
        .find(|title| width_of(title) <= room)
        .cloned()
        .unwrap_or_else(|| format!(" {} ", elide_right(&short, room.saturating_sub(2))))
}

/// ` Diff <path><tail> ` for a pane `width` wide: the path gives way
/// first, from its start, so the position and the search's match stay.
fn diff_title(path: &str, tail: &str, width: usize) -> String {
    let room = width.saturating_sub(4);
    let path_room = room.saturating_sub(width_of(tail) + width_of("Diff "));
    let path = if path_room == 0 {
        String::new()
    } else {
        view::elide_left(path, path_room)
    };
    format!(" {} ", elide_right(&format!("Diff {path}{tail}"), room))
}

/// The list's rows, scrolled so the cursor shows, or what it waits for.
fn file_lines(
    palette: &Palette,
    files: &PrFiles,
    words: &Words,
    inner: Rect,
) -> Vec<Line<'static>> {
    let width = usize::from(inner.width);
    let fetching = format!("Fetching {} head…", words.short);
    let waiting = match &files.stage {
        FilesStage::Checking | FilesStage::Listing => Some("Loading files…"),
        FilesStage::Fetching => Some(fetching.as_str()),
        FilesStage::Failed(message) => {
            // Git's words were made safe when they were read.
            return markdown::wrap_text(message, Style::new().fg(palette.error), width);
        }
        FilesStage::Ready if files.rows.is_empty() => Some("No files changed"),
        FilesStage::Ready => None,
    };
    if let Some(text) = waiting {
        return vec![Line::raw(text.to_string())];
    }
    let height = usize::from(inner.height).max(1);
    let start = file_start(files.cursor, inner.height);
    files
        .rows
        .iter()
        .enumerate()
        .skip(start)
        .take(height)
        .map(|(i, row)| {
            // The mark GitHub has been asked for and not yet answered.
            let pending = files
                .marking
                .as_ref()
                .is_some_and(|m| row.file.path.as_bytes() == m.path.as_bytes());
            file_row(palette, row, i == files.cursor, pending, width)
        })
        .collect()
}

/// The first file the list draws in `height` lines, scrolled just enough
/// to keep the cursor's in view. Shared with the mouse.
fn file_start(cursor: usize, height: u16) -> usize {
    (cursor + 1).saturating_sub(usize::from(height).max(1))
}

/// The fewest columns of path a row keeps before its counts give way.
const MIN_PATH: usize = 6;

/// `<status> <viewed> <path>  +A −D`, exactly `width` wide (at least the
/// four columns of status and mark): the path loses its start to fit, and
/// on a list too narrow for both the counts give way to it. While its
/// viewed mark is `pending` on GitHub, the mark is a dim `…`.
fn file_row(
    palette: &Palette,
    row: &PrFile,
    cursor: bool,
    pending: bool,
    width: usize,
) -> Line<'static> {
    let colour = status_colour(palette, row.file.status);
    let (mark, mark_style) = match row.viewed {
        _ if pending => ("…", dim()),
        Some(ViewedState::Viewed) => ("✓", Style::new().fg(palette.success)),
        Some(ViewedState::Dismissed) => ("~", Style::new().fg(palette.warning)),
        Some(ViewedState::Unviewed) => (" ", Style::new()),
        None => ("?", dim()),
    };
    let counts: Vec<Span<'static>> = match row.counts {
        FileCounts::Lines { added, removed } => vec![
            Span::styled(format!("  +{added}"), Style::new().fg(palette.added)),
            Span::styled(format!(" −{removed}"), Style::new().fg(palette.removed)),
        ],
        FileCounts::Binary => vec![Span::styled("  bin", dim())],
        FileCounts::Unknown => Vec::new(),
    };
    let mut counts_width: usize = counts.iter().map(Span::width).sum();
    let counts = if width < 4 + MIN_PATH + counts_width {
        counts_width = 0;
        Vec::new()
    } else {
        counts
    };
    let path = drawn_path(&row.file);
    let room = width.saturating_sub(4 + counts_width);
    let path = view::elide_left(&path, room);
    let pad = room.saturating_sub(width_of(&path));
    let mut spans = vec![
        Span::styled(row.file.status.to_string(), Style::new().fg(colour)),
        Span::raw(" "),
        Span::styled(mark, mark_style),
        Span::raw(" "),
        Span::raw(path),
        Span::raw(" ".repeat(pad)),
    ];
    spans.extend(counts);
    let line = Line::from(spans);
    if cursor {
        view::reversed(line, width)
    } else {
        line
    }
}

/// The review panel's inside, in `main`: where the detail is drawn.
pub fn detail_area(main: Rect) -> Rect {
    Block::bordered().inner(main)
}

/// How far the detail scrolls in `inner`, with `body` its rendered
/// description.
pub fn detail_max_scroll(
    detail: &PrDetail,
    words: &Words,
    body: &[Line<'static>],
    threads: Option<&str>,
    inner: Rect,
) -> usize {
    // Colours do not change how many lines there are.
    detail_lines(
        &Palette::DEFAULT,
        detail,
        words,
        body,
        threads,
        usize::from(inner.width),
    )
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
/// a small screen still shows them, then labels, reviewers, the review
/// `threads` line and `body`, the description already rendered.
pub fn detail_lines(
    palette: &Palette,
    detail: &PrDetail,
    words: &Words,
    body: &[Line<'static>],
    threads: Option<&str>,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = markdown::wrap_text(&detail.title, bold(), width);
    let by = format!("{}{} by ", words.sign, detail.number);
    lines.push(Line::from(vec![
        Span::raw(by.clone()),
        Span::styled(
            elide_right(&detail.author, width.saturating_sub(width_of(&by))),
            bold(),
        ),
    ]));
    let state = match (detail.state, detail.draft) {
        (PrState::Merged, _) => ("merged", palette.pr_merged),
        (PrState::Closed, _) => ("closed", palette.pr_closed),
        (_, true) => ("draft", palette.pr_draft),
        _ => ("open", palette.pr_open),
    };
    let (word, color) = match detail.mergeable {
        Mergeability::Mergeable => ("mergeable", palette.success),
        Mergeability::Conflicting => ("conflicts", palette.error),
        Mergeability::Unknown => ("unknown", palette.neutral),
    };
    lines.push(Line::from(vec![
        Span::styled(state.0, Style::new().fg(state.1)),
        Span::styled(" · ", dim()),
        Span::styled(word, Style::new().fg(color)),
    ]));
    lines.extend(merge_line(palette, detail, width));
    let branches = format!("{} ← {}", detail.base, detail.head);
    lines.extend(markdown::wrap_text(&branches, Style::new(), width));
    if !detail.checks.is_empty() {
        lines.push(Line::styled("Checks", bold()));
        for check in &detail.checks {
            let (glyph, word, color) = check_look(palette, check.status);
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
    if let Some(threads) = threads {
        lines.extend(markdown::wrap_text(threads, Style::new(), width));
    }
    if !body.is_empty() {
        lines.push(Line::default());
        lines.extend(body.iter().cloned());
    }
    lines
}

/// How the pull request may be merged, or why it may not, in fixed words.
fn merge_line(palette: &Palette, detail: &PrDetail, width: usize) -> Vec<Line<'static>> {
    let (text, style) = match merge_options(detail) {
        Ok(methods) => {
            let labels: Vec<&str> = methods.iter().map(|m| m.label()).collect();
            (format!("Merge: {}", labels.join(" · ")), Style::new())
        }
        // Already merged is done, not a problem: it is not drawn as one.
        Err(MergeBlock::Merged) => (
            format!("Cannot merge: {}", MergeBlock::Merged.message()),
            dim(),
        ),
        Err(block) => (
            format!("Cannot merge: {}", block.message()),
            Style::new().fg(palette.error),
        ),
    };
    markdown::wrap_text(&text, style, width)
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

fn check_look(palette: &Palette, status: CheckStatus) -> (&'static str, &'static str, Color) {
    match status {
        CheckStatus::Success => ("✓", "passed", palette.success),
        CheckStatus::Failure => ("✗", "failed", palette.error),
        CheckStatus::Pending => ("●", "pending", palette.pending),
        CheckStatus::Neutral => ("○", "neutral", palette.neutral),
        CheckStatus::Skipped => ("–", "skipped", palette.neutral),
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    const GITHUB: &Words = &kika_forge::forge::GitHubForge::WORDS;

    use super::*;
    use crate::features::review::ReviewIntent;
    use crate::features::review::state::tests::{
        detail_from, fixture_detail, fixture_queues, repo,
    };
    use crate::model::Context;
    use crate::view::buffer_rows;
    use kika_forge::ForgeHost;
    use kika_forge::cli::CliStatus;
    use kika_forge::review::fixtures::hostile_detail;

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
    fn the_detail_counts_its_review_threads_or_says_why_not() {
        use crate::features::review::threads::tests::fixture_discussion;
        let mut state = reviewing(ReviewScreen::Detail(42));
        state.review.detail = Some(fixture_detail());
        state.review.discussion = Some(fixture_discussion());
        let rows = draw(&state, 80, 30);
        let threads = rows
            .iter()
            .position(|r| {
                r.contains("Threads: 2 unresolved of 5 · 1 addressed · 3 conversation comments")
            })
            .unwrap_or_else(|| panic!("{rows:#?}"));
        // Right after the reviewers, which wrap to two lines here.
        assert!(rows[threads - 2].contains("Reviewers: "), "{rows:#?}");
        assert!(rows[threads - 1].contains("core requested"), "{rows:#?}");

        state.review.discussion = None;
        state.review.discussion_fetch.loading = true;
        let rows = draw(&state, 80, 30);
        assert!(contains(&rows, "Threads: loading…"), "{rows:#?}");

        state.review.discussion_fetch.loading = false;
        state.review.discussion_fetch.error = Some(ReviewProblem::Status(ForgeStatus::NoAccess));
        let rows = draw(&state, 80, 30);
        assert!(
            contains(
                &rows,
                "Threads: could not be read (No access to the repository with gh's login), r to"
            ),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_detail_says_how_it_may_merge_or_why_not() {
        let mut state = reviewing(ReviewScreen::Detail(42));
        state.review.detail = Some(fixture_detail());
        let rows = draw(&state, 80, 30);
        assert!(contains(&rows, "Merge: merge commit · squash"), "{rows:#?}");
        assert!(!contains(&rows, "Cannot merge"), "{rows:#?}");

        let mut detail = fixture_detail();
        detail.merge_state = kika_forge::review::MergeState::Behind;
        state.review.detail = Some(detail);
        let rows = draw(&state, 80, 30);
        assert!(
            contains(
                &rows,
                "Cannot merge: the base moved on; the branch must be updated first"
            ),
            "{rows:#?}"
        );
        assert!(!contains(&rows, "Merge: "), "{rows:#?}");

        let mut detail = fixture_detail();
        detail.state = PrState::Merged;
        state.review.detail = Some(detail);
        let rows = draw(&state, 80, 30);
        assert!(contains(&rows, "Cannot merge: already merged"), "{rows:#?}");
    }

    /// The style of the first cell of the row holding `text`.
    fn style_at(state: &AppState, text: &str) -> Style {
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
        terminal
            .draw(|frame| crate::view::render(state, frame))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rows = buffer_rows(&buffer);
        let y = rows.iter().position(|r| r.contains(text)).unwrap();
        let row = &rows[y];
        let col = row[..row.find(text).unwrap()].chars().count();
        buffer[(col as u16, y as u16)].style()
    }

    #[test]
    fn already_merged_is_dim_and_a_real_block_is_red() {
        let mut state = reviewing(ReviewScreen::Detail(42));
        let mut detail = fixture_detail();
        detail.state = PrState::Merged;
        state.review.detail = Some(detail);
        let merged = style_at(&state, "Cannot merge: already merged");
        assert_ne!(merged.fg, Some(Color::Red));
        assert!(merged.add_modifier.contains(Modifier::DIM), "{merged:?}");

        let mut detail = fixture_detail();
        detail.merge_state = kika_forge::review::MergeState::Blocked;
        state.review.detail = Some(detail);
        let blocked = style_at(&state, "Cannot merge: a required review");
        assert_eq!(blocked.fg, Some(Color::Red));
    }

    #[test]
    fn the_merge_key_shows_in_the_bar_at_80_columns() {
        let mut state = reviewing(ReviewScreen::Detail(42));
        state.review.detail = Some(fixture_detail());
        for height in [24, 30] {
            let rows = draw(&state, 80, height);
            let bar = rows.last().unwrap();
            assert!(bar.contains("M merge"), "{bar:?}");
        }
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
        state.update(crate::intent::AppIntent::Review(ReviewIntent::Enter));
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
    fn on_gitlab_a_problem_names_gitlab_and_glab() {
        let mut state = on_gitlab(reviewing(ReviewScreen::Detail(42)));
        state.review.detail_fetch.error = Some(ReviewProblem::Limited(1_700_003_600));
        let rows = draw(&state, 80, 8);
        assert!(
            contains(&rows, "GitLab is rate limited until 23:13 UTC"),
            "{rows:#?}"
        );
        state.review.detail_fetch.error = Some(ReviewProblem::Status(ForgeStatus::Offline));
        let rows = draw(&state, 80, 8);
        assert!(contains(&rows, "GitLab could not be reached"), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("GitHub")), "{rows:#?}");
        let gitlab = kika_forge::forge::words(kika_forge::url::ForgeKind::GitLab);
        let text = |status| problem_text(ReviewProblem::Status(status), gitlab);
        assert_eq!(
            text(ForgeStatus::NotLoggedIn),
            (
                "glab is not logged in to this host".to_string(),
                Some("Run glab auth login.".to_string())
            )
        );
        assert_eq!(
            text(ForgeStatus::NoAccess),
            (
                "No access to the repository with glab's login".to_string(),
                Some("Check glab auth status.".to_string())
            )
        );
        assert_eq!(
            text(ForgeStatus::RateLimited).0,
            "GitLab is rate limiting this account"
        );
        assert_eq!(text(ForgeStatus::Failed).0, "GitLab gave no usable answer");
        // GitHub keeps its own.
        assert_eq!(
            problem_text(ReviewProblem::Status(ForgeStatus::NotLoggedIn), GITHUB).1,
            Some("Run gh auth login.".to_string())
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
        queues = kika_forge::review::fixtures::with_title(queues, 15, &long);
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
        let lines = detail_lines(&Palette::DEFAULT, &detail, GITHUB, &[], None, 28);
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
        state.update(crate::intent::AppIntent::Review(ReviewIntent::Enter));
        let rows = draw(&state, 40, 10);
        assert!(contains(&rows, "[forge] github"), "{rows:#?}");
        assert!(
            contains(&rows, "The remote is on no forge host in the"),
            "{rows:#?}"
        );
        assert!(contains(&rows, "│global config, or not"), "{rows:#?}");
        let problems = [
            ReviewProblem::NoForgeHost,
            ReviewProblem::Limited(1_700_003_600),
            ReviewProblem::Missing(7),
            ReviewProblem::Unsupported(kika_forge::forge::Unsupported {
                kind: kika_forge::url::ForgeKind::GitLab,
                capability: kika_forge::forge::Capability::Review,
            }),
            ReviewProblem::Status(ForgeStatus::Cli(CliStatus::NotInstalled)),
            ReviewProblem::Status(ForgeStatus::Cli(CliStatus::Unusable)),
            ReviewProblem::Status(ForgeStatus::NotLoggedIn),
            ReviewProblem::Status(ForgeStatus::Offline),
            ReviewProblem::Status(ForgeStatus::NoAccess),
            ReviewProblem::Status(ForgeStatus::RateLimited),
            ReviewProblem::Status(ForgeStatus::Failed),
        ];
        for problem in problems {
            let (what, next) = problem_text(problem, GITHUB);
            for width in [20, 40, 60] {
                let lines = problem_lines(&Palette::DEFAULT, problem, GITHUB, width);
                let text: String = lines
                    .iter()
                    .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
                    .collect::<Vec<_>>()
                    .join(" ");
                let words: Vec<&str> = text.split_whitespace().collect();
                let want: Vec<&str> = what
                    .split_whitespace()
                    .chain(next.iter().flat_map(|n| n.split_whitespace()))
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

    fn pr_file(
        status: char,
        path: &[u8],
        counts: FileCounts,
        viewed: Option<ViewedState>,
    ) -> PrFile {
        PrFile {
            file: kika_git::commit_files::CommitFile {
                status,
                path: kika_git::path::RepoPath::from_bytes(path),
                orig_path: None,
            },
            counts,
            viewed,
        }
    }

    /// Pull request 12's files screen, ready, with every kind of row.
    fn files_state() -> AppState {
        use kika_git::commit_files::CommitId;
        let mut state = reviewing(ReviewScreen::Files(12));
        let id = |c: char| CommitId::parse(&c.to_string().repeat(40)).unwrap();
        let lines = |added, removed| FileCounts::Lines { added, removed };
        state.review.files = Some(PrFiles {
            number: 12,
            head: id('1'),
            base: id('2'),
            range: None,
            whole: None,
            view: ReviewView::Whole,
            commits: None,
            rows: vec![
                pr_file('M', b"src/cache.rs", lines(3, 1), Some(ViewedState::Viewed)),
                pr_file(
                    'M',
                    b"src/lib.rs",
                    lines(1, 2),
                    Some(ViewedState::Dismissed),
                ),
                pr_file(
                    'A',
                    b"docs/new.md",
                    lines(9, 0),
                    Some(ViewedState::Unviewed),
                ),
                pr_file('A', b"logo.png", FileCounts::Binary, None),
                pr_file('D', b"evil\x1b]52;c;eA==\x07\x1b[2Jname", lines(0, 4), None),
            ],
            cursor: 0,
            keep: None,
            focus: FilesFocus::List,
            stage: FilesStage::Ready,
            fetched: false,
            seq: None,
            marking: None,
            thread: None,
            jump: None,
            reader: None,
            pick: None,
            reopen_thread: None,
        });
        state
    }

    #[test]
    fn each_file_row_shows_status_viewed_mark_path_and_counts() {
        let state = files_state();
        let rows = draw(&state, 120, 20);
        assert!(
            contains(&rows, "files (1/5) · whole pull request"),
            "{rows:#?}"
        );
        for (status, mark, path, counts) in [
            ("M", "✓", "src/cache.rs", "+3 −1"),
            ("M", "~", "src/lib.rs", "+1 −2"),
            ("A", " ", "docs/new.md", "+9 −0"),
            ("A", "?", "logo.png", "bin"),
        ] {
            let want = format!("{status} {mark} {path}");
            let row = rows
                .iter()
                .find(|r| r.contains(&want))
                .unwrap_or_else(|| panic!("{want:?} missing in {rows:#?}"));
            assert!(row.contains(counts), "{row:?}");
        }
        // A control character in a local name never reaches the terminal.
        let evil = rows.iter().find(|r| r.contains("evil")).unwrap();
        assert!(!evil.chars().any(char::is_control), "{evil:?}");
        assert!(evil.contains("name") && evil.contains("−4"), "{evil:?}");
    }

    #[test]
    fn a_file_past_the_first_page_of_marks_draws_its_mark_when_scrolled_to() {
        use crate::features::review::files::tests::large_ready;
        use kika_forge::github::fixtures::GRAPHQL_RATE_LIMITED;
        use kika_forge::review::fixtures::files_page;

        let mut state = large_ready(files_page(100..150, None));
        state.review.files.as_mut().unwrap().cursor = 120;
        let rows = draw(&state, 100, 24);
        assert!(contains(&rows, "M ✓ f0120"), "{rows:#?}");
        assert!(contains(&rows, "M ~ f0118"), "{rows:#?}");
        assert!(!contains(&rows, "M ? "), "{rows:#?}");

        // Where paging stopped, the files past it are unknown.
        let mut state = large_ready(GRAPHQL_RATE_LIMITED.to_string());
        state.review.files.as_mut().unwrap().cursor = 120;
        let rows = draw(&state, 100, 24);
        assert!(contains(&rows, "M ? f0120"), "{rows:#?}");
    }

    #[test]
    fn the_files_screen_says_what_it_waits_for_or_why_it_failed() {
        let mut state = files_state();
        for (stage, text) in [
            (FilesStage::Checking, "Loading files…"),
            (FilesStage::Listing, "Loading files…"),
            (FilesStage::Fetching, "Fetching PR head…"),
            (
                FilesStage::Failed(crate::features::review::files::NO_FETCH_REMOTE.into()),
                "PR head is not here",
            ),
        ] {
            state.review.files.as_mut().unwrap().stage = stage;
            let rows = draw(&state, 120, 20);
            assert!(contains(&rows, text), "{text:?} missing in {rows:#?}");
        }
    }

    /// `files_state` with `src/cache.rs`'s diff shown and searched for
    /// `cache`, the first of its two matches current.
    fn searched_files_state() -> AppState {
        use crate::diff_search::DiffSearch;
        use kika_git::diff::{DiffLine, LineKind};
        let mut state = files_state();
        state.diff.path = Some("src/cache.rs".into());
        state.diff.done = true;
        state.diff.lines = ["@@ -1 +1 @@", "-old cache", "+new cache"]
            .iter()
            .map(|t| DiffLine {
                kind: match t.as_bytes()[0] {
                    b'-' => LineKind::Removed,
                    b'+' => LineKind::Added,
                    _ => LineKind::Hunk,
                },
                text: t.to_string(),
            })
            .collect();
        let texts: Vec<String> = state.diff.lines.iter().map(|l| l.text.clone()).collect();
        let files = state.review.files.as_mut().unwrap();
        files.focus = FilesFocus::Diff;
        state.diff.search = Some(DiffSearch::new("cache", &texts, 0));
        state
    }

    /// The style of the cell where `cache` starts on the row showing `needle`.
    fn cache_style(buffer: &Buffer, needle: &str) -> Style {
        let rows = buffer_rows(buffer);
        let y = rows.iter().position(|r| r.contains(needle)).unwrap();
        let at = rows[y].find(needle).unwrap() + needle.find("cache").unwrap();
        let col = rows[y][..at].chars().count();
        buffer[(col as u16, y as u16)].style()
    }

    fn draw_buffer(state: &AppState) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
        terminal
            .draw(|frame| crate::view::render(state, frame))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn the_review_diff_marks_search_matches_like_the_workspace_diff() {
        let state = searched_files_state();
        let buffer = draw_buffer(&state);
        let rows = buffer_rows(&buffer);
        assert!(
            contains(&rows, "Diff src/cache.rs · 1/3 · match 1 of 2"),
            "{rows:#?}"
        );
        let current = cache_style(&buffer, "-old cache");
        let other = cache_style(&buffer, "+new cache");
        assert_eq!(current.bg, Some(Palette::DEFAULT.current_match_bg));
        assert_eq!(other.bg, Some(Palette::DEFAULT.match_bg));
    }

    #[test]
    fn under_no_color_the_review_screen_draws_no_colour_but_keeps_its_marks() {
        let mut state = searched_files_state();
        state.palette = Palette::NO_COLOR;
        let buffer = draw_buffer(&state);
        crate::test_support::assert_no_colour(&buffer);
        let current = cache_style(&buffer, "-old cache").add_modifier;
        assert!(current.contains(Modifier::BOLD | Modifier::UNDERLINED | Modifier::REVERSED));
        let plain = cache_style(&buffer, "+new cache").add_modifier;
        assert!(plain.contains(Modifier::UNDERLINED), "{plain:?}");
        assert!(!plain.contains(Modifier::BOLD), "{plain:?}");
        assert!(crate::test_support::reversed_somewhere(
            &buffer,
            "src/cache.rs"
        ));
    }

    #[test]
    fn the_files_screen_draws_at_80_by_24_and_on_tiny_terminals() {
        for stage in [
            FilesStage::Ready,
            FilesStage::Fetching,
            FilesStage::Checking,
            FilesStage::Failed("fatal: could not read from remote".into()),
        ] {
            for (width, height) in [(80, 24), (40, 10), (20, 5), (10, 3), (3, 2), (1, 1)] {
                let mut state = files_state();
                state.size = (width, height);
                state.review.files.as_mut().unwrap().stage = stage.clone();
                // Drawing must not panic at any size.
                draw(&state, width, height);
            }
        }
        let rows = draw(&files_state(), 80, 24);
        assert!(contains(&rows, "files (1/5) · whole"), "{rows:#?}");
        for counts in ["+3 −1", "+1 −2", "bin", "−4"] {
            assert!(contains(&rows, counts), "{counts:?} missing in {rows:#?}");
        }
    }

    /// `files_state()` showing the second of three commits.
    fn in_commit_view() -> AppState {
        use crate::features::review::PrCommit;
        use kika_git::commit_files::{CommitId, CommitRange};
        let mut state = files_state();
        let id = |c: char| CommitId::parse(&c.to_string().repeat(40)).unwrap();
        let commit = |oid, parent| PrCommit {
            range: CommitRange {
                oid: id(oid),
                parent: Some(id(parent)),
            },
            subject: "s".into(),
        };
        let files = state.review.files.as_mut().unwrap();
        files.commits = Some((
            vec![commit('a', '3'), commit('b', 'a'), commit('1', 'b')],
            false,
        ));
        files.view = ReviewView::Commit(1);
        state
    }

    #[test]
    fn the_file_list_title_says_which_commit_is_shown() {
        let state = in_commit_view();
        let rows = draw(&state, 200, 24);
        assert!(
            contains(&rows, " PR #12 · files (1/5) · commit 2/3 bbbbbbb "),
            "{rows:#?}"
        );
        let rows = draw(&state, 120, 24);
        assert!(
            contains(&rows, " files (1/5) · commit 2/3 bbbbbbb "),
            "{rows:#?}"
        );
        // A narrow list keeps the commit before the count.
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, " 2/3 bbbbbbb "), "{rows:#?}");
        let rows = draw(&files_state(), 200, 24);
        assert!(
            contains(&rows, " PR #12 · files (1/5) · whole pull request "),
            "{rows:#?}"
        );
    }

    use crate::features::review::state::tests::on_gitlab;

    #[test]
    fn a_gitlab_repository_shows_merge_requests_by_bang_number() {
        let mut queue = on_gitlab(reviewing(ReviewScreen::Queue));
        queue.review.queues = Some(fixture_queues());
        queue.review.queue = QueueKind::AllOpen;
        let rows = draw(&queue, 80, 12);
        assert!(
            rows.iter()
                .any(|r| r.starts_with("│▸ !15  Draft: new gadget API ")),
            "{rows:#?}"
        );
        assert!(contains(&rows, "  !12  Cache widget lookups"), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("#1")), "{rows:#?}");
        queue.review.queue = QueueKind::Authored;
        let rows = draw(&queue, 80, 12);
        assert!(
            contains(&rows, "No merge requests in this queue"),
            "{rows:#?}"
        );
        queue.review.queues = None;
        queue.review.queues_fetch.loading = true;
        let rows = draw(&queue, 80, 12);
        assert!(contains(&rows, "Loading merge requests…"), "{rows:#?}");

        let mut detail = on_gitlab(reviewing(ReviewScreen::Detail(42)));
        let rows = draw(&detail, 80, 8);
        assert!(contains(&rows, "Loading merge request !42…"), "{rows:#?}");
        detail.review.detail = Some(fixture_detail());
        let rows = draw(&detail, 80, 30);
        assert!(contains(&rows, "Review · acme/widgets !42 "), "{rows:#?}");
        assert!(contains(&rows, "!42 by octocat"), "{rows:#?}");
        assert!(!contains(&rows, "#42"), "{rows:#?}");
        detail.review.detail = None;
        detail.review.detail_fetch.error = Some(ReviewProblem::Missing(42));
        let rows = draw(&detail, 80, 8);
        assert!(contains(&rows, "No merge request !42"), "{rows:#?}");
    }

    #[test]
    fn a_github_repository_still_shows_pull_requests_by_hash_number() {
        let mut queue = reviewing(ReviewScreen::Queue);
        queue.forge.hosts = vec![ForgeHost::parse("github.com").unwrap()];
        queue.review.queues = Some(fixture_queues());
        queue.review.queue = QueueKind::Authored;
        let rows = draw(&queue, 80, 12);
        assert!(
            contains(&rows, "No pull requests in this queue"),
            "{rows:#?}"
        );
        let mut detail = reviewing(ReviewScreen::Detail(42));
        detail.forge.hosts = queue.forge.hosts.clone();
        detail.review.detail = Some(fixture_detail());
        let rows = draw(&detail, 80, 30);
        assert!(contains(&rows, "Review · acme/widgets #42 "), "{rows:#?}");
        assert!(contains(&rows, "#42 by octocat"), "{rows:#?}");
        assert!(!contains(&rows, "!42"), "{rows:#?}");
    }

    #[test]
    fn the_file_list_title_names_a_merge_request_on_gitlab() {
        let state = files_state();
        let files = state.review.files.as_ref().unwrap();
        let gitlab = kika_forge::forge::words(kika_forge::url::ForgeKind::GitLab);
        assert_eq!(
            files_title(files, gitlab, 0, 200),
            " MR !12 · files (1/5) · whole merge request "
        );
        assert_eq!(
            files_title(files, GITHUB, 0, 200),
            " PR #12 · files (1/5) · whole pull request "
        );
    }

    #[test]
    fn the_file_list_title_never_overflows_its_border() {
        let mut state = in_commit_view();
        for pending in [0, 1, 500] {
            for width in 0..70 {
                let title =
                    files_title(state.review.files.as_ref().unwrap(), GITHUB, pending, width);
                assert!(
                    width_of(&title) <= width.saturating_sub(2) || width < 4,
                    "{pending} {width}: {title:?}"
                );
            }
        }
        // The count gives way only to the short view label.
        let files = state.review.files.as_ref().unwrap();
        assert!(files_title(files, GITHUB, 3, 70).contains("· 3 pending"));
        assert!(files_title(files, GITHUB, 3, 30).contains("3 pending"));
        for (width, height) in [(80, 24), (40, 10), (20, 5), (10, 3), (3, 2), (1, 1)] {
            state.size = (width, height);
            draw(&state, width, height);
        }
    }

    fn row_width(line: &Line<'_>) -> usize {
        line.width()
    }

    fn row_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn rows_fill_the_list_exactly_with_wide_names_renames_and_the_cursor() {
        let lines = |added, removed| FileCounts::Lines { added, removed };
        let wide = pr_file(
            'M',
            "文档/很长的目录名字/数据文件.rs".as_bytes(),
            lines(12, 3),
            None,
        );
        let mut rename = pr_file('R', b"src/old/name.rs", lines(0, 0), None);
        rename.file.orig_path = Some("src/older/name.rs".into());
        for width in [60, 30, 21, 17] {
            for row in [&wide, &rename] {
                for cursor in [false, true] {
                    let line = file_row(&Palette::DEFAULT, row, cursor, false, width);
                    assert_eq!(row_width(&line), width, "{:?}", row_text(&line));
                }
            }
        }
        let text = row_text(&file_row(&Palette::DEFAULT, &rename, false, false, 60));
        assert!(
            text.contains("src/older/name.rs → src/old/name.rs"),
            "{text:?}"
        );
        // Cut from the start, keeping the file name and the counts.
        let text = row_text(&file_row(&Palette::DEFAULT, &wide, false, false, 30));
        assert!(
            text.contains('…') && text.contains("数据文件.rs"),
            "{text:?}"
        );
        assert!(text.ends_with("+12 −3"), "{text:?}");
    }

    #[test]
    fn on_a_narrow_list_the_counts_give_way_to_the_path() {
        let row = pr_file(
            'M',
            b"src/cache.rs",
            FileCounts::Lines {
                added: 120,
                removed: 45,
            },
            Some(ViewedState::Viewed),
        );
        let text = row_text(&file_row(&Palette::DEFAULT, &row, false, false, 12));
        assert_eq!(width_of(&text), 12, "{text:?}");
        assert!(!text.contains('+'), "{text:?}");
        assert!(text.starts_with("M ✓ …"), "{text:?}");
        let text = row_text(&file_row(&Palette::DEFAULT, &row, false, false, 30));
        assert!(text.ends_with("+120 −45"), "{text:?}");
    }

    #[test]
    fn a_narrow_diff_title_shortens_the_path_and_keeps_the_match() {
        let path = "src/some/deeply/nested/module/with/a/long/name.rs";
        let title = diff_title(path, " · 3/120+ · match 2 of 7", 50);
        assert!(width_of(&title) <= 48, "{title:?}");
        assert!(title.contains("match 2 of 7"), "{title:?}");
        assert!(
            title.contains("…") && title.contains("name.rs"),
            "{title:?}"
        );
        // With no room for a path, the tail still leads.
        let title = diff_title(path, " · 3/120+ · match 2 of 7", 30);
        assert!(title.starts_with(" Diff  · 3/120+"), "{title:?}");
        // Drawn on a 50-column screen with a search active.
        let mut state = files_state();
        state.diff.path = Some(path.into());
        state.diff.lines = vec![kika_git::diff::DiffLine {
            kind: kika_git::diff::LineKind::Added,
            text: "+cache".into(),
        }];
        let files = state.review.files.as_mut().unwrap();
        files.rows[0].file.path = path.into();
        files.focus = FilesFocus::Diff;
        state.diff.search = Some(crate::diff_search::DiffSearch::new(
            "cache",
            &["+cache".to_string()],
            0,
        ));
        let rows = draw(&state, 80, 10);
        assert!(contains(&rows, "match 1 of 1"), "{rows:#?}");
    }

    #[test]
    fn outside_ready_the_diff_pane_says_why_it_is_empty() {
        let mut state = files_state();
        state.review.files.as_mut().unwrap().stage = FilesStage::Fetching;
        let rows = draw(&state, 120, 20);
        assert!(
            contains(&rows, "The diff shows once the files are listed"),
            "{rows:#?}"
        );
        state.review.files.as_mut().unwrap().stage = FilesStage::Failed("nope".into());
        let rows = draw(&state, 120, 20);
        assert!(contains(&rows, "No diff to show"), "{rows:#?}");
    }
}
