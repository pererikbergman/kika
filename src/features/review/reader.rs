//! The thread reader: one review thread of the selected file read in full
//! over the files screen, resolved threads included, with `]`/`[` through
//! the file's threads. It reads only the discussion already loaded and
//! never moves the diff. Drawn by `reader_view.rs`.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::files::{PrFiles, files_screen};
use super::markdown;
use super::state::RenderedBody;
use super::threads::{ThreadScope, ready_threads};
use super::threads_view::{context_line, dim, header_style, short_time, span_label, top_label};
use crate::intent::Scroll;
use crate::keymap::Reason;
use crate::model::AppState;
use crate::theme::Palette;
use kika_forge::anchor::Placement;
use kika_forge::discussion::PrDiscussion;
use kika_git::safe_text::sanitize;

const NO_FILE_THREADS: &str = "No review threads on this file";
const NO_MORE: &str = "No more threads on this file";

/// The open reader: the file's threads by index into the discussion's,
/// which of them is shown, its first line shown, and its lines as drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadReader {
    pub threads: Vec<usize>,
    pub at: usize,
    pub scroll: usize,
    pub rendered: Option<RenderedBody>,
}

impl ThreadReader {
    /// The thread shown, by index into the discussion's.
    pub fn shown(&self) -> Option<usize> {
        self.threads.get(self.at).copied()
    }
}

/// The reader is open over the files screen, so it has the keys.
pub(crate) fn thread_reader_open(state: &AppState) -> bool {
    state.review_files_shown() && files_screen(state).is_some_and(|f| f.reader.is_some())
}

/// The selected file's threads in the order `]` visits them, the top
/// block first, resolved ones included.
fn file_order(files: &PrFiles, discussion: &PrDiscussion) -> Vec<usize> {
    let Some(row) = files.selected() else {
        return Vec::new();
    };
    let threads = ThreadScope::of(files, discussion).file_threads(&row.file.path, None);
    threads
        .top
        .into_iter()
        .chain(threads.lines.into_iter().map(|(_, t)| t))
        .collect()
}

/// Why `enter` cannot read a thread: the files are not shown and listed,
/// the threads are loading or failed, or the file has none.
pub(crate) fn thread_reader_disabled(state: &AppState) -> Option<Reason> {
    match ready_threads(state) {
        Err(reason) => Some(reason),
        Ok((files, discussion)) => file_order(files, discussion)
            .is_empty()
            .then_some(NO_FILE_THREADS.into()),
    }
}

/// Where a thread is, as the files screen says it.
fn where_label(scope: &ThreadScope, t: usize) -> String {
    let Some(thread) = scope.discussion.threads.get(t) else {
        return String::new();
    };
    match scope.placement(t) {
        Some(Placement::Line { .. }) => span_label(thread).unwrap_or_else(|| "line".into()),
        Some(Placement::OtherView) => "on the whole pull request".into(),
        Some(placement) => top_label(thread, placement),
        None => String::new(),
    }
}

/// The shown thread's file and place, as its header names them, made
/// safe to draw: what a reply's composer says it answers.
pub(super) fn thread_label(files: &PrFiles, discussion: &PrDiscussion, t: usize) -> String {
    let path = files
        .selected()
        .map(|row| sanitize(row.file.path.as_bytes()))
        .unwrap_or_default();
    let scope = ThreadScope::of(files, discussion);
    let place = sanitize(where_label(&scope, t).as_bytes());
    format!("{path} · {place}")
}

/// Every line of thread `t` of the file at `path`, at `width`: a header,
/// the lines it was written on when it is not on a line here, then each
/// comment in full, and how many more GitHub has.
pub fn reader_lines(
    palette: &Palette,
    scope: &ThreadScope,
    t: usize,
    path: &str,
    width: usize,
) -> Vec<Line<'static>> {
    let Some(thread) = scope.discussion.threads.get(t) else {
        return Vec::new();
    };
    let total = thread.comments_total.max(thread.comments.len() as u64);
    let count = if total == 1 {
        "1 comment".to_string()
    } else {
        format!("{total} comments")
    };
    let state = if thread.resolved {
        "resolved"
    } else {
        "unresolved"
    };
    let header = format!("{path} · {} · {state} · {count}", where_label(scope, t));
    let look = header_style(palette, thread, scope.placement(t));
    let mut lines = markdown::wrap_text(&header, look.add_modifier(Modifier::BOLD), width);
    if !matches!(scope.placement(t), Some(Placement::Line { .. })) {
        lines.extend(
            thread
                .context
                .iter()
                .map(|c| context_line(palette, c, width)),
        );
    }
    lines.push(Line::default());
    for comment in &thread.comments {
        lines.push(Line::from(vec![
            Span::styled(
                comment.author.clone(),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" · {}", short_time(&comment.created)), dim()),
        ]));
        lines.extend(markdown::render(&comment.body, width));
        lines.push(Line::default());
    }
    let more = total.saturating_sub(thread.comments.len() as u64);
    if more > 0 {
        let what = if more == 1 { "comment" } else { "comments" };
        lines.push(Line::styled(format!("{more} more {what} on GitHub"), dim()));
    }
    lines
}

