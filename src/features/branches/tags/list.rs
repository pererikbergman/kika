//! The Branches panel's Tags tab: the tags as last loaded and the
//! selection, and the tag a create prompt will make. Pure; the model
//! loads the list and moves the selection.

use crate::git::commit_files::CommitId;
use crate::git::tags::{TagName, TagRef};
use crate::list_filter;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagsList {
    pub tags: Vec<TagRef>,
    /// False until the first load lands, to tell "loading" from "none".
    pub loaded: bool,
    pub error: Option<String>,
    pub selected: Option<usize>,
    /// The list load whose result is awaited; older ones are dropped.
    pub list_seq: Option<u64>,
    /// What the tab is narrowed to, kept for the session.
    pub filter: String,
    /// The tag a range selection started on, so a reload keeps the range
    /// on the same tags.
    pub range_anchor: Option<TagName>,
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
        self.reconcile();
    }

    /// Move the selection by `delta` over the tags the filter shows,
    /// stopping at either end. Says whether it moved.
    pub fn move_by(&mut self, delta: isize) -> bool {
        let Some(current) = self.selected else {
            return false;
        };
        let Some(next) = list_filter::step(&self.visible(), current, delta) else {
            return false;
        };
        self.selected = Some(next);
        next != current
    }

    /// The indexes of the tags the filter shows, in list order.
    pub fn visible(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.tags.len()).collect();
        }
        let names: Vec<String> = self.tags.iter().map(|t| t.name.display()).collect();
        list_filter::visible(&self.filter, names.iter().map(String::as_str))
    }

    /// The filter hides the selected tag, which happens only when it hides
    /// every tag.
    pub fn selected_hidden(&self) -> bool {
        self.selected_tag()
            .is_some_and(|t| !list_filter::matches(&self.filter, &t.name.display()))
    }

    /// The tags the range covers, in list order; empty without one.
    pub fn range(&self) -> Vec<usize> {
        let anchor = self
            .range_anchor
            .as_ref()
            .and_then(|anchor| self.tags.iter().position(|t| &t.name == anchor));
        match (anchor, self.selected) {
            (Some(anchor), Some(cursor)) => list_filter::range(&self.visible(), anchor, cursor),
            _ => Vec::new(),
        }
    }

    /// Narrow the list to `text`. A changed filter drops the range, which
    /// was made over the rows shown before; says whether it did.
    pub fn set_filter(&mut self, text: String) -> bool {
        let mut cleared = false;
        if text != self.filter {
            self.filter = text;
            cleared = self.range_anchor.take().is_some();
        }
        self.reconcile();
        cleared
    }

    /// Start a range at the selected tag, or clear the one there is. A
    /// tag the filter hides starts none.
    pub fn toggle_range(&mut self) {
        if self.range_anchor.take().is_none() && !self.selected_hidden() {
            self.range_anchor = self.selected_tag().map(|t| t.name.clone());
        }
    }

    /// After the filter or the list changed: a range whose first tag is
    /// gone is cleared, and a hidden selection moves to the first tag
    /// shown.
    pub fn reconcile(&mut self) {
        if let Some(anchor) = &self.range_anchor
            && !self.tags.iter().any(|t| &t.name == anchor)
        {
            self.range_anchor = None;
        }
        if self.selected_hidden()
            && let Some(&first) = self.visible().first()
        {
            self.selected = Some(first);
        }
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
    fn moving_skips_the_tags_the_filter_hides() {
        let mut list = list(&["v3", "x", "v2", "v1"]);
        list.filter = "v".into();
        assert_eq!(list.visible(), [0, 2, 3]);
        assert!(list.move_by(1));
        assert_eq!(selected(&list).as_deref(), Some("v2"), "x is hidden");
        assert!(list.move_by(5));
        assert!(!list.move_by(1));
        assert!(list.move_by(-1));
        assert_eq!(selected(&list).as_deref(), Some("v2"));
    }

    #[test]
    fn a_filter_that_hides_the_selection_moves_it_to_the_first_shown() {
        let mut list = list(&["a", "b1", "b2"]);
        list.filter = "b".into();
        list.reconcile();
        assert_eq!(selected(&list).as_deref(), Some("b1"));
        list.filter = "zzz".into();
        list.reconcile();
        assert!(list.selected_hidden());
        assert_eq!(selected(&list).as_deref(), Some("b1"), "nowhere to go");
        // A reload under a filter lands on a shown tag.
        list.filter = "b2".into();
        list.loaded(Ok(vec![tag("a"), tag("b1"), tag("b2")]));
        assert_eq!(selected(&list).as_deref(), Some("b2"));
    }

    #[test]
    fn a_range_follows_its_tags_and_a_reload_without_its_anchor_clears_it() {
        let mut list = list(&["a", "b", "c"]);
        list.range_anchor = Some("a".into());
        list.move_by(2);
        assert_eq!(list.range(), [0, 1, 2]);
        list.filter = "c".into();
        assert!(list.range().is_empty(), "the anchor is hidden");
        list.filter.clear();
        list.loaded(Ok(vec![tag("b"), tag("c")]));
        assert_eq!(list.range_anchor, None);
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
