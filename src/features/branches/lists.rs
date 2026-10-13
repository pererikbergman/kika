//! The Branches panel's filters and range deletes: narrowing the Local,
//! Remotes and Tags tabs as the user types, and building the question
//! asked before deleting the branches or tags, or removing the remotes, a
//! range covers. Core `range_delete.rs` asks it and runs the answer. Every
//! name an op carries comes from the loaded list, never from the filter.

use super::remotes::RemoteLine;
use crate::effect::Effect;
use crate::list_filter::InputTarget;
use crate::list_input::SELECTION_CLEARED;
use crate::model::AppState;
use crate::op::GitOp;
use crate::popup::{DeleteRangeConfirm, RangeKind};

impl AppState {
    /// Narrow the Branches panel's tab `target` filters. The log follows a
    /// selection the filter moved.
    pub(crate) fn apply_branches_filter(
        &mut self,
        target: InputTarget,
        text: String,
    ) -> Vec<Effect> {
        let panel = &mut self.branches;
        let (before, cleared, after) = match target {
            InputTarget::BranchesFilter => {
                let list = &mut panel.list;
                let before = list.selected;
                (before, list.set_filter(text), list.selected)
            }
            InputTarget::RemotesFilter => {
                let list = &mut panel.remotes.list;
                let before = list.selected;
                (before, list.set_filter(text), list.selected)
            }
            InputTarget::TagsFilter => {
                let list = &mut panel.tags;
                let before = list.selected;
                (before, list.set_filter(text), list.selected)
            }
            _ => return vec![],
        };
        if cleared {
            self.notice = Some(SELECTION_CLEARED.into());
        }
        if before == after {
            vec![]
        } else {
            self.load_branch_log(true)
        }
    }

    /// The range's branches `git branch -d` may take: never the current
    /// one, and never one whose commits are not merged, since only `y`
    /// answers and a range is never forced.
    pub(crate) fn branches_range_confirm(&self) -> DeleteRangeConfirm {
        let (mut unmerged, mut current) = (0, 0);
        let mut names = Vec::new();
        for i in self.branches.list.range() {
            let branch = &self.branches.list.rows[i];
            if branch.head {
                current += 1;
            } else if !branch.merged {
                unmerged += 1;
            } else {
                names.push(branch.name.clone());
            }
        }
        DeleteRangeConfirm {
            kind: RangeKind::Branches,
            names: names.iter().map(|n| n.display()).collect(),
            skipped: vec![
                ("not fully merged".into(), unmerged),
                ("current".into(), current),
            ],
            op: GitOp::DeleteBranches(names),
        }
    }

    /// Every tag in the range, each with the object it held when listed.
    pub(crate) fn tags_range_confirm(&self) -> DeleteRangeConfirm {
        let tags: Vec<_> = self
            .branches
            .tags
            .range()
            .into_iter()
            .map(|i| &self.branches.tags.rows[i])
            .collect();
        DeleteRangeConfirm {
            kind: RangeKind::Tags,
            names: tags.iter().map(|t| t.name.display()).collect(),
            skipped: vec![],
            op: GitOp::DeleteTags(
                tags.iter()
                    .map(|t| (t.name.clone(), t.ref_oid.clone()))
                    .collect(),
            ),
        }
    }

