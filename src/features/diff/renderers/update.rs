//! The renderers' side of `AppState`: cycling the Diff pane's renderer,
//! running it on the shown diff, taking its rows back and keeping them in
//! step with the pane's width and the focused view's cursor.

use super::{ExternalRenderer, RenderedDiff, StyledRow, external_diff};
use crate::diff_pane::focused_hunks;
use crate::effect::{Effect, Tag};
use crate::git::diff::DiffRequest;
use crate::model::{AppState, Focus};

impl AppState {
    /// Take the renderers found. The active one is dropped, and its rows
    /// with it, when it is no longer among them.
    pub(crate) fn renderers_found(&mut self, renderers: Vec<ExternalRenderer>) {
        self.renderers = renderers;
        if self.renderer.is_some_and(|i| i >= self.renderers.len()) {
            self.renderer = None;
            self.rendered = None;
        }
    }

    /// Render again when a focus or mode change gave the diff another
    /// width than `cols`, so a renderer's columns match the pane.
    pub(crate) fn rerender_if_resized(
        &mut self,
        cols: u16,
        mut effects: Vec<Effect>,
    ) -> Vec<Effect> {
        let pending = self.rendered.as_ref().is_some_and(|r| r.seq.is_some());
        if self.pane_cols() != cols && self.active_renderer().is_some() && !pending {
            effects.extend(self.render_shown());
        }
        effects
    }

    /// The renderer the Diff pane uses, if not the built-in view.
    pub(crate) fn active_renderer(&self) -> Option<&ExternalRenderer> {
        self.renderer.and_then(|i| self.renderers.get(i))
    }

    /// The diff the Diff pane shows: the selected file's from the Files
    /// panel, the focused view's side with the Diff pane focused (renames
    /// off, as its patch has them), nothing behind a blame or a conflict.
    /// A commit's or a stash's file is never rendered, so it is not one of
    /// these.
    pub(crate) fn shown_request(&self) -> Option<DiffRequest> {
        match &self.focus {
            Focus::Commits if self.commits.open.is_some() => None,
            Focus::Stash if self.stash.open.is_some() => None,
            Focus::Status | Focus::Files | Focus::Branches | Focus::Commits | Focus::Stash => self
                .selected
                .and_then(|i| self.files.get(i))
                .map(|entry| DiffRequest::for_entry(entry, self.diff_options)),
            Focus::Diff => match self.hunk_view()? {
                // Like the commit's file in the Commits panel, never rendered.
                view if view.commit.is_some() => None,
                view => Some(DiffRequest {
                    path: view.path.clone(),
                    orig_path: None,
                    side: view.side,
                    options: self.diff_options,
                    commit: None,
                }),
            },
            Focus::Blame(_) | Focus::Conflict(_) => None,
        }
    }

    /// The rendered diff, when a renderer is active and it is of the
    /// shown file and side.
    pub(crate) fn rendered_for_shown(&self) -> Option<&RenderedDiff> {
        self.active_renderer()?;
        let request = self.shown_request()?;
        self.rendered
            .as_ref()
            .filter(|r| r.path == request.path && r.side == request.side)
    }

    /// The rendered diff when the Diff pane draws it. With the pane
    /// focused, only while the selection has rows: a cursor on rows the
    /// renderer did not draw would lie, so the built-in view is drawn. A
    /// tool's rows have no cursor to lie with, so they are always drawn.
    pub(crate) fn rendered_shown(&self) -> Option<&RenderedDiff> {
        let rendered = self.rendered_for_shown()?;
        if self.display_only() {
            return Some(rendered);
        }
        match self.hunk_view() {
            Some(view) if rendered.error.is_none() && !rendered.rendering() => {
                let picked = view.picked();
                rendered
                    .shows(view.hunk, picked.as_deref())
                    .then_some(rendered)
            }
            _ => Some(rendered),
        }
    }

    /// Step through built-in, each renderer, and back to built-in.
    pub(crate) fn cycle_renderer(&mut self) -> Vec<Effect> {
        if self.renderers.is_empty() {
            self.notice = Some(external_diff::NO_RENDERER.into());
            return vec![];
        }
        self.renderer = match self.renderer {
            None => Some(0),
            Some(i) if i + 1 < self.renderers.len() => Some(i + 1),
            Some(_) => None,
        };
        // Another renderer's rows must not show under this one's name.
        self.rendered = None;
        self.render_shown()
    }

