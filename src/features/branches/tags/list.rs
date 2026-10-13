//! The Branches panel's Tags tab: the tags as last loaded and the
//! selection, and the tag a create prompt will make. Pure; the model
//! loads the list and moves the selection.

use crate::filtered_list::{FilteredList, Row};
use crate::git::commit_files::CommitId;
use crate::git::tags::{TagName, TagRef};

/// The tags as last loaded, with the cursor, the filter and the range.
/// The cursor follows a tag by name across reloads.
pub(crate) type TagsList = FilteredList<TagRef>;

/// A failed reload keeps the last tags, which may be out of date.
impl Row for TagRef {
    type Key = TagName;
    const KEEP_ROWS_ON_ERROR: bool = true;
    fn key(&self) -> TagName {
        self.name.clone()
    }
    fn filter_text(&self) -> String {
        self.name.display()
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::load_list;

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
        load_list(&mut list, Ok(names.iter().map(|n| tag(n)).collect()));
        list
    }

    fn selected(list: &TagsList) -> Option<String> {
        list.selected_row().map(|t| t.name.display())
    }

    #[test]
    fn the_first_load_selects_the_first_tag_and_an_empty_one_nothing() {
        assert!(!TagsList::default().loaded);
        assert_eq!(selected(&list(&["v2", "v1"])).as_deref(), Some("v2"));
        let empty = list(&[]);
        assert!(empty.loaded);
        assert_eq!(empty.selected_row(), None);
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
        load_list(&mut list, Ok(vec![tag("a"), tag("b"), tag("c")]));
        assert_eq!(selected(&list).as_deref(), Some("c"));
        load_list(&mut list, Ok(vec![tag("a")]));
        assert_eq!(selected(&list).as_deref(), Some("a"));
        load_list(&mut list, Ok(vec![]));
        assert_eq!(list.selected_row(), None);
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
        load_list(&mut list, Ok(vec![tag("a"), tag("b1"), tag("b2")]));
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
        load_list(&mut list, Ok(vec![tag("b"), tag("c")]));
        assert_eq!(list.range_anchor, None);
    }

    #[test]
    fn a_failed_reload_keeps_the_list_and_says_why() {
        let mut list = list(&["a"]);
        load_list(&mut list, Err("broken".into()));
        assert_eq!(list.rows.len(), 1);
        assert_eq!(list.error.as_deref(), Some("broken"));
        load_list(&mut list, Ok(vec![tag("a")]));
        assert_eq!(list.error, None);
    }
}
