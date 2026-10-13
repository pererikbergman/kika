//! What the stage keys do to the Files panel's entries: the `GitOp` that
//! stages or unstages one file, a range, or everything. Their argv stays in
//! core `op_command`, with `--` before the paths and literal pathspecs.

use crate::git::status::{EntryKind, FileEntry};
use crate::op::GitOp;

/// Why staging everything is refused while conflicts exist.
pub(crate) const CONFLICTS_REFUSAL: &str = "Resolve conflicts before staging everything";

/// What one key press does to `entry`: stage it while it has unstaged
/// changes, else unstage it. Conflicts are never touched, since staging
/// one would mark it resolved.
pub(crate) fn toggle_for(entry: &FileEntry) -> Option<GitOp> {
    if entry.kind == EntryKind::Conflicted {
        return None;
    }
    if has_unstaged(entry) {
        return Some(GitOp::Stage(vec![entry.path.clone()]));
    }
    // A staged rename also deletes its old path, which must be unstaged too.
    let paths = entry
        .orig_path
        .iter()
        .chain([&entry.path])
        .cloned()
        .collect();
    Some(GitOp::Unstage(paths))
}

/// What the stage-all key does: stage everything while anything is
/// unstaged, else unstage everything. Refused while conflicts exist,
/// because `add -A` would mark them resolved and `reset` would drop their
/// conflict stages.
pub(crate) fn toggle_all(files: &[FileEntry]) -> Result<Option<GitOp>, String> {
    if files.iter().any(|f| f.kind == EntryKind::Conflicted) {
        return Err(CONFLICTS_REFUSAL.into());
    }
    if files.is_empty() {
        return Ok(None);
    }
    Ok(Some(if files.iter().any(has_unstaged) {
        GitOp::StageAll
    } else {
        GitOp::UnstageAll
    }))
}

/// What one key press does to a range of entries: stage them all while
/// any has unstaged changes, else unstage them all, with each staged
/// rename's old path. Conflicts are skipped, as one at a time.
pub(crate) fn toggle_many(entries: &[&FileEntry]) -> Option<GitOp> {
    let entries: Vec<&FileEntry> = entries
        .iter()
        .copied()
        .filter(|e| e.kind != EntryKind::Conflicted)
        .collect();
    if entries.is_empty() {
        return None;
    }
    if entries.iter().any(|e| has_unstaged(e)) {
        return Some(GitOp::Stage(
            entries.iter().map(|e| e.path.clone()).collect(),
        ));
    }
    let paths = entries
        .iter()
        .flat_map(|e| e.orig_path.iter().chain([&e.path]))
        .cloned()
        .collect();
    Some(GitOp::Unstage(paths))
}

