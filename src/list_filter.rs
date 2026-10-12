//! Narrowing a list as the user types, and the rows a range selection
//! spans. Pure: the model keeps the text and the anchor, and asks here
//! which rows show and which a range covers. Lists keep their own order,
//! so a range means the same rows on screen and in the op.

use crate::popup::LineInput;

/// The most chars a filter or search takes. Plenty to type, and it keeps
/// each frame cheap whatever is pasted.
pub const MAX_INPUT: usize = 256;

/// Why a key typed into a full input does nothing.
pub const INPUT_FULL: &str = "The filter is full (256 characters)";

/// What the one text input on the bottom bar is typing into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputTarget {
    FilesFilter,
    CommitsFilter,
    BranchesFilter,
    RemotesFilter,
    TagsFilter,
    /// Narrows the Commits panel's Reflog tab.
    ReflogFilter,
    /// Narrows the files of the commit opened from the Commits panel.
    CommitFilesFilter,
    /// Narrows the Stash panel's list.
    StashFilter,
    DiffSearch,
    /// Searches the diff preview a side panel shows.
    PreviewSearch,
    /// Searches the blame's line text.
    BlameSearch,
    /// Searches the commit opened from a blame line.
    BlameCommitSearch,
    /// The path git narrows the log to; read on Enter, not as typed.
    LogPath,
    /// The author git narrows the log to; read on Enter, not as typed.
    LogAuthor,
    /// Narrows the shown review queue.
    ReviewFilter,
    /// Narrows the Worktrees popup's list.
    WorktreesFilter,
    /// Narrows the Command log popup.
    CommandLogFilter,
}

impl InputTarget {
    /// What the bar draws before the typed text.
    pub fn prompt(self) -> &'static str {
        match self {
            InputTarget::FilesFilter
            | InputTarget::CommitsFilter
            | InputTarget::BranchesFilter
            | InputTarget::RemotesFilter
            | InputTarget::TagsFilter
            | InputTarget::ReflogFilter
            | InputTarget::CommitFilesFilter
            | InputTarget::StashFilter
            | InputTarget::DiffSearch
            | InputTarget::PreviewSearch
            | InputTarget::BlameSearch
            | InputTarget::BlameCommitSearch
            | InputTarget::ReviewFilter
            | InputTarget::WorktreesFilter
            | InputTarget::CommandLogFilter => "/",
            InputTarget::LogPath => "path: ",
            InputTarget::LogAuthor => "author: ",
        }
    }

    /// Whether git runs the query on Enter, rather than the list
    /// narrowing as each key is typed.
    pub fn is_log_query(self) -> bool {
        matches!(self, InputTarget::LogPath | InputTarget::LogAuthor)
    }
}

/// Which half of the log query an input sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFilterField {
    Path,
    Author,
}

impl LogFilterField {
    pub fn target(self) -> InputTarget {
        match self {
            LogFilterField::Path => InputTarget::LogPath,
            LogFilterField::Author => InputTarget::LogAuthor,
        }
    }
}

/// The open text input: where the text goes, and the text so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListInput {
    pub target: InputTarget,
    pub line: LineInput,
}

/// Whether `text` matches `query`: each whitespace-separated term must
/// appear in `text` as a subsequence. Case is ignored unless the query has
/// an uppercase char. Chars are compared one by one, never bytes, so
/// multi-byte text matches as it reads. A blank query matches everything.
pub fn matches(query: &str, text: &str) -> bool {
    let exact = query.chars().any(char::is_uppercase);
    query.split_whitespace().all(|term| {
        let mut rest = text.chars();
        term.chars().all(|q| rest.any(|t| same_char(q, t, exact)))
    })
}

/// `a` and `b` are the same char, ignoring case unless `exact`.
pub(crate) fn same_char(a: char, b: char, exact: bool) -> bool {
    a == b || (!exact && a.to_lowercase().eq(b.to_lowercase()))
}