impl AppState {
    /// The reader's inside at the current screen size.
    fn reader_inner(&self) -> ratatui::layout::Rect {
        super::reader_view::reader_inner(self.review_area())
    }

    /// `enter`: read the thread `]`/`[` went to when it is on this file,
    /// else the file's first.
    pub(crate) fn open_thread_reader(&mut self) {
        if let Some(reason) = thread_reader_disabled(self) {
            self.notice = Some(reason);
            return;
        }
        let Ok((files, discussion)) = ready_threads(self) else {
            return;
        };
        let threads = file_order(files, discussion);
        let at = files
            .thread
            .and_then(|t| threads.iter().position(|&x| x == t))
            .unwrap_or(0);
        if let Some(files) = &mut self.review.files {
            files.reader = Some(ThreadReader {
                threads,
                at,
                scroll: 0,
                rendered: None,
            });
        }
        self.render_thread_reader();
    }

    /// Render the shown thread for the reader's width, once: when it opens
    /// or steps, and when the screen is resized.
    pub(super) fn render_thread_reader(&mut self) {
        let width = usize::from(self.reader_inner().width);
        let (Some(files), Some(discussion)) = (&self.review.files, &self.review.discussion) else {
            return;
        };
        let Some(reader) = &files.reader else {
            return;
        };
        if let Some(drawn) = reader.rendered.as_ref().filter(|r| r.width == width) {
            // A taller screen may have room for lines scrolled past.
            let max = self.reader_max_scroll(drawn.lines.len());
            if let Some(reader) = self.review.files.as_mut().and_then(|f| f.reader.as_mut()) {
                reader.scroll = reader.scroll.min(max);
            }
            return;
        }
        let path = files
            .selected()
            .map(|row| row.file.path.display())
            .unwrap_or_default();
        let lines = match reader.shown() {
            Some(t) => reader_lines(
                &self.palette,
                &ThreadScope::of(files, discussion),
                t,
                path,
                width,
            ),
            None => Vec::new(),
        };
        let max = self.reader_max_scroll(lines.len());
        if let Some(reader) = self.review.files.as_mut().and_then(|f| f.reader.as_mut()) {
            reader.rendered = Some(RenderedBody { width, lines });
            reader.scroll = reader.scroll.min(max);
        }
    }

    fn reader_max_scroll(&self, lines: usize) -> usize {
        lines.saturating_sub(usize::from(self.reader_inner().height))
    }

    /// `j`/`k`/`pgdn`/`pgup` in the reader, kept within its lines.
    pub(crate) fn scroll_thread_reader(&mut self, by: Scroll) {
        let page = usize::from(self.reader_inner().height)
            .saturating_sub(1)
            .max(1);
        let lines = files_screen(self)
            .and_then(|f| f.reader.as_ref())
            .and_then(|r| r.rendered.as_ref())
            .map_or(0, |r| r.lines.len());
        let max = self.reader_max_scroll(lines);
        let Some(reader) = self.review.files.as_mut().and_then(|f| f.reader.as_mut()) else {
            return;
        };
        reader.scroll = match by {
            Scroll::LineDown => reader.scroll.saturating_add(1),
            Scroll::LineUp => reader.scroll.saturating_sub(1),
            Scroll::PageDown => reader.scroll.saturating_add(page),
            Scroll::PageUp => reader.scroll.saturating_sub(page),
        }
        .min(max);
    }

    /// `]`/`[` in the reader: the file's next or previous thread, without
    /// wrapping round. It becomes the selected thread, so `]` on the diff
    /// goes on from it.
    pub(crate) fn step_thread_reader(&mut self, delta: i32) {
        let Some(files) = self.review.files.as_mut() else {
            return;
        };
        let Some(reader) = files.reader.as_mut() else {
            return;
        };
        let next = isize::try_from(delta)
            .ok()
            .and_then(|d| reader.at.checked_add_signed(d))
            .filter(|&at| at < reader.threads.len());
        let Some(at) = next else {
            self.notice = Some(NO_MORE.into());
            return;
        };
        reader.at = at;
        reader.scroll = 0;
        reader.rendered = None;
        files.thread = reader.shown();
        files.jump = None;
        self.render_thread_reader();
    }

