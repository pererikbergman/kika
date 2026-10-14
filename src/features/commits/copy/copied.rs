//! The copied commits: what a paste cherry-picks onto the current branch.
//! Pure; `AppState` keeps one and turns a paste into an op.
//!
//! Copy order is kept only so the set reads as the user built it. A paste
//! never picks in this order; it asks git for parents before children
//! (see `super::git`).

use kika_git::commit_files::CommitId;

/// One copied commit, with what the bar names it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopiedCommit {
    /// The full id from the log, never the short one, so it stays one
    /// commit however the repository grows.
    pub id: CommitId,
    pub short: String,
    pub subject: String,
}

impl CopiedCommit {
    /// `short` is made from `id`, never taken from the log's text.
    pub fn new(id: CommitId, subject: String) -> Self {
        let short = super::git::short(&id).to_string();
        Self { id, short, subject }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CopiedCommits {
    entries: Vec<CopiedCommit>,
}

impl CopiedCommits {
    /// Add `entry`, or take out the one with its id. Returns true when it
    /// was added.
    pub fn toggle(&mut self, entry: CopiedCommit) -> bool {
        match self.entries.iter().position(|e| e.id == entry.id) {
            Some(at) => {
                self.entries.remove(at);
                false
            }
            None => {
                self.entries.push(entry);
                true
            }
        }
    }

    pub fn contains(&self, id: &CommitId) -> bool {
        self.entries.iter().any(|e| &e.id == id)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The ids in copy order.
    pub fn ids(&self) -> Vec<CommitId> {
        self.entries.iter().map(|e| e.id.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(hex: char) -> CopiedCommit {
        CopiedCommit {
            id: CommitId::parse(&hex.to_string().repeat(40)).unwrap(),
            short: hex.to_string().repeat(7),
            subject: format!("subject {hex}"),
        }
    }

    #[test]
    fn the_short_id_is_made_from_the_full_one() {
        let id = CommitId::parse(&"ab".repeat(20)).unwrap();
        assert_eq!(CopiedCommit::new(id, "s".into()).short, "abababa");
    }

    #[test]
    fn toggling_adds_an_entry_and_toggling_it_again_removes_it() {
        let mut set = CopiedCommits::default();
        assert!(set.toggle(entry('a')));
        assert!(set.contains(&entry('a').id));
        assert_eq!(set.len(), 1);
        assert!(!set.toggle(entry('a')));
        assert!(!set.contains(&entry('a').id));
        assert!(set.is_empty());
    }

    #[test]
    fn ids_keep_copy_order() {
        let mut set = CopiedCommits::default();
        for hex in ['c', 'a', 'b'] {
            set.toggle(entry(hex));
        }
        let ids = ['c', 'a', 'b'].map(|hex| entry(hex).id);
        assert_eq!(set.ids(), ids);
        // Taking one out keeps the others' order.
        set.toggle(entry('a'));
        assert_eq!(set.ids(), [entry('c').id, entry('b').id]);
    }

    #[test]
    fn clear_empties_the_set() {
        let mut set = CopiedCommits::default();
        set.toggle(entry('a'));
        set.toggle(entry('b'));
        set.clear();
        assert!(set.is_empty());
        assert_eq!(set.ids(), []);
    }
}
