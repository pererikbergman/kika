use crate::diff_mark::DiffMark;
use crate::diff_pane::DiffPane;
use crate::diff_search::DiffSearch;
use crate::features::blame;
use crate::features::branches;
use crate::features::branches::BranchesTab;
use crate::features::branches::remotes;
use crate::features::commit;
use crate::features::commits;
use crate::features::commits::custom_patch::{self, CustomPatch};
use crate::features::conflicts;
use crate::features::diff::renderers::view::{render_rendered, tool_title};
use crate::features::diff::view::{hunk_title, hunk_title_parts, options_note, render_diff_pane};
use crate::features::operation;
use crate::features::stash;
use crate::git::FileEntry;
use crate::git::bisect::BisectState;
use crate::git::diff::{DiffLine, DiffOptions, EmojiZwj, LineKind, sanitize};
use crate::git::emoji;
use crate::git::patch::{FileKind, PatchError};
use crate::git::remote::RemoteOp;
use crate::git::status::{EntryKind, Head};
use crate::hunk_view::{HunkView, NO_CHANGES, RowKind, drawn_text};
use crate::keymap::{Binding, KeyPattern, KeyScope, available, available_commands, registry};
use crate::layout::{Panel, SidePanel, help_rows, workspace_layout};
use crate::model::{AppState, Context, Focus};
use crate::popup_view;
use crate::remote_run::RemoteRun;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};

pub(crate) use crate::layout::screen_layout;

/// Draw the state. Reads the model only; never mutates it.
pub fn render(state: &AppState, frame: &mut Frame) {
    let [main, status] = screen_layout(frame.area());

    match state.context {
        Context::Workspace => render_workspace(state, frame, main),
        Context::Review { .. } => crate::features::review::view::render(state, frame, main),
    }
    if state.help_open {
        render_help(state, frame, main);
    }
    commit::view::render_commit(&state.commit, state.commit_on_terminal, frame, main);
    popup_view::render_popups(state, frame, main);
    let (bar, cursor) = status_bar(state, status.width);
    frame.render_widget(bar, status);
    if let Some(col) = cursor.and_then(|col| u16::try_from(col).ok())
        && col < status.width
    {
        frame.set_cursor_position((status.x + col, status.y));
    }
}

/// Every panel the layout has room for: the left column, then the diff
/// pane showing what the focus asks for.
fn render_workspace(state: &AppState, frame: &mut Frame, main: Rect) {
    let layout = workspace_layout(main, state.focus.panel(), state.expanded, state.screen_mode);
    for side in SidePanel::ALL {
        let Some(area) = layout.rect(Panel::Side(side)) else {
            continue;
        };
        let look = PanelLook::of(state, Panel::Side(side), area);
        let expanded = side == state.expanded;
        match side {
            SidePanel::Status => {
                operation::view::render_status_panel(state, &look, expanded, frame, area)
            }
            SidePanel::Files => render_files_panel(state, &look, expanded, frame, area),
            SidePanel::Branches => {
                let spinner = state
                    .remote
                    .as_ref()
                    .and_then(|run| Some((run.branch.as_ref()?, run.spinner())));
                let diff = match &state.diff_mark {
                    Some(DiffMark::Branch(name)) => Some(name),
                    _ => None,
                };
                let marks = branches::view::RowMarks { spinner, diff };
                branches::view::render_panel(
                    &state.branches,
                    &state.forge,
                    state.now,
                    &look,
                    expanded,
                    marks,
                    frame,
                    area,
                )
            }
            SidePanel::Commits => {
                commits::view::render_commits_panel(state, &look, expanded, frame, area)
            }
            SidePanel::Stash => {
                stash::view::render_stash_panel(&state.stash, &look, expanded, frame, area)
            }
        }
    }
    let Some(area) = layout.rect(Panel::Diff) else {
        return;
    };
    let look = PanelLook::of(state, Panel::Diff, area);
    let options = state.diff_options;
    let label = state.active_renderer().map(|r| r.label.as_str());
    // R steps to the next renderer before it gets back to built-in.
    let last = state
        .renderer
        .is_some_and(|i| i + 1 >= state.renderers.len());
    // R is a Diff pane key; `l` reaches the diff from every side panel.
    let hint = match (state.focus.is_side(), last) {
        (false, true) => "Press R for the built-in diff",
        (false, false) => "Press R for the next renderer",
        (true, true) => "Press l, then R, for the built-in diff",
        (true, false) => "Press l, then R, for the next renderer",
    };
    match (&state.focus, state.rendered_shown(), label) {
        // What the selected branch holds, before acting on it.
        // A remote's header row has no log; what the remote is instead.
        (Focus::Branches, ..)
            if state.branches.tab == BranchesTab::Remotes
                && state.branches.remotes.selected_branch().is_none() =>
        {
            remotes::view::render_remote(&state.branches.remotes, &look, frame, area)
        }
        (Focus::Branches, ..) => branches::view::render_log(&state.branches, &look, frame, area),
        // A commit's file, streamed like a working-tree diff; never rendered.
        (Focus::Commits, ..) if state.commits.open.is_some() => {
            render_diff_pane(&state.diff, options, &look, frame, area)
        }
        // A stash's file, the same way.
        (Focus::Stash, ..) if state.stash.open.is_some() => {
            render_diff_pane(&state.diff, options, &look, frame, area)
        }
        // A tool's rows show no patch lines, so no cursor is drawn on them.
        (Focus::Diff(view), Some(rendered), Some(label)) if state.display_only() => {
            let (side, tail) = hunk_title_parts(view, options);
            let path = view.path.display();
            let block = look.block(tool_title(&look, side, path, &tail, label));
            render_rendered(rendered, block, None, hint, frame, area);
        }
        (Focus::Diff(view), Some(rendered), Some(label)) => {
            let block = look.block(hunk_title(view, options, label, &look));
            render_rendered(rendered, block, Some(view), hint, frame, area);
        }
        (Focus::Diff(view), None, Some(label)) if state.rendered_for_shown().is_some() => {
            // Short, so it fits the pane's title at 80 columns.
            let suffix = format!("{label} (unmapped)");
            let view = HunkPane { view, picks: None };
            render_hunk_view(view, options, state.emoji_zwj, &suffix, &look, frame, area)
        }
        (Focus::Diff(view), ..) => {
            let picks = state
                .custom_patch
                .as_ref()
                .filter(|p| view.commit.as_ref().is_some_and(|c| p.is_from(c)));
            let view = HunkPane { view, picks };
            render_hunk_view(view, options, state.emoji_zwj, "", &look, frame, area)
        }
        (Focus::Conflict(view), ..) => {
            conflicts::view::render(view, &state.keymap, &look, frame, area)
        }
        (Focus::Blame(view), ..) => match &view.commit {
            Some(commit) => blame::view::render_commit_view(commit, &look, frame, area),
            None => blame::view::render_blame(view, &look, frame, area),
        },
        (_, Some(rendered), Some(label)) if state.display_only() => {
            let (path, tail) = (rendered.path.display(), options_note(options));
            let block = look.block(tool_title(&look, "Diff", path, &tail, label));
            render_rendered(rendered, block, None, hint, frame, area);
        }
        (_, Some(rendered), Some(label)) => {
            let tail = format!("{} · {label}", options_note(options));
            let block = look.block(look.path_title("Diff", rendered.path.display(), &tail));
            render_rendered(rendered, block, None, hint, frame, area);
        }
        _ => render_diff_pane(&state.diff, options, &look, frame, area),
    }
}

/// How a panel's frame is drawn this time: whether it has the keys, the
/// screen mode its title names, and whether it has room for more than a
/// title row.
pub(crate) struct PanelLook {
    pub(crate) number: u8,
    pub(crate) focused: bool,
    /// ` · half` or ` · full` on the focused panel, else empty.
    pub(crate) mode: String,
    /// Columns of title between the corners.
    pub(crate) title_width: usize,
    /// No bottom border: no room for one.
    pub(crate) top_only: bool,
    /// One row: the title is all there is, so it carries the summary.
    pub(crate) title_only: bool,
}

impl PanelLook {
    fn of(state: &AppState, panel: Panel, area: Rect) -> Self {
        let focused = state.focus.panel() == panel;
        let mode = match state.screen_mode.note() {
            Some(note) if focused => format!(" · {note}"),
            _ => String::new(),
        };
        Self {
            number: panel.number(),
            focused,
            mode,
            title_width: usize::from(area.width.saturating_sub(2)),
            top_only: area.height < 3,
            title_only: area.height < 2,
        }
    }

    /// ` [n] <text><mode> `, fitted to the border: the padding goes
    /// first, then `text` loses its end to `…`. The key and the mode stay.
    pub(crate) fn title(&self, text: &str) -> String {
        let key = format!("[{}]", self.number);
        let full = format!(" {key} {text}{} ", self.mode);
        if width_of(&full) <= self.title_width {
            return full;
        }
        let bare = format!("{key} {text}{}", self.mode);
        if width_of(&bare) <= self.title_width {
            return bare;
        }
        let room = self
            .title_width
            .saturating_sub(width_of(&key) + 1 + width_of(&self.mode));
        if room == 0 {
            return elide_right(&key, self.title_width);
        }
        format!("{key} {}{}", elide_right(text, room), self.mode)
    }

    /// Columns of title between the corners.
    pub(crate) fn title_room(&self) -> usize {
        self.title_width
    }

    /// Whether ` [n] <text><mode> ` fits the border whole.
    pub(crate) fn fits(&self, text: &str) -> bool {
        width_of(&format!(" [{}] {text}{} ", self.number, self.mode)) <= self.title_width
    }

    /// Whether `[n] <text><mode>` fits the border, with no space either side.
    pub(crate) fn fits_bare(&self, text: &str) -> bool {
        width_of(&format!("[{}] {text}{}", self.number, self.mode)) <= self.title_width
    }

    /// ` [n] <head> <path><tail><mode> `, the path losing its start so
    /// the rest of the title fits.
    pub(crate) fn path_title(&self, head: &str, path: &str, tail: &str) -> String {
        let padded = width_of(&format!(" [{}] {head} {tail}{} ", self.number, self.mode));
        // With no room for even `…` beside the padding, `title` drops the
        // padding; give the path that room.
        let fixed = if padded < self.title_width {
            padded
        } else {
            padded.saturating_sub(2)
        };
        let room = self.title_width.saturating_sub(fixed).max(1);
        self.title(&format!("{head} {}{tail}", elide_left(path, room)))
    }

    /// The panel's frame with `title`. A panel too short for both borders
    /// drops its bottom one, so the title still shows; the corners and
    /// sides stay so it lines up with the panels around it.
    pub(crate) fn block(&self, title: String) -> Block<'static> {
        let block = panel_block(title, self.focused);
        if self.top_only {
            block.borders(Borders::TOP | Borders::LEFT | Borders::RIGHT)
        } else {
            block
        }
    }
}

/// A panel's frame: focused, a thick coloured border and a bold title, so
/// the panel with the keys stands out without colour too; else a thin dim
/// border.
pub(crate) fn panel_block(title: String, focused: bool) -> Block<'static> {
    let block = Block::bordered().title(title);
    if focused {
        block
            .border_type(BorderType::Thick)
            .title_style(Style::new().add_modifier(Modifier::BOLD))
            .border_style(Style::new().fg(FOCUSED))
    } else {
        block.border_style(Style::new().fg(Color::DarkGray))
    }
}

/// The bisect under the "bisecting" marker: its marks and outcome, the
/// commit being tested, and where it started. A commit is shown as tested
/// once `HEAD` is detached, as the Status panel's mark keys need. Every
/// string here is already terminal-safe.
pub(crate) fn bisect_lines(
    state: &AppState,
    head: &Head,
    bisect: &BisectState,
    width: usize,
) -> Vec<Line<'static>> {
    let row = |text: String| Line::raw(elide_right(&text, width));
    let mut lines = vec![row(format!(
        "Bisect: {} good · {} bad · {} skipped",
        bisect.good, bisect.bad, bisect.skip
    ))];
    // The outcome comes right after the counts, so a short panel clips
    // the details before it.
    if let Some(found) = &bisect.first_bad {
        let short = state.commits.short_oid(&found.commit);
        lines.push(Line::styled(
            elide_right(
                &format!("{FOUND_MARK} Found: {short} {}", found.subject),
                width,
            ),
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
    } else if bisect.only_skipped {
        lines.push(row("Only skipped commits left".to_string()));
    }
    lines.push(row(match head {
        Head::Detached(oid) => match state.commits.rows.iter().find(|r| &r.oid == oid) {
            Some(commit) => format!("Testing: {} {}", commit.short_oid, commit.subject),
            None => format!("Testing: {}", short_hash(oid)),
        },
        _ => "Mark a good and a bad commit".to_string(),
    }));
    if !bisect.start.is_empty() {
        lines.push(row(format!("Started from: {}", bisect.start)));
    }
    if !bisect.terms.is_default() {
        lines.push(row(format!(
            "Terms: {}/{}",
            bisect.terms.bad, bisect.terms.good
        )));
    }
    lines
}

/// Marks the found commit's line, so it stands out without colour too.
const FOUND_MARK: &str = "◆";

/// `name` then `suffix` in `width` columns. The name gives way first, so
/// the suffix (counts, markers) is cut only when it cannot fit at all.
pub(crate) fn fit(name: &str, suffix: &str, width: usize) -> String {
    let room = width.saturating_sub(width_of(suffix)).max(1);
    format!("{}{suffix}", elide_left(name, room))
}

/// Fit `text` into `max` columns by replacing its end with `…`.
pub(crate) fn elide_right(text: &str, max: usize) -> String {
    if width_of(text) <= max {
        return text.to_string();
    }
    let mut used = 0;
    let kept: String = text
        .chars()
        .take_while(|c| {
            used += width_of(c.encode_utf8(&mut [0; 4]));
            used < max
        })
        .collect();
    if max == 0 {
        String::new()
    } else {
        format!("{kept}…")
    }
}

/// Commit ids are shown at this length; enough to recognise one.
const SHORT_HASH: usize = 7;

pub(crate) fn short_hash(oid: &str) -> &str {
    oid.char_indices()
        .nth(SHORT_HASH)
        .map_or(oid, |(end, _)| &oid[..end])
}

/// The border colour of the panel or commit box that has the keys.
pub(crate) const FOCUSED: Color = Color::Cyan;

/// Popups wider than this are harder to read; it is a common message width.
pub(crate) const COMMIT_WIDTH: u16 = 72;

/// A small popup asking `question`, wrapped to fit and grown to hold it,
/// so the answer keys at its end are never cut off.
pub(crate) fn render_question(title: &str, question: &str, frame: &mut Frame, area: Rect) {
    let widest = question.lines().map(width_of).max().unwrap_or(0);
    let width = u16::try_from(widest + 4)
        .unwrap_or(u16::MAX)
        .min(area.width);
    // One column of padding on each side inside the border.
    let rows = wrap(question, usize::from(width.saturating_sub(4)));
    let height = u16::try_from(rows.len() + 2).unwrap_or(u16::MAX);
    let popup = centered(area, width, height);
    let lines: Vec<Line> = rows
        .into_iter()
        .map(|row| Line::raw(format!(" {row}")))
        .collect();
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title)),
        popup,
    );
}

/// One terminal cell of clipped text.
#[derive(Clone)]
enum Cell {
    Blank,
    Glyph(String),
    /// The right half of the wide glyph before it.
    Rest,
}

/// The glyph `clip` is building: what joins it may make it wider.
struct Glyph {
    start: usize,
    width: usize,
    text: String,
}

/// For each char of `line`, whether it continues the glyph before it
/// instead of starting one: a zero-width char, an emoji that a ZWJ or a
/// skin tone joins to a glyph starting with an emoji, or a regional
/// indicator that pairs with a lone one before it into a flag. Indicators
/// pair left to right, so an odd one out stands alone. `clip` lays glyphs
/// out by it and the search marks snap to it, so a mark never cuts a glyph
/// the terminal draws as one. A zero-width char with no glyph before it
/// continues nothing.
fn continues_glyph(line: &str) -> Vec<bool> {
    let mut glyph_is_emoji = None;
    let mut before = None;
    // The glyph so far is a single regional indicator, waiting for its pair.
    let mut open_flag = false;
    line.chars()
        .map(|c| {
            let flag = emoji::is_regional_indicator(c);
            let joins = glyph_is_emoji == Some(true)
                && (emoji::is_pictographic(c) && before == Some('\u{200D}')
                    || emoji::is_modifier(c) && before.is_some_and(emoji::is_pictographic));
            let pairs = open_flag && flag;
            let zero = width_of(c.encode_utf8(&mut [0; 4])) == 0;
            let continues = glyph_is_emoji.is_some() && (joins || pairs || zero);
            if !continues && !zero {
                glyph_is_emoji = Some(emoji::is_pictographic(c));
            }
            open_flag = flag && !continues;
            before = Some(c);
            continues
        })
        .collect()
}

impl Glyph {
    /// The cells it covers in `cells`, which show columns from `from`.
    fn span(&self, cells: &[Cell], from: usize) -> Option<std::ops::Range<usize>> {
        let at = self.start.checked_sub(from)?;
        (at < cells.len()).then(|| at..(at + self.width).min(cells.len()))
    }

    /// Draw it into `cells` if it fits whole; a glyph cut by the edge
    /// leaves its cells blank.
    fn place(&self, cells: &mut [Cell], from: usize) {
        let Some(span) = self.span(cells, from) else {
            return;
        };
        if span.len() < self.width {
            cells[span].fill(Cell::Blank);
            return;
        }
        cells[span.start] = Cell::Glyph(self.text.clone());
        cells[span.start + 1..span.end].fill(Cell::Rest);
    }

    fn blank(&self, cells: &mut [Cell], from: usize) {
        if let Some(span) = self.span(cells, from) {
            cells[span].fill(Cell::Blank);
        }
    }
}

/// Columns `from..from + width` of `line`. A wide glyph cut by an edge
/// becomes blank, and a `…` replaces the edge cell where text is cut off.
/// An emoji ZWJ sequence, or an emoji with a skin-tone modifier, is one
/// glyph, as the terminal buffer clusters it: what follows the first emoji
/// joins it. Script joiners do not join that way. A glyph is as wide as
/// ratatui measures all of it, so `❤` grows from one column to two when a
/// U+FE0F joins it.
pub(crate) fn clip(line: &str, from: usize, width: usize) -> String {
    clip_cut(line, from, width).0
}

/// [`clip`], and whether the text ran past the right edge, so its last
/// cell is the `…`.
fn clip_cut(line: &str, from: usize, width: usize) -> (String, bool) {
    let mut cells = vec![Cell::Blank; width];
    let mut col: usize = 0;
    let mut last: Option<Glyph> = None;
    for (c, continues) in line.chars().zip(continues_glyph(line)) {
        let w = width_of(c.encode_utf8(&mut [0; 4]));
        if let Some(glyph) = last.as_mut().filter(|_| continues) {
            // A combining mark or a joined emoji stays with the glyph before it.
            // It is redrawn at its new width: `🏳️‍⚧` measures 3 until the
            // last U+FE0F brings it back to 2.
            glyph.blank(&mut cells, from);
            glyph.text.push(c);
            glyph.width = width_of(&glyph.text);
            col = glyph.start + glyph.width;
            glyph.place(&mut cells, from);
            continue;
        }
        if w == 0 {
            continue;
        }
        let glyph = Glyph {
            start: col,
            width: w,
            text: c.to_string(),
        };
        glyph.place(&mut cells, from);
        last = Some(glyph);
        col += w;
    }
    if width > 0 && from > 0 && col > 0 {
        if let Some(Cell::Rest) = cells.get(1) {
            cells[1] = Cell::Blank;
        }
        cells[0] = Cell::Glyph("…".into());
    }
    let cut = width > 0 && col > from + width;
    if cut {
        if let Cell::Rest = cells[width - 1] {
            cells[width - 2] = Cell::Blank;
        }
        cells[width - 1] = Cell::Glyph("…".into());
    }
    let text = cells
        .iter()
        .map(|cell| match cell {
            Cell::Blank => " ",
            Cell::Glyph(glyph) => glyph,
            Cell::Rest => "",
        })
        .collect::<String>()
        .trim_end()
        .to_string();
    (text, cut)
}

/// `text` broken into rows of at most `width` columns, at a space where
/// one fits, else mid-word. Leading indentation is kept.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let mut rest = line;
        let first = rows.len();
        while width_of(rest) > width {
            let mut used = 0;
            let mut cut = 0;
            let mut space = None;
            for (i, c) in rest.char_indices() {
                used += width_of(c.encode_utf8(&mut [0; 4]));
                if used > width {
                    // A space just past the edge is the best place to break.
                    if c == ' ' {
                        space = Some(i);
                    }
                    break;
                }
                cut = i + c.len_utf8();
                if c == ' ' && i > 0 {
                    space = Some(i);
                }
            }
            // A glyph wider than the row still has to go somewhere.
            let cut = cut.max(rest.chars().next().map_or(0, char::len_utf8));
            let (row, next) = match space {
                Some(i) => (&rest[..i], &rest[i + 1..]),
                None => (&rest[..cut], &rest[cut..]),
            };
            rows.push(row.to_string());
            rest = next;
        }
        // An empty line is one row; an empty tail after a break is none.
        if !rest.is_empty() || rows.len() == first {
            rows.push(rest.to_string());
        }
    }
    rows
}

/// The last `room` of `rows`. When they do not all fit, the first row
/// says how many earlier ones are cut, since the end of git's output is
/// usually where it says what went wrong.
pub(crate) fn tail(rows: &[String], room: usize) -> Vec<Line<'_>> {
    let mut lines: Vec<Line> = Vec::new();
    let mut shown = rows;
    if rows.len() > room && room >= 2 {
        shown = &rows[rows.len() - (room - 1)..];
        lines.push(Line::styled(
            format!("… {} earlier lines", rows.len() - shown.len()),
            Style::new().fg(Color::DarkGray),
        ));
    } else if rows.len() > room {
        shown = &rows[rows.len() - room..];
    }
    lines.extend(shown.iter().map(|row| Line::raw(row.as_str())));
    lines
}

/// The fewest columns typed text keeps on the bar while a notice shows.
const TYPED_FLOOR: usize = 8;

