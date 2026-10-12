//! Draws the Branches panel's Remotes tab, and, for a remote's header
//! row, what that remote is in the Diff pane's place. Reads state only.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::{RemoteRow, RemotesList};
use crate::git::remotes::Remote;
use crate::view::{PanelLook, elide_right, no_matches, range_row, scrolled};

/// What the tab shows when there are no remotes.
const NO_REMOTES: &str = "No remotes. Press a to add one.";

/// What a remote with no branches says, since adding one never fetches.
pub const NO_BRANCHES: &str = "No branches yet. Press f to fetch.";

/// The tab's body in `rows` by `width`: why the load failed, a
/// placeholder, or a header row per remote with its branches indented
/// below, as the filter shows them, the selection and the range reversed
/// and scrolled into view.
pub fn list_body(list: &RemotesList, rows: usize, width: usize) -> Paragraph<'static> {
    if let Some(error) = &list.error {
        let mut lines: Vec<Line> = error.lines().map(|l| Line::from(l.to_string())).collect();
        lines.push(Line::from("Press r to retry"));
        return Paragraph::new(lines).wrap(Wrap { trim: false });
    }
    if !list.loaded {
        return Paragraph::new("Loading…");
    }
    if list.remotes.is_empty() {
        return Paragraph::new(NO_REMOTES);
    }
    let shown = list.visible();
    if shown.is_empty() {
        return Paragraph::new(no_matches());
    }
    let all = list.rows();
    let range = list.range();
    let lines = scrolled(&shown, list.selected, rows)
        .iter()
        .map(|&i| {
            let line = match all[i] {
                RemoteRow::Remote(r) => header(&list.remotes[r], width),
                RemoteRow::Branch(r, b) => {
                    let short = list.remotes[r].branches[b].short_display();
                    Line::from(elide_right(&format!("  {short}"), width))
                }
            };
            range_row(line, i, list.selected, &range, width)
        })
        .collect::<Vec<_>>();
    Paragraph::new(lines)
}

/// `name  url`, the name bold; the URL loses its end to fit. A group no
/// configured remote owns says so in the URL's place.
fn header(remote: &Remote, width: usize) -> Line<'static> {
    let name = remote.name.display();
    let rest = match (&remote.url, remote.configured) {
        (Some(url), _) => format!("  {url}"),
        (None, false) => "  (not configured)".to_string(),
        (None, true) => String::new(),
    };
    let text = elide_right(&format!("{name}{rest}"), width);
    let (bold, plain) = match text.strip_prefix(name.as_str()) {
        Some(plain) => (name.clone(), plain.to_string()),
        None => (text, String::new()),
    };
    Line::from(vec![
        Span::styled(bold, Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(plain),
    ])
}

