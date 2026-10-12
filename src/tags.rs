//! The Branches panel's Tags tab: the tags as last loaded and the
//! selection, and the tag a create prompt will make. Pure; the model
//! loads the list and moves the selection.

use crate::git::commit_files::CommitId;
use crate::git::tags::TagRef;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagsList {
    pub tags: Vec<TagRef>,
    /// False until the first load lands, to tell "loading" from "none".
    pub loaded: bool,
    pub error: Option<String>,
    pub selected: Option<usize>,
    /// The list load whose result is awaited; older ones are dropped.
    pub list_seq: Option<u64>,
}

/// A tag being made from the Commits panel. The commit is taken when `t`
/// or `T` is pressed, so neither a reload nor a move of the selection
/// while the user types can retarget it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTag {
    pub target: CommitId,
    pub annotated: bool,
    /// The name, already checked, once an annotated tag asks for its
    /// message.
    pub name: Option<String>,
}

impl TagsList {
    /// Take a list load's result. The selection stays on the same name;
    /// if that tag is gone, near the same index. A failure keeps the last
    /// list, which may be out of date.
    pub fn loaded(&mut self, result: Result<Vec<TagRef>, String>) {
        self.loaded = true;
        let tags = match result {
            Ok(tags) => tags,
            Err(message) => {
                self.error = Some(message);
                return;
            }
        };
        self.error = None;
        let old = self.selected_tag().map(|t| t.name.clone());
        self.tags = tags;
        self.selected = if self.tags.is_empty() {
            None
        } else {
            let last = self.tags.len() - 1;
            old.and_then(|name| self.tags.iter().position(|t| t.name == name))
                .or_else(|| self.selected.map(|i| i.min(last)))
                .or(Some(0))
        };
    }

    /// Move the selection by `delta`, stopping at either end. Says whether
    /// it moved.
    pub fn move_by(&mut self, delta: isize) -> bool {
        let Some(current) = self.selected else {
            return false;
        };
        let next = current
            .saturating_add_signed(delta)
            .min(self.tags.len().saturating_sub(1));
        self.selected = Some(next);
        next != current
    }

    pub fn selected_tag(&self) -> Option<&TagRef> {
        self.selected.and_then(|i| self.tags.get(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(name: &str) -> TagRef {
        let oid = "1".repeat(40);
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

    fn list(names: &[&str]) -> TagsList {
        let mut list = TagsList::default();
        list.loaded(Ok(names.iter().map(|n| tag(n)).collect()));
        list
    }

    fn selected(list: &TagsList) -> Option<String> {
        list.selected_tag().map(|t| t.name.display())
    }

    #[test]
    fn the_first_load_selects_the_first_tag_and_an_empty_one_nothing() {
        assert!(!TagsList::default().loaded);
        assert_eq!(selected(&list(&["v2", "v1"])).as_deref(), Some("v2"));
        let empty = list(&[]);
        assert!(empty.loaded);
        assert_eq!(empty.selected, None);
    }

    #[test]
    fn moving_stops_at_either_end() {
        let mut list = list(&["a", "b"]);
        assert!(!list.move_by(-1));
        assert!(list.move_by(1));
        assert!(!list.move_by(1));
        assert_eq!(selected(&list).as_deref(), Some("b"));
        assert!(!TagsList::default().move_by(1));
    }

    #[test]
    fn a_reload_keeps_the_same_name_or_else_the_place() {
        let mut list = list(&["b", "c"]);
        list.move_by(1);
        list.loaded(Ok(vec![tag("a"), tag("b"), tag("c")]));
        assert_eq!(selected(&list).as_deref(), Some("c"));
        list.loaded(Ok(vec![tag("a")]));
        assert_eq!(selected(&list).as_deref(), Some("a"));
        list.loaded(Ok(vec![]));
        assert_eq!(list.selected, None);
    }

    #[test]
    fn a_failed_reload_keeps_the_list_and_says_why() {
        let mut list = list(&["a"]);
        list.loaded(Err("broken".into()));
        assert_eq!(list.tags.len(), 1);
        assert_eq!(list.error.as_deref(), Some("broken"));
        list.loaded(Ok(vec![tag("a")]));
        assert_eq!(list.error, None);
    }
}
