//! Draws the Branches panel, and the selected branch's log in the Diff
//! pane's place while the panel has focus. Reads state only.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use crate::branches::{BranchesPanel, BranchesTab};
use crate::forge::{CiState, ForgeState, ForgeStatus, ForgeView, PrBadge, PrState};
use crate::git::branches::{Branch, BranchName, LogRow, Track};
use crate::remotes_view;
use crate::tags_view;
use crate::view::{PanelLook, clip, elide_right, fit, pad, reversed, width_of};

/// Columns given to a log row's author, so subjects line up.
const LOG_AUTHOR: usize = 16;

/// The branch list: why it failed, a placeholder until the first load,
/// or one row per branch with the selection reversed. Collapsed, the
/// current branch stands in for the list; with only a title row, the
/// title names it. `spinner` names the branch a fetch, pull or push runs
/// for, and the frame to draw in place of its marker.
pub fn render_panel(
    panel: &BranchesPanel,
    forge: &ForgeState,
    look: &PanelLook,
    expanded: bool,
    spinner: Option<(&BranchName, &str)>,
    frame: &mut Frame,
    area: Rect,
) {
    let row = |branch: &Branch, width| {
        let frame = spinner
            .filter(|(name, _)| **name == branch.name)
            .map(|(_, frame)| frame);
        branch_row(branch, frame, forge.badges.get(&branch.name), width)
    };
    let current = panel.current();
    let title = match current {
        Some(branch) if look.title_only => {
            look.title(&format!("Branches · {}", branch.name.display()))
        }
        _ if expanded => look.title(&tabs_title(panel.tab, look)),
        _ => look.title("Branches"),
    };
    let mut block = look.block(title);
    let inner = block.inner(area);
    let width = usize::from(inner.width);
    if let Some(line) = forge_line(forge, width) {
        block = block.title_bottom(Line::from(line));
    }
    let body = if expanded && panel.tab == BranchesTab::Remotes {
        remotes_view::list_body(&panel.remotes, usize::from(inner.height), width)
    } else if expanded && panel.tab == BranchesTab::Tags {
        tags_view::list_body(&panel.tags, usize::from(inner.height), width)
    } else if let Some(error) = &panel.error {
        let mut lines: Vec<Line> = error.lines().map(Line::from).collect();
        lines.push(Line::from("Press r to retry"));
        Paragraph::new(lines).wrap(Wrap { trim: false })
    } else if !panel.loaded {
        Paragraph::new("Loading…")
    } else if panel.branches.is_empty() {
        Paragraph::new("No branches")
    } else if !expanded {
        Paragraph::new(current.map(|b| row(b, width)).unwrap_or_default())
    } else {
        let rows = usize::from(inner.height);
        let selected = panel.selected.unwrap_or(0);
        // Scrolled just enough to keep the selection in view.
        let first = selected.saturating_sub(rows.saturating_sub(1));
        let lines = panel
            .branches
            .iter()
            .enumerate()
            .skip(first)
            .take(rows)
            .map(|(i, branch)| {
                let line = Line::from(row(branch, width));
                if Some(i) == panel.selected {
                    reversed(line, width)
                } else {
                    line
                }
            })
            .collect::<Vec<_>>();
        Paragraph::new(lines)
    };
    frame.render_widget(body.block(block), area);
}