    /// Run the active renderer on the shown diff. A user effect, so the
    /// busy indicator shows while it runs.
    pub(crate) fn render_shown(&mut self) -> Vec<Effect> {
        let Some(renderer) = self.active_renderer().cloned() else {
            return vec![];
        };
        let Some(request) = self.shown_request() else {
            return vec![];
        };
        let seq = self.take_seq();
        match &mut self.rendered {
            Some(r) if r.path == request.path && r.side == request.side => {
                r.seq = Some(seq);
                r.error = None;
            }
            _ => self.rendered = Some(RenderedDiff::new(seq, request.path.clone(), request.side)),
        }
        let tag = self.user_tag(seq);
        vec![Effect::RenderDiff {
            tag,
            request,
            renderer,
            cols: self.pane_cols(),
        }]
    }

    pub(crate) fn diff_rendered(&mut self, tag: Tag, result: Result<Vec<StyledRow>, String>) {
        // Every user effect reports back once, stale or not.
        self.user_done(tag);
        if tag.generation != self.generation {
            return;
        }
        if let Some(rendered) = &mut self.rendered
            && rendered.apply(tag.seq, result)
        {
            self.remap();
        }
    }

    /// Keep the rows of the selected hunk or lines inside the pane.
    pub(crate) fn follow_cursor(&mut self) {
        let rows = self.pane_rows();
        let (Some(view), Some(rendered)) = (
            focused_hunks(&self.focus, &self.diff.hunks),
            &mut self.rendered,
        ) else {
            return;
        };
        let picked = view.picked();
        let selection = rendered.mapped_rows(view.hunk, picked.as_deref());
        rendered.scroll_to(&selection, rows);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::FileEntry;
    use crate::git::diff::DiffSide;
    use crate::intent::{AppIntent, Scroll};
    use crate::keymap::{self, KeyPress};
    use crate::test_support::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn renderer(label: &str) -> ExternalRenderer {
        ExternalRenderer {
            label: label.into(),
            argv: vec![label.into()],
            kind: crate::features::diff::renderers::RendererKind::Pager,
        }
    }

    /// `files` loaded on an 80x24 screen, with renderers `labels` found.
    fn with_renderers(files: Vec<FileEntry>, labels: &[&str]) -> AppState {
        let (mut state, _) = with_files(files, 24);
        state.update(AppIntent::RenderersFound(
            labels.iter().map(|l| renderer(l)).collect(),
        ));
        state
    }

    /// The one `RenderDiff` among `effects`: its tag, request and cols.
    fn render_of(effects: &[Effect]) -> (Tag, DiffRequest, u16) {
        let renders: Vec<_> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::RenderDiff {
                    tag, request, cols, ..
                } => Some((*tag, request.clone(), *cols)),
                _ => None,
            })
            .collect();
        match renders.as_slice() {
            [render] => render.clone(),
            other => panic!("expected one RenderDiff in {effects:?}, got {other:?}"),
        }
    }

    fn rendered(tag: Tag, text: &str) -> AppIntent {
        AppIntent::DiffRendered {
            tag,
            result: Ok(crate::features::diff::renderers::ansi::parse(
                text.as_bytes(),
            )),
        }
    }

    #[test]
    fn cycling_renders_the_shown_diff_at_the_pane_width() {
        let mut state = with_renderers(three_files(), &["delta"]);
        let busy = state.busy_count();
        let (tag, request, cols) = render_of(&state.update(AppIntent::CycleRenderer));
        assert_eq!(state.renderer, Some(0));
        assert_eq!(cols, state.pane_cols());
        // 80 columns: the Diff pane is 53 wide, less borders and gutter.
        assert_eq!(cols, 50);
        assert!(tag.user);
        assert_eq!(state.busy_count(), busy + 1);
        assert_eq!(
            request,
            DiffRequest::for_entry(&state.files[0], state.diff_options)
        );
        assert!(state.rendered.as_ref().unwrap().rendering());
    }

    #[test]
    fn a_wider_screen_renders_again_and_a_taller_one_does_not() {
        let mut state = with_renderers(three_files(), &["delta"]);
        state.update(AppIntent::CycleRenderer);
        let effects = state.update(AppIntent::Resize {
            width: 120,
            height: 24,
        });
        let (_, _, cols) = render_of(&effects);
        assert_eq!(cols, state.pane_cols());
        assert_eq!(cols, 77);
        let effects = state.update(AppIntent::Resize {
            width: 120,
            height: 40,
        });
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::RenderDiff { .. }))
        );
    }

    #[test]
    fn zooming_renders_again_at_the_diffs_new_width() {
        let mut state = with_renderers(three_files(), &["delta"]);
        let (tag, _, _) = render_of(&state.update(AppIntent::CycleRenderer));
        state.update(rendered(tag, "row\n"));
        // Focus the diff, then zoom it: half, then full.
        // The diff's own view renders on focus; answer it.
        let (tag, _, _) = render_of(&press(&mut state, KeyCode::Char('l')));
        state.update(rendered(tag, "row\n"));
        let mut widths = vec![state.pane_cols()];
        for _ in 0..2 {
            let (tag, _, cols) = render_of(&press(&mut state, KeyCode::Char('z')));
            assert_eq!(cols, state.pane_cols());
            widths.push(cols);
            state.update(rendered(tag, "row\n"));
        }
        // 80 columns: 2/3, then 3/4, then all of it, less borders and gutter.
        assert_eq!(widths, [50, 57, 77]);
    }

    #[test]
    fn r_works_in_the_diff_reached_from_any_side_panel() {
        let mut state = with_renderers(three_files(), &["delta"]);
        press(&mut state, KeyCode::Char('1'));
        press(&mut state, KeyCode::Char('l'));
        assert!(matches!(state.focus, Focus::Diff));
        render_of(&state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('R'),
            KeyModifiers::SHIFT,
        ))));
        assert_eq!(state.renderer, Some(0));
    }

    #[test]
    fn r_steps_through_each_renderer_and_back_to_the_built_in_view() {
        let mut state = with_renderers(three_files(), &["delta", "riff"]);
        // R is a Diff pane key.
        press(&mut state, KeyCode::Enter);
        let shift_r = |state: &mut AppState| {
            state.update(AppIntent::Key(KeyPress::new(
                KeyCode::Char('R'),
                KeyModifiers::SHIFT,
            )))
        };
        let (_, _, _) = render_of(&shift_r(&mut state));
        assert_eq!(state.renderer, Some(0));
        let (_, _, _) = render_of(&shift_r(&mut state));
        assert_eq!(state.renderer, Some(1));
        assert_eq!(shift_r(&mut state), vec![]);
        assert_eq!(state.renderer, None);
        assert_eq!(state.rendered, None);
    }

    #[test]
    fn without_renderers_r_says_so_and_does_nothing() {
        let (mut state, _) = with_files(three_files(), 24);
        assert_eq!(state.update(AppIntent::CycleRenderer), vec![]);
        assert_eq!(state.notice, Some(external_diff::NO_RENDERER.into()));
        assert_eq!(state.renderer, None);
        state.notice = None;
        press(&mut state, KeyCode::Enter);
        assert_eq!(press(&mut state, KeyCode::Char('R')), vec![]);
        assert_eq!(state.notice, Some(external_diff::NO_RENDERER.into()));
    }

    #[test]
    fn a_stale_render_is_dropped_but_busy_still_falls() {
        let mut state = with_renderers(three_files(), &["delta"]);
        let (old, _, _) = render_of(&state.update(AppIntent::CycleRenderer));
        let (new, _, _) = render_of(&state.update(AppIntent::SelectNext));
        let busy = state.busy_count();
        state.update(rendered(old, "old\n"));
        assert_eq!(state.busy_count(), busy - 1);
        assert!(state.rendered.as_ref().unwrap().rows.is_empty());
        state.update(rendered(new, "new\n"));
        let shown = state.rendered_shown().expect("rendered rows shown");
        assert_eq!(shown.path, "b.rs".into());
        assert_eq!(shown.rows.len(), 1);
    }

    #[test]
    fn scrolling_moves_the_rendered_rows_while_they_are_shown() {
        let mut state = with_renderers(three_files(), &["delta"]);
        let (tag, _, _) = render_of(&state.update(AppIntent::CycleRenderer));
        state.update(rendered(tag, &"row\n".repeat(50)));
        state.update(AppIntent::ScrollDiff(Scroll::PageDown));
        assert_eq!(state.rendered.as_ref().unwrap().scroll, state.pane_rows());
        assert_eq!(state.diff.scroll, 0);
    }

    #[test]
    fn staging_options_and_side_changes_render_again() {
        let mut state = with_renderers(three_files(), &["delta"]);
        state.update(AppIntent::CycleRenderer);
        render_of(&press(&mut state, KeyCode::Char('w')));
        let (_, request, _) = render_of(&press(&mut state, KeyCode::Enter));
        assert_eq!(
            (request.side, request.orig_path),
            (DiffSide::Worktree, None)
        );
        let (_, request, _) = render_of(&press(&mut state, KeyCode::Char('t')));
        assert_eq!(request.side, DiffSide::Index);
        // Back to the Files panel on the entry's side again.
        let (_, request, _) = render_of(&press(&mut state, KeyCode::Esc));
        assert_eq!(request.side, DiffSide::Worktree);
        let (tag, op) = op_of(&press(&mut state, KeyCode::Char(' ')));
        let status = refresh_tag(&mut state);
        state.update(finished(tag, op, Ok(())));
        let effects = state.update(AppIntent::StatusLoaded {
            tag: Tag {
                seq: state.status_seq.unwrap(),
                ..status
            },
            result: Ok(repo(three_files())),
        });
        render_of(&effects);
    }

    /// `TWO_HUNKS` as a decorating renderer draws it: a file banner, a box
    /// per hunk, and old and new line numbers before each line.
    const DECORATED: &str = "───────\na.rs\n───────\n┌────┐\n│ @1 │\n└────┘\n\
        1 ⋮   │a\n  ⋮ 1 │b\n  ⋮ 2 │c\n2 ⋮ 3 │d\n┌────┐\n│ @9 │\n└────┘\n\
        9 ⋮   │e\n  ⋮10 │f\n";

    /// Focus the first of `three_files` with `TWO_HUNKS` loaded, through
    /// renderer `delta` when `render` is set.
    fn focused_through(render: bool) -> AppState {
        let mut state = with_renderers(three_files(), &["delta"]);
        if render {
            state.update(AppIntent::CycleRenderer);
        }
        let effects = press(&mut state, KeyCode::Enter);
        let (tag, _) = patch_load(&effects);
        state.update(AppIntent::PatchLoaded {
            tag,
            result: Ok(patch_of(TWO_HUNKS)),
        });
        if render {
            let (tag, _, _) = render_of(&effects);
            state.update(rendered(tag, DECORATED));
        }
        state
    }

    #[test]
    fn line_staging_through_a_renderer_stages_what_the_built_in_view_does() {
        let mut built_in = focused_through(false);
        let mut through = focused_through(true);
        for state in [&mut built_in, &mut through] {
            press(state, KeyCode::Char('v'));
            press(state, KeyCode::Char('j'));
        }
        let shown = through
            .rendered_shown()
            .expect("the rendered rows are shown");
        let picked = view(&through).selected_range();
        assert_eq!(shown.mapped_rows(0, Some(&picked)), [7], "the `b` row");
        let expected = op_of(&press(&mut built_in, KeyCode::Char(' '))).1;
        let (_, op) = op_of(&press(&mut through, KeyCode::Char(' ')));
        assert_eq!(op, expected);
    }

    fn tool(label: &str) -> ExternalRenderer {
        ExternalRenderer {
            kind: crate::features::diff::renderers::RendererKind::Tool,
            ..renderer(label)
        }
    }

    #[test]
    fn r_cycles_through_the_pagers_then_the_tool_then_built_in() {
        let mut state = with_renderers(three_files(), &[]);
        state.update(AppIntent::RenderersFound(vec![
            renderer("delta"),
            tool("difft"),
        ]));
        let mut seen = Vec::new();
        for _ in 0..3 {
            state.update(AppIntent::CycleRenderer);
            seen.push(state.active_renderer().map(|r| r.label.clone()));
        }
        assert_eq!(seen, [Some("delta".into()), Some("difft".into()), None]);

        state.update(AppIntent::RenderersFound(vec![tool("difft")]));
        let mut seen = Vec::new();
        for _ in 0..2 {
            state.update(AppIntent::CycleRenderer);
            seen.push((
                state.active_renderer().map(|r| r.label.clone()),
                state.display_only(),
            ));
        }
        assert_eq!(seen, [(Some("difft".into()), true), (None, false)]);
    }

    /// The first of `three_files` focused with `TWO_HUNKS` loaded, drawn
    /// by tool `difft` as rows that match no patch line.
    fn focused_through_tool() -> AppState {
        let mut state = with_renderers(three_files(), &[]);
        state.update(AppIntent::RenderersFound(vec![tool("difft")]));
        state.update(AppIntent::CycleRenderer);
        let effects = press(&mut state, KeyCode::Enter);
        let (tag, _) = patch_load(&effects);
        state.update(AppIntent::PatchLoaded {
            tag,
            result: Ok(patch_of(TWO_HUNKS)),
        });
        let (tag, _, _) = render_of(&effects);
        state.update(rendered(tag, "1 old one   1 new one\n2 same      2 same\n"));
        state
    }

    #[test]
    fn the_tool_view_shows_its_rows_unmapped() {
        let state = focused_through_tool();
        assert!(state.display_only());
        let shown = state.rendered_shown().expect("the tool rows are shown");
        assert_eq!(shown.rows.len(), 2);
        assert!(shown.map.is_empty());
    }

    #[test]
    fn in_the_tool_view_the_hunk_keys_say_why_and_keep_the_selection() {
        for code in [KeyCode::Char('j'), KeyCode::Down, KeyCode::Char('k')] {
            let mut state = focused_through_tool();
            assert_eq!(view(&state).hunk, 0);
            assert_eq!(press(&mut state, code), vec![], "{code:?}");
            assert_eq!(state.notice, Some(keymap::DISPLAY_ONLY.into()), "{code:?}");
            assert_eq!(view(&state).hunk, 0, "{code:?}");
        }
    }

    #[test]
    fn the_tool_view_refuses_staging_discard_and_hunk_edits() {
        for intent in [
            AppIntent::ToggleHunkStage,
            AppIntent::EnterLineMode,
            AppIntent::Discard,
            AppIntent::EditHunk,
        ] {
            let mut state = focused_through_tool();
            assert_eq!(state.update(intent.clone()), vec![], "{intent:?}");
            assert_eq!(
                state.notice,
                Some(keymap::DISPLAY_ONLY.into()),
                "{intent:?}"
            );
            assert_eq!(state.popups, [], "{intent:?}");
            assert_eq!(view(&state).lines, None, "{intent:?}");
        }
        // Line mode entered before the tool, then staged by intent.
        let mut state = focused_through(false);
        state.update(AppIntent::EnterLineMode);
        state.update(AppIntent::RenderersFound(vec![tool("difft")]));
        state.update(AppIntent::CycleRenderer);
        state.notice = None;
        assert_eq!(state.update(AppIntent::ToggleLinesStage), vec![]);
        assert_eq!(state.notice, Some(keymap::DISPLAY_ONLY.into()));
    }

    #[test]
    fn back_on_the_built_in_view_a_hunk_stages_again() {
        let mut state = focused_through_tool();
        assert_eq!(state.update(AppIntent::ToggleHunkStage), vec![]);
        state.update(AppIntent::CycleRenderer);
        assert!(!state.display_only());
        op_of(&state.update(AppIntent::ToggleHunkStage));
    }

    #[test]
    fn the_tool_view_leaves_whole_file_staging_alone() {
        let mut state = with_renderers(three_files(), &[]);
        state.update(AppIntent::RenderersFound(vec![tool("difft")]));
        state.update(AppIntent::CycleRenderer);
        assert!(state.display_only());
        assert_eq!(state.focus, Focus::Files);
        op_of(&press_char(&mut state, ' '));
    }

    #[test]
    fn hunk_moves_scroll_the_rendered_rows_to_the_hunk() {
        let (mut state, _) = with_files(three_files(), 6);
        state.update(AppIntent::RenderersFound(vec![renderer("delta")]));
        state.update(AppIntent::CycleRenderer);
        let effects = press(&mut state, KeyCode::Enter);
        let (tag, _) = patch_load(&effects);
        state.update(AppIntent::PatchLoaded {
            tag,
            result: Ok(patch_of(TWO_HUNKS)),
        });
        let (tag, _, _) = render_of(&effects);
        state.update(rendered(tag, DECORATED));
        // 3 rows: hunk 0's lines are rows 6 to 9, so its first is on top.
        assert_eq!(state.rendered.as_ref().unwrap().scroll, 6);
        // Hunk 1's are rows 13 and 14, so the last is at the bottom.
        press(&mut state, KeyCode::Char('j'));
        assert_eq!(state.rendered.as_ref().unwrap().scroll, 12);
    }
}