/// The busy indicator and, while a command runs, the key that stops it;
/// then why the last key did nothing, else the last operation's error, else
/// the key hints. The indicator goes first so a narrow bar clips the rest,
/// not it. Hints come from the keymap, so the bar always matches what the
/// keys do; disabled ones are dimmed. An error wider than the bar's `width`
/// columns ends in `…`.
///
/// While a filter or search is typed, `/` and the text come after the
/// indicator, and the column of its cursor is returned too. A notice
/// takes its room before the text, so a full input can say why.
fn status_bar(state: &AppState, width: u16) -> (Line<'_>, Option<usize>) {
    let mut spans = Vec::new();
    let mut cursor = None;
    let mut room = usize::from(width);
    // A fetch, pull or push says which, and a tag push where to;
    // anything else is loading.
    let busy = match &state.remote {
        Some(RemoteRun {
            op: RemoteOp::PushTag { remote, .. },
            ..
        }) => Some(format!(" pushing tag to {}… ", remote.display())),
        Some(run) => Some(format!(" {} ", run.op.doing())),
        None => (state.busy > 0).then(|| " loading… ".to_string()),
    };
    if let Some(busy) = busy {
        room = room.saturating_sub(width_of(&busy));
        spans.push(Span::raw(busy));
    }
    if state.command_running
        && let Some(cancel) = cancel_hint(state)
    {
        room = room.saturating_sub(width_of(&cancel) + 1);
        spans.push(Span::raw(format!(" {cancel}")));
    }
    // Found before the input is laid out, so a full input leaves the
    // notice room to say why the key did nothing.
    let remote_notice = state.remote.as_ref().and_then(|run| run.notice.as_deref());
    let notice = state
        .notice
        .as_deref()
        .or(state.notice_text.as_deref())
        .or(remote_notice);
    if let Some(input) = state
        .input
        .as_ref()
        .filter(|_| state.key_focus() == KeyScope::Input)
    {
        // Typed text is drawn as it would be in a list: never raw. Text
        // wider than the bar keeps the part around the cursor, so the
        // cursor never leaves the screen.
        let before: String = input.line.text.chars().take(input.line.col).collect();
        let after: String = input.line.text.chars().skip(input.line.col).collect();
        let (before, after) = (sanitize(before.as_bytes()), sanitize(after.as_bytes()));
        let prompt = input.target.prompt();
        // ` ` and the prompt, then a cell for the cursor after the text.
        let text_room = room.saturating_sub(width_of(prompt) + 2);
        // A notice keeps its room, but the text keeps a few cells.
        let text_room = notice
            .map_or(text_room, |notice| {
                text_room
                    .saturating_sub(width_of(notice) + 1)
                    .max(text_room.min(TYPED_FLOOR))
            })
            .max(1);
        let before = if width_of(&before) + width_of(&after) <= text_room {
            before
        } else {
            elide_left(&before, text_room)
        };
        let after = elide_right(&after, text_room.saturating_sub(width_of(&before)));
        let used = usize::from(width).saturating_sub(room);
        cursor = Some(used + 1 + width_of(prompt) + width_of(&before));
        let typed = format!(" {prompt}{before}{after}");
        room = room.saturating_sub(width_of(&typed) + 1);
        spans.push(Span::styled(
            typed,
            Style::new().add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(notice) = notice {
        spans.push(Span::styled(
            elide_right(&format!(" {notice}"), room),
            Style::new().fg(Color::Yellow),
        ));
    } else if let Some(error) = &state.op_error {
        let text = format!(" error: {}", error.lines().next().unwrap_or_default());
        spans.push(Span::styled(
            elide_right(&text, room),
            Style::new().fg(Color::Red),
        ));
    } else {
        let focus = state.key_focus();
        let note = focus.bar_note();
        if let Some(note) = note {
            spans.push(Span::styled(
                format!(" {note}"),
                Style::new().fg(Color::DarkGray),
            ));
        }
        let mut room = room.saturating_sub(note.map_or(0, |n| width_of(n) + 1));
        let copied = copied_note(state);
        if let Some(copied) = &copied {
            spans.push(Span::styled(
                format!(" {copied}"),
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ));
            room = room.saturating_sub(width_of(copied) + 1);
        }
        // While commits are copied, paste and clear lead the hints, so a
        // narrow bar keeps them; so does Esc while it clears something.
        // The filter, range and search keys come last, and go first on a
        // narrow bar.
        let mut leading: Vec<&str> = if copied.is_some() {
            COPIED_HINTS.to_vec()
        } else {
            Vec::new()
        };
        let (more_leading, trailing) = list_hints(state, focus);
        leading.extend(more_leading);
        let mut available = available(registry(), &state.keymap, focus, state);
        available.retain(|(binding, _)| {
            binding.bar || leading.contains(&binding.id) || trailing.contains(&binding.id)
        });
        // Help and quit stay at the end, as `fit_hints` keeps them.
        // The leading ones keep the order they are listed in.
        available.sort_by_key(|(binding, _)| {
            if let Some(at) = leading.iter().position(|id| *id == binding.id) {
                (0, at)
            } else if pinned(binding) {
                (3, 0)
            } else if trailing.contains(&binding.id) {
                (2, 0)
            } else {
                (1, 0)
            }
        });
        let hints: Vec<(String, bool, bool)> = available
            .into_iter()
            .map(|(binding, disabled)| {
                let key = state.keymap.keys(binding).first().map(KeyPattern::label);
                let label = if binding.scope == focus && binding.id.ends_with(".escape") {
                    esc_label(state, focus).unwrap_or(binding.label)
                } else {
                    binding.label
                };
                let hint = format!("{} {label}", key.unwrap_or_default());
                (hint, disabled.is_some(), pinned(binding))
            })
            .collect();
        for (i, (hint, disabled)) in fit_hints(&hints, room).into_iter().enumerate() {
            let first = i == 0 && note.is_none() && copied.is_none();
            spans.push(Span::raw(if first { " " } else { "  " }));
            spans.push(Span::styled(hint, dimmed_if(disabled)));
        }
    }
    (Line::from(spans), cursor)
}

/// The filter, range and search hints for `focus`: those that lead the
/// bar, as Esc does while it has a range or filter to clear, and those
/// that close it.
fn list_hints(state: &AppState, focus: KeyScope) -> (Vec<&'static str>, Vec<&'static str>) {
    match focus {
        KeyScope::Files => {
            let clears = state.files_anchor.is_some() || !state.files_filter.is_empty();
            let mut leading = if searching(state, focus) {
                vec!["files.search_next", "files.search_prev"]
            } else {
                vec![]
            };
            if clears || searching(state, focus) {
                leading.push("files.escape");
            }
            (
                leading,
                vec!["files.filter", "files.range", "files.search_diff"],
            )
        }
        KeyScope::Commits => {
            let panel = &state.commits;
            let clears = panel.range_anchor.is_some() || !panel.filter.is_empty();
            let leading = if clears {
                vec!["commits.escape"]
            } else {
                vec![]
            };
            (leading, vec!["commits.filter", "commits.range"])
        }
        KeyScope::Branches | KeyScope::Remotes | KeyScope::Tags => {
            let panel = &state.branches;
            let (clears, ids) = match focus {
                KeyScope::Branches => (
                    panel.range_anchor.is_some() || !panel.filter.is_empty(),
                    ["branches.escape", "branches.filter", "branches.range"],
                ),
                KeyScope::Remotes => (
                    panel.remotes.range_anchor.is_some() || !panel.remotes.filter.is_empty(),
                    ["remotes.escape", "remotes.filter", "remotes.range"],
                ),
                _ => (
                    panel.tags.range_anchor.is_some() || !panel.tags.filter.is_empty(),
                    ["tags.escape", "tags.filter", "tags.range"],
                ),
            };
            let leading = if clears { vec![ids[0]] } else { vec![] };
            (leading, vec![ids[1], ids[2]])
        }
        KeyScope::Reflog => {
            let leading = if state.commits.reflog.filter.is_empty() {
                vec![]
            } else {
                vec!["reflog.escape"]
            };
            (leading, vec!["reflog.filter"])
        }
        KeyScope::CommitFiles => {
            let [search, next, prev, escape] = SEARCH_KEYS[1];
            let mut leading = if searching(state, focus) {
                vec![next, prev]
            } else {
                vec![]
            };
            if commit_files_filtered(state) || searching(state, focus) {
                leading.push(escape);
            }
            (leading, vec!["commit_files.filter", search])
        }
        KeyScope::Stash => {
            let list = &state.stash;
            let leading = if list.range_anchor.is_some() || !list.filter.is_empty() {
                vec!["stash.escape"]
            } else {
                vec![]
            };
            (leading, vec!["stash.filter", "stash.range"])
        }
        KeyScope::Diff | KeyScope::StashFiles | KeyScope::Blame | KeyScope::BlameCommit => {
            let [search, next, prev, escape] = match focus {
                KeyScope::Diff => SEARCH_KEYS[0],
                KeyScope::StashFiles => SEARCH_KEYS[2],
                KeyScope::Blame => SEARCH_KEYS[3],
                _ => SEARCH_KEYS[4],
            };
            if searching(state, focus) {
                (vec![next, prev, escape], vec![search])
            } else {
                (vec![], vec![search])
            }
        }
        _ => (vec![], vec![]),
    }
}

/// Per pane with a search: its open, next, previous and Esc bindings.
const SEARCH_KEYS: [[&str; 4]; 5] = [
    [
        "diff.search",
        "diff.search_next",
        "diff.search_prev",
        "diff.escape",
    ],
    [
        "commit_files.search",
        "commit_files.search_next",
        "commit_files.search_prev",
        "commit_files.escape",
    ],
    [
        "stash_files.search",
        "stash_files.search_next",
        "stash_files.search_prev",
        "stash_files.escape",
    ],
    [
        "blame.search",
        "blame.search_next",
        "blame.search_prev",
        "blame.escape",
    ],
    [
        "blame_commit.search",
        "blame_commit.search_next",
        "blame_commit.search_prev",
        "blame_commit.escape",
    ],
];

/// The pane the keys of `focus` search has a search: the focused Diff
/// pane's, the blame's, its commit's, or a side panel's diff preview.
fn searching(state: &AppState, focus: KeyScope) -> bool {
    match (focus, &state.focus) {
        (KeyScope::Diff, Focus::Diff(view)) => view.search.is_some(),
        (KeyScope::Blame, Focus::Blame(view)) => view.search.is_some(),
        (KeyScope::BlameCommit, Focus::Blame(view)) => {
            view.commit.as_ref().is_some_and(|c| c.search.is_some())
        }
        (KeyScope::Files | KeyScope::CommitFiles | KeyScope::StashFiles, _) => {
            state.diff.search.is_some()
        }
        _ => false,
    }
}

/// What Esc's next press does, where that differs from its own label: in
/// a commit's files it clears the filter first, and a search after a list
/// layer is gone.
fn esc_label(state: &AppState, focus: KeyScope) -> Option<&'static str> {
    if focus == KeyScope::CommitFiles && commit_files_filtered(state) {
        Some("clear")
    } else if esc_clears_search(state, focus) {
        Some("clear search")
    } else {
        None
    }
}

/// Esc's next press clears a search, so its hint says so. In Files a
/// range or a filter goes first, and in a commit's files the filter.
fn esc_clears_search(state: &AppState, focus: KeyScope) -> bool {
    let list_first = match focus {
        KeyScope::Files => state.files_anchor.is_some() || !state.files_filter.is_empty(),
        KeyScope::CommitFiles => commit_files_filtered(state),
        _ => false,
    };
    searching(state, focus) && !list_first
}

/// The open commit's files have a filter set.
fn commit_files_filtered(state: &AppState) -> bool {
    state
        .commits
        .open
        .as_ref()
        .is_some_and(|open| !open.filter.is_empty())
}

/// The bindings that lead the hints while commits are copied.
const COPIED_HINTS: &[&str] = &["commits.paste", "commits.clear_copied"];

/// `N copied` while commits wait to be pasted, so the set shows on every
/// screen size and from every panel.
fn copied_note(state: &AppState) -> Option<String> {
    let n = state.copied.len();
    (n > 0 && state.context == Context::Workspace).then(|| format!("{n} copied"))
}

/// The cancel key's hint, if the keymap leaves it a key. Shown only while a
/// command runs, so it is not a `bar` binding.
fn cancel_hint(state: &AppState) -> Option<String> {
    let binding = registry()
        .iter()
        .find(|b| b.id == "workspace.cancel_command")?;
    let key = state.keymap.keys(binding).first()?;
    Some(format!("{} {}", key.label(), binding.label))
}

/// Help and quit: the hints that must stay in the bar, however narrow.
fn pinned(binding: &Binding) -> bool {
    use crate::intent::AppIntent::{CloseHelp, OpenHelp, Quit};
    matches!(binding.intent, OpenHelp | CloseHelp | Quit)
}

/// The hints that fit in `room` columns, each whole. Pinned ones stay;
/// others go from the end, and a `…` says some went. Each hint takes its
/// width plus two columns before it.
fn fit_hints(hints: &[(String, bool, bool)], room: usize) -> Vec<(String, bool)> {
    let cost = |text: &str| width_of(text) + 2;
    let all = |h: &(String, bool, bool)| (h.0.clone(), h.1);
    if hints.iter().map(|h| cost(&h.0)).sum::<usize>() <= room {
        return hints.iter().map(all).collect();
    }
    let mut used = cost("…")
        + hints
            .iter()
            .filter(|h| h.2)
            .map(|h| cost(&h.0))
            .sum::<usize>();
    let mut kept: Vec<(String, bool)> = Vec::new();
    for hint in hints.iter().filter(|h| !h.2) {
        if used + cost(&hint.0) > room {
            break;
        }
        used += cost(&hint.0);
        kept.push(all(hint));
    }
    kept.push(("…".to_string(), true));
    kept.extend(hints.iter().filter(|h| h.2).map(all));
    // Too narrow even for those: drop from the end, never mid-hint.
    while used > room {
        match kept.pop() {
            Some((text, _)) => used -= cost(&text),
            None => break,
        }
    }
    kept
}

fn dimmed_if(disabled: bool) -> Style {
    if disabled {
        Style::new().fg(Color::DarkGray)
    } else {
        Style::new()
    }
}

/// A popup over `area` listing every key the focused panel answers to,
/// the user's own commands after the built-in ones, below a rule. A list
/// taller than the screen scrolls; `▲`/`▼` on the border say rows are cut,
/// and the bottom one says which key scrolls.
fn render_help(state: &AppState, frame: &mut Frame, area: Rect) {
    let focus = state.panel_focus();
    let labels = |keys: &[KeyPattern]| {
        let labels: Vec<String> = keys.iter().map(KeyPattern::label).collect();
        labels.join("/")
    };
    let builtin: Vec<_> = available(registry(), &state.keymap, focus, state)
        .into_iter()
        .map(|(binding, disabled)| {
            let keys = labels(state.keymap.keys(binding));
            (keys, binding.description.to_string(), disabled.is_some())
        })
        .collect();
    let builtin_count = builtin.len();
    // The description is the user's text: one line, safe to draw.
    let commands = available_commands(&state.keymap, focus)
        .into_iter()
        .map(|command| {
            let description = sanitize(command.description.as_bytes());
            (labels(&command.keys), description, false)
        });
    let entries: Vec<(String, String, bool)> = builtin.into_iter().chain(commands).collect();
    let key_width = entries
        .iter()
        .map(|(k, _, _)| width_of(k))
        .max()
        .unwrap_or(0);
    let mut lines: Vec<Line> = entries
        .iter()
        .map(|(keys, description, disabled)| {
            let pad = key_width - width_of(keys);
            Line::styled(
                format!(" {keys}{}  {description} ", " ".repeat(pad)),
                dimmed_if(*disabled),
            )
        })
        .collect();
    let title = format!(" Keys: {} ", help_name(state));
    let content_width = lines
        .iter()
        .map(Line::width)
        .chain([width_of(&title)])
        .max()
        .unwrap_or(0);
    if entries.len() > builtin_count {
        lines.insert(
            builtin_count,
            Line::styled("─".repeat(content_width), Style::new().fg(Color::DarkGray)),
        );
    }
    let popup = centered(
        area,
        u16::try_from(content_width + 2).unwrap_or(u16::MAX),
        u16::try_from(lines.len() + 2).unwrap_or(u16::MAX),
    );
    let rows = help_rows(frame.area());
    let total = lines.len();
    let first = state.help_scroll.min(total.saturating_sub(rows));
    let mut block = Block::bordered().title(title);
    if first > 0 {
        block = block.title(Line::raw("▲").right_aligned());
    }
    if first + rows < total {
        let down = registry()
            .iter()
            .find(|b| b.id == "help.down")
            .map(|b| {
                let keys: Vec<String> =
                    state.keymap.keys(b).iter().map(KeyPattern::label).collect();
                keys.join("/")
            })
            .unwrap_or_default();
        block = block.title_bottom(Line::raw(format!(" {down} more ▼")).right_aligned());
    }
    frame.render_widget(Clear, popup);
    let first = u16::try_from(first).unwrap_or(u16::MAX);
    frame.render_widget(Paragraph::new(lines).scroll((first, 0)).block(block), popup);
}

/// What the help list is titled with: the focused panel's name, also for
/// panels that only have the workspace's keys.
fn help_name(state: &AppState) -> &'static str {
    let scope = state.panel_focus();
    match (scope, state.focus.panel()) {
        (KeyScope::Workspace, Panel::Side(side)) => side.title(),
        _ => scope.title(),
    }
}

/// A `width` x `height` rectangle centred in `area`, shrunk to fit it.
pub(crate) fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

/// The Files panel: an error, a placeholder until the first load, or one
/// line per file. Collapsed, a count stands in for the list.
fn render_files_panel(
    state: &AppState,
    look: &PanelLook,
    expanded: bool,
    frame: &mut Frame,
    area: Rect,
) {
    let count = match state.files.len() {
        1 => "1 file".to_string(),
        n => format!("{n} files"),
    };
    let shown = state.files_visible();
    let range = state.files_range();
    let selected = match state.files_anchor {
        Some(_) => format!(" · {} selected", range.len()),
        None => String::new(),
    };
    let title = if !state.files_filter.is_empty() {
        let counts = format!("{} of {}{selected}", shown.len(), state.files.len());
        look.title(&filter_title(look, "Files", &state.files_filter, &counts))
    } else if !selected.is_empty() {
        look.title(&format!("Files{selected}"))
    } else if look.title_only && state.files_loaded && state.status_error.is_none() {
        look.title(&format!("Files · {count}"))
    } else {
        look.title("Files")
    };
    let block = look.block(title);
    let inner = block.inner(area);
    let panel = if let Some(error) = &state.status_error {
        let mut lines: Vec<Line> = error.lines().map(Line::from).collect();
        lines.push(Line::from("Press r to retry"));
        Paragraph::new(lines).wrap(Wrap { trim: false })
    } else if !state.files_loaded {
        Paragraph::new("Loading…")
    } else if state.files.is_empty() {
        Paragraph::new("No changes")
    } else if shown.is_empty() {
        Paragraph::new(no_matches())
    } else if !expanded {
        Paragraph::new(count)
    } else {
        Paragraph::new(file_lines(
            &state.files,
            &shown,
            state.selected,
            &range,
            inner,
        ))
    };
    frame.render_widget(panel.block(block), area);
}

/// The `shown` files that fit in `area`, scrolled so `selected` is visible.
/// It and the files in `range` are drawn reversed. When files below the
/// window are cut, the last row says how many, so the cut is never silent.
fn file_lines(
    files: &[FileEntry],
    shown: &[usize],
    selected: Option<usize>,
    range: &[usize],
    area: Rect,
) -> Vec<Line<'static>> {
    let rows = usize::from(area.height);
    let width = usize::from(area.width);
    let line = |&i: &usize| range_row(file_line(&files[i], width), i, selected, range, width);
    if shown.len() <= rows {
        return shown.iter().map(line).collect();
    }
    let selected = selected
        .and_then(|s| shown.iter().position(|&i| i == s))
        .unwrap_or(0);
    if rows <= 1 {
        // No room for a "+N more" row; the selection matters more.
        return shown.iter().skip(selected).take(rows).map(line).collect();
    }
    // One row is kept for "+N more" unless the window reaches the end.
    let more = rows - 1;
    let start = selected.saturating_sub(more - 1);
    if start + rows >= shown.len() {
        let start = shown.len() - rows;
        return shown.iter().skip(start).map(line).collect();
    }
    let mut lines: Vec<Line> = shown.iter().skip(start).take(more).map(line).collect();
    lines.push(Line::from(format!("+{} more", shown.len() - start - more)));
    lines
}

/// `<name> · /<query> <counts>` for a filtered list's title, the query
/// made safe to draw and cut short so the counts still fit.
pub(crate) fn filter_title(look: &PanelLook, name: &str, query: &str, counts: &str) -> String {
    let query = sanitize(query.as_bytes());
    // Measured once, so the cost stays linear in the query's length.
    let fixed = width_of(&format!(
        " [{}] {name} · / {counts}{} ",
        look.number, look.mode
    ));
    let room = look.title_width.saturating_sub(fixed).max(1);
    format!("{name} · /{} {counts}", elide_right(&query, room))
}

/// What a list says when its filter hides every row.
pub(crate) fn no_matches() -> Line<'static> {
    Line::styled("No matches", Style::new().fg(Color::DarkGray))
}

/// A list's title text: `base` alone, or with ` · N selected` while a
/// range is set, or as [`filter_title`] with the shown count while a
/// filter is.
pub(crate) fn list_title(
    look: &PanelLook,
    base: &str,
    filter: &str,
    shown: usize,
    total: usize,
    range: Option<usize>,
) -> String {
    let selected = range.map_or_else(String::new, |n| format!(" · {n} selected"));
    if !filter.is_empty() {
        let counts = format!("{shown} of {total}{selected}");
        filter_title(look, base, filter, &counts)
    } else {
        format!("{base}{selected}")
    }
}

/// The part of `shown` that fits in `rows`, scrolled just enough to keep
/// `selected` in view.
pub(crate) fn scrolled(shown: &[usize], selected: Option<usize>, rows: usize) -> &[usize] {
    let at = selected
        .and_then(|s| shown.iter().position(|&i| i == s))
        .unwrap_or(0);
    let first = at.saturating_sub(rows.saturating_sub(1));
    let end = shown.len().min(first + rows);
    &shown[first.min(end)..end]
}