    /// The configured remotes whose header rows are in the range. Branch
    /// rows stay: deleting a remote's branch would need the network. A
    /// group no configured remote owns, or a name like an option, is
    /// never passed to `remote remove`.
    pub(crate) fn remotes_range_confirm(&self) -> DeleteRangeConfirm {
        let list = &self.branches.remotes.list;
        let (mut unconfigured, mut option_like) = (0, 0);
        let mut names = Vec::new();
        for i in list.range() {
            let RemoteLine::Remote(remote) = &list.rows[i] else {
                continue;
            };
            if !remote.configured {
                unconfigured += 1;
            } else if remote.name.0.starts_with(b"-") {
                option_like += 1;
            } else {
                names.push(remote.name.clone());
            }
        }
        DeleteRangeConfirm {
            kind: RangeKind::Remotes,
            names: names.iter().map(|n| n.display()).collect(),
            skipped: vec![
                ("not configured".into(), unconfigured),
                ("named like an option".into(), option_like),
            ],
            op: GitOp::RemoveRemotes(names),
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyModifiers};

    use super::*;
    use crate::confirm_question::{ConfirmQuestion, Then};
    use crate::features::branches::BranchesTab;
    use crate::git::branches::{Branch, Track};
    use crate::git::commit_files::CommitId;
    use crate::git::remotes::{Remote, RemoteBranch, RemoteName};
    use crate::git::tags::TagRef;
    use crate::intent::AppIntent;
    use crate::keymap::{FILTER_HIDES_ROW, KeyPress, KeyScope};
    use crate::layout::{Panel, SidePanel};
    use crate::list_filter::{MAX_INPUT, input_full_notice};
    use crate::model::Context;
    use crate::op::{op_command, op_steps};
    use crate::popup::Popup;
    use crate::test_support::{load_list, load_remotes};

    fn press(state: &mut AppState, code: KeyCode) -> Vec<Effect> {
        let mods = match code {
            KeyCode::Char(c) if c.is_ascii_uppercase() => KeyModifiers::SHIFT,
            _ => KeyModifiers::NONE,
        };
        state.update(AppIntent::Key(KeyPress::new(code, mods)))
    }

    fn key(state: &mut AppState, c: char) -> Vec<Effect> {
        press(state, KeyCode::Char(c))
    }

    /// Type `text`, `\n` as Enter.
    fn type_text(state: &mut AppState, text: &str) {
        for c in text.chars() {
            match c {
                '\n' => press(state, KeyCode::Enter),
                c => key(state, c),
            };
        }
    }

    fn branch(name: &str, head: bool, merged: bool) -> Branch {
        Branch {
            name: name.into(),
            head,
            upstream: None,
            track: Track::None,
            merged,
        }
    }

    fn tag(name: &str) -> TagRef {
        let oid = "a".repeat(40);
        TagRef {
            name: name.into(),
            target: CommitId::parse(&oid).unwrap(),
            target_short: oid[..7].into(),
            ref_oid: oid,
            annotated: false,
            date: "2026-10-01".into(),
            subject: "s".into(),
        }
    }

    fn remote(name: &str, configured: bool, branches: &[&str]) -> Remote {
        Remote {
            name: name.into(),
            url: configured.then(|| format!("/srv/{name}")),
            configured,
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

    /// A workspace with the Branches panel focused on `tab`, its lists
    /// loaded as given.
    fn on_tab(
        tab: BranchesTab,
        branches: Vec<Branch>,
        remotes: Vec<Remote>,
        tags: Vec<TagRef>,
    ) -> AppState {
        let mut state = AppState::new(Context::Workspace);
        load_list(&mut state.branches.list, Ok(branches));
        load_remotes(&mut state.branches.remotes.list, Ok(remotes));
        load_list(&mut state.branches.tags, Ok(tags));
        state.update(AppIntent::FocusPanel(Panel::Side(SidePanel::Branches)));
        state.branches.tab = tab;
        state
    }

    /// `main` (current), merged `a` and `b`, unmerged `wip`.
    fn local() -> AppState {
        on_tab(
            BranchesTab::Local,
            vec![
                branch("main", true, true),
                branch("a", false, true),
                branch("wip", false, false),
                branch("b", false, true),
            ],
            vec![],
            vec![],
        )
    }

    /// Configured `fork`, `origin` with `main`, and `stray`, which no
    /// configured remote owns.
    fn remotes() -> AppState {
        on_tab(
            BranchesTab::Remotes,
            vec![],
            vec![
                remote("fork", true, &[]),
                remote("origin", true, &["main"]),
                remote("stray", false, &["old"]),
            ],
            vec![],
        )
    }

    fn tags() -> AppState {
        on_tab(
            BranchesTab::Tags,
            vec![],
            vec![],
            vec![tag("v3"), tag("v2"), tag("x"), tag("v1")],
        )
    }

    /// The filter, visible rows and range of the tab shown.
    fn shown(state: &AppState) -> (String, Vec<usize>, Vec<usize>) {
        let panel = &state.branches;
        match panel.tab {
            BranchesTab::Local => {
                let list = &panel.list;
                (list.filter.clone(), list.visible(), list.range())
            }
            BranchesTab::Remotes => {
                let list = &panel.remotes.list;
                (list.filter.clone(), list.visible(), list.range())
            }
            BranchesTab::Tags => {
                let list = &panel.tags;
                (list.filter.clone(), list.visible(), list.range())
            }
        }
    }

    fn has_range(state: &AppState) -> bool {
        let panel = &state.branches;
        match panel.tab {
            BranchesTab::Local => panel.list.range_anchor.is_some(),
            BranchesTab::Remotes => panel.remotes.list.range_anchor.is_some(),
            BranchesTab::Tags => panel.tags.range_anchor.is_some(),
        }
    }

    /// The one op `effects` run.
    fn op_of(effects: &[Effect]) -> GitOp {
        let ops: Vec<&GitOp> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::RunOp { op, .. } => Some(op),
                _ => None,
            })
            .collect();
        match ops.as_slice() {
            [op] => (*op).clone(),
            other => panic!("expected one RunOp, got {other:?}"),
        }
    }

    /// Every argv `op` runs, step by step.
    fn argvs(op: &GitOp) -> Vec<Vec<String>> {
        op_steps(op)
            .iter()
            .map(|step| {
                op_command(step)
                    .args()
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect()
            })
            .collect()
    }

    fn range_popup(state: &AppState) -> &ConfirmQuestion {
        match state.popups.as_slice() {
            [Popup::Question(q)] if q.scope == KeyScope::DeleteRange => q,
            other => panic!("expected the range question, got {other:?}"),
        }
    }

    #[test]
    fn slash_filters_each_tab_enter_keeps_it_and_esc_clears_the_range_then_the_filter() {
        // Each tab, a query that narrows it, and the rows that shows.
        let cases: [(AppState, &str, &[usize]); 3] = [
            (local(), "a", &[0, 1]),
            (remotes(), "main", &[1, 2]),
            (tags(), "v", &[0, 1, 3]),
        ];
        for (mut state, query, expected) in cases {
            let tab = state.branches.tab;
            key(&mut state, '/');
            let input = state.input.as_ref().expect("the input opens");
            assert_eq!(input.target.prompt(), "/", "{tab:?}");
            assert_eq!(state.key_focus(), KeyScope::Input);
            type_text(&mut state, query);
            assert_eq!(shown(&state).1, expected, "{tab:?}");
            press(&mut state, KeyCode::Enter);
            assert_eq!(state.input, None);
            assert_eq!(shown(&state).0, query, "{tab:?}: Enter keeps it");

            key(&mut state, 'V');
            assert!(has_range(&state), "{tab:?}");
            press(&mut state, KeyCode::Esc);
            assert!(!has_range(&state), "{tab:?}: the range goes first");
            assert_eq!(shown(&state).0, query, "{tab:?}");
            press(&mut state, KeyCode::Esc);
            assert_eq!(shown(&state).0, "", "{tab:?}: then the filter");
            assert!(!state.should_quit);
            press(&mut state, KeyCode::Esc);
            assert!(state.should_quit, "{tab:?}: then Esc quits, as before");
        }
    }

    #[test]
    fn the_filter_stops_at_its_limit_in_each_tab() {
        for mut state in [local(), remotes(), tags()] {
            key(&mut state, '/');
            for _ in 0..MAX_INPUT + 5 {
                key(&mut state, 'z');
            }
            assert_eq!(shown(&state).0.chars().count(), MAX_INPUT);
            assert_eq!(state.notice, Some(input_full_notice().into()));
        }
    }

    #[test]
    fn v_starts_and_clears_a_range_and_a_changed_filter_clears_it() {
        for mut state in [local(), remotes(), tags()] {
            let tab = state.branches.tab;
            key(&mut state, 'V');
            key(&mut state, 'j');
            assert_eq!(shown(&state).2, [0, 1], "{tab:?}");
            key(&mut state, 'V');
            assert!(!has_range(&state), "{tab:?}");
            key(&mut state, 'V');
            key(&mut state, '/');
            key(&mut state, 'i');
            assert!(!has_range(&state), "{tab:?}");
            assert_eq!(state.notice, Some(SELECTION_CLEARED.into()), "{tab:?}");
        }
    }

    #[test]
    fn switching_tabs_clears_the_range_left_behind_and_keeps_each_filter() {
        let mut state = local();
        load_remotes(
            &mut state.branches.remotes.list,
            Ok(vec![remote("o", true, &[])]),
        );
        type_text(&mut state, "/a\n");
        key(&mut state, 'V');
        key(&mut state, ']');
        assert_eq!(state.branches.tab, BranchesTab::Remotes);
        assert_eq!(state.branches.list.range_anchor, None);
        assert_eq!(state.branches.list.filter, "a");
        type_text(&mut state, "/o\nV");
        key(&mut state, '[');
        assert_eq!(state.branches.remotes.list.range_anchor, None);
        assert_eq!(state.branches.remotes.list.filter, "o");
        assert_eq!(shown(&state).0, "a", "Local's filter is still there");
    }

    #[test]
    fn d_over_a_local_range_asks_and_only_y_deletes_the_merged_branches() {
        let mut state = local();
        key(&mut state, 'V');
        for _ in 0..3 {
            key(&mut state, 'j');
        }
        assert_eq!(key(&mut state, 'd'), vec![]);
        assert_eq!(state.key_focus(), KeyScope::DeleteRange);
        let confirm = range_popup(&state);
        assert_eq!(
            confirm.text,
            "Delete 2 branches (a, b)? Kept: 1 not fully merged, 1 current. (y/n)"
        );
        assert_eq!(
            confirm.then,
            Then::Op(GitOp::DeleteBranches(vec!["a".into(), "b".into()]))
        );

        for code in [KeyCode::Enter, KeyCode::Char('x'), KeyCode::Char('D')] {
            assert_eq!(press(&mut state, code), vec![], "{code:?}");
            assert_eq!(state.popups.len(), 1, "{code:?} leaves it open");
        }
        key(&mut state, 'q');
        assert!(!state.should_quit, "q is not quit here");

        let op = op_of(&key(&mut state, 'y'));
        assert_eq!(argvs(&op), [["branch", "-d", "--", "a", "b"]]);
        assert!(!argvs(&op)[0].contains(&"-D".to_string()));
        assert_eq!(state.popups, []);
        assert_eq!(state.branches.list.range_anchor, None, "the range is done");

        for code in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut state = local();
            type_text(&mut state, "Vjd");
            assert_eq!(press(&mut state, code), vec![], "{code:?}");
            assert_eq!(state.popups, [], "{code:?}");
            assert!(
                state.branches.list.range_anchor.is_some(),
                "{code:?} keeps it"
            );
            assert_eq!(state.update(AppIntent::ConfirmDeleteRange), vec![]);
        }
    }

