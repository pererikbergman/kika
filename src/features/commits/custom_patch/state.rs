//! A custom patch: changes picked from one commit, whole files or single
//! changed lines, to take out of it, move to another commit, move to the
//! index or split off as a commit of their own. Pure state; the rewrite
//! runs in `custom_patch::git`.

use std::collections::{BTreeMap, BTreeSet};

use kika_git::commit_files::{CommitId, CommitRange};
use kika_git::patch::LineId;
use kika_git::path::RepoPath;

/// What is picked from one file of the source commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilePick {
    /// The file's whole change in the commit.
    Whole,
    /// These changed lines, by their ids in the commit's diff of the file.
    Lines(BTreeSet<LineId>),
}

/// The picks, all from `source`, which always has a parent: a root commit
/// has nothing to diff against that a rebase can stop above.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomPatch {
    pub source: CommitRange,
    /// For titles and notices.
    pub short_oid: String,
    pub files: BTreeMap<RepoPath, FilePick>,
}

/// What a rewrite does with the patch. Each is asked about with `y` only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchAction {
    /// Take it out of the source commit.
    Remove,
    /// Take it out of the source and put it into this commit.
    MoveTo(CommitId),
    /// Take it out of the source and leave it staged.
    ToIndex,
    /// Take it out of the source and commit it right after, with this
    /// message.
    NewCommit(String),
}

/// A rewrite as the patch menu names it; the model fills in the target
/// or the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchChoice {
    Remove,
    /// To the commit under the Commits panel's cursor.
    MoveToSelected,
    ToIndex,
    /// After asking for its message.
    NewCommit,
}

impl CustomPatch {
    /// An empty patch over `source`.
    pub fn new(source: CommitRange, short_oid: String) -> Self {
        Self {
            source,
            short_oid,
            files: BTreeMap::new(),
        }
    }

    /// Pick the whole file, or drop it when anything of it is picked.
    pub fn toggle_file(&mut self, path: &RepoPath) {
        if self.files.remove(path).is_none() {
            self.files.insert(path.clone(), FilePick::Whole);
        }
    }

    /// Take `lines` out when every one is picked already, else add the
    /// missing ones. A file left with no lines is dropped. A whole file
    /// counts as having every line; call [`Self::expand`] first so taking
    /// some out leaves the rest.
    pub fn toggle_lines(&mut self, path: &RepoPath, lines: &BTreeSet<LineId>) {
        if lines.is_empty() {
            return;
        }
        if lines.iter().all(|&id| self.contains_line(path, id)) {
            match self.files.get_mut(path) {
                Some(FilePick::Lines(picked)) => {
                    picked.retain(|id| !lines.contains(id));
                    if picked.is_empty() {
                        self.files.remove(path);
                    }
                }
                _ => {
                    self.files.remove(path);
                }
            }
            return;
        }
        match self.files.get_mut(path) {
            Some(FilePick::Lines(picked)) => picked.extend(lines.iter().copied()),
            Some(FilePick::Whole) => {}
            None => {
                self.files
                    .insert(path.clone(), FilePick::Lines(lines.clone()));
            }
        }
    }

    /// Turn a whole-file pick into its lines, `all` being every changed
    /// line of the file, so some can be taken out.
    pub fn expand(&mut self, path: &RepoPath, all: &BTreeSet<LineId>) {
        if let Some(pick @ FilePick::Whole) = self.files.get_mut(path) {
            *pick = FilePick::Lines(all.clone());
        }
    }

    pub fn contains_line(&self, path: &RepoPath, id: LineId) -> bool {
        match self.files.get(path) {
            Some(FilePick::Whole) => true,
            Some(FilePick::Lines(lines)) => lines.contains(&id),
            None => false,
        }
    }

    pub fn file_state(&self, path: &RepoPath) -> Option<FilePick> {
        self.files.get(path).cloned()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// `patch <short>: N files`, for the Commits panel's title.
    pub fn summary(&self) -> String {
        let n = self.files.len();
        let files = if n == 1 { "file" } else { "files" };
        format!("patch {}: {n} {files}", self.short_oid)
    }

    /// Whether picks from `range` may join this patch: only one commit's.
    pub fn is_from(&self, range: &CommitRange) -> bool {
        self.source == *range
    }

    /// Why picking from another commit is refused.
    pub fn other_commit_notice(&self) -> String {
        format!(
            "The custom patch holds changes from {}; reset it first",
            self.short_oid
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch() -> CustomPatch {
        let range = CommitRange {
            oid: CommitId::parse("bbbb").unwrap(),
            parent: CommitId::parse("aaaa"),
        };
        CustomPatch::new(range, "bbbb".into())
    }

    fn ids(lines: &[u32]) -> BTreeSet<LineId> {
        lines.iter().map(|&n| LineId::added(n)).collect()
    }

    fn path() -> RepoPath {
        RepoPath::from("a.txt")
    }

    #[test]
    fn toggling_lines_adds_a_set_then_removes_it_when_all_are_present() {
        let mut p = patch();
        p.toggle_lines(&path(), &ids(&[2, 3]));
        assert_eq!(p.file_state(&path()), Some(FilePick::Lines(ids(&[2, 3]))));
        assert!(p.contains_line(&path(), LineId::added(2)));
        assert!(!p.contains_line(&path(), LineId::added(4)));
        assert!(!p.contains_line(&path(), LineId::removed(2)));

        // Some present: the rest are added.
        p.toggle_lines(&path(), &ids(&[3, 4]));
        assert_eq!(
            p.file_state(&path()),
            Some(FilePick::Lines(ids(&[2, 3, 4])))
        );

        // All present: they go, the others stay.
        p.toggle_lines(&path(), &ids(&[3, 4]));
        assert_eq!(p.file_state(&path()), Some(FilePick::Lines(ids(&[2]))));
    }

    #[test]
    fn a_file_left_with_no_lines_is_dropped() {
        let mut p = patch();
        p.toggle_lines(&path(), &ids(&[2]));
        p.toggle_lines(&path(), &ids(&[2]));
        assert_eq!(p.file_state(&path()), None);
        assert!(p.is_empty());
        // An empty set changes nothing.
        p.toggle_lines(&path(), &BTreeSet::new());
        assert!(p.is_empty());
    }

    #[test]
    fn toggling_a_file_flips_whole() {
        let mut p = patch();
        p.toggle_file(&path());
        assert_eq!(p.file_state(&path()), Some(FilePick::Whole));
        assert!(p.contains_line(&path(), LineId::removed(9)));
        p.toggle_file(&path());
        assert_eq!(p.file_state(&path()), None);
        // Some lines picked: the file goes.
        p.toggle_lines(&path(), &ids(&[1]));
        p.toggle_file(&path());
        assert_eq!(p.file_state(&path()), None);
    }

    #[test]
    fn an_expanded_whole_file_loses_only_the_lines_taken_out() {
        let mut p = patch();
        p.toggle_file(&path());
        p.expand(&path(), &ids(&[1, 2, 3]));
        p.toggle_lines(&path(), &ids(&[2]));
        assert_eq!(p.file_state(&path()), Some(FilePick::Lines(ids(&[1, 3]))));
    }

    #[test]
    fn the_summary_names_the_source_and_counts_files() {
        let mut p = patch();
        p.toggle_file(&path());
        assert_eq!(p.summary(), "patch bbbb: 1 file");
        p.toggle_file(&RepoPath::from("b.txt"));
        assert_eq!(p.summary(), "patch bbbb: 2 files");
        assert_eq!(
            p.other_commit_notice(),
            "The custom patch holds changes from bbbb; reset it first"
        );
    }
}
