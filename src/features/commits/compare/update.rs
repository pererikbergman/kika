//! Compare's `update` handlers: `M` marks a log commit or local branch,
//! and Enter on another one asks for both ends as commits, then opens
//! what changed between them in the commit-files view.

use super::PendingDiff;
use crate::commit_files::CommitFilesList;
use crate::commits::CommitsTab;
use crate::diff_mark::{DiffEnd, DiffMark};
use crate::effect::{Effect, Tag};
use crate::git::commit_files::{CommitId, CommitRange};
use crate::keymap::KeyScope;
use crate::layout::{Panel, SidePanel};
use crate::model::AppState;

/// What Esc or a second `M` says once the mark is gone.
pub(crate) const MARK_CLEARED: &str = "Diff mark cleared";

/// Marking a commit or branch and comparing another one with it.
impl AppState {
    /// `M`: mark the selected log commit or local branch, or clear the
    /// mark when it is already on it.
    pub(crate) fn toggle_diff_mark(&mut self) {
        let mark = match self.panel_focus() {
            KeyScope::Commits => {
                let Some(row) = self.commits.rows.get(self.commits.selected) else {
                    return;
                };
                let Some(id) = CommitId::parse(&row.oid) else {
                    return;
                };
                DiffMark::Commit {
                    id,
                    short: row.short_oid.clone(),
                }
            }
            KeyScope::Branches => match self.branches.selected_branch() {
                Some(branch) => DiffMark::Branch(branch.name.clone()),
                None => return,
            },
            _ => return,
        };
        if self.diff_mark.as_ref() == Some(&mark) {
            self.diff_mark = None;
            self.notice = Some(MARK_CLEARED.into());
        } else {
            self.notice_text = Some(format!("Marked {} for diffing", mark.label()));
            self.diff_mark = Some(mark);
        }
    }

    /// Enter in Branches: compare the mark with the selected branch.
    pub(crate) fn open_branch_diff(&mut self) -> Vec<Effect> {
        if self.panel_focus() != KeyScope::Branches {
            return vec![];
        }
        let Some(name) = self.branches.selected_branch().map(|b| b.name.clone()) else {
            return vec![];
        };
        if matches!(&self.diff_mark, Some(DiffMark::Branch(marked)) if *marked == name) {
            return vec![];
        }
        let label = name.display();
        self.open_comparison(DiffEnd::Branch(name), label)
    }

    /// Read the mark and `target` as commits, to list what changed from
    /// the mark to `target`. Nothing without a mark.
    pub(crate) fn open_comparison(&mut self, target: DiffEnd, label: String) -> Vec<Effect> {
        let Some((base, against)) = self.diff_mark.as_ref().map(|m| (m.end(), m.label())) else {
            return vec![];
        };
        let seq = self.take_seq();
        self.pending_diff = Some(PendingDiff {
            seq,
            against,
            target: label,
        });
        let tag = self.user_tag(seq);
        vec![Effect::ResolveDiff { tag, base, target }]
    }

