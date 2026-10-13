//! Copy and paste's `update` handlers: copying the selected commit or a
//! range into the copied set, and pasting the set as a cherry-pick.

use super::CopyIntent;
use super::copied::CopiedCommit;
use super::keys;
use crate::effect::Effect;
use crate::git::commit_files::CommitId;
use crate::model::{AppState, Context};
use crate::op::GitOp;

/// Copying commits' half of `update`, which `commits::update` passes
/// `CommitsIntent::Copy` to. A free function, so its name cannot clash
/// with handlers.
pub(crate) fn update(state: &mut AppState, intent: CopyIntent) -> Vec<Effect> {
    match intent {
        CopyIntent::Toggle if state.commits.list.range_anchor.is_some() => state.copy_range(),
        CopyIntent::Toggle => state.copy_commit(),
        CopyIntent::Clear => {
            state.copied.clear();
            state.notice = Some("Cleared copied commits".into());
        }
        CopyIntent::Paste => return state.paste_commits(),
        CopyIntent::CopyText(what) => return state.copy_commit_text(what),
        CopyIntent::MessageLoaded { tag, oid, result } => {
            return state.commit_message_loaded(tag, oid, result);
        }
    }
    Vec::new()
}

/// Copying commits and pasting them onto the current branch.
impl AppState {
    /// Add the selected commit to the copied set, or take it out. Pure, so
    /// it works while an operation runs.
    pub(crate) fn copy_commit(&mut self) {
        let Some((id, _)) = self.commit_for_action(keys::copy_disabled) else {
            return;
        };
        let row = self.commits.list.rows.get(self.commits.list.selected);
        let subject = row.map(|r| r.subject.clone()).unwrap_or_default();
        let entry = CopiedCommit::new(id, subject);
        let short = entry.short.clone();
        let added = self.copied.toggle(entry);
        let verb = if added { "Copied" } else { "Removed" };
        let n = self.copied.len();
        self.notice_text = Some(format!("{verb} {short} ({n} copied)"));
    }

    /// Cherry-pick the copied commits on the terminal, so hooks and
    /// signing work as on the command line. The hand-off puts them parents
    /// first. The set stays, to paste onto another branch too.
    pub(crate) fn paste_commits(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if let Some(reason) = keys::paste_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        self.run_op_where_it_belongs(GitOp::CherryPick(self.copied.ids()))
    }

    /// Copy the range's commits, in screen order, or take them all out
    /// when every one is copied already. Merge commits are skipped.
    pub(crate) fn copy_range(&mut self) {
        if let Some(reason) = keys::commits_copy_disabled(self) {
            self.notice = Some(reason);
            return;
        }
        let mut merges = 0;
        let mut picks = Vec::new();
        for i in self.commits.list.range() {
            let row = &self.commits.list.rows[i];
            if row.parents.len() > 1 {
                merges += 1;
            } else if let Some(id) = CommitId::parse(&row.oid) {
                picks.push(CopiedCommit::new(id, row.subject.clone()));
            }
        }
        let remove = picks.iter().all(|pick| self.copied.contains(&pick.id));
        let count = picks.len();
        for pick in picks {
            if self.copied.contains(&pick.id) == remove {
                self.copied.toggle(pick);
            }
        }
        self.commits.list.range_anchor = None;
        let verb = if remove { "Removed" } else { "Copied" };
        let skipped = match merges {
            0 => String::new(),
            1 => "1 merge skipped, ".to_string(),
            n => format!("{n} merges skipped, "),
        };
        let total = self.copied.len();
        self.notice_text = Some(format!("{verb} {count} ({skipped}{total} copied)"));
    }
}

#[cfg(test)]
mod tests {
    use crate::features::commits::CommitsIntent;
    use crate::features::commits::copy::CopyIntent;
    use crate::git::repo_state::Operation;
    use crate::git::status::Head;
    use crate::intent::AppIntent;
    use crate::keymap;
    use crate::model::AppState;
    use crate::op::GitOp;
    use crate::popup::Popup;
    use crate::test_support::{
        COPIED_OID, attached_of, changed, cid, copy_ready, finished, one_copied, op_of, press_char,
        untracked, with_merge,
    };

    #[test]
    fn c_copies_the_rows_full_id_and_c_again_takes_it_out() {
        let mut state = copy_ready();
        assert_eq!(press_char(&mut state, 'c'), vec![]);
        assert_eq!(state.copied.ids(), [cid(COPIED_OID)]);
        assert!(!state.copied.contains(&cid("1111111")));
        assert_eq!(
            state.notice_text.as_deref(),
            Some("Copied 1111111 (1 copied)")
        );
        assert_eq!(press_char(&mut state, 'c'), vec![]);
        assert!(state.copied.is_empty());
        assert_eq!(
            state.notice_text.as_deref(),
            Some("Removed 1111111 (0 copied)")
        );
        // The next key clears the notice.
        press_char(&mut state, 'j');
        assert_eq!(state.notice_text, None);
    }

