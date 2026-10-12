//! Searching the focused Diff pane: where a typed text appears in the rows
//! as drawn, and which match the cursor is on. Pure; the hunk view keeps
//! one and the view highlights its matches.

use crate::list_filter::same_char;

/// Where one match sits: a drawn row and the char offsets `start..end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    pub row: usize,
    pub start: usize,
    pub end: usize,
}

/// A search in progress: the query, every match in row order, and the one
/// `n` and `N` step from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffSearch {
    pub query: String,
    pub matches: Vec<SearchMatch>,
    /// Index into `matches`; `None` when there are none.
    pub current: Option<usize>,
}

impl DiffSearch {
    /// Search `rows` for `query`. The current match is the first at or
    /// after row `from`, else the first, so a search starts where the
    /// user is looking.
    pub fn new(query: &str, rows: &[String], from: usize) -> Self {
        let matches = find(rows, query);
        let current = at_or_after(&matches, from);
        Self {
            query: query.to_string(),
            matches,
            current,
        }
    }

    /// Search new rows for the same query, as after a reload. The current
    /// match stays on the first at or after the old one's row.
    pub fn refresh(&mut self, rows: &[String]) {
        let from = self.current_match().map_or(0, |m| m.row);
        *self = Self::new(&self.query, rows, from);
    }

    pub fn current_match(&self) -> Option<&SearchMatch> {
        self.current.and_then(|i| self.matches.get(i))
    }

    /// Step to the next match, after the last back to the first. Returns
    /// whether it wrapped.
    pub fn next(&mut self) -> bool {
        let Some(at) = self.current else {
            return false;
        };
        let next = (at + 1) % self.matches.len();
        self.current = Some(next);
        next <= at && self.matches.len() > 1
    }

    /// Step to the previous match, before the first round to the last.
    /// Returns whether it wrapped.
    pub fn prev(&mut self) -> bool {
        let Some(at) = self.current else {
            return false;
        };
        let prev = at.checked_sub(1).unwrap_or(self.matches.len() - 1);
        self.current = Some(prev);
        prev >= at && self.matches.len() > 1
    }
}

/// The first match on row `row` or later, else the first one.
fn at_or_after(matches: &[SearchMatch], row: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    Some(matches.iter().position(|m| m.row >= row).unwrap_or(0))
}

/// Every place `query` appears in `rows`, as a substring, not fuzzily.
/// Case is ignored unless the query has an uppercase char. Matches in a row
/// do not overlap. An empty query finds nothing.
pub fn find(rows: &[String], query: &str) -> Vec<SearchMatch> {
    let query: Vec<char> = query.chars().collect();
    if query.is_empty() {
        return Vec::new();
    }
    let exact = query.iter().any(|c| c.is_uppercase());
    let mut found = Vec::new();
    for (row, text) in rows.iter().enumerate() {
        let text: Vec<char> = text.chars().collect();
        let mut start = 0;
        while start + query.len() <= text.len() {
            let hit = query
                .iter()
                .zip(&text[start..])
                .all(|(&q, &t)| same_char(q, t, exact));
            if hit {
                found.push(SearchMatch {
                    row,
                    start,
                    end: start + query.len(),
                });
                start += query.len();
            } else {
                start += 1;
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|t| t.to_string()).collect()
    }

    fn at(row: usize, start: usize, end: usize) -> SearchMatch {
        SearchMatch { row, start, end }
    }

    #[test]
    fn a_row_can_hold_several_matches_that_never_overlap() {
        let found = find(&rows(&["-foo foo", " bar", "+aaaa"]), "foo");
        assert_eq!(found, [at(0, 1, 4), at(0, 5, 8)]);
        assert_eq!(find(&rows(&["aaaa"]), "aa"), [at(0, 0, 2), at(0, 2, 4)]);
    }

    #[test]
    fn offsets_count_chars_not_bytes() {
        assert_eq!(find(&rows(&["+é日x"]), "x"), [at(0, 3, 4)]);
    }

    #[test]
    fn case_is_smart() {
        let text = rows(&["Foo foo FOO"]);
        assert_eq!(find(&text, "foo").len(), 3);
        assert_eq!(find(&text, "Foo"), [at(0, 0, 3)]);
    }

    #[test]
    fn next_and_prev_wrap_both_ways() {
        let mut search = DiffSearch::new("x", &rows(&["x", "y", "x x"]), 0);
        assert_eq!(search.current, Some(0));
        assert!(!search.next());
        assert!(!search.next());
        assert_eq!(search.current, Some(2));
        assert!(search.next(), "past the last back to the first");
        assert_eq!(search.current, Some(0));
        assert!(search.prev(), "before the first round to the last");
        assert_eq!(search.current, Some(2));
        assert!(!search.prev());
        assert_eq!(search.current_match(), Some(&at(2, 0, 1)));
    }

    #[test]
    fn no_matches_leave_no_current() {
        let mut search = DiffSearch::new("zz", &rows(&["a", "b"]), 0);
        assert!(search.matches.is_empty());
        assert_eq!(search.current, None);
        assert!(!search.next());
        assert!(!search.prev());
        assert_eq!(search.current, None);
    }

    #[test]
    fn a_new_search_starts_at_or_after_the_given_row() {
        let text = rows(&["x", "y", "x", "x"]);
        assert_eq!(DiffSearch::new("x", &text, 1).current, Some(1));
        assert_eq!(DiffSearch::new("x", &text, 9).current, Some(0));
    }

    #[test]
    fn a_refresh_keeps_the_current_match_near_its_row() {
        let mut search = DiffSearch::new("x", &rows(&["x", "x", "x"]), 0);
        search.next();
        // Row 1 lost its match: the next one on or after row 1 is current.
        search.refresh(&rows(&["x", "y", "y", "x"]));
        assert_eq!(search.current_match(), Some(&at(3, 0, 1)));
    }
}