    #[test]
    fn d_over_a_tags_range_deletes_each_tag_with_its_listed_object() {
        let mut state = tags();
        type_text(&mut state, "/v\nVjj");
        key(&mut state, 'd');
        let confirm = range_popup(&state);
        assert_eq!(
            confirm.text,
            "Delete 3 tags (v3, v2, v1)? They are not deleted on any remote. (y/n)"
        );
        press(&mut state, KeyCode::Enter);
        assert_eq!(state.popups.len(), 1);
        let op = op_of(&key(&mut state, 'y'));
        let oid = "a".repeat(40);
        let delete = |name: &str| {
            vec![
                "update-ref".to_string(),
                "--no-deref".into(),
                "-d".into(),
                format!("refs/tags/{name}"),
                oid.clone(),
            ]
        };
        assert_eq!(argvs(&op), [delete("v3"), delete("v2"), delete("v1")]);
    }

    #[test]
    fn d_over_a_remotes_range_removes_only_the_configured_headers_in_it() {
        let mut state = remotes();
        // fork, origin, origin/main, stray, stray/old.
        type_text(&mut state, "Vjjjj");
        key(&mut state, 'd');
        let confirm = range_popup(&state);
        assert_eq!(
            confirm.text,
            "Remove 2 remotes (fork, origin)? Their remote-tracking branches are deleted, \
             and branches tracking them lose their upstream. Kept: 1 not configured. (y/n)"
        );
        press(&mut state, KeyCode::Enter);
        assert_eq!(state.popups.len(), 1);
        let op = op_of(&key(&mut state, 'y'));
        assert_eq!(
            argvs(&op),
            [["remote", "remove", "fork"], ["remote", "remove", "origin"]]
        );
    }