    /// Both ends are commits now: open their files in the Commits panel,
    /// in place of the log, as a commit's files open.
    pub(crate) fn diff_resolved(
        &mut self,
        tag: Tag,
        result: Result<CommitRange, String>,
    ) -> Vec<Effect> {
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        let current = self.pending_diff.as_ref().map(|p| p.seq);
        if tag.generation != self.generation || current != Some(tag.seq) {
            return vec![];
        }
        let Some(pending) = self.pending_diff.take() else {
            return vec![];
        };
        let range = match result {
            Ok(range) => range,
            Err(message) => {
                let first = message.lines().next().unwrap_or_default();
                self.notice_text = Some(format!("Could not compare: {first}"));
                return vec![];
            }
        };
        let mut effects = self.close_commit_files();
        effects.extend(self.focus_panel(Panel::Side(SidePanel::Commits)));
        self.commits.tab = CommitsTab::Log;
        let seq = self.take_seq();
        let mut open = CommitFilesList::begin(range.clone(), pending.target, seq);
        open.against = Some(pending.against);
        self.commits.open = Some(open);
        let tag = self.user_tag(seq);
        effects.push(Effect::LoadCommitFiles { tag, range });
        effects
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use super::*;
    use crate::git::branches::{Branch, BranchName, Track};
    use crate::intent::AppIntent;
    use crate::model::Focus;
    use crate::test_support::{oid, press, type_text, with_log};

    /// The one `ResolveDiff` in `effects`.
    fn resolve(effects: &[Effect]) -> (Tag, DiffEnd, DiffEnd) {
        match effects {
            [Effect::ResolveDiff { tag, base, target }] => (*tag, base.clone(), target.clone()),
            other => panic!("expected one ResolveDiff, got {other:?}"),
        }
    }

    fn id(i: usize) -> CommitId {
        CommitId::parse(&oid(i)).unwrap()
    }

    #[test]
    fn enter_on_another_commit_compares_it_with_the_marked_one() {
        let mut state = with_log(4);
        type_text(&mut state, "jjM");
        assert_eq!(
            state.notice_text.as_deref(),
            Some(format!("Marked {} for diffing", &oid(2)[33..]).as_str())
        );
        type_text(&mut state, "j");

        let (tag, base, target) = resolve(&press(&mut state, KeyCode::Enter));
        assert_eq!(
            (base, target),
            (DiffEnd::Commit(id(2)), DiffEnd::Commit(id(3)))
        );
        assert!(tag.user);

        let range = CommitRange {
            oid: id(3),
            parent: Some(id(2)),
        };
        let effects = state.update(AppIntent::DiffResolved {
            tag,
            result: Ok(range.clone()),
        });

        let open = state.commits.open.as_ref().expect("the comparison opens");
        assert_eq!(open.range.parent, Some(id(2)));
        assert_eq!(open.against.as_deref(), Some(&oid(2)[33..]));
        assert_eq!(open.short_oid, &oid(3)[33..]);
        assert_eq!(state.focus, Focus::Commits);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::LoadCommitFiles { range: r, .. } if *r == range))
        );
        // Answered once: a repeat is stale.
        assert_eq!(
            state.update(AppIntent::DiffResolved {
                tag,
                result: Ok(range),
            }),
            vec![]
        );
    }

    #[test]
    fn enter_on_the_marked_commit_opens_it_against_its_parent_as_before() {
        let mut state = with_log(4);
        type_text(&mut state, "jM");

        let effects = press(&mut state, KeyCode::Enter);

        match effects.as_slice() {
            [Effect::LoadCommitFiles { range, .. }] => {
                assert_eq!((&range.oid, &range.parent), (&id(1), &Some(id(2))));
            }
            other => panic!("expected the commit's files, got {other:?}"),
        }
        assert_eq!(state.commits.open.as_ref().unwrap().against, None);
    }

    #[test]
    fn m_on_the_marked_commit_clears_the_mark() {
        let mut state = with_log(2);
        type_text(&mut state, "M");
        assert!(state.diff_mark.is_some());
        type_text(&mut state, "M");
        assert_eq!(state.diff_mark, None);
        assert_eq!(state.notice.as_deref(), Some(MARK_CLEARED));
    }

    fn branch(name: &str) -> Branch {
        Branch {
            name: name.into(),
            head: false,
            upstream: None,
            track: Track::None,
            merged: true,
        }
    }

    fn with_branches() -> AppState {
        let mut state = with_log(2);
        state
            .branches
            .loaded(Ok(vec![branch("main"), branch("topic")]));
        state.focus = Focus::Branches;
        state.expanded = SidePanel::Branches;
        state
    }

    #[test]
    fn enter_on_another_branch_compares_the_two_branches() {
        let mut state = with_branches();
        type_text(&mut state, "Mj");

        let (tag, base, target) = resolve(&press(&mut state, KeyCode::Enter));

        assert_eq!(base, DiffEnd::Branch(BranchName::from_bytes(b"main")));
        assert_eq!(target, DiffEnd::Branch(BranchName::from_bytes(b"topic")));
        state.update(AppIntent::DiffResolved {
            tag,
            result: Ok(CommitRange {
                oid: id(1),
                parent: Some(id(0)),
            }),
        });
        let open = state.commits.open.as_ref().unwrap();
        assert_eq!(
            (open.short_oid.as_str(), open.against.as_deref()),
            ("topic", Some("main"))
        );
        assert_eq!(state.focus, Focus::Commits);
        assert_eq!(state.panel_focus(), KeyScope::CommitFiles);
    }

    #[test]
    fn a_comparison_opens_with_no_filter_whatever_the_last_list_had() {
        let mut state = with_branches();
        let range = CommitRange {
            oid: id(1),
            parent: Some(id(0)),
        };
        let mut before = CommitFilesList::begin(range.clone(), "old".into(), 1);
        before.filter = "lib".into();
        state.commits.open = Some(before);
        type_text(&mut state, "Mj");

        let (tag, ..) = resolve(&press(&mut state, KeyCode::Enter));
        state.update(AppIntent::DiffResolved {
            tag,
            result: Ok(range),
        });

        let open = state.commits.open.as_ref().unwrap();
        assert_eq!(open.against.as_deref(), Some("main"));
        assert_eq!(open.filter, "");
    }

    #[test]
    fn enter_in_branches_needs_a_mark_on_another_branch() {
        let mut state = with_branches();
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(
            state.notice.as_deref(),
            Some("Mark a commit or branch first (M)")
        );

        type_text(&mut state, "M");
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(
            state.notice.as_deref(),
            Some("Pick another branch or commit to diff against")
        );
    }

    #[test]
    fn a_failed_resolve_says_why_and_opens_nothing() {
        let mut state = with_branches();
        type_text(&mut state, "Mj");
        let busy = state.busy;
        let (tag, ..) = resolve(&press(&mut state, KeyCode::Enter));
        assert_eq!(state.busy, busy + 1);

        state.update(AppIntent::DiffResolved {
            tag,
            result: Err("No branch named topic".into()),
        });

        assert_eq!(state.commits.open, None);
        assert_eq!(
            state.notice_text.as_deref(),
            Some("Could not compare: No branch named topic")
        );
        assert_eq!(state.busy, busy);
    }
}