/// The Diff pane for a remote's header row, or for an empty tab: the
/// remote's name and URL, and how to see its branches if it has none.
pub fn render_remote(list: &RemotesList, look: &PanelLook, frame: &mut Frame, area: Rect) {
    let Some(remote) = list.selected_remote() else {
        let text = if list.loaded {
            NO_REMOTES
        } else {
            "Loading…"
        };
        frame.render_widget(
            Paragraph::new(text).block(look.block(look.title("Remote"))),
            area,
        );
        return;
    };
    let name = remote.name.display();
    let mut lines = vec![Line::from(name.clone())];
    match &remote.url {
        Some(url) => lines.push(Line::from(url.clone())),
        None => lines.push(Line::from(
            "Not a configured remote: refs left under this name",
        )),
    }
    if remote.branches.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(NO_BRANCHES));
    }
    let block = look.block(look.title(&format!("Remote · {name}")));
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block),
        area,
    );
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::features::branches::BranchesTab;
    use crate::git::remotes::{RemoteBranch, RemoteName};
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

    fn remote(name: &str, url: Option<&str>, branches: &[&str]) -> Remote {
        Remote {
            name: name.into(),
            url: url.map(Into::into),
            configured: url.is_some(),
            branches: branches
                .iter()
                .map(|short| RemoteBranch {
                    remote: RemoteName::from(name),
                    short: short.as_bytes().to_vec(),
                    refname: format!("refs/remotes/{name}/{short}").into_bytes(),
                })
                .collect(),
        }
    }

    /// The Branches panel focused on its Remotes tab, showing `remotes`.
    fn focused(remotes: Vec<Remote>) -> AppState {
        let mut state = AppState::new(Context::Workspace);
        state.files_loaded = true;
        state.focus = Focus::Branches;
        state.expanded = SidePanel::Branches;
        state.branches.loaded(Ok(vec![]));
        state.branches.tab = BranchesTab::Remotes;
        state.branches.remotes.loaded(Ok(remotes));
        state
    }

    #[test]
    fn the_tab_draws_each_remote_then_its_branches_indented() {
        let mut state = focused(vec![
            remote("fork", Some("/srv/fork"), &[]),
            remote("origin", Some("https://h/r"), &["feature/x", "main"]),
            remote("stray", None, &["y"]),
        ]);
        state.branches.remotes.move_by(2);
        let refname = b"refs/remotes/origin/feature/x".to_vec();
        state.branches.log.begin(1, refname);
        let rows = draw(&state, 80, 24);
        let at = |needle: &str| {
            rows.iter()
                .position(|r| r.contains(needle))
                .unwrap_or_else(|| panic!("{needle} in {rows:#?}"))
        };
        assert!(at("fork  /srv/fork") < at("origin  https://h/r"));
        assert_eq!(
            at("┃  feature/x"),
            at("origin  https://h/r") + 1,
            "{rows:#?}"
        );
        assert_eq!(at("┃  main"), at("origin  https://h/r") + 2, "{rows:#?}");
        assert!(contains(&rows, "stray  (not configured)"), "{rows:#?}");
        // The selected branch's log takes the Diff pane.
        assert!(contains(&rows, "Commits · origin/feature/x"), "{rows:#?}");
    }

    #[test]
    fn the_title_marks_the_tab_shown() {
        let mut state = focused(vec![remote("origin", Some("/r"), &[])]);
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, " [3] Loc [Remotes] Tag "), "{rows:#?}");
        state.branches.tab = BranchesTab::Local;
        assert!(contains(&draw(&state, 80, 24), " [3] [Local] Rem Tag "));
        // With room, the panel's name stays too.
        state.screen_mode = crate::layout::ScreenMode::Full;
        let rows = draw(&state, 80, 24);
        assert!(
            contains(&rows, " [3] Branches · [Local] Remotes Tags "),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_url_with_credentials_and_control_characters_draws_redacted_and_safe() {
        use crate::git::fake::{FakeRunner, ok};
        let fake = FakeRunner::new()
            .expect(
                &["config", "-z", "--get-regexp", r"^remote\..*\.url$"],
                ok(b"remote.origin.url\nhttps://me:t0ken@h/\x1b[31mr\0"),
            )
            .expect(
                &[
                    "for-each-ref",
                    "--format=%(refname)%00%(symref)",
                    "refs/remotes",
                ],
                ok(b""),
            );
        let remotes = crate::git::remotes::list_remotes(&fake).unwrap();
        let state = focused(remotes);
        let rows = draw(&state, 80, 24);
        assert!(
            contains(&rows, "origin  https://h/\u{FFFD}[31mr"),
            "{rows:#?}"
        );
        assert!(!contains(&rows, "t0ken"), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains('\x1b')), "{rows:#?}");
    }

    #[test]
    fn a_header_row_shows_the_remote_and_how_to_fetch_when_it_has_no_branches() {
        let mut state = focused(vec![
            remote("fork", Some("/srv/fork"), &[]),
            remote("origin", Some("/srv/origin"), &["main"]),
        ]);
        let rows = draw(&state, 100, 24);
        assert!(contains(&rows, "Remote · fork"), "{rows:#?}");
        assert!(contains(&rows, NO_BRANCHES), "{rows:#?}");

        state.branches.remotes.move_by(1);
        let rows = draw(&state, 100, 24);
        assert!(contains(&rows, "Remote · origin"), "{rows:#?}");
        assert!(contains(&rows, "/srv/origin"), "{rows:#?}");
        assert!(!contains(&rows, NO_BRANCHES), "{rows:#?}");
    }

    #[test]
    fn a_filter_draws_safe_with_its_count_and_a_range_reversed() {
        let mut state = focused(vec![
            remote("fork", Some("/srv/fork"), &["x"]),
            remote("origin", Some("/srv/origin"), &["main", "topic"]),
        ]);
        state.screen_mode = crate::layout::ScreenMode::Full;
        state.branches.remotes.set_filter("o\x1b[31m".into());
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| render(&state, frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rows = buffer_rows(&buffer);
        assert!(
            buffer
                .content()
                .iter()
                .all(|c| !c.symbol().contains('\x1b'))
        );
        assert!(contains(&rows, "0 of 5"), "{rows:#?}");

        // origin's header and its two branches show; fork's do not.
        state.branches.remotes.set_filter("origin".into());
        state.branches.remotes.toggle_range();
        state.branches.remotes.move_by(1);
        terminal.draw(|frame| render(&state, frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rows = buffer_rows(&buffer);
        assert!(contains(&rows, "/origin 3 of 5 · 2 selected"), "{rows:#?}");
        assert!(!contains(&rows, "/srv/fork"), "{rows:#?}");
        let reversed = crate::test_support::reversed_at;
        assert!(reversed(&buffer, "origin  /srv/origin"));
        assert!(reversed(&buffer, "  main"));
        assert!(!reversed(&buffer, "  topic"));
    }

    #[test]
    fn the_tab_says_loading_none_or_why_it_failed() {
        let mut state = focused(vec![]);
        assert!(contains(&draw(&state, 80, 24), NO_REMOTES));
        state.branches.remotes = RemotesList::default();
        assert!(contains(&draw(&state, 80, 24), "Loading…"));
        state
            .branches
            .remotes
            .loaded(Err("fatal: \u{FFFD}bad".into()));
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, "fatal: \u{FFFD}bad"), "{rows:#?}");
        assert!(contains(&rows, "Press r to retry"), "{rows:#?}");
    }

    #[test]
    fn wide_names_are_elided_to_the_panel_and_the_selection_fills_its_row() {
        let wide = "山田太郎".repeat(5);
        let mut state = focused(vec![remote(&wide, Some("/srv/r"), &[&wide, "main"])]);
        state.branches.remotes.move_by(1);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(&state, frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let rows = buffer_rows(buffer);
        let y = rows
            .iter()
            .position(|r| r.contains("┃  山田"))
            .unwrap_or_else(|| panic!("{rows:#?}"));
        assert!(rows[y].contains('…'), "{rows:#?}");
        assert!(
            rows[y - 1].contains("┃山田") && rows[y - 1].contains('…'),
            "{rows:#?}"
        );
        assert_eq!(crate::view::width_of(&rows[y]), 80, "{rows:#?}");
        // The whole inside of the row is reversed, up to the border. The
        // cell a wide glyph spills into is drawn with the glyph, so it is
        // skipped.
        let y = y as u16;
        let mut x = 1;
        while buffer[(x, y)].symbol() != "┃" {
            let style = buffer[(x, y)].style();
            assert!(
                style.add_modifier.contains(Modifier::REVERSED),
                "column {x} of {:?}",
                rows[usize::from(y)]
            );
            x += crate::view::width_of(buffer[(x, y)].symbol()).max(1) as u16;
        }
    }

    #[test]
    fn the_remove_question_is_shown_whole_at_80_by_24() {
        use crate::op::GitOp;
        use crate::popup::{Popup, RemoveRemoteConfirm};
        let mut state = focused(vec![remote("origin", Some("/srv/r"), &["main"])]);
        state.popups.push(Popup::RemoveRemote(RemoveRemoteConfirm {
            name: "origin".into(),
            op: GitOp::RemoveRemote("origin".into()),
        }));
        let rows = draw(&state, 80, 24);
        let text: String = rows
            .iter()
            .map(|r| r.trim_matches(|c: char| c == '│' || c.is_whitespace()))
            .collect::<Vec<_>>()
            .join(" ");
        for part in [
            "Remove remote",
            "origin?",
            "remote-tracking",
            "lose their",
            "upstream. (y/n)",
        ] {
            assert!(text.contains(part), "{part:?} in {rows:#?}");
        }
    }

    #[test]
    fn a_long_url_loses_its_end_and_tiny_terminals_do_not_panic() {
        let long = format!("https://h/{}", "very-long-path/".repeat(10));
        let state = focused(vec![remote("origin", Some(&long), &["main"])]);
        let rows = draw(&state, 80, 24);
        let row = rows
            .iter()
            .find(|r| r.contains("origin  https://"))
            .unwrap_or_else(|| panic!("{rows:#?}"));
        assert!(row.contains('…'), "{row:?}");
        for (w, h) in [(1, 1), (2, 3), (10, 4), (20, 6), (0, 0)] {
            draw(&state, w, h);
        }
    }
}