/// Untracked, or changed in the worktree (including partly staged files).
fn has_unstaged(entry: &FileEntry) -> bool {
    entry.kind == EntryKind::Untracked || entry.worktree != '.'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op::run_op;
    use crate::test_support::{file_entry as entry, paths};

    fn stage(paths: &[&str]) -> Option<GitOp> {
        Some(GitOp::Stage(paths.iter().map(|&p| p.into()).collect()))
    }

    fn unstage(paths: &[&str]) -> Option<GitOp> {
        Some(GitOp::Unstage(paths.iter().map(|&p| p.into()).collect()))
    }

    #[test]
    fn files_with_unstaged_changes_are_staged() {
        let unstaged = entry("a.rs", '.', 'M', EntryKind::Changed);
        let untracked = entry("new.rs", '?', '?', EntryKind::Untracked);
        let partly = entry("mm.rs", 'M', 'M', EntryKind::Changed);
        assert_eq!(toggle_for(&unstaged), stage(&["a.rs"]));
        assert_eq!(toggle_for(&untracked), stage(&["new.rs"]));
        assert_eq!(toggle_for(&partly), stage(&["mm.rs"]));
    }

    #[test]
    fn staged_only_files_are_unstaged() {
        let staged = entry("a.rs", 'M', '.', EntryKind::Changed);
        assert_eq!(toggle_for(&staged), unstage(&["a.rs"]));
    }

    #[test]
    fn a_staged_rename_unstages_both_paths() {
        let mut renamed = entry("new.rs", 'R', '.', EntryKind::Renamed);
        renamed.orig_path = Some("old.rs".into());
        assert_eq!(toggle_for(&renamed), unstage(&["old.rs", "new.rs"]));
    }

    #[test]
    fn conflicted_files_are_left_alone() {
        let conflicted = entry("both.rs", 'U', 'U', EntryKind::Conflicted);
        assert_eq!(toggle_for(&conflicted), None);
    }

    #[test]
    fn toggle_all_stages_everything_while_anything_is_unstaged() {
        let staged = entry("a.rs", 'M', '.', EntryKind::Changed);
        for other in [
            entry("b.rs", '.', 'M', EntryKind::Changed),
            entry("c.rs", '?', '?', EntryKind::Untracked),
        ] {
            let files = [staged.clone(), other];
            assert_eq!(toggle_all(&files), Ok(Some(GitOp::StageAll)));
        }
    }

    #[test]
    fn toggle_all_unstages_everything_when_all_is_staged() {
        let files = [
            entry("a.rs", 'M', '.', EntryKind::Changed),
            entry("b.rs", 'A', '.', EntryKind::Changed),
        ];
        assert_eq!(toggle_all(&files), Ok(Some(GitOp::UnstageAll)));
    }

    #[test]
    fn toggle_all_on_a_clean_tree_does_nothing() {
        assert_eq!(toggle_all(&[]), Ok(None));
    }

    #[test]
    fn toggle_all_refuses_while_conflicts_exist() {
        let files = [
            entry("a.rs", '.', 'M', EntryKind::Changed),
            entry("both.rs", 'U', 'U', EntryKind::Conflicted),
        ];
        assert_eq!(
            toggle_all(&files),
            Err("Resolve conflicts before staging everything".into())
        );
    }

    #[test]
    fn a_range_toggles_and_discards_as_one_op() {
        let unstaged = entry("a.rs", '.', 'M', EntryKind::Changed);
        let staged = entry("s.rs", 'M', '.', EntryKind::Changed);
        let new = entry("n.txt", '?', '?', EntryKind::Untracked);
        let conflict = entry("c.rs", 'U', 'U', EntryKind::Conflicted);
        assert_eq!(
            toggle_many(&[&unstaged, &conflict, &staged]),
            stage(&["a.rs", "s.rs"])
        );
        assert_eq!(toggle_many(&[&staged, &conflict]), unstage(&["s.rs"]));
        assert_eq!(toggle_many(&[&conflict]), None);
        assert_eq!(
            GitOp::discard_many(&[&unstaged, &staged, &new, &conflict]),
            Some(GitOp::DiscardFiles {
                restore: paths(&["a.rs"]),
                delete: paths(&["n.txt"]),
            })
        );
        assert_eq!(
            GitOp::discard_many(&[&unstaged]),
            Some(GitOp::DiscardWorktree(paths(&["a.rs"])))
        );
        assert_eq!(
            GitOp::discard_many(&[&new]),
            Some(GitOp::DeleteUntracked(paths(&["n.txt"])))
        );
        assert_eq!(GitOp::discard_many(&[&staged, &conflict]), None);
    }

    #[test]
    #[cfg_attr(windows, ignore = "Windows forbids * in file names")]
    fn a_file_named_like_a_glob_stages_and_unstages_only_itself_in_real_git() {
        use crate::git::fake::{TempDir, git};
        use crate::git::status::read_status;

        let tmp = TempDir::new("ops");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        for name in ["a.rs", "*.rs"] {
            std::fs::write(dir.join(name), "old\n").unwrap();
        }
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        for name in ["a.rs", "*.rs"] {
            std::fs::write(dir.join(name), "new\n").unwrap();
        }
        let runner = crate::git::CliRunner::new(dir);
        let letters = || {
            let mut files: Vec<_> = read_status(&runner)
                .unwrap()
                .files
                .into_iter()
                .map(|f| {
                    let xy = f.status_letters();
                    (f.path.display().to_string(), xy)
                })
                .collect();
            files.sort();
            files
        };
        let glob = paths(&["*.rs"]);

        run_op(&runner, &GitOp::Stage(glob.clone())).unwrap();
        assert_eq!(
            letters(),
            [("*.rs".into(), "M ".into()), ("a.rs".into(), " M".into())]
        );

        run_op(&runner, &GitOp::Unstage(glob)).unwrap();
        assert_eq!(
            letters(),
            [("*.rs".into(), " M".into()), ("a.rs".into(), " M".into())]
        );
    }

    /// Regression: a name that is not UTF-8 used to be decoded lossily into
    /// argv, so toggling it staged its look-alike instead.
    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_utf8_stages_and_unstages_itself_not_its_look_alike() {
        use crate::git::fake::{LOOK_ALIKE, RAW_NAME, entry_named, git, look_alike_repo};

        let tmp = look_alike_repo();
        let dir = tmp.path();
        std::fs::write(dir.join(LOOK_ALIKE), "look-alike new\n").unwrap();
        let runner = crate::git::CliRunner::new(dir);
        let letters = |raw: &[u8]| entry_named(&runner, raw).map(|e| e.status_letters());
        assert_eq!(letters(RAW_NAME).as_deref(), Some(" D"));

        let raw = entry_named(&runner, RAW_NAME).unwrap();
        run_op(&runner, &toggle_for(&raw).unwrap()).unwrap();
        assert_eq!(letters(RAW_NAME).as_deref(), Some("D "));
        assert_eq!(letters(LOOK_ALIKE.as_bytes()).as_deref(), Some(" M"));

        git(dir, &["add", "--", LOOK_ALIKE]);
        let raw = entry_named(&runner, RAW_NAME).unwrap();
        run_op(&runner, &toggle_for(&raw).unwrap()).unwrap();
        assert_eq!(letters(RAW_NAME).as_deref(), Some(" D"));
        assert_eq!(letters(LOOK_ALIKE.as_bytes()).as_deref(), Some("M "));
    }
}