/// `<name> · path: <path> · author: <author>`, naming the parts set, each
/// made safe, cut so the title fits the border with `reserve` columns to
/// spare. As room runs out, `name` goes, then the parts share what is left
/// (the path loses its start, so the file name stays, the author its end),
/// then the labels go, and last only the path's end is left. The result
/// always fits, so [`PanelLook::title`] never cuts the end it kept.
pub(crate) fn query_title(
    look: &PanelLook,
    name: &str,
    path: Option<&str>,
    author: Option<&str>,
    reserve: usize,
) -> String {
    let author = author.map(|a| sanitize(a.as_bytes()));
    let (path_label, author_label) = ("path: ", "author: ");
    let join = |labels: bool, path: Option<String>, author: Option<String>| {
        let parts: Vec<String> = [
            path.map(|p| {
                if labels {
                    format!("{path_label}{p}")
                } else {
                    p
                }
            }),
            author.map(|a| {
                if labels {
                    format!("{author_label}{a}")
                } else {
                    a
                }
            }),
        ]
        .into_iter()
        .flatten()
        .collect();
        parts.join(" · ")
    };
    let whole = format!(
        "{name} · {}",
        join(true, path.map(str::to_string), author.clone())
    );
    if look.fits(&format!("{whole}{}", " ".repeat(reserve))) {
        return whole;
    }
    // Measured without the padding `title` drops first when it must.
    let room = |labels: bool, reserve: usize| {
        let empty = join(
            labels,
            path.map(|_| String::new()),
            author.as_ref().map(|_| String::new()),
        );
        let fixed = width_of(&format!("[{}] {empty}{}", look.number, look.mode));
        look.title_width.saturating_sub(fixed + reserve)
    };
    let shares = usize::from(path.is_some()) + usize::from(author.is_some());
    let cut = |labels: bool, each: usize| {
        join(
            labels,
            path.map(|p| elide_left(p, each)),
            author.as_deref().map(|a| elide_right(a, each)),
        )
    };
    // Each part keeps `…` and a few columns, or the tier gives way.
    const PART_MIN: usize = 4;
    let each = room(true, reserve) / shares.max(1);
    if each >= PART_MIN {
        return cut(true, each);
    }
    let each = room(false, 0) / shares.max(1);
    if each >= PART_MIN {
        return cut(false, each);
    }
    let last = width_of(&format!("[{}] {}", look.number, look.mode));
    let room = look.title_width.saturating_sub(last);
    match (path, &author) {
        (Some(path), _) => elide_left(path, room),
        (None, Some(author)) => elide_right(author, room),
        (None, None) => String::new(),
    }
}

/// Row `i` drawn as the cursor or a range row: both reversed, and within a
/// range the cursor bold and underlined too, so the moving end shows.
pub(crate) fn range_row<'a>(
    line: Line<'a>,
    i: usize,
    selected: Option<usize>,
    range: &[usize],
    width: usize,
) -> Line<'a> {
    if Some(i) == selected && !range.is_empty() {
        reversed(line, width).patch_style(Modifier::BOLD | Modifier::UNDERLINED)
    } else if Some(i) == selected || range.contains(&i) {
        reversed(line, width)
    } else {
        line
    }
}

/// Highlight a whole row, not just its text, by padding it to `width`.
pub(crate) fn reversed(mut line: Line<'_>, width: usize) -> Line<'_> {
    let pad = width.saturating_sub(line.width());
    line.spans.push(Span::raw(" ".repeat(pad)));
    line.style(Style::new().add_modifier(Modifier::REVERSED))
}

/// Where in the streamed diff the pane is: ` · <first shown>/<loaded>`,
/// `+` while more are coming; empty with nothing to show.
pub(crate) fn diff_position(diff: &DiffPane) -> String {
    if diff.lines.is_empty() || diff.error.is_some() {
        return String::new();
    }
    let first = (diff.scroll + 1).min(diff.lines.len());
    let more = if diff.done { "" } else { "+" };
    format!(" · {first}/{}{more}", diff.lines.len())
}

/// The inside of a streamed Diff pane at `inner`'s size: an error, a
/// delayed "Loading…", "No diff", or the visible lines, with the
/// search's matches marked on them.
pub(crate) fn diff_pane_body(diff: &DiffPane, inner: Rect) -> Paragraph<'static> {
    let (rows, width) = (usize::from(inner.height), usize::from(inner.width));
    if let Some(error) = &diff.error {
        let lines = error.lines().map(|l| Line::from(l.to_string()));
        return Paragraph::new(lines.collect::<Vec<_>>()).wrap(Wrap { trim: false });
    }
    if diff.show_loading {
        return Paragraph::new("Loading…");
    }
    if diff.done && diff.lines.is_empty() {
        return Paragraph::new("No diff");
    }
    // Only the visible slice: a 50,000-line diff costs a screenful per frame.
    let start = diff.scroll.min(diff.lines.len());
    let end = start.saturating_add(rows).min(diff.lines.len());
    Paragraph::new(
        diff.lines[start..end]
            .iter()
            .zip(start..)
            .map(|(line, at)| styled_diff_line(line, width, &marks_of(&diff.search, at)))
            .collect::<Vec<_>>(),
    )
}

/// The focused Diff pane: one side of the file as hunks, the selected one
/// marked by a reversed `@@` row and a `▌` gutter, so it reads without colour.
/// In line mode the picked lines are marked that way instead.
/// `suffix`, when set, follows the title after a `·`.
fn render_hunk_view(
    pane: HunkPane,
    options: DiffOptions,
    emoji: EmojiZwj,
    suffix: &str,
    look: &PanelLook,
    frame: &mut Frame,
    area: Rect,
) {
    let view = pane.view;
    let block = look.block(hunk_title(view, options, suffix, look));
    let inner = block.inner(area);
    let (rows, width) = (usize::from(inner.height), usize::from(inner.width));
    // A commit's change gives a second column to what the patch holds.
    let picked_ids = pane
        .picks
        .map(|patch| custom_patch::view::picked_rows(view, patch));
    let gutters = if picked_ids.is_some() { 2 } else { 1 };
    let pane = match &view.patch {
        None if view.loading() => Paragraph::new("Loading…"),
        None => Paragraph::new("Could not read the diff"),
        Some(patch) if patch.kind == FileKind::Binary => {
            Paragraph::new(PatchError::Binary.message())
        }
        Some(patch) if patch.kind == FileKind::Unsupported => {
            Paragraph::new(PatchError::Unsupported.message())
        }
        Some(patch) if patch.hunks.is_empty() => Paragraph::new(NO_CHANGES),
        Some(_) => {
            let picked = view.selected_range();
            let lines = view
                .rows()
                .into_iter()
                .enumerate()
                .skip(view.scroll)
                .take(rows)
                .map(|(at, row)| {
                    let in_hunk = row.hunk == Some(view.hunk);
                    // In line mode only the picked lines are marked.
                    let selected = if view.lines.is_some() {
                        in_hunk && row.line.is_some_and(|i| picked.contains(&i))
                    } else {
                        in_hunk
                    };
                    let gutter = if selected { "▌" } else { " " };
                    let style = match row.kind {
                        RowKind::Header => Style::new().add_modifier(Modifier::BOLD),
                        RowKind::HunkHeader => Style::new().fg(Color::Cyan),
                        RowKind::Added => Style::new().fg(Color::Green),
                        RowKind::Removed => Style::new().fg(Color::Red),
                        RowKind::Context => Style::new(),
                        RowKind::NoNewline => Style::new().fg(Color::DarkGray),
                    };
                    let mut spans = vec![Span::styled(gutter, Style::new().fg(FOCUSED))];
                    if let Some(ids) = &picked_ids {
                        let held = matches!((row.hunk, row.line), (Some(h), Some(l)) if ids.contains(&(h, l)));
                        let mark = if held { "●" } else { " " };
                        spans.push(Span::styled(mark, commits::view::PATCH_MARK));
                    }
                    spans.extend(clipped_marked(
                        &drawn_text(&row, emoji),
                        width.saturating_sub(gutters),
                        style,
                        &marks_of(&view.search, at),
                    ));
                    let line = Line::from(spans);
                    let mark = match view.lines {
                        Some(_) => row.line.is_some(),
                        None => row.kind == RowKind::HunkHeader,
                    };
                    if selected && mark {
                        reversed(line, width)
                    } else {
                        line
                    }
                })
                .collect::<Vec<_>>();
            Paragraph::new(lines)
        }
    };
    frame.render_widget(pane.block(block), area);
}

/// The focused hunk view, with the custom patch when the view is of the
/// commit it holds changes from.
#[derive(Clone, Copy)]
struct HunkPane<'a> {
    view: &'a HunkView,
    picks: Option<&'a CustomPatch>,
}

/// [`search_marks`] of a pane's search, if it has one.
pub(crate) fn marks_of(search: &Option<DiffSearch>, at: usize) -> Vec<(usize, usize, Style)> {
    search
        .as_ref()
        .map_or_else(Vec::new, |s| search_marks(s, at))
}

/// Matches of the search on row `at`: char ranges and their style, the
/// current match standing out from the rest.
fn search_marks(search: &DiffSearch, at: usize) -> Vec<(usize, usize, Style)> {
    // Matches are in row order, so this row's are found by bisection.
    let from = search.matches.partition_point(|m| m.row < at);
    let to = search.matches.partition_point(|m| m.row <= at);
    search.matches[from..to]
        .iter()
        .zip(from..)
        .map(|(m, i)| {
            let style = if Some(i) == search.current {
                CURRENT_MATCH
            } else {
                MATCH
            };
            (m.start, m.end, style)
        })
        .collect()
}

/// A search match, and the one `n` and `N` step from.
pub(crate) const MATCH: Style = Style::new().fg(Color::Black).bg(Color::Yellow);
/// The current one is also bold and underlined, so it stands out without
/// colour.
pub(crate) const CURRENT_MATCH: Style = Style::new()
    .fg(Color::Black)
    .bg(Color::LightRed)
    .add_modifier(Modifier::BOLD.union(Modifier::UNDERLINED));

/// `text` in `style`, split so each char range in `marks` (in order, not
/// overlapping) takes its own style on top. Ranges past the end of a
/// clipped text are cut to it. Each range widens to whole glyphs: the
/// buffer measures each span on its own, so a span that ended inside an
/// emoji sequence or before a combining mark would split what `clip` laid
/// out as one glyph, and shift the rest of the row. A glyph holding the
/// current match keeps its style even when another match shares it.
fn marked_spans(text: String, style: Style, marks: &[(usize, usize, Style)]) -> Vec<Span<'static>> {
    if marks.is_empty() {
        return vec![Span::styled(text, style)];
    }
    let continues = continues_glyph(&text);
    let chars: Vec<char> = text.chars().collect();
    let inside = |i: usize| continues.get(i).copied().unwrap_or(false);
    let piece = |from: usize, to: usize| {
        chars[from.min(chars.len())..to.min(chars.len())]
            .iter()
            .collect::<String>()
    };
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut at = 0;
    // The mark the last marked span carries.
    let mut last_mark = None;
    for &(start, end, mark) in marks {
        let mut start = start.min(chars.len()).max(at);
        while start > at && inside(start) {
            start -= 1;
        }
        let mut end = end.min(chars.len());
        while inside(end) {
            end += 1;
        }
        // A match in the glyph the last one widened to: that span already
        // holds it, and takes this match's style on top, unless it holds
        // the current match, which keeps its own look so `n` and `N` always
        // show where they stopped.
        if end <= start {
            if at > 0
                && last_mark != Some(CURRENT_MATCH)
                && let Some(last) = spans.last_mut()
            {
                last.style = last.style.patch(mark);
                last_mark = Some(mark);
            }
            continue;
        }
        spans.push(Span::styled(piece(at, start), style));
        spans.push(Span::styled(piece(start, end), style.patch(mark)));
        last_mark = Some(mark);
        at = end;
    }
    spans.push(Span::styled(piece(at, chars.len()), style));
    spans.retain(|span| !span.content.is_empty());
    spans
}

/// `line` clipped to `width` from its start, with `marks` drawn over
/// `style`. A cut line's `…` stays in `style`: it stands for text off
/// screen, not for the match, and a marked `…` would read as matched.
pub(crate) fn clipped_marked(
    line: &str,
    width: usize,
    style: Style,
    marks: &[(usize, usize, Style)],
) -> Vec<Span<'static>> {
    let (text, cut) = clip_cut(line, 0, width);
    if !cut {
        return marked_spans(text, style, marks);
    }
    let ellipsis = text.chars().count() - 1;
    let marks: Vec<_> = marks
        .iter()
        .map(|&(start, end, mark)| (start, end.min(ellipsis), mark))
        .filter(|&(start, end, _)| start < end)
        .collect();
    marked_spans(text, style, &marks)
}

/// Where a search is, for a pane's title: " · match i of n", with `+` on
/// the count while `more` lines can still come, or " · no matches". Never
/// the query: the bottom bar shows that, escaped.
pub(crate) fn found_note(search: Option<&DiffSearch>, more: bool) -> String {
    let more = if more { "+" } else { "" };
    match search {
        Some(search) => match search.current {
            Some(i) => format!(" · match {} of {}{more}", i + 1, search.matches.len()),
            None => " · no matches".to_string(),
        },
        None => String::new(),
    }
}

/// `text` followed by spaces up to `width` columns.
pub(crate) fn pad(text: &str, width: usize) -> String {
    format!("{text}{}", " ".repeat(width.saturating_sub(width_of(text))))
}

/// Colour by kind; `---`/`+++` arrive as headers, so they are never red or
/// green. A line wider than `width` ends in `…`, so a cut is never silent.
/// `marks` are search matches, drawn over the colour.
pub(crate) fn styled_diff_line(
    line: &DiffLine,
    width: usize,
    marks: &[(usize, usize, Style)],
) -> Line<'static> {
    let style = match line.kind {
        LineKind::Added => Style::new().fg(Color::Green),
        LineKind::Removed => Style::new().fg(Color::Red),
        LineKind::Hunk => Style::new().fg(Color::Cyan),
        LineKind::Header => Style::new().add_modifier(Modifier::BOLD),
        LineKind::Context => Style::new(),
    };
    Line::from(clipped_marked(&line.text, width, style, marks))
}

/// `XY path`, or `XY orig → path` for renames and copies, fitted to `width`
/// columns. Paths lose their start first, since the file name is at the end;
/// for renames the new path keeps as much room as it can. A conflict reads
/// `! XY path` in red bold, so it stands out without colour too.
fn file_line(entry: &FileEntry, width: usize) -> Line<'static> {
    let conflicted = entry.kind == EntryKind::Conflicted;
    let letters = if conflicted {
        format!("! {}", entry.status_letters())
    } else {
        entry.status_letters()
    };
    let prefix = width_of(&letters) + 1;
    let text = match &entry.orig_path {
        None => {
            let path = elide_left(entry.path.display(), width.saturating_sub(prefix));
            format!("{letters} {path}")
        }
        Some(orig) => {
            let room = width.saturating_sub(prefix + width_of(" → "));
            let orig_room = room
                .saturating_sub(width_of(entry.path.display()))
                .max(1)
                .min(width_of(orig.display()));
            let orig = elide_left(orig.display(), orig_room);
            let path = elide_left(entry.path.display(), room.saturating_sub(orig_room));
            format!("{letters} {orig} → {path}")
        }
    };
    if conflicted {
        Line::styled(
            text,
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        )
    } else {
        Line::from(text)
    }
}