    #[test]
    fn copying_works_while_an_op_runs_but_never_copies_a_merge() {
        let mut state = copy_ready();
        state.hold_op(4);
        press_char(&mut state, 'c');
        assert_eq!(state.copied.len(), 1);

        let mut merge = one_copied();
        merge.commits.list.selected = 0;
        merge.commits.list.rows[0].parents = vec!["c1".into(), "d0".into()];
        assert_eq!(press_char(&mut merge, 'c'), vec![]);
        assert_eq!(
            merge.notice,
            Some("Cannot cherry-pick a merge commit".into())
        );
        assert_eq!(
            merge.update(AppIntent::Commits(CommitsIntent::Copy(CopyIntent::Toggle))),
            vec![]
        );
        assert_eq!(merge.copied.ids(), [cid(COPIED_OID)]);
    }

    #[test]
    fn shift_c_clears_the_set() {
        let mut state = one_copied();
        assert_eq!(press_char(&mut state, 'C'), vec![]);
        assert!(state.copied.is_empty());
        assert_eq!(state.notice, Some("Cleared copied commits".into()));
    }

    #[test]
    fn v_cherry_picks_the_copied_ids_on_the_terminal() {
        let mut state = one_copied();
        state.commits.list.selected = 2;
        state.commits.list.rows[2].oid = "2".repeat(40);
        press_char(&mut state, 'c');
        let (tag, op) = attached_of(&press_char(&mut state, 'v'));
        assert_eq!(
            op,
            GitOp::CherryPick(vec![cid(COPIED_OID), cid(&"2".repeat(40))])
        );
        assert_eq!(state.held_seq(), Some(tag.seq));
        // The set stays, to paste on another branch too.
        assert_eq!(state.copied.len(), 2);
    }

    #[test]
    fn v_says_why_it_cannot_paste_and_starts_nothing() {
        let busy = "Finish or abort the operation in progress";
        let changes = "Commit or stash your changes first";
        type Setup = fn(&mut AppState);
        let cases: [(&str, Setup, &str); 6] = [
            ("empty set", |s| s.copied.clear(), "Nothing copied"),
            (
                "unborn HEAD",
                |s| s.branch.as_mut().unwrap().head = Head::Unborn("main".into()),
                "No commit to cherry-pick onto",
            ),
            (
                "merge in progress",
                |s| s.branch.as_mut().unwrap().in_progress = vec![Operation::Merge],
                busy,
            ),
            (
                "staged change",
                |s| s.files.rows = vec![changed("a.rs", 'M', '.')],
                changes,
            ),
            (
                "unstaged change",
                |s| s.files.rows = vec![changed("a.rs", '.', 'M')],
                changes,
            ),
            ("op running", |s| s.hold_op(9), keymap::OP_RUNNING),
        ];
        for (case, setup, reason) in cases {
            let mut state = one_copied();
            setup(&mut state);
            assert_eq!(press_char(&mut state, 'v'), vec![], "{case}");
            assert_eq!(state.notice, Some(reason.into()), "{case}");
            assert_eq!(
                state.update(AppIntent::Commits(CommitsIntent::Copy(CopyIntent::Paste))),
                vec![],
                "{case}"
            );
        }
    }

    #[test]
    fn an_untracked_file_alone_does_not_stop_a_paste() {
        let mut state = one_copied();
        state.files.rows = vec![untracked("new.txt")];
        let (_, op) = attached_of(&press_char(&mut state, 'v'));
        assert_eq!(op, GitOp::CherryPick(vec![cid(COPIED_OID)]));
    }

    #[test]
    fn the_set_survives_a_checkout_and_goes_with_the_repository() {
        let mut state = one_copied();
        let (tag, op) = op_of(&state.start_op(Ok(Some(GitOp::Checkout("topic".into())))));
        state.update(finished(tag, op, Ok(())));
        assert_eq!(state.copied.ids(), [cid(COPIED_OID)]);
        state.switch_repo();
        assert!(state.copied.is_empty());
    }

    #[test]
    fn a_failed_paste_says_could_not_cherry_pick_and_how_to_go_on() {
        let mut state = one_copied();
        let (tag, op) = attached_of(&press_char(&mut state, 'v'));
        state.update(finished(tag, op, Err("CONFLICT in a.txt".into())));
        match state.popups.as_slice() {
            [Popup::Error(error)] => {
                assert_eq!(error.title, "Could not cherry-pick");
                assert_eq!(
                    error.next,
                    "If git stopped on conflicts or an empty pick, resolve what it says, then continue, skip or abort from the Status panel."
                );
            }
            other => panic!("expected one error, got {other:?}"),
        }
    }

    #[test]
    fn a_range_of_only_merges_cannot_be_copied() {
        let mut state = with_merge();
        press_char(&mut state, 'j');
        press_char(&mut state, 'V');
        press_char(&mut state, 'c');
        assert_eq!(
            state.notice,
            Some("Cannot cherry-pick a merge commit".into())
        );
        assert!(state.copied.is_empty());
    }
}