    /// Open the reader again on the thread GitHub knows as `node_id`,
    /// after the discussion was read again; nothing when it is no longer
    /// one of the selected file's threads.
    pub(super) fn reopen_thread_reader(&mut self, node_id: &str) {
        let Ok((files, discussion)) = ready_threads(self) else {
            return;
        };
        let found = file_order(files, discussion)
            .into_iter()
            .find(|&t| discussion.threads[t].node_id.as_deref() == Some(node_id));
        let Some(t) = found else {
            return;
        };
        if let Some(files) = &mut self.review.files {
            files.thread = Some(t);
        }
        self.open_thread_reader();
    }

    /// `esc`: back to the files screen as it was.
    pub(crate) fn close_thread_reader(&mut self) {
        if let Some(files) = &mut self.review.files {
            files.reader = None;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use crossterm::event::{KeyCode, KeyModifiers};

    use super::*;
    use crate::features::review::ReviewIntent;
    use crate::features::review::commit_view::tests::assert_local;
    use crate::features::review::files::{FilesFocus, ReviewView};
    use crate::features::review::state::ReviewProblem;
    use crate::features::review::state::tests::{press, type_char};
    use crate::features::review::threads::tests::{
        cache_diff, chunk, detail_then_load, files_with, fixture_discussion,
    };
    use crate::intent::AppIntent;
    use crate::keymap::{KeyPress, KeyScope};
    use kika_forge::ForgeStatus;

    pub(crate) fn reader(state: &AppState) -> &ThreadReader {
        state
            .review
            .files
            .as_ref()
            .and_then(|f| f.reader.as_ref())
            .expect("the reader is open")
    }

    /// Both fixture files listed at 100x30, cache.rs's diff streamed.
    pub(crate) fn two_files() -> AppState {
        let mut state = files_with(&["src/cache.rs", "src/lib.rs"], fixture_discussion());
        state.size = (100, 30);
        chunk(&mut state, cache_diff(), true);
        state
    }

    #[test]
    fn enter_opens_the_selected_thread_or_the_files_first() {
        // `]` went to cache.rs's line 3 thread: enter reads it.
        let mut state = two_files();
        type_char(&mut state, ']');
        assert_eq!(state.review.files.as_ref().unwrap().thread, Some(0));
        let effects = press(&mut state, KeyCode::Enter);
        assert_eq!(effects, []);
        // Old line 2 (resolved) before new line 3.
        assert_eq!(reader(&state).threads, [1, 0]);
        assert_eq!(reader(&state).shown(), Some(0));
        assert_eq!(reader(&state).scroll, 0);
        assert!(reader(&state).rendered.is_some());

        // None selected: the file's first, which is the resolved one.
        let mut state = two_files();
        press(&mut state, KeyCode::Enter);
        assert_eq!(reader(&state).shown(), Some(1));
        assert!(state.review.discussion.as_ref().unwrap().threads[1].resolved);

        // lib.rs: its top block (the outdated thread) first.
        let mut state = two_files();
        type_char(&mut state, 'j');
        state.update(AppIntent::Review(ReviewIntent::OpenThreadReader));
        assert_eq!(reader(&state).threads, [3, 2]);
        assert_eq!(reader(&state).shown(), Some(3));
        // A selection on another file is not this file's: the first.
        let mut state = two_files();
        type_char(&mut state, 'j');
        state.review.files.as_mut().unwrap().thread = Some(0);
        press(&mut state, KeyCode::Enter);
        assert_eq!(reader(&state).shown(), Some(3));
        // With the diff focused too.
        let mut state = two_files();
        press(&mut state, KeyCode::Tab);
        assert_eq!(state.review.files.as_ref().unwrap().focus, FilesFocus::Diff);
        press(&mut state, KeyCode::Enter);
        assert_eq!(reader(&state).shown(), Some(1));
    }

    #[test]
    fn the_reader_says_why_it_cannot_open() {
        // Not on the files screen, nor while they list.
        let (mut state, _) = detail_then_load();
        assert_eq!(
            thread_reader_disabled(&state).as_deref(),
            Some("Open the files first")
        );
        type_char(&mut state, 'f');
        assert_eq!(
            thread_reader_disabled(&state).as_deref(),
            Some("Open the files first")
        );
        let mut state = two_files();
        state.review.discussion_fetch.loading = true;
        state.update(AppIntent::Review(ReviewIntent::OpenThreadReader));
        assert_eq!(
            state.notice.as_deref(),
            Some("Review threads are still loading")
        );
        state.review.discussion_fetch.loading = false;
        state.review.discussion_fetch.error = Some(ReviewProblem::Status(ForgeStatus::Offline));
        press(&mut state, KeyCode::Enter);
        assert_eq!(
            state.notice.as_deref(),
            Some("Review threads could not be read")
        );
        assert!(state.review.files.as_ref().unwrap().reader.is_none());
        let mut state = files_with(&["src/other.rs"], fixture_discussion());
        press(&mut state, KeyCode::Enter);
        assert_eq!(state.notice.as_deref(), Some(NO_FILE_THREADS));
        assert!(state.review.files.as_ref().unwrap().reader.is_none());
    }

    #[test]
    fn brackets_step_through_the_files_threads_and_keys_scroll_within_it() {
        let mut state = two_files();
        state.size = (100, 12);
        state.diff.scroll = 2;
        let mut effects = press(&mut state, KeyCode::Enter);
        assert_eq!(reader(&state).shown(), Some(1));
        // At the start: no previous.
        effects.extend(type_char(&mut state, '['));
        assert_eq!(state.notice.as_deref(), Some(NO_MORE));
        assert_eq!(reader(&state).shown(), Some(1));
        // Scroll the resolved thread, then step: back to its top.
        effects.extend(type_char(&mut state, ']'));
        assert_eq!(reader(&state).shown(), Some(0));
        assert_eq!(state.review.files.as_ref().unwrap().thread, Some(0));
        let lines = reader(&state).rendered.as_ref().unwrap().lines.len();
        let rows = usize::from(state.reader_inner().height);
        let max = lines - rows;
        assert!(max > rows, "{lines} lines in {rows} rows");
        effects.extend(type_char(&mut state, 'k'));
        assert_eq!(reader(&state).scroll, 0);
        effects.extend(type_char(&mut state, 'j'));
        effects.extend(press(&mut state, KeyCode::Down));
        assert_eq!(reader(&state).scroll, 2);
        effects.extend(press(&mut state, KeyCode::PageDown));
        assert_eq!(reader(&state).scroll, 2 + rows - 1);
        for _ in 0..10 {
            effects.extend(press(&mut state, KeyCode::PageDown));
        }
        assert_eq!(reader(&state).scroll, max);
        // A taller screen of the same width pulls the scroll back.
        let mut tall = state.clone();
        tall.update(AppIntent::Resize {
            width: 100,
            height: 200,
        });
        assert_eq!(reader(&tall).scroll, 0);
        effects.extend(press(&mut state, KeyCode::Up));
        effects.extend(press(&mut state, KeyCode::PageUp));
        assert_eq!(reader(&state).scroll, max - 1 - (rows - 1));
        effects.extend(type_char(&mut state, '['));
        assert_eq!(reader(&state).shown(), Some(1));
        assert_eq!(reader(&state).scroll, 0);
        assert_eq!(state.review.files.as_ref().unwrap().thread, Some(1));
        effects.extend(type_char(&mut state, ']'));
        effects.extend(type_char(&mut state, ']'));
        assert_eq!(state.notice.as_deref(), Some(NO_MORE));
        assert_eq!(reader(&state).shown(), Some(0));
        // Esc closes it; the diff never moved.
        effects.extend(press(&mut state, KeyCode::Esc));
        assert!(state.review.files.as_ref().unwrap().reader.is_none());
        assert_eq!(state.diff.scroll, 2);
        assert_local(&effects);
        assert_eq!(effects, []);
        // `]` on the diff goes on from the thread read last.
        type_char(&mut state, ']');
        assert_eq!(state.review.files.as_ref().unwrap().thread, Some(3));
    }

    #[test]
    fn moving_the_file_or_switching_the_view_closes_the_reader() {
        let mut state = two_files();
        press(&mut state, KeyCode::Enter);
        state.review_move(1);
        assert!(state.review.files.as_ref().unwrap().reader.is_none());

        let mut state = two_files();
        press(&mut state, KeyCode::Enter);
        state.review.files.as_mut().unwrap().commits = Some((
            vec![crate::features::review::PrCommit {
                range: kika_git::commit_files::CommitRange {
                    oid: kika_git::commit_files::CommitId::parse(&"4".repeat(40)).unwrap(),
                    parent: None,
                },
                subject: "one".into(),
            }],
            false,
        ));
        state.pick_review_commit(1);
        let files = state.review.files.as_ref().unwrap();
        assert_eq!(files.view, ReviewView::Commit(0));
        assert!(files.reader.is_none());
    }

    #[test]
    fn the_reader_has_its_own_keys_and_ctrl_c_quits() {
        let mut state = two_files();
        press(&mut state, KeyCode::Enter);
        assert!(thread_reader_open(&state));
        assert_eq!(state.key_focus(), KeyScope::ReviewThread);
        // `q` and `?` are not quit and help here.
        assert_eq!(type_char(&mut state, 'q'), []);
        assert!(!state.should_quit);
        type_char(&mut state, '?');
        assert!(!state.help_open);
        assert!(thread_reader_open(&state));
        state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(state.should_quit);
    }
}