/// Fit `text` into `max` columns by replacing its start with `…`.
pub(crate) fn elide_left(text: &str, max: usize) -> String {
    if width_of(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut kept = 0;
    let start = text
        .char_indices()
        .rev()
        .take_while(|(_, c)| {
            kept += width_of(c.encode_utf8(&mut [0; 4]));
            kept < max
        })
        .last()
        .map_or(text.len(), |(i, _)| i);
    format!("…{}", &text[start..])
}

/// Display width in terminal columns, measured the way ratatui lays text out.
pub(crate) fn width_of(text: &str) -> usize {
    Span::raw(text).width()
}

/// A drawn buffer's rows as text, for tests.
#[cfg(test)]
pub(crate) fn buffer_rows(buffer: &ratatui::buffer::Buffer) -> Vec<String> {
    (0..buffer.area.height)
        .map(|y| row_text(buffer, y, 0..buffer.area.width))
        .collect()
}

/// Columns `xs` of row `y` as text. The blank cell after a wide glyph is
/// skipped so the text reads like the screen.
#[cfg(test)]
pub(crate) fn row_text(
    buffer: &ratatui::buffer::Buffer,
    y: u16,
    xs: std::ops::Range<u16>,
) -> String {
    let mut row = String::new();
    let mut x = xs.start;
    while x < xs.end {
        let symbol = buffer[(x, y)].symbol();
        row.push_str(symbol);
        x += width_of(symbol).max(1) as u16;
    }
    row
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::features::blame::CommitView;
    use crate::git::diff::{DiffLine, DiffSide, LineKind};
    use crate::git::repo_state::Operation;
    use crate::git::status::EntryKind;
    use crate::test_support::OID;
    use crate::test_support::screen::{
        TWO_HUNKS, assert_no_split_cluster, blame_line, blamed, branch, contains, diff_line, draw,
        draw_buffer, draw_with_cursor, file, find, hunks, loaded, on, status_focused, thin,
        with_diff,
    };

    /// Draw a workspace whose Files panel is exactly `panel` columns wide and
    /// return only that panel's columns of each row.
    fn draw_files(state: &AppState, panel: u16, height: u16) -> Vec<String> {
        let width = (panel..panel * 4)
            .find(|&w| {
                let [main, _] = screen_layout(Rect::new(0, 0, w, height));
                let layout =
                    workspace_layout(main, state.focus.panel(), state.expanded, state.screen_mode);
                layout.rect(Panel::Side(SidePanel::Files)).map(|r| r.width) == Some(panel)
            })
            .expect("a total width for this panel width");
        thin(draw(state, width, height))
            .into_iter()
            .map(|row| {
                let mut used = 0;
                row.chars()
                    .take_while(|c| {
                        used += width_of(c.encode_utf8(&mut [0; 4])).max(1);
                        used <= usize::from(panel)
                    })
                    .collect()
            })
            .collect()
    }

    fn renamed(orig: &str, path: &str) -> FileEntry {
        FileEntry {
            path: path.into(),
            orig_path: Some(orig.into()),
            index: 'R',
            worktree: '.',
            kind: EntryKind::Renamed,
        }
    }

    /// The Files panel's title row and the row below it.
    fn files_rows(rows: &[String]) -> (usize, &String) {
        let title = rows
            .iter()
            .position(|row| row.contains(" Files "))
            .unwrap_or_else(|| panic!("no Files panel: {rows:#?}"));
        (title, &rows[title + 1])
    }

    #[test]
    fn a_short_terminal_collapses_the_other_panels_to_title_rows() {
        let mut state = loaded((0..10).map(|i| file(&format!("f{i}"))).collect());
        state.selected = Some(0);

        // 80x24: the others drop to a title and a line each, so Files
        // keeps 13 rows inside its border.
        let rows = thin(draw(&state, 80, 24));
        assert!(rows[0].starts_with("┌ [1] Status ─"), "{rows:#?}");
        assert!(rows[1].starts_with("│main "), "{rows:#?}");
        let (title, first) = files_rows(&rows);
        assert_eq!(title, 2, "{rows:#?}");
        assert!(first.starts_with("│ M f0 "), "{rows:#?}");
        for row in &rows[3..16] {
            assert!(row.starts_with('│'), "{rows:#?}");
        }
        assert!(rows[16].starts_with('└'), "{rows:#?}");
        for (row, name) in [(17, "[3] Branches"), (19, "[4] Commits"), (21, "[5] Stash")] {
            assert!(rows[row].starts_with(&format!("┌ {name} ─")), "{rows:#?}");
            assert!(rows[row + 1].starts_with('│'), "{rows:#?}");
        }

        // 80x12: only their titles, on a top border with corners.
        // A title row carries the branch, the panel's one line.
        let rows = thin(draw(&state, 80, 12));
        assert!(rows[0].starts_with("┌ [1] Status · main ─"), "{rows:#?}");
        let (title, first) = files_rows(&rows);
        assert_eq!(title, 1, "{rows:#?}");
        assert!(first.starts_with("│ M f0 "), "{rows:#?}");
        assert!(contains(&rows, "│ M f3 "), "{rows:#?}");
        for (row, name) in [(8, "[3] Branches"), (9, "[4] Commits"), (10, "[5] Stash")] {
            assert!(rows[row].starts_with(&format!("┌ {name} ─")), "{rows:#?}");
        }

        // 80x7: the farthest go first; Files keeps its rows.
        let rows = thin(draw(&state, 80, 7));
        assert!(rows[0].starts_with("┌ [1] Status · main ─"), "{rows:#?}");
        assert!(!contains(&rows, "[3] Branches"), "{rows:#?}");
        assert!(!contains(&rows, "[5] Stash"), "{rows:#?}");
        let (title, first) = files_rows(&rows);
        assert_eq!(title, 1, "{rows:#?}");
        assert!(first.starts_with("│ M f0 "), "{rows:#?}");
    }

    #[test]
    fn a_collapsed_status_shows_the_branch_and_an_expanded_one_the_rest() {
        let mut main = branch(Head::Branch("main".into()));
        main.upstream = Some("origin/main".into());
        main.ahead_behind = Some((0, 3));
        let mut state = on(main);
        state.files = vec![file("a.rs")];
        let rows = thin(draw(&state, 80, 24));
        assert!(rows[1].starts_with("│main "), "{rows:#?}");
        assert!(!contains(&rows, "origin/main"), "{rows:#?}");

        // Height 10: 9 rows over the bar; four title rows leave Status five.
        let rows = thin(draw(&status_focused(&state), 80, 10));
        assert!(rows[1].starts_with("│main "), "{rows:#?}");
        assert!(rows[2].starts_with("│origin/main ↑0 ↓3 "), "{rows:#?}");
        let (title, first) = files_rows(&rows);
        assert_eq!(title, 5, "{rows:#?}");
        assert!(first.starts_with("┌ [3] Branches"), "{rows:#?}");
    }

    #[test]
    fn a_collapsed_files_panel_counts_the_files() {
        let mut state = status_focused(&loaded(vec![file("a.rs"), file("b.rs")]));
        let rows = draw(&state, 80, 24);
        assert!(files_rows(&rows).1.starts_with("│2 files "), "{rows:#?}");
        assert!(!contains(&rows, " M a.rs"), "{rows:#?}");
        state.files.pop();
        let rows = draw(&state, 80, 24);
        assert!(files_rows(&rows).1.starts_with("│1 file "), "{rows:#?}");
        state.files.clear();
        let rows = draw(&state, 80, 24);
        assert!(files_rows(&rows).1.starts_with("│No changes "), "{rows:#?}");
    }

    #[test]
    fn review_bar_shows_review_keys_and_still_quits() {
        let rows = draw(&AppState::new(Context::Review { number: Some(1) }), 40, 5);
        assert!(rows[4].trim_end().ends_with("? keys  q quit"), "{rows:#?}");
        assert!(!rows[4].contains("stage"), "{rows:#?}");
    }

    #[test]
    fn the_bar_offers_close_while_help_is_open() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        state.help_open = true;
        let rows = draw(&state, 80, 20);
        assert!(rows[19].starts_with(" ? close  q quit "), "{rows:#?}");
    }

    #[test]
    fn a_disabled_hint_is_dimmed() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        let buffer = draw_buffer(&state, 80, 8);
        let (x, y) = find(&buffer, "space stage");
        assert_ne!(buffer[(x, y)].style().fg, Some(Color::DarkGray));
        let (x, y) = find(&buffer, "r refresh");
        assert_ne!(buffer[(x, y)].style().fg, Some(Color::DarkGray));

        state.op_seq = Some(1);
        let buffer = draw_buffer(&state, 80, 8);
        let (x, y) = find(&buffer, "space stage");
        assert_eq!(buffer[(x, y)].style().fg, Some(Color::DarkGray));
        let (x, y) = find(&buffer, "r refresh");
        assert_ne!(buffer[(x, y)].style().fg, Some(Color::DarkGray));
    }

    #[test]
    fn a_notice_replaces_the_hints_in_yellow_after_loading() {
        let mut state = loaded(vec![file("a.rs")]);
        state.notice = Some("An operation is running".into());
        state.op_error = Some("older failure".into());
        state.busy = 1;
        let buffer = draw_buffer(&state, 80, 8);
        let rows = draw(&state, 80, 8);
        assert!(
            rows[7].starts_with(" loading…  An operation is running"),
            "{rows:#?}"
        );
        assert!(!contains(&rows, "space stage"), "{rows:#?}");
        assert!(!contains(&rows, "older failure"), "{rows:#?}");
        let (x, y) = find(&buffer, "An operation");
        assert_eq!(buffer[(x, y)].style().fg, Some(Color::Yellow));
    }

    #[test]
    fn the_fixup_candidates_notice_fits_80_columns_and_elides_when_narrow() {
        use crate::features::commits::rewrite::fixup_base::candidates_notice;
        let shorts: Vec<String> = (0..20).map(|i| format!("abc{i:04}")).collect();
        let notice = candidates_notice(&shorts);
        let mut state = loaded(vec![file("a.rs")]);
        state.notice = Some(notice.clone().into());

        let rows = draw(&state, 80, 8);
        assert_eq!(rows[7].trim(), notice, "{rows:#?}");
        assert!(notice.ends_with("abc0002 +17 more"), "{notice}");
        let buffer = draw_buffer(&state, 80, 8);
        let (x, y) = find(&buffer, "Staged lines");
        assert_eq!(buffer[(x, y)].style().fg, Some(Color::Yellow));

        for width in [30, 8] {
            let rows = draw(&state, width, 8);
            let bar = rows[7].trim_end();
            assert!(bar.ends_with('…'), "{width}: {rows:#?}");
            assert!(bar.chars().count() <= usize::from(width), "{rows:#?}");
        }
    }

    #[test]
    fn help_lists_the_keys_of_the_focused_panel() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        state.help_open = true;
        // Tall enough for every Files key; a shorter screen clips the list.
        let rows = draw(&state, 80, 60);
        assert!(contains(&rows, " Keys: Files "), "{rows:#?}");
        let row = |needle: &str| {
            rows.iter()
                .find(|row| row.contains(needle))
                .unwrap_or_else(|| panic!("{needle:?} not in {rows:#?}"))
                .clone()
        };
        assert!(
            row("Stage the selected file").contains("space "),
            "{rows:#?}"
        );
        assert!(row("Reload the changed files").contains(" r "), "{rows:#?}");
        assert!(row("Quit Kika").contains("q/esc/ctrl-c "), "{rows:#?}");
        assert!(row("Select the next file").contains("j/↓ "), "{rows:#?}");
        // Help's own close key is in the bar, not in the panel's list.
        assert!(!contains(&rows, "Close this menu"), "{rows:#?}");
    }

    /// `state` with one global `[keys]` table applied.
    fn remapped(mut state: AppState, entries: &[(&str, &[&str])]) -> AppState {
        state.update(crate::intent::AppIntent::ConfigLoaded {
            repo: state.repo.clone(),
            keymap: crate::keymap::remap(entries),
            errors: vec![],
        });
        state
    }

    #[test]
    fn help_and_the_bar_show_the_remapped_keys() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        let mut state = remapped(state, &[("files.stage", &["x"])]);
        let rows = draw(&state, 80, 30);
        assert!(rows[29].starts_with(" x stage  a all"), "{rows:#?}");

        state.help_open = true;
        let rows = draw(&state, 80, 30);
        let stage = rows
            .iter()
            .find(|row| row.contains("Stage the selected file"))
            .unwrap_or_else(|| panic!("no stage row in {rows:#?}"));
        assert!(stage.contains(" x  "), "{stage:?}");
        assert!(!stage.contains("space"), "{stage:?}");
    }

    #[test]
    fn a_disabled_binding_leaves_help_and_the_bar() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        let mut state = remapped(state, &[("files.discard", &[])]);
        let rows = draw(&state, 80, 30);
        assert!(
            rows[29].starts_with(" space stage  a all  c commit  m menu  r refresh"),
            "{rows:#?}"
        );

        state.help_open = true;
        let rows = draw(&state, 80, 30);
        assert!(contains(&rows, "Stage the selected file"), "{rows:#?}");
        assert!(!contains(&rows, "Discard the selected file"), "{rows:#?}");
    }

    #[test]
    fn remapped_focus_keys_show_in_help_and_work() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        let mut state = remapped(
            state,
            &[
                ("workspace.zoom", &["Z"]),
                ("workspace.focus_status", &["x"]),
            ],
        );
        state.help_open = true;
        let rows = draw(&state, 100, 50);
        let row = |needle: &str| {
            rows.iter()
                .find(|row| row.contains(needle))
                .unwrap_or_else(|| panic!("{needle:?} not in {rows:#?}"))
                .clone()
        };
        assert!(row("Zoom the focused panel").contains(" Z "), "{rows:#?}");
        assert!(row("Focus the Status panel").contains(" x "), "{rows:#?}");

        state.help_open = false;
        state.update(crate::intent::AppIntent::Key(crate::keymap::KeyPress::new(
            crossterm::event::KeyCode::Char('x'),
            crossterm::event::KeyModifiers::NONE,
        )));
        assert_eq!(state.focus, Focus::Status);
    }

    #[test]
    fn disabled_help_rows_are_dimmed() {
        let mut state = loaded(vec![]);
        state.help_open = true;
        let buffer = draw_buffer(&state, 80, 60);
        let (x, y) = find(&buffer, "Stage the selected file");
        assert_eq!(buffer[(x, y)].style().fg, Some(Color::DarkGray));
        let (x, y) = find(&buffer, "Quit Kika");
        assert_ne!(buffer[(x, y)].style().fg, Some(Color::DarkGray));
    }

    #[test]
    fn tiny_terminals_do_not_panic_with_help_open() {
        let mut state = loaded(vec![file("a.rs")]);
        state.help_open = true;
        state.notice = Some("No file selected".into());
        for context in [Context::Workspace, Context::Review { number: Some(2) }] {
            state.context = context;
            for (w, h) in [(0, 0), (1, 1), (10, 2), (2, 10), (3, 3), (5, 4), (20, 6)] {
                draw(&state, w, h);
            }
        }
    }

    #[test]
    fn review_context_shows_number_in_title() {
        let rows = draw(&AppState::new(Context::Review { number: Some(123) }), 30, 5);
        assert!(rows[0].contains("Review #123"), "{rows:#?}");
    }

    #[test]
    fn workspace_shows_loading_before_status_arrives() {
        let mut state = AppState::new(Context::Workspace);
        state.busy = 1;
        let rows = thin(draw(&state, 40, 13));
        assert!(rows[1].contains(" Files "), "{rows:#?}");
        assert!(rows[2].starts_with("│Loading…"), "{rows:#?}");
        assert!(rows[12].contains("loading…"), "{rows:#?}");
    }

    #[test]
    fn workspace_lists_files_with_status_letters_and_renames() {
        let mut state = loaded(vec![renamed("old.txt", "new.txt")]);
        state.files.push(FileEntry {
            path: "new_file".into(),
            orig_path: None,
            index: '?',
            worktree: '?',
            kind: EntryKind::Untracked,
        });
        let rows = thin(draw(&state, 80, 24));
        assert!(contains(&rows, "│R  old.txt → new.txt "), "{rows:#?}");
        assert!(contains(&rows, "│?? new_file "), "{rows:#?}");
        assert!(!contains(&rows, "loading…"), "{rows:#?}");
        assert!(
            rows[23].starts_with(
                " space stage  a all  c commit  d discard  m menu  r refresh  …  ? keys  q quit"
            ),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_conflicted_row_is_marked_and_red_bold() {
        let mut state = loaded(vec![file("a.txt")]);
        state.files.insert(
            0,
            FileEntry {
                path: "both.txt".into(),
                orig_path: None,
                index: 'U',
                worktree: 'U',
                kind: EntryKind::Conflicted,
            },
        );
        state.selected = Some(1);
        let buffer = draw_buffer(&state, 80, 24);
        let rows = thin(buffer_rows(&buffer));
        assert!(contains(&rows, "│! UU both.txt "), "{rows:#?}");
        assert!(contains(&rows, "│ M a.txt "), "{rows:#?}");
        let (x, y) = find(&buffer, "! UU both.txt");
        for x in x..x + 13 {
            let style = buffer[(x, y)].style();
            assert_eq!(style.fg, Some(Color::Red), "{x}");
            assert!(style.add_modifier.contains(Modifier::BOLD), "{x}");
        }
        let (x, y) = find(&buffer, " M a.txt");
        assert_ne!(buffer[(x + 1, y)].style().fg, Some(Color::Red));
    }

    #[test]
    fn workspace_says_no_changes_when_clean() {
        assert!(contains(&draw(&loaded(vec![]), 40, 13), "No changes"));
    }

    #[test]
    fn workspace_shows_status_error_wrapped_with_a_retry_hint() {
        let mut state = loaded(vec![]);
        state.status_error =
            Some("fatal: not a git repository (or any of the parent directories): .git".into());
        let rows = draw_files(&state, 40, 13);
        assert!(contains(&rows, "│fatal: not a git repository"), "{rows:#?}");
        assert!(contains(&rows, "directories): .git"), "{rows:#?}");
        assert!(contains(&rows, "│Press r to retry"), "{rows:#?}");
    }

    #[test]
    fn multi_line_error_keeps_its_lines() {
        let mut state = loaded(vec![]);
        state.status_error = Some("error: first\nhint: second".into());
        let rows = draw_files(&state, 40, 13);
        assert!(contains(&rows, "│error: first "), "{rows:#?}");
        assert!(contains(&rows, "│hint: second "), "{rows:#?}");
    }

    #[test]
    fn long_path_is_left_elided_to_fit_the_panel() {
        // Inner width 18: " M " leaves 15 columns for the path.
        let state = loaded(vec![file("src/deeply/nested/module/file.rs")]);
        let rows = draw_files(&state, 20, 11);
        assert_eq!(rows[2], "│ M …module/file.rs│", "{rows:#?}");
    }

    #[test]
    fn long_rename_keeps_the_new_path_and_elides_the_old_one_first() {
        // Inner width 28: "R  " + orig + " → " + path.
        let state = loaded(vec![renamed("src/old/location/a.rs", "src/new/b.rs")]);
        let rows = draw_files(&state, 30, 11);
        assert_eq!(rows[2], "│R  …tion/a.rs → src/new/b.rs│", "{rows:#?}");
    }

    #[test]
    fn very_long_rename_elides_both_paths() {
        let state = loaded(vec![renamed("old/name.rs", "src/brand/new/location/b.rs")]);
        let rows = draw_files(&state, 20, 11);
        assert_eq!(rows[2], "│R  … → …ation/b.rs│", "{rows:#?}");
    }

    #[test]
    fn wide_characters_are_measured_by_display_width() {
        // Each CJK glyph is two columns wide; inner width 14 leaves 11 for the path.
        let state = loaded(vec![file("日本語/日本語/長い名前.md")]);
        let rows = draw_files(&state, 16, 11);
        assert_eq!(rows[2], "│ M …い名前.md │", "{rows:#?}");
        assert!(width_of(&rows[2]) <= 16, "{rows:#?}");
    }

    #[test]
    fn a_reordering_file_name_is_drawn_with_a_visible_escape() {
        let state = loaded(vec![file("evil\u{202E}txt.exe")]);
        let width = 80;
        let mut terminal = Terminal::new(TestBackend::new(width, 12)).unwrap();
        terminal.draw(|frame| render(&state, frame)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(
            buffer
                .content()
                .iter()
                .all(|cell| !cell.symbol().contains('\u{202E}')),
            "a raw U+202E reached the buffer"
        );
        let rows = buffer_rows(buffer);
        let row = rows
            .iter()
            .find(|row| row.contains("evil"))
            .unwrap_or_else(|| panic!("{rows:#?}"));
        assert!(row.contains("evil<U+202E>txt.exe"), "{rows:#?}");
        assert!(width_of(row) <= usize::from(width), "{rows:#?}");
    }

    #[test]
    fn a_long_name_with_escapes_still_fits_its_ellipsis() {
        // Inner width 14 leaves 11 for the path; the escape counts its full width.
        let state = loaded(vec![file("a/long/\u{202E}folder/name\u{200B}")]);
        let rows = draw_files(&state, 16, 11);
        assert_eq!(rows[2], "│ M …me<U+200B>│", "{rows:#?}");
        assert!(width_of(&rows[2]) <= 16, "{rows:#?}");
    }

    #[test]
    fn overflowing_list_says_how_many_files_are_hidden() {
        // Height 12: the bar, four title rows and two borders leave 5 rows,
        // so 4 files and "+N more".
        let files = (0..10).map(|i| file(&format!("f{i}"))).collect();
        let rows = draw_files(&loaded(files), 30, 12);
        assert!(contains(&rows, "│ M f3 "), "{rows:#?}");
        assert!(!contains(&rows, "│ M f4 "), "{rows:#?}");
        assert!(contains(&rows, "│+6 more "), "{rows:#?}");
    }

    #[test]
    fn list_that_fits_exactly_has_no_more_line() {
        let files = (0..4).map(|i| file(&format!("f{i}"))).collect();
        let rows = draw_files(&loaded(files), 30, 12);
        assert!(contains(&rows, "│ M f3 "), "{rows:#?}");
        assert!(!contains(&rows, "more"), "{rows:#?}");
    }

    #[test]
    fn busy_indicator_survives_a_narrow_status_bar() {
        let mut state = loaded(vec![]);
        state.busy = 1;
        let rows = draw(&state, 12, 4);
        assert!(rows[3].contains("loading…"), "{rows:#?}");
    }

    #[test]
    fn tiny_terminals_do_not_panic() {
        let mut state = loaded((0..5).map(|i| file(&format!("file-{i}"))).collect());
        state
            .files
            .push(renamed("a/very/long/old", "a/very/long/new"));
        for (w, h) in [(0, 0), (1, 1), (10, 2), (2, 10), (3, 3), (5, 4)] {
            draw(&state, w, h);
            state.status_error = Some("boom".into());
            draw(&state, w, h);
            state.status_error = None;
        }
    }

    #[test]
    fn an_op_error_replaces_the_hints_with_its_first_line_in_red() {
        let mut state = loaded(vec![file("a.rs")]);
        state.op_error = Some("fatal: Unable to create index.lock\nhint: another git".into());
        let buffer = draw_buffer(&state, 80, 8);
        let rows = draw(&state, 80, 8);
        assert!(
            rows[7].starts_with(" error: fatal: Unable to create index.lock"),
            "{rows:#?}"
        );
        assert!(!contains(&rows, "hint: another git"), "{rows:#?}");
        assert!(!contains(&rows, "space stage"), "{rows:#?}");
        let (x, y) = find(&buffer, "error:");
        assert_eq!(buffer[(x, y)].style().fg, Some(Color::Red));
    }

    #[test]
    fn an_op_error_keeps_the_busy_indicator_in_front() {
        let mut state = loaded(vec![file("a.rs")]);
        state.op_error = Some("boom".into());
        state.busy = 1;
        let rows = draw(&state, 80, 8);
        assert!(rows[7].starts_with(" loading…  error: boom"), "{rows:#?}");
    }

    #[test]
    fn tiny_terminals_do_not_panic_with_an_op_error() {
        let mut state = loaded(vec![file("a.rs")]);
        state.op_error = Some("fatal: a long error line\nsecond".into());
        for busy in [0, 1] {
            state.busy = busy;
            for (w, h) in [(0, 0), (1, 1), (10, 2), (2, 10), (3, 3), (5, 4)] {
                draw(&state, w, h);
            }
        }
    }

    #[test]
    fn the_selected_file_row_is_reversed_across_the_panel() {
        let mut state = loaded(vec![file("a.rs"), file("b.rs")]);
        state.selected = Some(1);
        let buffer = draw_buffer(&state, 100, 13);
        let (x, y) = find(&buffer, " M b.rs");
        assert!(
            buffer[(x, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        // The padding after the name is highlighted too.
        assert!(
            buffer[(x + 10, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        let (x, y) = find(&buffer, " M a.rs");
        assert!(
            !buffer[(x, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn a_selection_below_the_first_screen_stays_visible() {
        // Height 12: 5 rows, so 4 files plus "+N more" at a time.
        let mut state = loaded((0..10).map(|i| file(&format!("f{i}"))).collect());
        state.selected = Some(5);
        let rows = draw_files(&state, 30, 12);
        assert!(contains(&rows, "│ M f5 "), "{rows:#?}");
        assert!(!contains(&rows, "│ M f1 "), "{rows:#?}");
        assert!(contains(&rows, "│+4 more "), "{rows:#?}");

        // Near the end the window shows the last files and no "more" row.
        state.selected = Some(8);
        let rows = draw_files(&state, 30, 12);
        for name in ["f6", "f7", "f8", "f9"] {
            assert!(contains(&rows, &format!("│ M {name} ")), "{rows:#?}");
        }
        assert!(!contains(&rows, "more"), "{rows:#?}");
    }

    #[test]
    fn a_one_row_list_shows_the_selection() {
        // Height 4: three rows over the bar, all Files', one inside its border.
        let mut state = loaded((0..10).map(|i| file(&format!("f{i}"))).collect());
        state.selected = Some(4);
        let rows = draw_files(&state, 30, 4);
        assert!(contains(&rows, "│ M f4 "), "{rows:#?}");
    }

    #[test]
    fn tiny_terminals_do_not_panic_with_a_diff_loaded() {
        let mut state = with_diff(
            (0..100)
                .map(|i| diff_line(LineKind::Added, &format!("+\u{65e5}\u{672c} {i}")))
                .collect(),
        );
        state.files = (0..20).map(|i| file(&format!("file-{i}"))).collect();
        for (selected, scroll) in [(0, 0), (19, 99), (10, 500)] {
            state.selected = Some(selected);
            state.diff.scroll = scroll;
            for (w, h) in [(0, 0), (1, 1), (10, 2), (2, 10), (3, 3), (5, 4), (7, 5)] {
                draw(&state, w, h);
            }
        }
    }

    #[test]
    fn wrap_breaks_at_spaces_keeps_indentation_and_splits_long_words() {
        assert_eq!(
            wrap("  --> src/a.rs:3 unused", 16),
            ["  --> src/a.rs:3", "unused"]
        );
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap("abcdefgh", 3), ["abc", "def", "gh"]);
        assert_eq!(wrap("a\n\nb", 5), ["a", "", "b"]);
        // A glyph wider than the row still makes progress.
        assert_eq!(wrap("日本", 1), ["日", "本"]);
        assert_eq!(wrap("", 0), Vec::<String>::new());
    }

    #[test]
    fn fit_shortens_the_name_before_the_suffix() {
        assert_eq!(fit("origin/main", " ↑2 ↓1", 40), "origin/main ↑2 ↓1");
        assert_eq!(fit("origin/main", " ↑2 ↓1", 12), "…/main ↑2 ↓1");
        // No room for the name: one `…` stands in for it.
        assert_eq!(fit("origin/main", " ↑2 ↓1", 3), "… ↑2 ↓1");
        assert_eq!(fit("detached", " abc1234", 12), "…hed abc1234");
        assert_eq!(fit("main", "", 0), "…");
    }

    #[test]
    fn elide_right_keeps_the_start() {
        assert_eq!(elide_right("rebasing, merging", 40), "rebasing, merging");
        assert_eq!(elide_right("rebasing, merging", 10), "rebasing,…");
        assert_eq!(elide_right("日本語", 4), "日…");
        assert_eq!(elide_right("abc", 1), "…");
        assert_eq!(elide_right("abc", 0), "");
        assert_eq!(elide_right("", 0), "");
    }

    #[test]
    fn clip_marks_cut_text_and_blanks_split_wide_glyphs() {
        assert_eq!(clip("abcdef", 0, 4), "abc…");
        assert_eq!(clip("abcdef", 1, 4), "…cd…");
        assert_eq!(clip("abcdef", 3, 4), "…ef");
        assert_eq!(clip("ab", 0, 4), "ab");
        assert_eq!(clip("日本語", 1, 4), "…本…");
        assert_eq!(clip("日本語", 1, 5), "…本語");
        assert_eq!(clip("日本語", 0, 3), "日…");
        assert_eq!(clip("anything", 0, 0), "");
    }

    #[test]
    fn clip_keeps_an_emoji_sequence_as_one_glyph() {
        let family = "👨\u{200D}👩\u{200D}👧";
        assert_eq!(clip(&format!("{family}X"), 0, 3), format!("{family}X"));
        // Cut after the sequence: the sequence never splits.
        assert_eq!(clip(&format!("ab{family}"), 0, 3), "ab…");
        assert_eq!(clip(&format!("ab{family}"), 0, 4), format!("ab{family}"));
        assert_eq!(clip(&format!("a{family}b"), 0, 3), "a …");
        // A `from` inside it blanks it like a split wide glyph.
        assert_eq!(clip(&format!("{family}ab"), 1, 3), "…ab");
        assert_eq!(clip(&format!("x{family}ab"), 2, 3), "…ab");
        // A skin tone and U+FE0F join the emoji before them too.
        let coder = "👩\u{1F3FD}\u{200D}💻";
        assert_eq!(clip(&format!("{coder}X"), 0, 3), format!("{coder}X"));
        let fire = "❤\u{FE0F}\u{200D}🔥";
        assert_eq!(clip(&format!("{fire}X"), 0, 3), format!("{fire}X"));
        // An escaped joiner leaves each emoji its own two columns.
        assert_eq!(clip("👨<U+200D>👩", 0, 12), "👨<U+200D>👩");
        assert_eq!(clip("👨<U+200D>👩", 0, 11), "👨<U+200D>…");
    }

    #[test]
    fn clip_counts_a_sequence_that_starts_narrow_as_two_columns() {
        // `❤` and `🏳` are one column alone; U+FE0F makes them two, and the
        // terminal buffer draws the whole sequence in those two.
        for start in [
            "❤\u{FE0F}\u{200D}🔥",
            "🏳\u{FE0F}\u{200D}⚧\u{FE0F}",
            "❤\u{FE0F}",
        ] {
            let line = format!("{start}ab");
            for (from, width, expected) in [
                (0, 1, "…".to_string()),
                (0, 2, " …".to_string()),
                (0, 3, format!("{start}…")),
                (0, 4, format!("{start}ab")),
                (1, 3, "…ab".to_string()),
                // Cut at both edges: each edge cell is a `…`.
                (1, 2, "……".to_string()),
                (2, 2, "…b".to_string()),
            ] {
                assert_eq!(
                    clip(&line, from, width),
                    expected,
                    "{start:?} {from} {width}"
                );
            }
        }
    }

    #[test]
    fn clip_of_a_sequence_into_one_or_two_columns_never_panics() {
        let family = "👨\u{200D}👩\u{200D}👧";
        assert_eq!(clip(family, 0, 1), "…");
        assert_eq!(clip(family, 0, 2), family);
        assert_eq!(clip(family, 1, 1), "…");
        assert_eq!(clip(family, 1, 2), "…");
        assert_eq!(clip("❤\u{FE0F}\u{200D}🔥", 0, 1), "…");
    }

    #[test]
    fn regional_indicators_pair_left_to_right() {
        assert_eq!(
            continues_glyph("a🇺🇸🇺🇸🇺"),
            [false, false, true, false, true, false]
        );
        // Anything between two indicators leaves each one alone.
        assert_eq!(continues_glyph("🇺x🇸"), [false, false, false]);
    }

    #[test]
    fn clip_keeps_a_flag_as_one_glyph() {
        assert_eq!(clip("🇺🇸x", 0, 10), "🇺🇸x");
        let lone = |s: &str| s.chars().any(emoji::is_regional_indicator);
        // Cut by the right edge, then by the left: the flag blanks out whole.
        for (line, from, width, expected) in [
            ("a🇺🇸", 0, 2, "a…"),
            ("a🇺🇸b", 0, 3, "a …"),
            ("🇺🇸x", 1, 3, "…x"),
            ("🇺🇸xy", 1, 2, "……"),
        ] {
            let out = clip(line, from, width);
            assert_eq!(out, expected, "{line:?} {from} {width}");
            assert!(!lone(&out), "{line:?} {from} {width}: {out:?}");
        }
    }

    #[test]
    fn clip_never_joins_letters_across_a_script_joiner() {
        // The consonant after `क्‍` still counts its own column.
        assert_eq!(clip("क्\u{200D}षabc", 0, 4), "क्\u{200D}षa…");
        assert_eq!(clip("a\u{200D}👨bc", 0, 4), "a\u{200D}👨…");
    }

    #[test]
    fn the_focused_pane_is_titled_by_its_side_and_marked() {
        let buffer = draw_buffer(&hunks(DiffSide::Worktree, TWO_HUNKS), 100, 20);
        let rows = buffer_rows(&buffer);
        assert!(contains(&rows, " Unstaged "), "{rows:#?}");
        assert!(!contains(&rows, " Diff "), "{rows:#?}");
        let (x, y) = find(&buffer, " Unstaged ");
        assert_eq!(buffer[(x - 1, y)].style().fg, Some(Color::Cyan));

        let rows = draw(&hunks(DiffSide::Index, TWO_HUNKS), 100, 20);
        assert!(contains(&rows, " Staged "), "{rows:#?}");
        assert!(!contains(&rows, "· lines"), "{rows:#?}");
        let rows = draw(&hunks(DiffSide::Untracked, TWO_HUNKS), 100, 20);
        assert!(contains(&rows, " Unstaged "), "{rows:#?}");
    }

    #[test]
    fn the_selected_hunk_has_a_reversed_header_and_a_gutter() {
        let mut state = hunks(DiffSide::Worktree, TWO_HUNKS);
        let reversed_at = |state: &AppState, needle: &str| {
            let buffer = draw_buffer(state, 100, 20);
            let (x, y) = find(&buffer, needle);
            buffer[(x, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        };
        assert!(reversed_at(&state, "@@ -1,2 +1,2 @@"));
        assert!(!reversed_at(&state, "@@ -9 +9 @@"));
        let rows = draw(&state, 100, 20);
        assert!(contains(&rows, "▌-old one"), "{rows:#?}");
        assert!(contains(&rows, " -old nine"), "{rows:#?}");
        // The heading's control bytes never reach the terminal.
        assert!(contains(&rows, "@@ -9 +9 @@ fn \u{FFFD}[2Jx"), "{rows:#?}");

        if let Focus::Diff(view) = &mut state.focus {
            view.next(20);
        }
        assert!(!reversed_at(&state, "@@ -1,2 +1,2 @@"));
        assert!(reversed_at(&state, "@@ -9 +9 @@"));
        let rows = draw(&state, 100, 20);
        assert!(contains(&rows, "▌+new nine"), "{rows:#?}");
    }

    /// A diff of a file whose name holds a ZWNJ, adding a ZWNJ word, an emoji
    /// ZWJ sequence and a Devanagari conjunct, each followed by `X`.
    const JOINED: &str = "diff --git a/می\u{200C}خواهم.rs b/می\u{200C}خواهم.rs\n\
        --- a/می\u{200C}خواهم.rs\n+++ b/می\u{200C}خواهم.rs\n\
        @@ -1 +1,3 @@\n-old\n+می\u{200C}خواهمX\n+👨\u{200D}👩\u{200D}👧X\n+क्\u{200D}षX\n";

    #[test]
    fn a_hunk_body_keeps_script_joiners_and_its_file_header_escapes_them() {
        let rows = draw(&hunks(DiffSide::Worktree, JOINED.as_bytes()), 100, 20);
        assert!(contains(&rows, "+می\u{200C}خواهمX"), "{rows:#?}");
        assert!(contains(&rows, "+क्\u{200D}षX"), "{rows:#?}");
        assert!(contains(&rows, "+++ b/می<U+200C>خواهم.rs"), "{rows:#?}");
        // Emoji sequences stay escaped: their width depends on the terminal.
        assert!(contains(&rows, "+👨<U+200D>👩<U+200D>👧X"), "{rows:#?}");
    }

    /// Where the `+` of the added row starting with `first` is drawn.
    fn added_row(buffer: &ratatui::buffer::Buffer, first: char) -> (u16, u16) {
        let area = buffer.area;
        (0..area.height)
            .flat_map(|y| (0..area.width.saturating_sub(1)).map(move |x| (x, y)))
            .find(|&(x, y)| {
                buffer[(x, y)].symbol() == "+" && buffer[(x + 1, y)].symbol().starts_with(first)
            })
            .unwrap_or_else(|| panic!("no added row starting with {first:?}"))
    }

    #[test]
    fn text_after_a_joined_word_lands_where_a_terminal_draws_it() {
        // Columns as a terminal counts them, one per letter, two per emoji,
        // none for a joiner or virama: not `width_of`, which the layout uses.
        let width = 100;
        let buffer = draw_buffer(&hunks(DiffSide::Worktree, JOINED.as_bytes()), width, 20);
        for (first, columns) in [('م', 1 + 7), ('👨', 1 + 2 + 8 + 2 + 8 + 2), ('क', 1 + 2)] {
            let (x, y) = added_row(&buffer, first);
            assert_eq!(buffer[(x + columns, y)].symbol(), "X", "{first:?}");
            // The focused Diff pane's border is drawn thick.
            assert_eq!(buffer[(width - 1, y)].symbol(), "┃", "{first:?}");
        }
    }

    #[test]
    fn a_clipped_joined_row_keeps_its_border_and_never_splits_a_cluster() {
        let long = "می\u{200C}خواهم क्\u{200D}ष 👨\u{200D}👩 ".repeat(8);
        let diff =
            format!("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+{long}\n");
        // Every width from 30 to 60 clips the row at a different point of it;
        // the tiny sizes only show that nothing splits or panics.
        let sizes = (30..=60).map(|w| (w, 12)).chain([(24, 8), (12, 5)]);
        for (width, height) in sizes {
            let buffer = draw_buffer(&hunks(DiffSide::Worktree, diff.as_bytes()), width, height);
            if height == 12 {
                let (_, y) = added_row(&buffer, 'م');
                assert_eq!(buffer[(width - 1, y)].symbol(), "┃", "{width}x{height}");
            }
            for cell in buffer.content() {
                let lead = cell.symbol().chars().next();
                assert!(
                    !matches!(lead, Some('\u{200C}' | '\u{200D}' | '\u{094D}')),
                    "{width}x{height}: a cell starts with {lead:?}"
                );
            }
        }
    }

    /// `JOINED` drawn where the terminal clusters emoji ZWJ sequences.
    fn joined_keep(diff: &str) -> AppState {
        let mut state = hunks(DiffSide::Worktree, diff.as_bytes());
        state.emoji_zwj = EmojiZwj::Keep;
        state
    }

    #[test]
    fn with_keep_an_emoji_sequence_is_one_cell_and_the_row_stays_aligned() {
        let width = 100;
        let buffer = draw_buffer(&joined_keep(JOINED), width, 20);
        let (x, y) = added_row(&buffer, '👨');
        assert_eq!(buffer[(x + 1, y)].symbol(), "👨\u{200D}👩\u{200D}👧");
        assert_eq!(buffer[(x + 1 + 2, y)].symbol(), "X");
        assert_eq!(buffer[(width - 1, y)].symbol(), "┃");
        let rows = buffer_rows(&buffer);
        assert!(!contains(&rows, "👨<U+200D>"), "{rows:#?}");
        // The script joiners and the header escape are as with `Escape`.
        assert!(contains(&rows, "+می\u{200C}خواهمX"), "{rows:#?}");
        assert!(contains(&rows, "+क्\u{200D}षX"), "{rows:#?}");
        assert!(contains(&rows, "+++ b/می<U+200C>خواهم.rs"), "{rows:#?}");
        for (first, columns) in [('م', 1 + 7), ('क', 1 + 2)] {
            let (x, y) = added_row(&buffer, first);
            assert_eq!(buffer[(x + columns, y)].symbol(), "X", "{first:?}");
            assert_eq!(buffer[(width - 1, y)].symbol(), "┃", "{first:?}");
        }
    }

    #[test]
    fn with_keep_a_clipped_joined_row_keeps_its_border_and_never_splits_a_cluster() {
        let family = "👨\u{200D}👩\u{200D}👧";
        let long = format!("می\u{200C}خواهم क्\u{200D}ष {family} ").repeat(8);
        let diff =
            format!("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+{long}\n");
        // The tiny sizes are the smallest that still show the row, and one
        // that does not, to show nothing splits or panics there.
        let sizes = (30..=60)
            .map(|w| (w, 12))
            .chain([(24, 9), (16, 9), (12, 5)]);
        for (width, height) in sizes {
            let buffer = draw_buffer(&joined_keep(&diff), width, height);
            let at = format!("{width}x{height}");
            // At 12x5 the pane is too short to reach the row.
            if let Some((_, y)) = find_added_row(&buffer, 'م') {
                assert_cut_row(&buffer, width, y, &at);
            } else {
                let rows = buffer_rows(&buffer);
                assert_eq!((width, height), (12, 5), "{at}: no row {rows:#?}");
            }
            assert_no_split_cluster(&buffer, family, &at);
        }
    }

    /// Where the `+` of the added row starting with `first` is drawn, if it is.
    fn find_added_row(buffer: &ratatui::buffer::Buffer, first: char) -> Option<(u16, u16)> {
        let area = buffer.area;
        (0..area.height)
            .flat_map(|y| (0..area.width.saturating_sub(1)).map(move |x| (x, y)))
            .find(|&(x, y)| {
                buffer[(x, y)].symbol() == "+" && buffer[(x + 1, y)].symbol().starts_with(first)
            })
    }

    /// Row `y` was cut: it ends in a `…` just inside the focused pane's border.
    fn assert_cut_row(buffer: &ratatui::buffer::Buffer, width: u16, y: u16, at: &str) {
        assert_eq!(buffer[(width - 1, y)].symbol(), "┃", "{at}");
        assert_eq!(buffer[(width - 2, y)].symbol(), "…", "{at}");
    }

    #[test]
    fn with_keep_a_sequence_that_starts_narrow_takes_two_columns() {
        let fire = "❤\u{FE0F}\u{200D}🔥";
        let flag = "🏳\u{FE0F}\u{200D}⚧\u{FE0F}";
        let diff = format!(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1,2 @@\n-x\n\
             +{fire}X\n+{flag}X\n"
        );
        let width = 100;
        let buffer = draw_buffer(&joined_keep(&diff), width, 20);
        for (first, sequence) in [('❤', fire), ('🏳', flag)] {
            let (x, y) = added_row(&buffer, first);
            assert_eq!(buffer[(x + 1, y)].symbol(), sequence);
            assert_eq!(buffer[(x + 1 + 2, y)].symbol(), "X", "{first:?}");
            assert_eq!(buffer[(width - 1, y)].symbol(), "┃", "{first:?}");
        }

        // Cut at every width: the `…` and the border hold, and nothing splits.
        let long = format!("{fire}a{flag}b ").repeat(12);
        let diff =
            format!("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+{long}\n");
        for width in (12..=60).chain([80]) {
            let buffer = draw_buffer(&joined_keep(&diff), width, 12);
            let at = width.to_string();
            let (x, y) = added_row(&buffer, '❤');
            assert_cut_row(&buffer, width, y, &at);
            // Each sequence is one 2-column cell, so the letters after them
            // land on the columns a terminal draws them at.
            let row: Vec<&str> = (x + 1..width - 2)
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            for (i, symbol) in row.iter().enumerate() {
                if *symbol == fire || *symbol == flag {
                    let after = row.get(i + 2).copied();
                    assert!(
                        after.is_none_or(|s| s == "a" || s == "b"),
                        "{at}: {after:?} two columns after {symbol:?}"
                    );
                }
            }
            for cell in buffer.content() {
                let symbol = cell.symbol();
                if symbol.contains('\u{200D}') {
                    assert!(symbol == fire || symbol == flag, "{at}: split {symbol:?}");
                }
            }
        }
    }

    #[test]
    fn with_keep_a_search_match_never_splits_an_emoji_sequence() {
        let family = "👨\u{200D}👩";
        let tail: String = ('a'..='z').collect();
        let diff = format!(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n\
             +Q{family}X{family}{tail}\n"
        );
        for query in ["👨", "👩", "\u{200D}", family, "Q👨", "👩X", "X👨"] {
            for width in [100, 24, 20] {
                let mut state = joined_keep(&diff);
                if let Focus::Diff(view) = &mut state.focus {
                    view.set_search(query, EmojiZwj::Keep);
                }
                let buffer = draw_buffer(&state, width, 12);
                let at = format!("{query:?} at {width}");
                let (x, y) = added_row(&buffer, 'Q');
                // `+Q`, the sequence in one 2-column cell, then `X`.
                assert_eq!(buffer[(x + 2, y)].symbol(), family, "{at}");
                assert_eq!(buffer[(x + 4, y)].symbol(), "X", "{at}");
                assert_eq!(buffer[(x + 5, y)].symbol(), family, "{at}");
                // The match is marked on the whole sequence.
                let marked = |x: u16| buffer[(x, y)].style().bg.is_some();
                if query != "X👨" {
                    assert!(marked(x + 2), "{at}: not marked");
                }
                if width == 100 {
                    assert_eq!(buffer[(x + 7, y)].symbol(), "a", "{at}");
                } else {
                    assert_cut_row(&buffer, width, y, &at);
                }
                assert_eq!(buffer[(width - 1, y)].symbol(), "┃", "{at}");
                for cell in buffer.content() {
                    let symbol = cell.symbol();
                    assert!(
                        !symbol.ends_with('\u{200D}') && !symbol.starts_with('\u{200D}'),
                        "{at}: a cell {symbol:?}"
                    );
                    if symbol.contains('👨') || symbol.contains('👩') {
                        assert_eq!(symbol, family, "{at}: a split sequence");
                    }
                }
            }
        }
    }

    #[test]
    fn marks_widen_to_whole_glyphs_and_a_second_mark_in_one_glyph_restyles_it() {
        let family = "👨\u{200D}👩";
        let text = format!("Q{family}X");
        let spans = |marks: &[(usize, usize, Style)]| -> Vec<(String, Style)> {
            marked_spans(text.clone(), Style::new(), marks)
                .into_iter()
                .map(|span| (span.content.into_owned(), span.style))
                .collect()
        };
        let plain = Style::new();
        // `👩` (chars 3..4) and the ZWJ (2..3) both widen to the sequence.
        for range in [(3, 4), (2, 3), (1, 2), (1, 4)] {
            assert_eq!(
                spans(&[(range.0, range.1, MATCH)]),
                [
                    ("Q".to_string(), plain),
                    (family.to_string(), MATCH),
                    ("X".to_string(), plain),
                ],
                "{range:?}"
            );
        }
        // Two matches in the one sequence: the current one wins.
        assert_eq!(
            spans(&[(1, 2, MATCH), (3, 4, CURRENT_MATCH)]),
            [
                ("Q".to_string(), plain),
                (family.to_string(), MATCH.patch(CURRENT_MATCH)),
                ("X".to_string(), plain),
            ]
        );
        // A combining mark stays with its letter too.
        let accented = marked_spans("e\u{301}x".into(), plain, &[(0, 1, MATCH)]);
        assert_eq!(accented[0].content, "e\u{301}");
    }

    #[test]
    fn the_current_match_keeps_its_style_when_a_later_match_shares_its_glyph() {
        let text = "e\u{301}\u{301}x".to_string();
        let spans = marked_spans(text, Style::new(), &[(1, 2, CURRENT_MATCH), (2, 3, MATCH)]);
        assert_eq!(spans[0].content, "e\u{301}\u{301}");
        assert_eq!(spans[0].style, CURRENT_MATCH);
        assert_eq!(spans[0].style.bg, CURRENT_MATCH.bg);
        let bold_underlined = Modifier::BOLD | Modifier::UNDERLINED;
        assert!(spans[0].style.add_modifier.contains(bold_underlined));
        assert_eq!(spans[1].content, "x");
    }

    #[test]
    fn a_current_match_sharing_a_glyph_with_another_draws_as_current() {
        let family = "👨\u{200D}👦\u{200D}👦";
        let diff = format!(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+Q{family}X\n"
        );
        for (width, height) in [(80, 24), (20, 10)] {
            let mut state = joined_keep(&diff);
            if let Focus::Diff(view) = &mut state.focus {
                view.set_search("👦", EmojiZwj::Keep);
                let search = view.search.as_ref().unwrap();
                assert_eq!((search.matches.len(), search.current), (2, Some(0)));
            }
            let buffer = draw_buffer(&state, width, height);
            let at = format!("{width}x{height}");
            let (x, y) = added_row(&buffer, 'Q');
            assert_eq!(buffer[(x + 2, y)].symbol(), family, "{at}");
            assert_eq!(buffer[(x + 2, y)].style().bg, CURRENT_MATCH.bg, "{at}");
            assert_eq!(buffer[(x + 4, y)].symbol(), "X", "{at}");
        }
    }

    #[test]
    fn a_search_for_half_a_flag_marks_the_whole_flag() {
        let flag = "🇺🇸";
        let diff = format!(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+{flag} ok\n"
        );
        for (width, height) in [(80, 24), (20, 10)] {
            let mut state = hunks(DiffSide::Worktree, diff.as_bytes());
            if let Focus::Diff(view) = &mut state.focus {
                view.set_search("🇺", EmojiZwj::Escape);
            }
            let buffer = draw_buffer(&state, width, height);
            let at = format!("{width}x{height}");
            let (x, y) = added_row(&buffer, '🇺');
            let cell = &buffer[(x + 1, y)];
            assert_eq!(cell.symbol(), flag, "{at}");
            assert_eq!(cell.style().bg, CURRENT_MATCH.bg, "{at}");
            assert_eq!(cell.style().fg, CURRENT_MATCH.fg, "{at}");
            assert!(
                cell.style()
                    .add_modifier
                    .contains(Modifier::BOLD | Modifier::UNDERLINED),
                "{at}"
            );
            // The flag's second column, the space, then `o`.
            assert_eq!(buffer[(x + 2, y)].symbol(), " ", "{at}");
            assert_eq!(buffer[(x + 1 + 3, y)].symbol(), "o", "{at}");
            assert_eq!(buffer[(width - 1, y)].symbol(), "┃", "{at}");
        }
    }

    #[test]
    fn at_80_by_24_a_keep_row_of_emoji_sequences_stays_aligned() {
        let family = "👨\u{200D}👩\u{200D}👧";
        let coder = "👩\u{1F3FD}\u{200D}💻";
        let fire = "❤\u{FE0F}\u{200D}🔥";
        let flag = "🏳\u{FE0F}\u{200D}⚧\u{FE0F}";
        let row = format!("{family}A{coder}B{fire}C{flag}D");
        let diff =
            format!("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+{row}\n");
        let buffer = draw_buffer(&joined_keep(&diff), 80, 24);
        let (x, y) = added_row(&buffer, '👨');
        for (i, (sequence, letter)) in [(family, "A"), (coder, "B"), (fire, "C"), (flag, "D")]
            .into_iter()
            .enumerate()
        {
            let at = x + 1 + 3 * i as u16;
            assert_eq!(buffer[(at, y)].symbol(), sequence, "{letter}");
            assert_eq!(buffer[(at + 2, y)].symbol(), letter);
        }
        assert_eq!(buffer[(79, y)].symbol(), "┃");
    }

    #[test]
    fn in_a_commits_change_the_lines_the_patch_holds_carry_a_mark() {
        use crate::features::commits::custom_patch::CustomPatch;
        use crate::git::commit_files::{CommitId, CommitRange};
        use crate::git::patch::LineId;
        let range = CommitRange {
            oid: CommitId::parse("b0b").unwrap(),
            parent: CommitId::parse("a0a"),
        };
        let mut state = loaded(vec![file("a.rs")]);
        let mut view = HunkView::for_commit("a.rs".into(), range.clone());
        view.loaded(crate::git::patch::parse(TWO_HUNKS).unwrap(), 20);
        state.focus = Focus::Diff(view);
        let mut patch = CustomPatch::new(range, "b0b".into());
        patch.toggle_lines(&"a.rs".into(), &[LineId::added(9)].into());
        state.custom_patch = Some(patch);

        let rows = draw(&state, 100, 20);

        assert!(contains(&rows, " Commit a.rs "), "{rows:#?}");
        assert!(contains(&rows, " ●+new nine"), "{rows:#?}");
        assert!(contains(&rows, "  -old nine"), "{rows:#?}");
        // The selected hunk keeps its own gutter.
        assert!(contains(&rows, "▌ +new one"), "{rows:#?}");

        // Another commit's view shows no marks.
        if let Focus::Diff(view) = &mut state.focus {
            view.commit.as_mut().unwrap().oid = CommitId::parse("c0c").unwrap();
        }
        let rows = draw(&state, 100, 20);
        assert!(!contains(&rows, "●"), "{rows:#?}");
    }

    #[test]
    fn the_patch_mark_keeps_wide_lines_aligned_at_any_width() {
        use crate::features::commits::custom_patch::CustomPatch;
        use crate::git::commit_files::{CommitId, CommitRange};
        use crate::git::patch::LineId;
        let range = CommitRange {
            oid: CommitId::parse("b0b").unwrap(),
            parent: CommitId::parse("a0a"),
        };
        let wide = "漢字漢字漢字漢字漢字漢字漢字漢字漢字漢字漢字漢字 👩\u{200D}💻 end";
        let diff =
            format!("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+{wide}\n");
        let mut state = loaded(vec![file("a.rs")]);
        let mut view = HunkView::for_commit("a.rs".into(), range.clone());
        view.loaded(crate::git::patch::parse(diff.as_bytes()).unwrap(), 20);
        state.focus = Focus::Diff(view);
        let mut patch = CustomPatch::new(range, "b0b".into());
        patch.toggle_lines(&"a.rs".into(), &[LineId::added(1)].into());
        state.custom_patch = Some(patch);

        for width in [100, 50, 30, 16] {
            let buffer = draw_buffer(&state, width, 12);
            let rows = buffer_rows(&buffer);
            let y = rows
                .iter()
                .position(|r| r.contains("●+"))
                .unwrap_or_else(|| {
                    panic!("{width}: {rows:#?}");
                });
            // Clipped to the pane, the right border stays where it is.
            assert_eq!(
                buffer[(width - 1, y as u16)].symbol(),
                "┃",
                "{width}: {rows:#?}"
            );
            assert!(contains(&rows, " -x"), "{width}: {rows:#?}");
        }
        let rows = draw(&state, 8, 4);
        assert_eq!(rows.len(), 4);
    }

    #[test]
    fn in_line_mode_the_picked_lines_are_reversed_and_the_header_is_not() {
        let mut state = hunks(DiffSide::Worktree, TWO_HUNKS);
        if let Focus::Diff(view) = &mut state.focus {
            view.enter_lines();
            view.next_line(true);
        }
        let buffer = draw_buffer(&state, 100, 20);
        let reversed_at = |needle: &str| {
            let (x, y) = find(&buffer, needle);
            buffer[(x, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        };
        assert!(reversed_at("-old one"));
        assert!(reversed_at("+new one"));
        assert!(!reversed_at("same"));
        assert!(!reversed_at("@@ -1,2 +1,2 @@"));
        let rows = buffer_rows(&buffer);
        assert!(contains(&rows, "▌-old one"), "{rows:#?}");
        assert!(contains(&rows, "▌+new one"), "{rows:#?}");
        assert!(contains(&rows, "  same"), "{rows:#?}");
        assert!(contains(&rows, " @@ -1,2 +1,2 @@"), "{rows:#?}");
        assert!(contains(&rows, " Unstaged a.rs · lines "), "{rows:#?}");

        // A one-line range marks only that line.
        if let Focus::Diff(view) = &mut state.focus {
            view.next_line(false);
        }
        let rows = draw(&state, 100, 20);
        assert!(contains(&rows, " -old one"), "{rows:#?}");
        assert!(contains(&rows, "▌+new one"), "{rows:#?}");
        assert!(
            rows[19].starts_with(" space lines  esc hunks  d discard  t staged/unstaged"),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_binary_patch_says_how_to_stage_it() {
        let binary = b"diff --git a/a.rs b/a.rs\nBinary files a/a.rs and b/a.rs differ\n";
        let rows = draw(&hunks(DiffSide::Worktree, binary), 100, 20);
        assert!(
            contains(&rows, "Binary file: stage it from the Files panel"),
            "{rows:#?}"
        );
    }

    #[test]
    fn an_empty_side_or_a_load_says_so() {
        let rows = draw(&hunks(DiffSide::Index, b""), 100, 20);
        assert!(contains(&rows, "No changes in this view"), "{rows:#?}");

        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        let mut view = HunkView::new("a.rs".into(), DiffSide::Worktree);
        view.seq = Some(3);
        state.focus = Focus::Diff(view);
        let rows = draw(&state, 100, 20);
        assert!(contains(&rows, "Loading…"), "{rows:#?}");
    }

    #[test]
    fn help_in_the_diff_focus_lists_the_diff_keys() {
        let mut state = hunks(DiffSide::Worktree, TWO_HUNKS);
        state.help_open = true;
        let rows = draw(&state, 100, 30);
        assert!(contains(&rows, " Keys: Diff "), "{rows:#?}");
        assert!(contains(&rows, "Stage the selected hunk"), "{rows:#?}");
        assert!(contains(&rows, "h/←"), "{rows:#?}");
        assert!(
            contains(&rows, "Clear the search, else go back"),
            "{rows:#?}"
        );
        assert!(!contains(&rows, "Select the next file"), "{rows:#?}");
    }

    /// `state` with one global `[[commands]]` entry for the file list.
    fn with_files_command(mut state: AppState, description: &str) -> AppState {
        use crate::features::custom_commands::{OutputMode, Panel, test_table};
        let mut table = test_table(
            Panel::Files,
            &["ctrl-t", "T"],
            &["true"],
            &[],
            OutputMode::None,
        );
        table.description = description.into();
        let keymap = crate::keymap::Keymap::default()
            .with_commands(&[table], &Default::default())
            .unwrap()
            .0;
        state.update(crate::intent::AppIntent::ConfigLoaded {
            repo: state.repo.clone(),
            keymap,
            errors: vec![],
        });
        state
    }

    #[test]
    fn help_lists_a_command_in_its_panel_only_and_never_in_the_bar() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        let mut state = with_files_command(state, "Tidy \x1b[2Jthe file");
        let rows = draw(&state, 100, 40);
        assert!(!contains(&rows, "ctrl-t"), "{rows:#?}");

        state.help_open = true;
        // Tall enough for every Files key, the filter and range ones too.
        let rows = draw(&state, 100, 60);
        let row = rows
            .iter()
            .find(|row| row.contains("Tidy"))
            .unwrap_or_else(|| panic!("no command row in {rows:#?}"));
        assert!(row.contains(" ctrl-t/T  "), "{row:?}");
        assert!(row.contains("Tidy \u{FFFD}[2Jthe file"), "{row:?}");
        // After the built-in rows.
        let at = |needle: &str| rows.iter().position(|r| r.contains(needle)).unwrap();
        assert!(at("Tidy") > at("Quit Kika"), "{rows:#?}");

        let mut diff = with_files_command(hunks(DiffSide::Worktree, TWO_HUNKS), "Tidy it");
        diff.help_open = true;
        let rows = draw(&diff, 100, 40);
        assert!(contains(&rows, " Keys: Diff "), "{rows:#?}");
        assert!(!contains(&rows, "Tidy"), "{rows:#?}");
    }

    #[test]
    fn scrolled_help_at_80_by_24_reaches_the_users_commands() {
        use crate::intent::{AppIntent, Scroll};
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        let mut state = with_files_command(state, "Tidy the file");
        state.update(AppIntent::Resize {
            width: 80,
            height: 24,
        });
        state.update(AppIntent::OpenHelp);
        assert!(!contains(&draw(&state, 80, 24), "Tidy"));
        for _ in 0..5 {
            state.update(AppIntent::ScrollHelp(Scroll::PageDown));
        }
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, "Tidy the file"), "{rows:#?}");
        assert!(!contains(&rows, "more ▼"), "{rows:#?}");
    }

    /// Whether `row` holds a help popup's `─` rule: a run of them between
    /// the popup's side borders.
    fn is_rule(row: &str) -> bool {
        row.split('│')
            .any(|cell| cell.chars().count() >= 10 && cell.chars().all(|c| c == '─'))
    }

    #[test]
    fn a_rule_separates_the_users_commands_from_the_built_in_keys() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        let mut state = with_files_command(state, "Tidy the file");
        state.help_open = true;
        let rows = draw(&state, 100, 60);
        let tidy = rows.iter().position(|r| r.contains("Tidy")).unwrap();
        assert!(is_rule(&rows[tidy - 1]), "{rows:#?}");
        assert!(!is_rule(&rows[tidy - 2]), "{rows:#?}");
        // Quit is the Files panel's last built-in row.
        assert!(rows[tidy - 2].contains("Quit Kika"), "{rows:#?}");
        assert_eq!(rows.iter().filter(|r| is_rule(r)).count(), 1, "{rows:#?}");

        let mut plain = loaded(vec![file("a.rs")]);
        plain.selected = Some(0);
        plain.help_open = true;
        let rows = draw(&plain, 100, 60);
        assert!(!rows.iter().any(|r| is_rule(r)), "{rows:#?}");
    }

    #[test]
    fn the_bar_offers_the_cancel_key_only_while_a_command_runs() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        let rows = draw(&state, 100, 20);
        assert!(!contains(&rows, "cancel command"), "{rows:#?}");

        state.command_running = true;
        state.busy = 1;
        let rows = draw(&state, 100, 20);
        assert!(
            rows[19].starts_with(" loading…  ctrl-x cancel command"),
            "{rows:#?}"
        );

        let state = remapped(state, &[("workspace.cancel_command", &["ctrl-k"])]);
        let rows = draw(&state, 100, 20);
        assert!(rows[19].contains("ctrl-k cancel command"), "{rows:#?}");
        assert!(!contains(&rows, "ctrl-x"), "{rows:#?}");
    }

    #[test]
    fn help_lists_the_cancel_key() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        state.help_open = true;
        let rows = draw(&state, 100, 60);
        let row = rows
            .iter()
            .find(|row| row.contains("Stop the running command"))
            .unwrap_or_else(|| panic!("no cancel row in {rows:#?}"));
        assert!(row.contains(" ctrl-x "), "{row:?}");
    }

    #[test]
    fn a_bidi_character_in_a_command_description_is_shown_escaped() {
        use crate::effect::Effect;
        use crate::features::custom_commands::{OutputMode, Panel, test_table};
        use crate::intent::AppIntent;
        use crate::keymap::KeyPress;
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        let mut state = with_files_command(state, "evil\u{202E}txt");
        state.help_open = true;
        let rows = draw(&state, 100, 60);
        assert!(contains(&rows, "evil<U+202E>txt"), "{rows:#?}");
        assert!(!contains(&rows, "\u{202E}"), "{rows:#?}");

        // Its output popup is titled with the same description.
        state.help_open = false;
        let mut table = test_table(Panel::Files, &["ctrl-t"], &["true"], &[], OutputMode::Popup);
        table.description = "evil\u{202E}txt".into();
        let keymap = crate::keymap::Keymap::default()
            .with_commands(&[table], &Default::default())
            .unwrap()
            .0;
        state.update(AppIntent::ConfigLoaded {
            repo: state.repo.clone(),
            keymap,
            errors: vec![],
        });
        let effects = state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('t'),
            KeyModifiers::CONTROL,
        )));
        let [Effect::RunCommand { title, .. }] = effects.as_slice() else {
            panic!("expected RunCommand, got {effects:?}");
        };
        state.update(AppIntent::CommandFinished {
            title: title.clone(),
            output: OutputMode::Popup,
            result: Ok("done".into()),
        });
        let rows = draw(&state, 100, 30);
        assert!(contains(&rows, "evil<U+202E>txt"), "{rows:#?}");
        assert!(!contains(&rows, "\u{202E}"), "{rows:#?}");
    }

    #[test]
    fn the_bar_in_the_diff_focus_offers_the_hunk_keys() {
        let rows = draw(&hunks(DiffSide::Worktree, TWO_HUNKS), 100, 20);
        assert!(
            rows[19].starts_with(
                " space hunk  v lines  t staged/unstaged  d discard  esc back  r refresh"
            ),
            "{rows:#?}"
        );
    }

    #[test]
    fn tiny_terminals_do_not_panic_with_the_diff_focused() {
        let state = hunks(DiffSide::Worktree, TWO_HUNKS);
        for (w, h) in [(0, 0), (1, 1), (10, 2), (2, 10), (3, 3), (5, 4), (20, 6)] {
            draw(&state, w, h);
        }
    }

    #[test]
    fn help_lists_the_diff_option_keys_from_files_and_diff() {
        let mut files = loaded(vec![file("a.rs")]);
        files.selected = Some(0);
        let mut diff = hunks(DiffSide::Worktree, TWO_HUNKS);
        for state in [&mut files, &mut diff] {
            state.help_open = true;
            let rows = draw(state, 100, 40);
            let row = |needle: &str| {
                rows.iter()
                    .find(|row| row.contains(needle))
                    .unwrap_or_else(|| panic!("{needle:?} not in {rows:#?}"))
                    .clone()
            };
            assert!(
                row("Show more context lines").contains(" +/= "),
                "{rows:#?}"
            );
            assert!(row("Show fewer context lines").contains(" - "), "{rows:#?}");
            assert!(
                row("Hide or show whitespace-only").contains(" w "),
                "{rows:#?}"
            );
        }
    }

    /// The style of the top-left corner and of the first title letter of
    /// the panel titled `title`.
    fn frame_style(buffer: &ratatui::buffer::Buffer, title: &str) -> (Style, Style) {
        let (x, y) = find(buffer, title);
        // The title starts one column in, after the corner, or at the edge
        // of a one-row panel.
        let corner = buffer[(x.saturating_sub(1), y)].style();
        let letter = buffer[(x + 1, y)].style();
        (corner, letter)
    }

    #[test]
    fn the_focused_panel_has_a_coloured_border_and_a_bold_title() {
        let mut state = with_diff(vec![diff_line(LineKind::Added, "+new")]);
        let buffer = draw_buffer(&state, 80, 24);
        let (border, title) = frame_style(&buffer, "[2] Files");
        assert_eq!(border.fg, Some(FOCUSED));
        assert!(title.add_modifier.contains(Modifier::BOLD));
        // Without colour too: the focused frame is thick, the others thin.
        let rows = buffer_rows(&buffer);
        assert!(contains(&rows, "┏ [2] Files ━"), "{rows:#?}");
        assert!(rows[0].starts_with("┌ [1] Status ─"), "{rows:#?}");
        assert!(contains(&rows, "┌ [0] Diff "), "{rows:#?}");
        for other in ["[1] Status", "[3] Branches", "[5] Stash", "[0] Diff"] {
            let (border, title) = frame_style(&buffer, other);
            assert_eq!(border.fg, Some(Color::DarkGray), "{other}");
            assert!(!title.add_modifier.contains(Modifier::BOLD), "{other}");
        }

        state.focus = Focus::Stash;
        state.expanded = SidePanel::Stash;
        let buffer = draw_buffer(&state, 80, 24);
        let (border, title) = frame_style(&buffer, "[5] Stash");
        assert_eq!(border.fg, Some(FOCUSED));
        assert!(title.add_modifier.contains(Modifier::BOLD));
        for other in ["[1] Status", "[2] Files", "[0] Diff"] {
            let (border, title) = frame_style(&buffer, other);
            assert_eq!(border.fg, Some(Color::DarkGray), "{other}");
            assert!(!title.add_modifier.contains(Modifier::BOLD), "{other}");
        }
    }

    /// A workspace with three files, `a.rs` selected and its diff loaded,
    /// focused on `panel`.
    fn focused_on(panel: Panel, mode: crate::layout::ScreenMode) -> AppState {
        let mut state = match panel {
            Panel::Diff => hunks(DiffSide::Worktree, TWO_HUNKS),
            Panel::Side(side) => {
                let mut state = with_diff(vec![diff_line(LineKind::Added, "+new")]);
                state.focus = Focus::side(side);
                state.expanded = side;
                state
            }
        };
        state.files = vec![file("a.rs"), file("src/b.rs"), file("c.txt")];
        state.diff.path = Some("a.rs".into());
        state.diff.lines = vec![diff_line(LineKind::Added, "+new")];
        state.diff.done = true;
        state.screen_mode = mode;
        state
    }

    const PANELS: [Panel; 6] = [
        Panel::Side(SidePanel::Status),
        Panel::Side(SidePanel::Files),
        Panel::Side(SidePanel::Branches),
        Panel::Side(SidePanel::Commits),
        Panel::Side(SidePanel::Stash),
        Panel::Diff,
    ];

    /// The title a panel is drawn with, up to the end of what must show.
    fn title_of(state: &AppState, panel: Panel) -> String {
        let mode = match state.screen_mode.note() {
            Some(note) if state.focus.panel() == panel => format!(" · {note}"),
            _ => String::new(),
        };
        match (panel, &state.focus) {
            // Expanded, the Branches and Commits panels name their tabs; at
            // 80 columns they take the place of the panel's name, the others
            // shortened.
            (Panel::Side(SidePanel::Branches), _) if state.expanded == SidePanel::Branches => {
                match state.screen_mode {
                    crate::layout::ScreenMode::Normal => format!(" [3] [Local] Rem Tag{mode} "),
                    crate::layout::ScreenMode::Half => format!(" [3] [Local] Remotes Tags{mode} "),
                    crate::layout::ScreenMode::Full => {
                        format!(" [3] Branches · [Local] Remotes Tags{mode} ")
                    }
                }
            }
            (Panel::Side(SidePanel::Commits), _) if state.expanded == SidePanel::Commits => {
                format!("[Log] Reflog{mode} ")
            }
            (Panel::Side(side), _) => format!(" [{}] {}{mode} ", side.number(), side.title()),
            (Panel::Diff, Focus::Diff(_)) => format!(" [0] Unstaged a.rs{mode} "),
            // The selected branch's commits take the diff's place.
            (Panel::Diff, Focus::Branches) => format!(" [0] Commits{mode} "),
            (Panel::Diff, _) => format!(" [0] Diff a.rs · 1/1{mode} "),
        }
    }

    #[test]
    fn every_title_is_whole_at_80_by_24_in_every_focus_and_mode() {
        use crate::layout::ScreenMode;
        for focus in PANELS {
            for mode in [ScreenMode::Normal, ScreenMode::Half, ScreenMode::Full] {
                let state = focused_on(focus, mode);
                let rows = draw(&state, 80, 24);
                let [main, _] = screen_layout(Rect::new(0, 0, 80, 24));
                let layout = workspace_layout(main, focus, state.expanded, mode);
                let mut drawn = 0;
                for panel in PANELS {
                    let Some(rect) = layout.rect(panel) else {
                        continue;
                    };
                    drawn += 1;
                    let title = title_of(&state, panel);
                    let row = row_text(
                        &draw_buffer(&state, 80, 24),
                        rect.y,
                        rect.x..rect.x + rect.width,
                    );
                    assert!(
                        row.contains(&title),
                        "{focus:?} {mode:?}: {title:?} in {row:?}\n{rows:#?}"
                    );
                }
                let expected = if mode == ScreenMode::Full { 1 } else { 6 };
                assert_eq!(drawn, expected, "{focus:?} {mode:?}");
            }
        }
    }

    #[test]
    fn any_screen_size_draws_without_panic() {
        use crate::layout::ScreenMode;
        for focus in PANELS {
            for mode in [ScreenMode::Normal, ScreenMode::Half, ScreenMode::Full] {
                let state = focused_on(focus, mode);
                for (w, h) in [(20, 5), (1, 1), (200, 60), (0, 0), (40, 12), (20, 6)] {
                    draw(&state, w, h);
                }
            }
        }
    }

    #[test]
    fn half_and_full_name_the_mode_in_the_focused_title() {
        use crate::layout::ScreenMode;
        let rows = draw(
            &focused_on(Panel::Side(SidePanel::Files), ScreenMode::Half),
            80,
            24,
        );
        assert!(contains(&rows, "┏ [2] Files · half ━"), "{rows:#?}");
        assert!(contains(&rows, "┌ [0] Diff a.rs · 1/1 ─"), "{rows:#?}");
        let rows = draw(&focused_on(Panel::Diff, ScreenMode::Full), 80, 24);
        assert!(
            rows[0].starts_with("┏ [0] Unstaged a.rs · full ━"),
            "{rows:#?}"
        );
        assert!(!contains(&rows, "[2] Files"), "{rows:#?}");
    }

    #[test]
    fn a_line_cut_at_the_pane_edge_ends_in_a_marker() {
        let long = format!("+{}", "x".repeat(300));
        let state = with_diff(vec![diff_line(LineKind::Added, &long)]);
        let rows = thin(draw(&state, 80, 12));
        // The diff pane is 53 wide: 51 columns inside.
        assert!(
            contains(&rows, &format!("│+{}…│", "x".repeat(49))),
            "{rows:#?}"
        );

        let bytes =
            format!("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-a\n{long}\n");
        let rows = thin(draw(&hunks(DiffSide::Worktree, bytes.as_bytes()), 80, 12));
        assert!(contains(&rows, "x…│"), "{rows:#?}");

        let rows = thin(draw(
            &blamed(vec![blame_line(OID, "Ann", 1, &"y".repeat(300))], 0),
            80,
            12,
        ));
        assert!(contains(&rows, "y…│"), "{rows:#?}");
    }

    #[test]
    fn a_bar_error_wider_than_the_bar_ends_in_a_marker() {
        let mut state = loaded(vec![file("a.rs")]);
        state.op_error = Some(format!("fatal: {}", "e".repeat(200)));
        let rows = draw(&state, 80, 8);
        assert_eq!(width_of(&rows[7]), 80, "{rows:#?}");
        assert!(rows[7].ends_with("ee…"), "{rows:#?}");
        state.busy = 1;
        let rows = draw(&state, 80, 8);
        assert!(
            rows[7].starts_with(" loading…  error: fatal: e"),
            "{rows:#?}"
        );
        assert!(rows[7].ends_with("ee…"), "{rows:#?}");
    }

    #[test]
    fn help_in_files_lists_list_scroll_and_focus_keys() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        state.help_open = true;
        let rows = draw(&state, 100, 50);
        let row = |needle: &str| {
            rows.iter()
                .find(|row| row.contains(needle))
                .unwrap_or_else(|| panic!("{needle:?} not in {rows:#?}"))
                .clone()
        };
        assert!(row("Select the next file").contains(" j/↓ "), "{rows:#?}");
        assert!(
            row("Select the previous file").contains(" k/↑ "),
            "{rows:#?}"
        );
        assert!(
            row("Scroll the diff down a page").contains(" PgDn "),
            "{rows:#?}"
        );
        assert!(
            row("Scroll the diff up a page").contains(" PgUp "),
            "{rows:#?}"
        );
        assert!(row("Focus the Status panel").contains(" 1 "), "{rows:#?}");
        assert!(row("Focus the diff").contains(" 0/l/→ "), "{rows:#?}");
        assert!(row("Focus the next panel").contains(" Tab "), "{rows:#?}");
        assert!(
            row("Focus the previous panel").contains(" ⇧tab "),
            "{rows:#?}"
        );
        assert!(row("Zoom the focused panel").contains(" z "), "{rows:#?}");
        assert!(row("Open the actions menu").contains(" m "), "{rows:#?}");
    }

    #[test]
    fn help_scrolls_at_80_by_24_and_says_more_is_below() {
        use crate::intent::{AppIntent, Scroll};
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        state.update(AppIntent::Resize {
            width: 80,
            height: 24,
        });
        state.update(AppIntent::OpenHelp);
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, " j/↓ more ▼"), "{rows:#?}");
        assert!(!contains(&rows, "▲"), "{rows:#?}");
        assert!(!contains(&rows, "Focus the next panel"), "{rows:#?}");

        state.update(AppIntent::ScrollHelp(Scroll::PageDown));
        state.update(AppIntent::ScrollHelp(Scroll::PageDown));
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, "▲"), "{rows:#?}");
        assert!(!contains(&rows, "more ▼"), "{rows:#?}");
        for needle in [
            "Focus the next panel",
            "Zoom the focused panel",
            "Quit Kika",
        ] {
            assert!(contains(&rows, needle), "{needle}: {rows:#?}");
        }
        // The bar still says how to close it and quit.
        assert!(rows[23].starts_with(" ? close  q quit"), "{rows:#?}");
    }

    #[test]
    fn help_from_a_side_panel_is_titled_with_its_name() {
        let mut state = loaded(vec![]);
        state.help_open = true;
        for side in [SidePanel::Status, SidePanel::Branches, SidePanel::Stash] {
            state.focus = Focus::side(side);
            let rows = draw(&state, 80, 24);
            let title = format!(" Keys: {} ", side.title());
            assert!(contains(&rows, &title), "{rows:#?}");
        }
    }

    #[test]
    fn the_bar_drops_whole_hints_and_keeps_help_and_quit() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        let all = [
            "space stage",
            "a all",
            "c commit",
            "d discard",
            "m menu",
            "r refresh",
            "bksp parent",
            "/ filter",
            "V range",
            "ctrl-f search diff",
            "? keys",
            "q quit",
            "…",
        ];
        for width in [130, 128, 127, 111, 110, 100, 80, 60, 40, 24] {
            let rows = draw(&state, width, 10);
            let bar = rows[9].trim();
            assert!(bar.ends_with("? keys  q quit"), "{width}: {bar:?}");
            for hint in bar.split("  ") {
                assert!(all.contains(&hint), "{width}: {hint:?} in {bar:?}");
            }
            assert_eq!(bar.contains('…'), width < 128, "{width}: {bar:?}");
            // The search hint drops first.
            assert_eq!(bar.contains("V range"), width >= 111, "{width}: {bar:?}");
        }
        let rows = draw(&state, 128, 10);
        assert!(
            rows[9]
                .trim()
                .ends_with("V range  ctrl-f search diff  ? keys  q quit"),
            "{:?}",
            rows[9]
        );
        assert!(!draw(&state, 80, 10)[9].contains("ctrl-f"));
        // Narrower than help and quit together: whole hints only, still.
        let rows = draw(&state, 10, 10);
        assert_eq!(rows[9].trim(), "…", "{rows:#?}");
    }

    #[test]
    fn titles_fit_their_panel_below_80_by_24() {
        use crate::layout::ScreenMode;
        for (w, h, focus, mode) in [
            (60, 20, Panel::Diff, ScreenMode::Half),
            (40, 12, Panel::Side(SidePanel::Files), ScreenMode::Normal),
            (40, 12, Panel::Side(SidePanel::Branches), ScreenMode::Half),
            (30, 24, Panel::Side(SidePanel::Commits), ScreenMode::Normal),
        ] {
            let state = focused_on(focus, mode);
            let buffer = draw_buffer(&state, w, h);
            let [main, _] = screen_layout(Rect::new(0, 0, w, h));
            let layout = workspace_layout(main, focus, state.expanded, mode);
            for panel in PANELS {
                let Some(rect) = layout.rect(panel) else {
                    continue;
                };
                let row = row_text(&buffer, rect.y, rect.x..rect.x + rect.width);
                let key = format!("[{}]", panel.number());
                assert!(row.contains(&key), "{w}x{h} {focus:?}: {row:?}");
                // Whole, or cut on purpose with `…`, the mode note kept.
                // The expanded Branches panel keeps its shown tab instead.
                let name = match panel {
                    Panel::Side(SidePanel::Branches) if state.expanded == SidePanel::Branches => {
                        "[Local]"
                    }
                    Panel::Side(side) => side.title(),
                    Panel::Diff if matches!(state.focus, Focus::Diff(_)) => "Unstaged",
                    Panel::Diff if state.focus == Focus::Branches => "Commits",
                    Panel::Diff => "Diff",
                };
                assert!(
                    row.contains(name) || row.contains('…'),
                    "{w}x{h} {focus:?}: {row:?}"
                );
                if state.focus.panel() == panel && mode == ScreenMode::Half {
                    assert!(row.contains("· half"), "{w}x{h}: {row:?}");
                }
            }
        }
    }

    #[test]
    fn a_title_row_carries_the_panels_one_line() {
        let mut state = status_focused(&loaded((0..12).map(|i| file(&format!("f{i}"))).collect()));
        let rows = draw(&state, 80, 12);
        assert!(contains(&rows, " [2] Files · 12 files ─"), "{rows:#?}");
        let mut merging = branch(Head::Branch("topic".into()));
        merging.in_progress = vec![Operation::Merge];
        state.branch = Some(merging);
        state.focus = Focus::Files;
        state.expanded = SidePanel::Files;
        let rows = draw(&state, 100, 12);
        assert!(
            rows[0].starts_with("┌ [1] Status · topic · merging ─"),
            "{rows:#?}"
        );
        // Narrower, the branch name gives way first.
        let rows = draw(&state, 80, 12);
        assert!(
            rows[0].starts_with("┌[1] Status · …c · merging┐"),
            "{rows:#?}"
        );
    }

    /// The side panels other than Files, each with its rect, on a
    /// `width` x `height` screen with Files focused and expanded.
    fn collapsed_rects(state: &AppState, width: u16, height: u16) -> Vec<(SidePanel, Rect)> {
        let [main, _] = screen_layout(Rect::new(0, 0, width, height));
        let layout = workspace_layout(main, state.focus.panel(), state.expanded, state.screen_mode);
        SidePanel::ALL
            .into_iter()
            .filter(|&side| side != SidePanel::Files)
            .filter_map(|side| Some((side, layout.rect(Panel::Side(side))?)))
            .collect()
    }

    #[test]
    fn a_two_row_collapsed_panel_has_corners_and_sides_at_80_by_16() {
        let mut state = loaded(vec![file("a.rs")]);
        state.focus = Focus::Files;
        state.expanded = SidePanel::Files;
        let buffer = draw_buffer(&state, 80, 16);
        let rows = buffer_rows(&buffer);
        let panels = collapsed_rects(&state, 80, 16);
        assert_eq!(panels.len(), 4, "{rows:#?}");
        for (side, area) in panels {
            assert_eq!(area.height, 2, "{side:?}: {rows:#?}");
            let right = area.right() - 1;
            let (top, line) = (area.y, area.y + 1);
            assert_eq!(buffer[(0, top)].symbol(), "┌", "{side:?}: {rows:#?}");
            assert_eq!(buffer[(right, top)].symbol(), "┐", "{side:?}: {rows:#?}");
            assert_eq!(buffer[(0, line)].symbol(), "│", "{side:?}: {rows:#?}");
            assert_eq!(buffer[(right, line)].symbol(), "│", "{side:?}: {rows:#?}");
        }
        // The Diff pane's corner meets the Status panel's, not a bare rule.
        let status = collapsed_rects(&state, 80, 16)[0].1;
        assert_eq!(buffer[(status.right(), 0)].symbol(), "┌", "{rows:#?}");
        // The summary still shows, inside the side borders.
        assert!(rows[1].starts_with("│main "), "{rows:#?}");
    }

    #[test]
    fn a_title_only_collapsed_panel_starts_with_a_corner() {
        let mut state = loaded(vec![file("a.rs")]);
        state.focus = Focus::Files;
        state.expanded = SidePanel::Files;
        let buffer = draw_buffer(&state, 80, 12);
        let rows = buffer_rows(&buffer);
        let panels = collapsed_rects(&state, 80, 12);
        assert!(!panels.is_empty(), "{rows:#?}");
        for (side, area) in panels {
            assert_eq!(area.height, 1, "{side:?}: {rows:#?}");
            assert_eq!(buffer[(0, area.y)].symbol(), "┌", "{side:?}: {rows:#?}");
            assert_eq!(
                buffer[(area.right() - 1, area.y)].symbol(),
                "┐",
                "{side:?}: {rows:#?}"
            );
        }
    }

    #[test]
    fn help_scrolls_to_its_last_entry_at_80_by_8() {
        use crate::intent::{AppIntent, Scroll};
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        state.update(AppIntent::Resize {
            width: 80,
            height: 8,
        });
        state.update(AppIntent::OpenHelp);
        let mut last = usize::MAX;
        while state.help_scroll != last {
            last = state.help_scroll;
            state.update(AppIntent::ScrollHelp(Scroll::PageDown));
        }
        let rows = draw(&state, 80, 8);
        assert!(contains(&rows, "Quit Kika"), "{rows:#?}");
        assert!(contains(&rows, "▲"), "{rows:#?}");
        assert!(!contains(&rows, "more ▼"), "{rows:#?}");
    }

    // --- List filters, ranges and the diff search ---

    /// The buffer cell where `needle` starts, by its char column.
    fn cell_at<'a>(buffer: &'a ratatui::buffer::Buffer, needle: &str) -> &'a ratatui::buffer::Cell {
        let rows = buffer_rows(buffer);
        let (y, row) = rows
            .iter()
            .enumerate()
            .find(|(_, row)| row.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} not on screen:\n{}", rows.join("\n")));
        let x = row[..row.find(needle).unwrap()].chars().count();
        &buffer[(x as u16, y as u16)]
    }

    fn typing(state: &mut AppState, target: crate::list_filter::InputTarget, text: &str) {
        state.input = Some(crate::list_filter::ListInput {
            target,
            line: crate::popup::LineInput::with_text(text),
        });
    }

    /// The style of the cell `skip` columns into where `needle` starts.
    fn style_in(buffer: &ratatui::buffer::Buffer, needle: &str, skip: u16) -> Style {
        let (x, y) = find(buffer, needle);
        buffer[(x + skip, y)].style()
    }

    /// "one" is the current match in `alpha one`, the other in `gamma one`.
    fn assert_marked(buffer: &ratatui::buffer::Buffer) {
        assert_eq!(style_in(buffer, "alpha one", 6).bg, CURRENT_MATCH.bg);
        assert!(
            style_in(buffer, "alpha one", 6)
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );
        assert_eq!(style_in(buffer, "gamma one", 6).bg, MATCH.bg);
        assert_ne!(
            style_in(buffer, "gamma one", 0).bg,
            MATCH.bg,
            "only the match"
        );
    }

    fn alpha_beta_gamma() -> Vec<DiffLine> {
        vec![
            diff_line(LineKind::Added, "+alpha one"),
            diff_line(LineKind::Context, " beta"),
            diff_line(LineKind::Removed, "-gamma one"),
        ]
    }

    #[test]
    fn the_preview_marks_its_matches_and_counts_them_with_a_plus_while_streaming() {
        let mut state = with_diff(alpha_beta_gamma());
        state.diff.done = false;
        state.diff.set_search("one");
        let buffer = draw_buffer(&state, 100, 12);
        assert_marked(&buffer);
        let rows = buffer_rows(&buffer);
        assert!(contains(&rows, "match 1 of 2+"), "{rows:#?}");
        assert!(!contains(&rows, "one ·"), "the query is not in the title");

        state.diff.done = true;
        let rows = draw(&state, 100, 12);
        assert!(contains(&rows, "match 1 of 2 "), "{rows:#?}");
        state.diff.set_search("zzz");
        assert!(contains(&draw(&state, 100, 12), "no matches"));
    }

    #[test]
    fn the_blame_marks_matches_in_the_text_only() {
        let lines = vec![
            blame_line(OID, "one", 1, "alpha one"),
            blame_line(OID, "one", 2, "beta"),
            blame_line(OID, "one", 3, "gamma one"),
        ];
        let mut state = blamed(lines, 1);
        if let Focus::Blame(view) = &mut state.focus {
            view.set_search("one");
        }
        let buffer = draw_buffer(&state, 100, 12);
        assert_marked(&buffer);
        // The author is also "one", and is never marked.
        assert_ne!(style_in(&buffer, "one  ", 0).bg, MATCH.bg);
        let rows = buffer_rows(&buffer);
        assert!(contains(&rows, "Blame a.rs · match 1 of 2"), "{rows:#?}");
    }

    #[test]
    fn the_blames_commit_marks_its_own_matches() {
        let mut state = blamed(vec![blame_line(OID, "Ann", 1, "x")], 0);
        if let Focus::Blame(view) = &mut state.focus {
            let mut commit = CommitView::new(OID.into(), 1);
            commit.loaded(Ok(alpha_beta_gamma()));
            commit.set_search("one");
            view.commit = Some(commit);
        }
        let buffer = draw_buffer(&state, 100, 12);
        assert_marked(&buffer);
        let rows = buffer_rows(&buffer);
        assert!(
            contains(&rows, "Commit abcdef1 · match 1 of 2"),
            "{rows:#?}"
        );
    }

    /// The Files preview of `lines`, searched for `query`.
    fn preview_searching(lines: Vec<DiffLine>, query: &str) -> AppState {
        let mut state = with_diff(lines);
        state.diff.set_search(query);
        state
    }

    /// A blame of `texts`, the cursor on the first, searched for `query`.
    fn blame_searching(texts: &[&str], query: &str) -> AppState {
        let lines = texts
            .iter()
            .enumerate()
            .map(|(i, text)| blame_line(OID, "Ann", i + 1, text))
            .collect();
        let mut state = blamed(lines, 0);
        if let Focus::Blame(view) = &mut state.focus {
            view.set_search(query);
        }
        state
    }

    /// A blame with its commit of `lines` open, the commit searched for `query`.
    fn blame_commit_searching(lines: Vec<DiffLine>, query: &str) -> AppState {
        let mut state = blamed(vec![blame_line(OID, "Ann", 1, "x")], 0);
        if let Focus::Blame(view) = &mut state.focus {
            let mut commit = CommitView::new(OID.into(), 1);
            commit.loaded(Ok(lines));
            commit.set_search(query);
            view.commit = Some(commit);
        }
        state
    }

    /// Whether `style` has either match background.
    fn match_styled(style: Style) -> bool {
        style.bg == MATCH.bg || style.bg == CURRENT_MATCH.bg
    }

    #[test]
    fn switching_from_files_to_commits_leaves_no_match_marked() {
        let mut state = preview_searching(alpha_beta_gamma(), "one");
        assert_marked(&draw_buffer(&state, 80, 24));
        state.update(crate::intent::AppIntent::FocusPanel(Panel::Side(
            SidePanel::Commits,
        )));
        let buffer = draw_buffer(&state, 80, 24);
        for cell in buffer.content() {
            assert!(!match_styled(cell.style()), "{cell:?}");
        }
    }

    #[test]
    fn the_clipping_ellipsis_is_never_drawn_as_a_match() {
        let long = "q".repeat(200);
        let text = format!("abc {long} end");
        let states = [
            (
                "preview",
                preview_searching(vec![diff_line(LineKind::Added, &format!("+{text}"))], &long),
            ),
            ("blame", blame_searching(&["x", &text], &long)),
            (
                "blame commit",
                blame_commit_searching(
                    vec![diff_line(LineKind::Added, &format!("+{text}"))],
                    &long,
                ),
            ),
        ];
        for (pane, state) in states {
            let buffer = draw_buffer(&state, 80, 24);
            let (x, y) = find(&buffer, "abc q");
            let ellipsis = (x..buffer.area.width)
                .find(|&x| buffer[(x, y)].symbol() == "…")
                .unwrap_or_else(|| {
                    panic!("{pane}: no … in {:?}", buffer_rows(&buffer)[y as usize])
                });
            assert!(
                !match_styled(buffer[(ellipsis, y)].style()),
                "{pane}: {:?}",
                buffer[(ellipsis, y)]
            );
            assert_eq!(buffer[(ellipsis - 1, y)].symbol(), "q", "{pane}");
            assert_eq!(
                buffer[(ellipsis - 1, y)].style().bg,
                CURRENT_MATCH.bg,
                "{pane}: the visible part is still marked"
            );
            assert_eq!(buffer[(x + 4, y)].style().bg, CURRENT_MATCH.bg, "{pane}");
            assert!(!match_styled(buffer[(x, y)].style()), "{pane}");
        }
    }

    #[test]
    fn a_marked_clip_drops_the_marks_from_its_ellipsis() {
        let spans = |line: &str, width, marks: &[(usize, usize, Style)]| -> Vec<(String, Style)> {
            clipped_marked(line, width, Style::new(), marks)
                .into_iter()
                .map(|span| (span.content.into_owned(), span.style))
                .collect()
        };
        let plain = Style::new();
        // A match across the cut stops before the `…`; one past it is gone.
        assert_eq!(
            spans("abcdefghij", 5, &[(2, 8, MATCH), (8, 9, CURRENT_MATCH)]),
            [
                ("ab".to_string(), plain),
                ("cd".to_string(), MATCH),
                ("…".to_string(), plain),
            ]
        );
        // A match only on the cut cell is dropped.
        assert_eq!(
            spans("abcdefghij", 5, &[(4, 5, MATCH)]),
            [("abcd…".to_string(), plain)]
        );
        // Uncut text keeps its marks, a `…` of its own included.
        assert_eq!(
            spans("abc…", 5, &[(3, 4, MATCH)]),
            [("abc".to_string(), plain), ("…".to_string(), MATCH)]
        );
    }

    #[test]
    fn each_search_pane_draws_at_80_by_24_and_at_20_by_5() {
        let states = [
            ("preview", preview_searching(alpha_beta_gamma(), "one")),
            (
                "blame",
                blame_searching(&["alpha one", "beta", "gamma one"], "one"),
            ),
            (
                "blame commit",
                blame_commit_searching(alpha_beta_gamma(), "one"),
            ),
        ];
        for (pane, state) in states {
            let buffer = draw_buffer(&state, 80, 24);
            let (x, y) = find(&buffer, "alpha one");
            for skip in 6..9 {
                assert_eq!(buffer[(x + skip, y)].style().bg, CURRENT_MATCH.bg, "{pane}");
                assert!(
                    buffer[(x + skip, y)]
                        .style()
                        .add_modifier
                        .contains(Modifier::BOLD | Modifier::UNDERLINED),
                    "{pane}"
                );
            }
            let buffer = draw_buffer(&state, 20, 5);
            assert_eq!(buffer.area, Rect::new(0, 0, 20, 5), "{pane}");
            assert_eq!(buffer.content().len(), 20 * 5, "{pane}");
        }
    }

    #[test]
    fn each_new_search_draws_its_text_escaped_and_its_keys_on_the_bar() {
        use crate::list_filter::InputTarget;
        let mut files = with_diff(alpha_beta_gamma());
        files.focus = Focus::Files;
        let mut blame = blamed(vec![blame_line(OID, "Ann", 1, "alpha one")], 0);
        let mut commit = blame.clone();
        if let Focus::Blame(view) = &mut commit.focus {
            let mut open = CommitView::new(OID.into(), 1);
            open.loaded(Ok(alpha_beta_gamma()));
            view.commit = Some(open);
        }
        for (state, target) in [
            (&mut files, InputTarget::PreviewSearch),
            (&mut blame, InputTarget::BlameSearch),
            (&mut commit, InputTarget::BlameCommitSearch),
        ] {
            typing(state, target, "a\u{202E}b");
            let rows = draw(state, 100, 20);
            assert!(
                rows[19].starts_with(" /a<U+202E>b"),
                "{target:?}: {rows:#?}"
            );
            assert!(!contains(&rows, "\u{202E}"), "{target:?}: {rows:#?}");
            state.input = None;
        }

        files.diff.set_search("one");
        if let Focus::Blame(view) = &mut blame.focus {
            view.set_search("one");
        }
        if let Focus::Blame(view) = &mut commit.focus {
            view.commit.as_mut().unwrap().set_search("one");
        }
        for state in [&files, &blame, &commit] {
            let rows = draw(state, 120, 20);
            assert!(
                rows[19].starts_with(" n next match  N previous match  esc clear search"),
                "{:?}",
                rows[19]
            );
        }
        // In Files a filter goes before the search, so Esc says "clear".
        files.files_filter = "a".into();
        let rows = draw(&files, 120, 20);
        assert!(
            rows[19].starts_with(" n next match  N previous match  esc clear "),
            "{:?}",
            rows[19]
        );
        assert!(!rows[19].contains("clear search"), "{:?}", rows[19]);
    }

    #[test]
    fn the_bar_shows_the_typed_filter_made_safe() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        typing(
            &mut state,
            crate::list_filter::InputTarget::FilesFilter,
            "a\u{202E}b",
        );
        let rows = draw(&state, 100, 20);
        assert!(rows[19].starts_with(" /a<U+202E>b"), "{rows:#?}");
        assert!(rows[19].contains("q and ? are typed"), "{rows:#?}");
        assert!(!contains(&rows, "\u{202E}"), "{rows:#?}");
    }

    #[test]
    fn the_bar_names_the_log_query_being_typed() {
        let mut state = loaded(vec![file("a.rs")]);
        typing(
            &mut state,
            crate::list_filter::InputTarget::LogAuthor,
            "a\u{202E}b",
        );
        let rows = draw(&state, 100, 20);
        assert!(rows[19].starts_with(" author: a<U+202E>b"), "{rows:#?}");
        typing(&mut state, crate::list_filter::InputTarget::LogPath, "src");
        let rows = draw(&state, 100, 20);
        assert!(rows[19].starts_with(" path: src"), "{rows:#?}");
    }

    #[test]
    fn a_filtered_files_title_counts_what_shows() {
        let files: Vec<FileEntry> = (0..7)
            .map(|i| file(&format!("a{i}.rs")))
            .chain((0..3).map(|i| file(&format!("b{i}.md"))))
            .collect();
        let mut state = loaded(files);
        state.selected = Some(7);
        state.files_filter = "md".into();
        let rows = draw(&state, 100, 30);
        assert!(contains(&rows, "Files · /md 3 of 10"), "{rows:#?}");
        assert!(contains(&rows, "b2.md"), "{rows:#?}");
        assert!(!contains(&rows, "a0.rs"), "{rows:#?}");

        state.files_filter = "zzz".into();
        let rows = draw(&state, 100, 30);
        assert!(contains(&rows, "No matches"), "{rows:#?}");
    }

    #[test]
    fn range_rows_are_drawn_like_the_selected_row() {
        let mut state = loaded(vec![file("a.rs"), file("b.rs"), file("c.rs")]);
        state.selected = Some(1);
        state.files_anchor = Some("c.rs".into());
        let buffer = draw_buffer(&state, 100, 20);
        let reversed = |needle| {
            cell_at(&buffer, needle)
                .modifier
                .contains(Modifier::REVERSED)
        };
        assert!(reversed("b.rs"), "the cursor row");
        assert!(reversed("c.rs"), "the anchor row");
        assert!(!reversed("a.rs"));
        assert!(contains(&buffer_rows(&buffer), "Files · 2 selected"));
        // The cursor, the end that moves, stands out from the rest.
        let bold = |needle| cell_at(&buffer, needle).modifier.contains(Modifier::BOLD);
        assert!(bold("b.rs"), "the cursor row");
        assert!(!bold("c.rs"), "the anchor row");
    }

    #[test]
    fn diff_matches_are_marked_and_the_current_one_stands_out() {
        let mut state = hunks(DiffSide::Worktree, TWO_HUNKS);
        if let Focus::Diff(view) = &mut state.focus {
            view.set_search("one", EmojiZwj::Escape);
        }
        let buffer = draw_buffer(&state, 100, 20);
        assert!(contains(&buffer_rows(&buffer), "match 1 of 2"));
        // `-old one` is row 4 and holds the current match; `+new one` the other.
        let style = |needle: &str, skip: u16| {
            let cell = cell_at(&buffer, needle);
            let (x, y) = position_of(&buffer, cell);
            buffer[(x + skip, y)].style()
        };
        assert_eq!(style("old one", 4).bg, CURRENT_MATCH.bg);
        assert_eq!(style("new one", 4).bg, MATCH.bg);
        assert_ne!(MATCH.bg, CURRENT_MATCH.bg);
        assert_eq!(
            style("old one", 0).bg,
            Some(Color::Reset),
            "only the match is marked"
        );
        let underlined = |skip| {
            style("old one", skip)
                .add_modifier
                .contains(Modifier::UNDERLINED)
        };
        assert!(underlined(4), "the current match stands out without colour");
        assert!(
            !style("new one", 4)
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );
    }

    /// Where `cell` sits in `buffer`.
    fn position_of(buffer: &ratatui::buffer::Buffer, cell: &ratatui::buffer::Cell) -> (u16, u16) {
        let index = buffer
            .content
            .iter()
            .position(|c| std::ptr::eq(c, cell))
            .unwrap();
        let (x, y) = buffer.pos_of(index);
        (x, y)
    }

    fn bisect_state() -> crate::git::bisect::BisectState {
        crate::git::bisect::BisectState {
            start: "main".into(),
            good: 2,
            bad: 1,
            skip: 3,
            ..Default::default()
        }
    }

    /// Bisecting with `bisect`, `HEAD` at `head`, the tested commit loaded
    /// and the Status panel focused.
    fn bisecting(head: Head, bisect: crate::git::bisect::BisectState) -> AppState {
        let mut status = branch(head);
        status.in_progress = vec![Operation::Bisect];
        status.bisect = Some(bisect);
        let mut state = on(status);
        state.commits.rows = vec![crate::git::log::CommitRow {
            oid: "abc1234def".into(),
            short_oid: "abc1234".into(),
            parents: vec![],
            author: "Ann".into(),
            time: 0,
            subject: "the tested one".into(),
            subject_altered: false,
        }];
        let graph = state.commits.builder.push(&state.commits.rows[0]);
        state.commits.graph = vec![graph];
        status_focused(&state)
    }

    /// `bisecting`, drawn on a screen wide enough for its lines.
    fn bisect_rows(head: Head, bisect: crate::git::bisect::BisectState) -> Vec<String> {
        thin(draw(&bisecting(head, bisect), 200, 30))
    }

    fn found(subject: &str) -> Option<crate::git::bisect::Found> {
        Some(crate::git::bisect::Found {
            commit: crate::git::commit_files::CommitId::parse("abc1234def").unwrap(),
            subject: subject.into(),
        })
    }

    #[test]
    fn at_80_by_24_the_found_line_comes_right_after_the_counts_and_lines_fit() {
        let mut bisect = bisect_state();
        bisect.first_bad = found(&"s".repeat(72));
        bisect.start = "a-long-branch-name-that-cannot-fit".into();
        let rows = thin(draw(
            &bisecting(Head::Detached("abc1234def".into()), bisect),
            80,
            24,
        ));
        let counts = rows
            .iter()
            .position(|r| r.starts_with("│Bisect: "))
            .unwrap();
        assert!(
            rows[counts + 1].starts_with("│◆ Found: abc1234 s"),
            "{rows:#?}"
        );
        assert!(rows[counts + 1].contains('…'), "{rows:#?}");
        assert!(rows[counts].contains('…'), "{rows:#?}");
        // Cut inside the panel: its right border is still drawn where the
        // top border's corner is.
        let right = rows[0].chars().position(|c| c == '┐').unwrap();
        for row in &rows[counts..counts + 4] {
            assert_eq!(row.chars().nth(right), Some('│'), "{row:?}");
        }
    }

    #[test]
    fn a_bisect_draws_at_tiny_sizes_without_panicking() {
        let mut bisect = bisect_state();
        bisect.first_bad = found("broke it");
        bisect.terms = crate::git::bisect::Terms {
            bad: "new".into(),
            good: "old".into(),
        };
        let state = bisecting(Head::Detached("abc1234def".into()), bisect);
        for (width, height) in [(20, 8), (10, 4), (3, 3), (1, 1), (0, 0), (80, 5)] {
            draw(&state, width, height);
        }
    }

    #[test]
    fn the_expanded_panel_shows_the_bisect_under_its_marker() {
        let rows = bisect_rows(Head::Detached("abc1234def".into()), bisect_state());
        for line in [
            "│bisecting",
            "│Bisect: 2 good · 1 bad · 3 skipped",
            "│Testing: abc1234 the tested one",
            "│Started from: main",
        ] {
            assert!(contains(&rows, line), "{line}: {rows:#?}");
        }
        assert!(!contains(&rows, "Terms:"), "{rows:#?}");
        assert!(!contains(&rows, "Found:"), "{rows:#?}");
    }

    #[test]
    fn before_git_checks_out_a_commit_the_panel_asks_for_marks() {
        let rows = bisect_rows(Head::Branch("main".into()), bisect_state());
        assert!(
            contains(&rows, "│Mark a good and a bad commit"),
            "{rows:#?}"
        );
        let rows = bisect_rows(Head::Detached("fedcba98765".into()), bisect_state());
        assert!(contains(&rows, "│Testing: fedcba9 "), "{rows:#?}");
    }

    #[test]
    fn the_found_commit_and_custom_terms_are_shown() {
        let mut bisect = bisect_state();
        bisect.first_bad = Some(crate::git::bisect::Found {
            commit: crate::git::commit_files::CommitId::parse("abc1234def").unwrap(),
            subject: "broke it".into(),
        });
        bisect.terms = crate::git::bisect::Terms {
            bad: "new".into(),
            good: "old".into(),
        };
        let rows = bisect_rows(Head::Detached("abc1234def".into()), bisect);
        assert!(contains(&rows, "│Terms: new/old"), "{rows:#?}");
        assert!(contains(&rows, "│◆ Found: abc1234 broke it"), "{rows:#?}");

        let mut stuck = bisect_state();
        stuck.only_skipped = true;
        let rows = bisect_rows(Head::Detached("abc1234def".into()), stuck);
        assert!(contains(&rows, "│Only skipped commits left"), "{rows:#?}");
    }

    #[test]
    fn a_hostile_start_and_subject_draw_without_control_characters() {
        let mut bisect = bisect_state();
        bisect.start = crate::git::diff::terminal_safe(b"evil\x1b[2J\x07\xe2\x80\xae");
        bisect.first_bad = Some(crate::git::bisect::Found {
            commit: crate::git::commit_files::CommitId::parse("abc1234def").unwrap(),
            subject: crate::git::diff::terminal_safe(b"bad\x1b]0;x\x07 \xe2\x80\xaeok"),
        });
        let rows = bisect_rows(Head::Detached("abc1234def".into()), bisect);
        assert!(
            contains(&rows, "│Started from: evil\u{FFFD}[2J"),
            "{rows:#?}"
        );
        assert!(
            contains(&rows, "│◆ Found: abc1234 bad\u{FFFD}]0;x"),
            "{rows:#?}"
        );
        for row in &rows {
            assert!(
                !row.chars().any(|c| c.is_control() || c == '\u{202E}'),
                "{row:?}"
            );
        }
    }

    #[test]
    fn a_long_bisect_line_is_cut_to_the_panel() {
        let mut bisect = bisect_state();
        bisect.start = "x".repeat(300);
        let rows = bisect_rows(Head::Detached("abc1234def".into()), bisect);
        let started = rows.iter().find(|r| r.contains("Started from:")).unwrap();
        assert!(started.contains('…'), "{started:?}");
    }

    #[test]
    fn a_long_announcement_keeps_the_way_to_reset_beside_the_bar_hints() {
        let subject = "s".repeat(72);
        let notice = crate::bisect::found_notice("abc1234", &subject);
        let mut quiet = loaded(vec![]);
        quiet.notice_text = Some(notice.clone());
        let mut busy = quiet.clone();
        busy.busy = 1;
        let mut running = quiet.clone();
        running.command_running = true;
        // The cancel hint takes 22 columns, so 50 leaves too little room
        // for any notice with it; 80 has room with every hint.
        let cases = [
            ("quiet", &quiet, 50),
            ("busy", &busy, 50),
            ("quiet", &quiet, 80),
            ("busy", &busy, 80),
            ("running", &running, 80),
        ];
        for (name, state, width) in cases {
            let rows = draw(state, width, 24);
            let bar = &rows[23];
            assert!(
                bar.contains("Bisect found abc1234 (a resets): s"),
                "{name} at {width}: {bar:?}"
            );
            assert!(bar.ends_with('…'), "{name} at {width}: {bar:?}");
            assert_eq!(width_of(bar), usize::from(width), "{name}: {bar:?}");
        }
    }

    #[test]
    fn the_status_bar_offers_good_bad_skip_and_abort_while_bisecting() {
        let mut status = branch(Head::Detached("abc1234def".into()));
        status.in_progress = vec![Operation::Bisect];
        status.bisect = Some(bisect_state());
        let rows = draw(&status_focused(&on(status)), 120, 24);
        for hint in ["s skip", "a abort", "g good", "b bad"] {
            assert!(rows[23].contains(hint), "{hint}: {:?}", rows[23]);
        }
    }

    #[test]
    fn a_huge_filter_title_is_elided_in_linear_time() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        state.files_filter = "a".repeat(10_000);
        let start = std::time::Instant::now();
        let rows = draw(&state, 100, 20);
        // Quadratic elision took seconds at this length; linear takes far
        // less than one.
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        assert!(contains(&rows, "… 0 of 1"), "{rows:#?}");
    }

    #[test]
    fn a_wide_query_is_elided_without_splitting_a_glyph() {
        let mut state = loaded(vec![file("日本語.md"), file("a.rs")]);
        state.selected = Some(0);
        state.files_filter = "日本語".repeat(20);
        let rows = draw(&state, 120, 20);
        assert!(contains(&rows, "… 0 of 2"), "{rows:#?}");
        state.files_filter = "日本".into();
        let rows = draw(&state, 120, 20);
        assert!(contains(&rows, "Files · /日本 1 of 2"), "{rows:#?}");
        assert!(contains(&rows, "日本語.md"), "{rows:#?}");
        assert!(!contains(&rows, "a.rs"), "{rows:#?}");
    }

    #[test]
    fn a_long_input_keeps_the_cursor_on_the_bar() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        typing(
            &mut state,
            crate::list_filter::InputTarget::FilesFilter,
            &"x".repeat(100),
        );
        for width in [80u16, 30, 12, 4] {
            let (line, cursor) = status_bar(&state, width);
            let cursor = cursor.expect("a cursor while typing");
            assert!(cursor < usize::from(width), "{width}: cursor at {cursor}");
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            let shown = if width > 4 { " /…x" } else { " /…" };
            assert!(text.starts_with(shown), "{width}: {text:?}");
        }
        // Short text shows whole, the cursor after it.
        typing(
            &mut state,
            crate::list_filter::InputTarget::FilesFilter,
            "日x",
        );
        let (line, cursor) = status_bar(&state, 80);
        assert!(line.spans[0].content.starts_with(" /日x"));
        assert_eq!(
            cursor,
            Some(5),
            "` /` then two columns for 日 and one for x"
        );
    }

    /// Draw at 80x24 with `text` typed into `target` and the full-input
    /// notice set, then check the bottom row keeps the prompt, part of the
    /// text, the cursor and the whole notice in yellow, and nothing raw.
    fn assert_full_notice_shows(target: crate::list_filter::InputTarget, text: &str) {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        typing(&mut state, target, text);
        state.notice = Some(crate::list_filter::input_full_notice().into());
        let (buffer, (cx, cy)) = draw_with_cursor(&state, 80, 24);
        let bar = &buffer_rows(&buffer)[23];
        let notice = crate::list_filter::input_full_notice();
        assert!(bar.contains(&notice), "{bar:?}");
        for skip in 0..width_of(&notice) as u16 {
            assert_eq!(style_in(&buffer, &notice, skip).fg, Some(Color::Yellow));
        }
        let typed = format!(" {}…x", target.prompt());
        assert!(bar.starts_with(&typed), "{bar:?}");
        assert_eq!(cy, 23);
        let notice_at = width_of(&bar[..bar.find(&notice).unwrap()]);
        assert!(
            usize::from(cx) < notice_at,
            "the cursor sits before the notice: {cx} in {bar:?}"
        );
        assert!(!bar.chars().any(char::is_control), "{bar:?}");
    }

    #[test]
    fn a_full_filter_still_shows_why_the_key_did_nothing() {
        let mut text = "x".repeat(crate::list_filter::MAX_INPUT - 2);
        text.insert_str(10, "\x1b\x07");
        assert_full_notice_shows(crate::list_filter::InputTarget::FilesFilter, &text);
    }

    #[test]
    fn a_full_diff_search_still_shows_why_the_key_did_nothing() {
        let text = "x".repeat(crate::list_filter::MAX_INPUT);
        assert_full_notice_shows(crate::list_filter::InputTarget::DiffSearch, &text);
    }

    #[test]
    fn a_full_input_on_a_narrow_bar_elides_the_notice() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        typing(
            &mut state,
            crate::list_filter::InputTarget::FilesFilter,
            &"x".repeat(crate::list_filter::MAX_INPUT),
        );
        state.notice = Some(crate::list_filter::input_full_notice().into());
        for width in [40u16, 20, 8, 4] {
            let (buffer, (cx, cy)) = draw_with_cursor(&state, width, 24);
            let bar = &buffer_rows(&buffer)[23];
            assert!(width_of(bar) <= usize::from(width), "{width}: {bar:?}");
            assert!(bar.starts_with(" /…"), "{width}: {bar:?}");
            assert_eq!(cy, 23);
            assert!(cx < width, "{width}: cursor at {cx}");
        }
        let (line, _) = status_bar(&state, 40);
        let notice = line.spans.last().unwrap();
        assert_eq!(notice.style.fg, Some(Color::Yellow));
        assert!(notice.content.starts_with(" Input"), "{notice:?}");
        assert!(notice.content.ends_with('…'), "{notice:?}");
    }

    #[test]
    fn a_match_after_a_wide_glyph_is_marked_where_it_shows() {
        let wide = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-a\n+日本x\n";
        let mut state = hunks(DiffSide::Worktree, wide.as_bytes());
        if let Focus::Diff(view) = &mut state.focus {
            view.set_search("x", EmojiZwj::Escape);
        }
        let buffer = draw_buffer(&state, 100, 20);
        let rows = buffer_rows(&buffer);
        let (y, row) = rows
            .iter()
            .enumerate()
            .find(|(_, r)| r.contains("+日本x"))
            .unwrap();
        // Columns, not chars: each wide glyph takes two.
        let x = width_of(&row[..row.find("本x").unwrap()]) + 2;
        assert_eq!(buffer[(x as u16, y as u16)].symbol(), "x");
        assert_eq!(buffer[(x as u16, y as u16)].style().bg, CURRENT_MATCH.bg);
    }

    #[test]
    fn filters_ranges_and_searches_draw_at_any_size() {
        let mut files = loaded(vec![file("a.rs"), file("b.rs"), file("日本.md")]);
        files.selected = Some(1);
        files.files_filter = "rs".into();
        files.files_anchor = Some("a.rs".into());
        let mut typing_files = files.clone();
        typing(
            &mut typing_files,
            crate::list_filter::InputTarget::FilesFilter,
            "rs\u{202E}日",
        );
        let mut search = hunks(DiffSide::Worktree, TWO_HUNKS);
        if let Focus::Diff(view) = &mut search.focus {
            view.set_search("one", EmojiZwj::Escape);
        }
        for state in [&files, &typing_files, &search] {
            for (w, h) in [(80, 24), (40, 10), (10, 3), (2, 2), (1, 1), (0, 0)] {
                draw(state, w, h);
            }
        }
    }

    #[test]
    fn esc_clear_leads_the_bar_while_there_is_something_to_clear() {
        let mut state = loaded(vec![file("a.rs"), file("b.rs")]);
        state.selected = Some(0);
        let rows = draw(&state, 120, 10);
        assert!(rows[9].contains("/ filter  V range"), "{:?}", rows[9]);
        assert!(!rows[9].contains("esc clear"), "{:?}", rows[9]);
        state.files_filter = "a".into();
        let rows = draw(&state, 120, 10);
        assert!(
            rows[9].starts_with(" esc clear  space stage"),
            "{:?}",
            rows[9]
        );

        let mut search = hunks(DiffSide::Worktree, TWO_HUNKS);
        let rows = draw(&search, 120, 20);
        assert!(rows[19].contains("/ search"), "{:?}", rows[19]);
        if let Focus::Diff(view) = &mut search.focus {
            view.set_search("one", EmojiZwj::Escape);
        }
        let rows = draw(&search, 120, 20);
        assert!(
            rows[19].starts_with(" n next match  N previous match  esc clear search"),
            "{:?}",
            rows[19]
        );
    }

    #[test]
    fn esc_clear_leads_the_stash_bar_while_it_has_a_filter_or_a_range() {
        let mut state = AppState::new(Context::Workspace);
        state.files_loaded = true;
        state.focus = Focus::Stash;
        state.stash.begin(1);
        state.stash.loaded(
            1,
            Ok(vec![crate::features::stash::StashEntry {
                index: 0,
                oid: crate::git::commit_files::CommitId::parse("a0").unwrap(),
                base: crate::git::commit_files::CommitId::parse("b0").unwrap(),
                untracked: None,
                subject: "On main: first".into(),
            }]),
        );
        let rows = draw(&state, 160, 10);
        assert!(rows[9].contains("/ filter"), "{:?}", rows[9]);
        assert!(rows[9].contains("V range"), "{:?}", rows[9]);
        assert!(!rows[9].contains("esc clear"), "{:?}", rows[9]);
        state.stash.set_filter("first".into());
        let rows = draw(&state, 160, 10);
        assert!(rows[9].starts_with(" esc clear"), "{:?}", rows[9]);
        state.stash.set_filter(String::new());
        state.stash.toggle_range();
        let rows = draw(&state, 160, 10);
        assert!(rows[9].starts_with(" esc clear"), "{:?}", rows[9]);
    }

    #[test]
    fn esc_clear_leads_the_reflog_tabs_bar_while_it_has_a_filter() {
        let mut state = AppState::new(Context::Workspace);
        state.files_loaded = true;
        state.focus = Focus::Commits;
        state.commits.tab = crate::features::commits::CommitsTab::Reflog;
        state.commits.reflog.data = Some(crate::features::commits::reflog::Reflog {
            entries: crate::features::commits::reflog::git::parse_records(include_bytes!(
                "git/fixtures/reflog/commit.bin"
            )),
            ..Default::default()
        });
        let rows = draw(&state, 120, 10);
        assert!(rows[9].contains("/ filter"), "{:?}", rows[9]);
        assert!(!rows[9].contains("esc clear"), "{:?}", rows[9]);
        assert!(!rows[9].contains("V range"), "{:?}", rows[9]);
        state.commits.reflog.set_filter("first".into());
        let rows = draw(&state, 120, 10);
        assert!(rows[9].starts_with(" esc clear"), "{:?}", rows[9]);
        assert!(rows[9].contains("/ filter"), "{:?}", rows[9]);
    }

    #[test]
    fn esc_clear_leads_a_commits_files_bar_while_it_has_a_filter_then_clear_search() {
        use crate::commit_files::CommitFilesList;
        use crate::git::commit_files::{CommitFile, CommitId, CommitRange};
        let mut state = AppState::new(Context::Workspace);
        state.files_loaded = true;
        state.focus = Focus::Commits;
        state.expanded = SidePanel::Commits;
        let range = CommitRange {
            oid: CommitId::parse("abc").unwrap(),
            parent: CommitId::parse("def"),
        };
        let mut open = CommitFilesList::begin(range, "abc".into(), 1);
        let file = CommitFile {
            status: 'M',
            path: "a.rs".into(),
            orig_path: None,
        };
        open.loaded(1, Ok(vec![file]));
        state.commits.open = Some(open);
        let rows = draw(&state, 160, 10);
        assert!(rows[9].contains("/ filter"), "{:?}", rows[9]);
        assert!(rows[9].contains("esc back"), "{:?}", rows[9]);
        assert!(!rows[9].contains("esc clear"), "{:?}", rows[9]);

        state.diff.set_search("one");
        let rows = draw(&state, 160, 10);
        assert!(
            rows[9].starts_with(" n next match  N previous match  esc clear search"),
            "{:?}",
            rows[9]
        );

        // The filter goes before the search, so Esc says "clear".
        state.commits.open.as_mut().unwrap().set_filter("a".into());
        let rows = draw(&state, 160, 10);
        assert!(
            rows[9].starts_with(" n next match  N previous match  esc clear "),
            "{:?}",
            rows[9]
        );
        assert!(!rows[9].contains("clear search"), "{:?}", rows[9]);

        state.diff.search = None;
        let rows = draw(&state, 160, 10);
        assert!(rows[9].starts_with(" esc clear "), "{:?}", rows[9]);
        assert!(rows[9].contains("/ filter"), "{:?}", rows[9]);
    }
}
