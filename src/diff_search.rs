//! Searching a pane of text: where a typed text appears in the rows as
//! drawn, and which match the cursor is on. Pure; the hunk view, the diff
//! preview, the blame and the blame's commit each keep one, and the view
//! highlights its matches.

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
        Self::new_in(query, rows.iter().map(String::as_str), from)
    }

    /// [`Self::new`] over borrowed rows, so a pane need not copy its lines.
    pub fn new_in<'a>(query: &str, rows: impl Iterator<Item = &'a str>, from: usize) -> Self {
        let matches = find_in(rows, query);
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
        self.refresh_in(rows.iter().map(String::as_str));
    }

    /// [`Self::refresh`] over borrowed rows.
    pub fn refresh_in<'a>(&mut self, rows: impl Iterator<Item = &'a str>) {
        let from = self.current_match().map_or(0, |m| m.row);
        *self = Self::new_in(&self.query, rows, from);
    }

    /// Search rows added after the others, the first of them being row
    /// `first`, as when a streamed diff brings more lines. The matches so
    /// far and the current one stay; with none current, the first match
    /// found becomes it.
    pub fn extend_in<'a>(&mut self, rows: impl Iterator<Item = &'a str>, first: usize) {
        let added = find_in(rows, &self.query).into_iter().map(|m| SearchMatch {
            row: m.row + first,
            ..m
        });
        self.matches.extend(added);
        if self.current.is_none() && !self.matches.is_empty() {
            self.current = Some(0);
        }
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
/// do not overlap. An empty query finds nothing. The panes search with
/// [`find_in`]; this is its owned-rows form.
#[cfg(test)]
pub fn find(rows: &[String], query: &str) -> Vec<SearchMatch> {
    find_in(rows.iter().map(String::as_str), query)
}

/// [`find`] over borrowed rows, so a caller need not copy its lines.
pub fn find_in<'a>(rows: impl Iterator<Item = &'a str>, query: &str) -> Vec<SearchMatch> {
    let query: Vec<char> = query.chars().collect();
    if query.is_empty() {
        return Vec::new();
    }
    let exact = query.iter().any(|c| c.is_uppercase());
    let mut found = Vec::new();
    for (row, text) in rows.enumerate() {
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

/// The scroll that shows `row` in a view of `rows` rows now scrolled to
/// `scroll`, moving it as little as it can: a row above goes to the top, a
/// row below to the bottom, and a row in view stays where it is.
pub fn reveal(row: usize, scroll: usize, rows: usize) -> usize {
    let rows = rows.max(1);
    if row < scroll {
        row
    } else if row >= scroll + rows {
        row + 1 - rows
    } else {
        scroll
    }
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
    fn find_in_reads_borrowed_rows_and_finds_what_find_does() {
        let text = rows(&["-foo Foo", "", "+foofoo", "é foo"]);
        for query in ["foo", "Foo", "o", "", "zz"] {
            assert_eq!(
                find_in(text.iter().map(String::as_str), query),
                find(&text, query),
                "{query:?}"
            );
        }
    }

    #[test]
    fn reveal_moves_the_least_that_shows_the_row() {
        // A 5-row view from row 10 shows rows 10..15.
        assert_eq!(reveal(3, 10, 5), 3, "above: the row goes to the top");
        assert_eq!(reveal(10, 10, 5), 10, "inside: no move");
        assert_eq!(reveal(14, 10, 5), 10, "inside at the bottom: no move");
        assert_eq!(reveal(15, 10, 5), 11, "below: the row goes to the bottom");
        assert_eq!(reveal(40, 10, 5), 36);
        assert_eq!(reveal(7, 0, 0), 7, "a zero-row view counts as one row");
    }

    #[test]
    fn extending_adds_the_new_rows_matches_and_keeps_the_current_one() {
        let mut search = DiffSearch::new("x", &rows(&["x x", "y"]), 0);
        search.next();
        assert_eq!(search.current, Some(1));
        search.extend_in(["a", "x"].into_iter(), 2);
        assert_eq!(search.matches, [at(0, 0, 1), at(0, 2, 3), at(3, 0, 1)]);
        assert_eq!(search.current, Some(1), "the same match stays current");

        let mut none = DiffSearch::new("x", &rows(&["y"]), 0);
        none.extend_in(["x"].into_iter(), 1);
        assert_eq!(
            none.current,
            Some(0),
            "the first match found becomes current"
        );
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