/// The tabs, the one shown in brackets. Where the whole title does not
/// fit, the panel's name goes first, then the other tabs shrink to three
/// letters, then to one, and last they go. The shown tab stays, in
/// brackets, shortened itself only when nothing else is left.
fn tabs_title(shown: BranchesTab, look: &PanelLook) -> String {
    const TABS: [(BranchesTab, &str); 3] = [
        (BranchesTab::Local, "Local"),
        (BranchesTab::Remotes, "Remotes"),
        (BranchesTab::Tags, "Tags"),
    ];
    let tabs = |others: usize| {
        TABS.iter()
            .map(|&(tab, name)| match tab == shown {
                true => format!("[{name}]"),
                false => name.chars().take(others).collect(),
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let whole = tabs(usize::MAX);
    let ladder = [format!("Branches · {whole}"), whole, tabs(3), tabs(1)];
    if let Some(title) = ladder.into_iter().find(|title| look.fits(title)) {
        return title;
    }
    // Only the shown tab is left; it shrinks too, rather than be cut.
    let name = TABS
        .iter()
        .find(|(tab, _)| *tab == shown)
        .map_or("", |t| t.1);
    let short = |n: usize| format!("[{}]", name.chars().take(n).collect::<String>());
    [short(usize::MAX), short(3), short(1)]
        .into_iter()
        .find(|title| look.fits_bare(title))
        .unwrap_or_else(|| short(1))
}

/// `* name  upstream ↑1 ↓2  #12 open ✓`: `*` marks the current branch,
/// counts of zero are left out, a deleted upstream says `gone`, and the
/// branch's pull request ends the row. The name loses its start first,
/// so the upstream, counts and pull request stay in sight. A `spinner`
/// frame takes the marker's place.
fn branch_row(
    branch: &Branch,
    spinner: Option<&str>,
    badge: Option<&PrBadge>,
    width: usize,
) -> String {
    let marker = match spinner {
        Some(frame) => format!("{frame} "),
        None if branch.head => "* ".to_string(),
        None => "  ".to_string(),
    };
    let track = match branch.track {
        Track::None => String::new(),
        Track::Gone => " gone".to_string(),
        Track::Counts { ahead, behind } => {
            let mut counts = String::new();
            if ahead > 0 {
                counts.push_str(&format!(" ↑{ahead}"));
            }
            if behind > 0 {
                counts.push_str(&format!(" ↓{behind}"));
            }
            counts
        }
    };
    let mut suffix = match &branch.upstream {
        Some(upstream) => format!("  {upstream}{track}"),
        None => String::new(),
    };
    if let Some(badge) = badge {
        let badge = format!("  {}", badge_text(badge));
        // Too narrow for both beside a name: the pull request stays.
        if width_of(&suffix) + width_of(&badge) + 3 > width {
            suffix = badge;
        } else {
            suffix.push_str(&badge);
        }
    }
    let name = fit(&branch.name.display(), &suffix, width.saturating_sub(2));
    elide_right(&format!("{marker}{name}"), width)
}

/// `#12 open ✓`: the number, the state, and a mark for the checks when
/// there are any: ✓ passed, ✗ failed, ● running.
fn badge_text(badge: &PrBadge) -> String {
    let state = match badge.state {
        PrState::Open => "open",
        PrState::Draft => "draft",
        PrState::Merged => "merged",
        PrState::Closed => "closed",
    };
    let ci = match badge.ci {
        Some(CiState::Success) => " ✓",
        Some(CiState::Failure) => " ✗",
        Some(CiState::Pending) => " ●",
        None => "",
    };
    format!("#{} {state}{ci}", badge.number)
}

/// The panel's bottom line about the forge, fitted to `width`, or none:
/// when it is off, the repository is not on a configured host, or all is
/// well. What went wrong comes first; the hint and ` · cached` (old
/// badges are still drawn) follow only where they fit, and what still
/// does not fit ends in `…`. Fixed words and the configured host only;
/// nothing the forge or `gh` said.
fn forge_line(forge: &ForgeState, width: usize) -> Option<String> {
    let repo = forge.repo.as_ref()?;
    let problem = match forge.status {
        ForgeView::Off | ForgeView::Cached | ForgeView::Fresh => return None,
        ForgeView::Updating => return Some(elide_right("GitHub: updating…", width)),
        ForgeView::Problem(problem) => problem,
    };
    let (what, hint) = match problem {
        ForgeStatus::NotLoggedIn => (
            "GitHub: not logged in",
            Some(format!(
                "run gh auth login --hostname {}",
                repo.host.to_ascii_lowercase()
            )),
        ),
        ForgeStatus::NoGh => (
            "GitHub: gh not found",
            Some("install the GitHub CLI to see pull requests".to_string()),
        ),
        ForgeStatus::Offline => ("GitHub: offline", None),
        ForgeStatus::NoAccess => ("GitHub: repository not found or no access", None),
        ForgeStatus::Failed => ("GitHub: could not load pull requests", None),
        ForgeStatus::RateLimited => (
            "GitHub: rate limited",
            forge
                .limited_until
                .map(|until| format!("until {} UTC", clock(until))),
        ),
    };
    // A rate limit leaves no room for `cached`: when it ends matters more,
    // and the line says no new answer came anyway.
    let cached =
        (!forge.badges.is_empty() && problem != ForgeStatus::RateLimited).then_some("cached");
    let join = |parts: &[Option<&str>]| {
        std::iter::once(what)
            .chain(parts.iter().flatten().copied())
            .collect::<Vec<_>>()
            .join(" · ")
    };
    let hint = hint.as_deref();
    let ladder = [
        join(&[hint, cached]),
        join(&[hint]),
        join(&[cached]),
        what.to_string(),
    ];
    let fitting = ladder.iter().find(|line| width_of(line) <= width);
    Some(fitting.cloned().unwrap_or_else(|| elide_right(what, width)))
}

/// `HH:MM` of the UTC day `secs` (since the epoch) falls in, by plain
/// arithmetic: no clock is read and no time zone is known.
fn clock(secs: i64) -> String {
    let of_day = secs.rem_euclid(86_400);
    format!("{:02}:{:02}", of_day / 3600, of_day % 3600 / 60)
}

/// The selected branch's newest commits, one per row, scrolled by the
/// log's own scroll.
pub fn render_log(panel: &BranchesPanel, look: &PanelLook, frame: &mut Frame, area: Rect) {
    let log = &panel.log;
    // Where in the log the pane is, as the Diff pane says it.
    let position = if log.rows.is_empty() || log.error.is_some() {
        String::new()
    } else {
        format!(" · {}/{}", log.scroll + 1, log.rows.len())
    };
    let title = match log.name() {
        Some(name) => look.title(&format!("Commits · {name}{position}")),
        None => look.title("Commits"),
    };
    let block = look.block(title);
    let inner = block.inner(area);
    let (rows, width) = (usize::from(inner.height), usize::from(inner.width));
    let body = if let Some(error) = &log.error {
        Paragraph::new(error.lines().map(Line::from).collect::<Vec<_>>()).wrap(Wrap { trim: false })
    } else if log.for_ref.is_none() {
        Paragraph::new(if panel.loaded {
            "No branch selected"
        } else {
            "Loading…"
        })
    } else if log.rows.is_empty() {
        Paragraph::new(if log.seq.is_some() {
            "Loading…"
        } else {
            "No commits"
        })
    } else {
        let lines = log
            .rows
            .iter()
            .skip(log.scroll)
            .take(rows)
            .map(|row| Line::from(clip(&log_row(row), 0, width)))
            .collect::<Vec<_>>();
        Paragraph::new(lines)
    };
    frame.render_widget(body.block(block), area);
}

/// `<oid> <date> <author> <subject>`, the author padded so subjects align.
fn log_row(row: &LogRow) -> String {
    format!(
        "{} {} {} {}",
        row.short_oid,
        row.date,
        pad(&elide_right(&row.author, LOG_AUTHOR), LOG_AUTHOR),
        row.subject
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::layout::SidePanel;
    use crate::model::{AppState, Context, Focus};
    use crate::view::{buffer_rows, render};

    fn draw(state: &AppState, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(state, frame)).unwrap();
        buffer_rows(terminal.backend().buffer())
    }

    fn contains(rows: &[String], needle: &str) -> bool {
        rows.iter().any(|row| row.contains(needle))
    }

    fn branch(name: &str, head: bool, upstream: Option<&str>, track: Track) -> Branch {
        Branch {
            name: name.into(),
            head,
            upstream: upstream.map(Into::into),
            track,
            merged: true,
        }
    }

    /// A workspace with the Branches panel focused, showing `branches`.
    fn focused(branches: Vec<Branch>) -> AppState {
        let mut state = AppState::new(Context::Workspace);
        state.files_loaded = true;
        state.focus = Focus::Branches;
        state.expanded = SidePanel::Branches;
        state.branches.loaded(Ok(branches));
        state
    }

    /// The Branches panel drawn on its own, focused, for checks that only
    /// care what it shows.
    pub(crate) fn draw_branches(state: &AppState, width: u16, height: u16) -> Vec<String> {
        let mut state = state.clone();
        state.files_loaded = true;
        state.focus = Focus::Branches;
        state.expanded = SidePanel::Branches;
        draw(&state, width, height)
    }

    fn pr(number: u64, state: PrState, ci: Option<CiState>) -> PrBadge {
        PrBadge { number, state, ci }
    }

    /// A focused panel of `rows`, each branch with its pull request, on a
    /// repository that matched github.com.
    fn with_badges(rows: &[(&str, Option<PrBadge>)]) -> AppState {
        let mut state = focused(
            rows.iter()
                .map(|(name, _)| branch(name, false, None, Track::None))
                .collect(),
        );
        state.forge.repo = Some(crate::forge::ForgeRepo {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "widgets".into(),
        });
        state.forge.status = ForgeView::Fresh;
        for (name, badge) in rows {
            if let Some(badge) = badge {
                state.forge.badges.insert((*name).into(), *badge);
            }
        }
        state
    }

    #[test]
    fn each_badge_form_is_drawn_after_the_track_counts() {
        let mut state = with_badges(&[
            ("ok", Some(pr(123, PrState::Open, Some(CiState::Success)))),
            ("wip", Some(pr(7, PrState::Draft, Some(CiState::Pending)))),
            ("done", Some(pr(9, PrState::Merged, None))),
            ("nope", Some(pr(4, PrState::Closed, Some(CiState::Failure)))),
            ("bare", None),
        ]);
        state.branches.branches[0].upstream = Some("origin/ok".into());
        state.branches.branches[0].track = Track::Counts {
            ahead: 1,
            behind: 0,
        };
        let rows = draw(&state, 160, 24);
        assert!(
            contains(&rows, "  ok  origin/ok ↑1  #123 open ✓"),
            "{rows:#?}"
        );
        assert!(contains(&rows, "  wip  #7 draft ●"), "{rows:#?}");
        assert!(contains(&rows, "  done  #9 merged "), "{rows:#?}");
        assert!(contains(&rows, "  nope  #4 closed ✗"), "{rows:#?}");
        assert!(contains(&rows, "  bare "), "{rows:#?}");
        assert!(!contains(&rows, "bare  #"), "{rows:#?}");
    }

    #[test]
    fn a_narrow_row_cuts_the_name_not_the_badge() {
        let long = "feature/".repeat(10);
        let row = branch_row(
            &branch(&long, false, Some("origin/x"), Track::None),
            None,
            Some(&pr(42, PrState::Open, Some(CiState::Failure))),
            34,
        );
        assert!(row.ends_with("origin/x  #42 open ✗"), "{row:?}");
        assert!(row.starts_with("  …"), "{row:?}");
        assert_eq!(crate::view::width_of(&row), 34);
    }

    /// The forge line drawn at `width` x 24, borders trimmed off.
    fn forge_line_at(state: &AppState, width: u16) -> Option<String> {
        let rows = draw(state, width, 24);
        rows.iter()
            .find_map(|row| row.find("GitHub:").map(|at| row[at..].to_string()))
            .map(|text| {
                text.trim_end_matches(['─', '┘', '╯', ' ', '│', '┛', '━'])
                    .to_string()
            })
    }

    #[test]
    fn each_forge_status_draws_its_exact_line() {
        let mut state = with_badges(&[("main", Some(pr(1, PrState::Open, None)))]);
        let line = |state: &AppState| forge_line_at(state, 240);
        assert_eq!(line(&state), None, "Fresh says nothing");
        state.forge.status = ForgeView::Cached;
        assert_eq!(line(&state), None);
        state.forge.status = ForgeView::Updating;
        assert_eq!(line(&state).as_deref(), Some("GitHub: updating…"));
        let cases = [
            (
                ForgeStatus::NotLoggedIn,
                "GitHub: not logged in · run gh auth login --hostname github.com",
            ),
            (
                ForgeStatus::NoGh,
                "GitHub: gh not found · install the GitHub CLI to see pull requests",
            ),
            (ForgeStatus::Offline, "GitHub: offline"),
            (
                ForgeStatus::NoAccess,
                "GitHub: repository not found or no access",
            ),
            (ForgeStatus::Failed, "GitHub: could not load pull requests"),
            // With no reset known, no time is given.
            (ForgeStatus::RateLimited, "GitHub: rate limited"),
        ];
        // With old badges still drawn, every problem but a rate limit says
        // they are cached.
        for (problem, want) in cases {
            state.forge.status = ForgeView::Problem(problem);
            let cached = match problem {
                ForgeStatus::RateLimited => want.to_string(),
                _ => format!("{want} · cached"),
            };
            assert_eq!(
                line(&state).as_deref(),
                Some(cached.as_str()),
                "{problem:?}"
            );
        }
        let badges = std::mem::take(&mut state.forge.badges);
        for (problem, want) in cases {
            state.forge.status = ForgeView::Problem(problem);
            assert_eq!(line(&state).as_deref(), Some(want), "{problem:?}");
        }
        state.forge.badges = badges;

        // Off, or a repository on no configured host: nothing at all.
        state.forge.status = ForgeView::Off;
        assert_eq!(line(&state), None);
        state.forge.status = ForgeView::Problem(ForgeStatus::Failed);
        state.forge.repo = None;
        assert_eq!(line(&state), None);
    }

    #[test]
    fn at_80_by_24_the_side_panel_line_says_what_is_wrong_first() {
        let mut state = with_badges(&[("main", Some(pr(1, PrState::Open, None)))]);
        let mut at_80 = |problem| {
            state.forge.status = ForgeView::Problem(problem);
            forge_line_at(&state, 80)
        };
        // The default side panel holds 25 columns: the hint and `cached`
        // give way, and what went wrong stays whole.
        assert_eq!(
            at_80(ForgeStatus::NotLoggedIn).as_deref(),
            Some("GitHub: not logged in")
        );
        assert_eq!(
            at_80(ForgeStatus::NoGh).as_deref(),
            Some("GitHub: gh not found")
        );
        assert_eq!(
            at_80(ForgeStatus::NoAccess).as_deref(),
            Some("GitHub: repository not f…")
        );
        // A little wider, `cached` comes back before the long hint.
        state.forge.status = ForgeView::Problem(ForgeStatus::NotLoggedIn);
        assert_eq!(
            forge_line(&state.forge, 30).as_deref(),
            Some("GitHub: not logged in · cached")
        );
        assert_eq!(
            forge_line(&state.forge, 63).as_deref(),
            Some("GitHub: not logged in · run gh auth login --hostname github.com")
        );
        // Collapsed, at the same size, the line is the same.
        state.focus = Focus::Files;
        state.expanded = SidePanel::Files;
        assert_eq!(
            forge_line_at(&state, 80).as_deref(),
            Some("GitHub: not logged in")
        );
        // However narrow, it never runs past the panel.
        for width in 0..30 {
            let line = forge_line(&state.forge, width).unwrap();
            assert!(crate::view::width_of(&line) <= width, "{width}: {line:?}");
        }
    }

    #[test]
    fn a_rate_limit_says_until_when_in_utc() {
        let mut state = with_badges(&[("main", Some(pr(1, PrState::Open, None)))]);
        state.forge.status = ForgeView::Problem(ForgeStatus::RateLimited);
        // 2023-11-14 22:13:20 UTC.
        state.forge.limited_until = Some(1_700_000_000);
        // Badges are drawn, but the line gives its room to the time.
        assert!(!state.forge.badges.is_empty());
        let full = "GitHub: rate limited · until 22:13 UTC";
        assert_eq!(forge_line(&state.forge, 80).as_deref(), Some(full));
        assert_eq!(forge_line(&state.forge, 38).as_deref(), Some(full));
        assert_eq!(
            forge_line(&state.forge, 37).as_deref(),
            Some("GitHub: rate limited")
        );
        assert_eq!(
            forge_line(&state.forge, 22).as_deref(),
            Some("GitHub: rate limited")
        );
        // Drawn: whole where the panel is wide, what is wrong first in the
        // default 80 x 24 side panel. No popup and no error.
        assert_eq!(forge_line_at(&state, 240).as_deref(), Some(full));
        assert_eq!(
            forge_line_at(&state, 80).as_deref(),
            Some("GitHub: rate limited")
        );
        assert!(state.popups.is_empty());
        assert_eq!(state.op_error, None);
        // Midnight and the last minute of a day.
        state.forge.limited_until = Some(1_699_920_000);
        assert!(
            forge_line(&state.forge, 80)
                .unwrap()
                .contains("until 00:00 UTC")
        );
        state.forge.limited_until = Some(1_699_920_000 + 86_399);
        assert!(
            forge_line(&state.forge, 80)
                .unwrap()
                .contains("until 23:59 UTC")
        );
    }

    #[test]
    fn a_tiny_terminal_draws_without_a_status_line_or_a_panic() {
        let mut state = with_badges(&[
            ("main", Some(pr(1, PrState::Open, Some(CiState::Success)))),
            (
                "日本語のブランチ",
                Some(pr(22, PrState::Draft, Some(CiState::Pending))),
            ),
        ]);
        state.forge.status = ForgeView::Problem(ForgeStatus::Offline);
        for (width, height) in [(10, 4), (80, 10), (80, 6), (20, 24), (1, 1)] {
            draw(&state, width, height);
        }
        let rows = draw(&state, 160, 24);
        assert!(
            contains(&rows, "日本語のブランチ  #22 draft ●"),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_narrow_row_with_an_upstream_keeps_the_badge_over_the_upstream() {
        let row = branch_row(
            &branch(
                "topic",
                false,
                Some("origin/topic"),
                Track::Counts {
                    ahead: 2,
                    behind: 0,
                },
            ),
            None,
            Some(&pr(12, PrState::Open, Some(CiState::Success))),
            24,
        );
        assert_eq!(row, "  topic  #12 open ✓");
        // Room for both: both stay.
        let wide = branch_row(
            &branch("topic", false, Some("origin/topic"), Track::None),
            None,
            Some(&pr(12, PrState::Open, None)),
            60,
        );
        assert_eq!(wide, "  topic  origin/topic  #12 open");
        // A wide name gives way by columns, not bytes.
        let cjk = branch_row(
            &branch("日本語のブランチ名前", false, None, Track::None),
            None,
            Some(&pr(3, PrState::Merged, None)),
            16,
        );
        assert!(cjk.ends_with("  #3 merged"), "{cjk:?}");
        assert!(crate::view::width_of(&cjk) <= 16, "{cjk:?}");
    }

    #[test]
    fn rows_show_the_current_marker_upstream_counts_and_gone() {
        let state = focused(vec![
            branch(
                "main",
                true,
                Some("origin/main"),
                Track::Counts {
                    ahead: 1,
                    behind: 2,
                },
            ),
            branch("old", false, Some("origin/old"), Track::Gone),
            branch("local", false, None, Track::None),
            branch(
                "synced",
                false,
                Some("origin/synced"),
                Track::Counts {
                    ahead: 0,
                    behind: 0,
                },
            ),
        ]);
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, "* main  origin/main ↑1 ↓2"), "{rows:#?}");
        assert!(contains(&rows, "  old  origin/old gone"), "{rows:#?}");
        assert!(contains(&rows, "  local "), "{rows:#?}");
        assert!(contains(&rows, "  synced  origin/synced "), "{rows:#?}");
        assert!(!contains(&rows, "↑0"), "{rows:#?}");
    }

    #[test]
    fn a_long_name_gives_way_to_the_upstream_and_counts() {
        let long = "feature/".repeat(10);
        let row = branch_row(
            &branch(
                &long,
                false,
                Some("origin/x"),
                Track::Counts {
                    ahead: 3,
                    behind: 0,
                },
            ),
            None,
            None,
            30,
        );
        assert!(row.ends_with("origin/x ↑3"), "{row:?}");
        assert!(row.starts_with("  …"), "{row:?}");
        assert_eq!(crate::view::width_of(&row), 30);
    }

    #[test]
    fn the_panel_says_loading_none_or_why_it_failed() {
        let mut state = focused(vec![]);
        assert!(contains(&draw(&state, 80, 24), "No branches"));
        state.branches = BranchesPanel::default();
        assert!(contains(&draw(&state, 80, 24), "Loading…"));
        state.branches.loaded(Err("fatal: \u{FFFD}broken".into()));
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, "fatal: \u{FFFD}broken"), "{rows:#?}");
        assert!(contains(&rows, "Press r to retry"), "{rows:#?}");
    }

    #[test]
    fn a_collapsed_panel_shows_the_current_branch() {
        let mut state = focused(vec![
            branch("topic", false, None, Track::None),
            branch("main", true, None, Track::None),
        ]);
        state.focus = Focus::Files;
        state.expanded = SidePanel::Files;
        let rows = draw(&state, 80, 40);
        assert!(contains(&rows, "* main"), "{rows:#?}");
        assert!(!contains(&rows, "topic"), "{rows:#?}");
    }

    #[test]
    fn a_running_push_spins_on_its_branch_row_and_names_itself_in_the_bar() {
        use crate::git::remote::RemoteOp;
        use crate::intent::AppIntent;
        use crate::remote_run::RemoteRun;

        let mut state = focused(vec![
            branch("main", true, None, Track::None),
            branch("topic", false, None, Track::None),
        ]);
        let push = RemoteOp::Push {
            force_with_lease: false,
        };
        state.remote = Some(RemoteRun::new(push, 1, Some("main".into())));
        state.busy = 1;

        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, "| main"), "{rows:#?}");
        assert!(!contains(&rows, "* main"), "{rows:#?}");
        assert!(contains(&rows, "  topic"), "{rows:#?}");
        let bar = rows.last().unwrap();
        assert!(bar.starts_with(" pushing… "), "{bar:?}");
        assert!(!contains(&rows, "loading…"), "{rows:#?}");

        state.update(AppIntent::SpinnerTick);
        assert!(contains(&draw(&state, 80, 24), "/ main"));

        // Collapsed, the panel spins on the same row.
        state.focus = Focus::Files;
        state.expanded = SidePanel::Files;
        let rows = draw(&state, 80, 40);
        assert!(contains(&rows, "/ main"), "{rows:#?}");
    }

    #[test]
    fn the_selected_row_is_reversed() {
        let mut state = focused(vec![
            branch("main", true, None, Track::None),
            branch("topic", false, None, Track::None),
        ]);
        state.branches.move_by(1);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(&state, frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let rows = buffer_rows(buffer);
        let y = rows.iter().position(|r| r.contains("  topic")).unwrap();
        let x = rows[y].chars().position(|c| c == 't').unwrap();
        let style = buffer[(x as u16, y as u16)].style();
        assert!(
            style
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
    }

    fn log_row_of(subject: &str) -> LogRow {
        LogRow {
            short_oid: "abc1234".into(),
            date: "2024-05-06".into(),
            author: "Ann".into(),
            subject: subject.into(),
        }
    }

    #[test]
    fn with_branches_focused_the_diff_slot_shows_the_selected_branch_commits() {
        let mut state = focused(vec![branch("main", true, None, Track::None)]);
        state.branches.log.begin(1, b"refs/heads/main".to_vec());
        state
            .branches
            .log
            .apply(1, Ok(vec![log_row_of("Second"), log_row_of("First")]));
        let rows = draw(&state, 100, 24);
        assert!(contains(&rows, "[0] Commits · main"), "{rows:#?}");
        let author = format!("Ann{}", " ".repeat(LOG_AUTHOR - 3));
        assert!(
            contains(&rows, &format!("abc1234 2024-05-06 {author} Second")),
            "{rows:#?}"
        );
        assert!(contains(&rows, "First"), "{rows:#?}");

        assert!(contains(&rows, "[0] Commits · main · 1/2 "), "{rows:#?}");

        state.branches.log.scroll = 1;
        let rows = draw(&state, 100, 24);
        assert!(!contains(&rows, "Second"), "{rows:#?}");
        assert!(contains(&rows, "[0] Commits · main · 2/2 "), "{rows:#?}");

        state.focus = Focus::Files;
        let rows = draw(&state, 100, 24);
        assert!(!contains(&rows, "Commits · main"), "{rows:#?}");
    }

    #[test]
    fn a_wide_author_and_subject_are_clipped_to_the_pane() {
        let mut state = focused(vec![branch("main", true, None, Track::None)]);
        state.branches.log.begin(1, b"refs/heads/main".to_vec());
        let wide = LogRow {
            author: "山田太郎山田太郎山田太郎".into(),
            subject: "日本語".repeat(30),
            ..log_row_of("")
        };
        state.branches.log.apply(1, Ok(vec![wide]));
        let rows = draw(&state, 80, 24);
        let row = rows
            .iter()
            .find(|r| r.contains("abc1234"))
            .expect("the commit row");
        // Sixteen columns of author: eight wide glyphs, elided to fit.
        assert!(row.contains("山田太郎山田太…"), "{row:?}");
        assert_eq!(crate::view::width_of(row), 80, "{rows:#?}");
        assert!(row.ends_with('│') || row.ends_with('┃'), "{row:?}");
    }

    #[test]
    fn the_log_says_loading_empty_or_why_it_failed() {
        let mut state = focused(vec![branch("main", true, None, Track::None)]);
        state.branches.log.begin(1, b"refs/heads/main".to_vec());
        assert!(contains(&draw(&state, 100, 24), "Loading…"));
        state.branches.log.apply(1, Ok(vec![]));
        assert!(contains(&draw(&state, 100, 24), "No commits"));
        state.branches.log.begin(2, b"refs/heads/main".to_vec());
        state.branches.log.apply(2, Err("fatal: bad ref".into()));
        assert!(contains(&draw(&state, 100, 24), "fatal: bad ref"));
    }

    #[test]
    fn tiny_terminals_do_not_panic_with_branches() {
        let mut state = focused(vec![branch(
            "main",
            true,
            Some("origin/main"),
            Track::Counts {
                ahead: 1,
                behind: 2,
            },
        )]);
        state.branches.log.begin(1, b"refs/heads/main".to_vec());
        state.branches.log.apply(1, Ok(vec![log_row_of("x")]));
        for (w, h) in [(1, 1), (2, 3), (10, 4), (20, 6), (0, 0)] {
            draw(&state, w, h);
        }
    }
}