    #[test]
    fn a_range_with_nothing_to_delete_says_so_and_asks_nothing() {
        let mut state = local();
        key(&mut state, 'V');
        key(&mut state, 'd');
        assert_eq!(
            state.notice,
            Some("No branch in the selection can be deleted with y".into())
        );
        assert_eq!(state.popups, []);

        let mut state = remotes();
        type_text(&mut state, "jjjVj");
        assert_eq!(key(&mut state, 'd'), vec![]);
        assert_eq!(
            state.notice,
            Some("No remote in the selection can be removed".into())
        );
        assert_eq!(state.popups, []);

        let mut state = tags();
        load_list(&mut state.branches.tags, Ok(vec![]));
        state.branches.tags.range_anchor = Some("gone".into());
        state.update(AppIntent::DeleteTag);
        assert_eq!(state.notice, Some("No tag in the selection".into()));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn filter_text_never_reaches_git() {
        const HOSTILE: &str = "--force -D x\x1b[31m";
        // A filter no row matches: no range, no op.
        for mut state in [local(), remotes(), tags()] {
            key(&mut state, '/');
            type_text(&mut state, HOSTILE);
            press(&mut state, KeyCode::Enter);
            key(&mut state, 'V');
            assert!(!has_range(&state));
            assert_eq!(key(&mut state, 'd'), vec![]);
            assert_eq!(state.notice, Some(FILTER_HIDES_ROW.into()));
            assert_eq!(state.popups, []);
        }
        // A filter that matches names like it: the argv holds the loaded
        // names, never the filter or one of its terms.
        let query = "--force -D x";
        let names = ["x--force-Dx", "y--force-Dx"];
        let cases = [
            on_tab(
                BranchesTab::Local,
                names.iter().map(|n| branch(n, false, true)).collect(),
                vec![],
                vec![],
            ),
            on_tab(
                BranchesTab::Remotes,
                vec![],
                names.iter().map(|n| remote(n, true, &[])).collect(),
                vec![],
            ),
            on_tab(
                BranchesTab::Tags,
                vec![],
                vec![],
                names.iter().map(|n| tag(n)).collect(),
            ),
        ];
        for mut state in cases {
            let tab = state.branches.tab;
            key(&mut state, '/');
            type_text(&mut state, query);
            press(&mut state, KeyCode::Enter);
            assert_eq!(shown(&state).1, [0, 1], "{tab:?}");
            type_text(&mut state, "Vjd");
            let op = op_of(&key(&mut state, 'y'));
            for argv in argvs(&op) {
                for arg in &argv {
                    let filtered = arg == query || query.split_whitespace().any(|t| arg == t);
                    assert!(!filtered, "{tab:?}: {argv:?}");
                }
                let named = argv
                    .iter()
                    .filter(|a| names.iter().any(|n| a.ends_with(n)))
                    .count();
                assert!(named >= 1, "{tab:?}: {argv:?}");
            }
        }
    }
}