/// The indexes of the `texts` that match `query`, in their order.
pub fn visible<'a>(query: &str, texts: impl Iterator<Item = &'a str>) -> Vec<usize> {
    texts
        .enumerate()
        .filter(|(_, text)| matches(query, text))
        .map(|(i, _)| i)
        .collect()
}

/// Where a cursor on `current` lands after moving `delta` rows among the
/// `visible` ones, stopping at the ends. A cursor on a hidden row goes to
/// the first visible one; `None` when none is.
pub fn step(visible: &[usize], current: usize, delta: isize) -> Option<usize> {
    match visible.iter().position(|&i| i == current) {
        Some(at) => Some(visible[at.saturating_add_signed(delta).min(visible.len() - 1)]),
        None => visible.first().copied(),
    }
}

/// The visible indexes from `anchor` to `cursor`, inclusive, either way
/// round. A row the filter hides is never part of a range, and the range is
/// empty while either end is hidden.
pub fn range(visible: &[usize], anchor: usize, cursor: usize) -> Vec<usize> {
    let at = |index| visible.iter().position(|&i| i == index);
    match (at(anchor), at(cursor)) {
        (Some(a), Some(c)) => visible[a.min(c)..=a.max(c)].to_vec(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_matches_as_a_subsequence() {
        assert!(matches("mod", "src/model.rs"));
        assert!(matches("smrs", "src/model.rs"));
        assert!(!matches("dom", "src/model.rs"), "order matters");
    }

    #[test]
    fn every_term_must_match() {
        assert!(matches("src rs", "src/model.rs"));
        assert!(!matches("src toml", "src/model.rs"));
    }

    #[test]
    fn case_is_ignored_until_the_query_has_an_uppercase_char() {
        assert!(matches("readme", "README.md"));
        assert!(matches("READ", "README.md"));
        assert!(!matches("Readme", "README.md"));
        assert!(matches("Readme", "src/Readme.md"));
    }

    #[test]
    fn a_blank_query_matches_everything() {
        assert!(matches("", "anything"));
        assert!(matches("  ", ""));
    }

    #[test]
    fn multi_byte_text_matches_by_char() {
        assert!(!matches("cafe", "café.rs"), "é is not e");
        assert!(matches("café", "src/café.rs"));
        assert!(matches("CAFÉ", "CAFÉ.rs"));
        assert!(matches("É", "École.md"));
        assert!(matches("é", "École.md"), "lowercase é matches É");
        assert!(matches("日本", "docs/日本語.md"));
        assert!(matches("日語", "docs/日本語.md"));
        assert!(!matches("語日", "docs/日本語.md"));
    }

    #[test]
    fn no_match_and_order_kept() {
        let texts = ["b.rs", "a.toml", "c.rs", "d.md"];
        assert_eq!(visible("rs", texts.iter().copied()), [0, 2]);
        assert_eq!(visible("zzz", texts.iter().copied()), Vec::<usize>::new());
        assert_eq!(visible("", texts.iter().copied()), [0, 1, 2, 3]);
    }

    #[test]
    fn a_range_runs_either_way_over_visible_rows_only() {
        let visible = [0, 2, 3, 5, 8];
        assert_eq!(range(&visible, 2, 5), [2, 3, 5]);
        assert_eq!(range(&visible, 5, 2), [2, 3, 5]);
        assert_eq!(range(&visible, 3, 3), [3]);
    }

    #[test]
    fn a_step_skips_hidden_rows_and_stops_at_the_ends() {
        let visible = [1, 4, 6];
        assert_eq!(step(&visible, 1, 1), Some(4));
        assert_eq!(step(&visible, 6, 1), Some(6));
        assert_eq!(step(&visible, 4, -5), Some(1));
        assert_eq!(step(&visible, 3, 1), Some(1), "from a hidden row");
        assert_eq!(step(&[], 3, 1), None);
    }

    #[test]
    fn a_range_with_a_hidden_end_is_empty() {
        let visible = [0, 2, 3];
        assert!(range(&visible, 1, 3).is_empty());
        assert!(range(&visible, 0, 4).is_empty());
    }
}
