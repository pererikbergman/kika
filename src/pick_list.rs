//! A popup list to pick one row from, such as the remote a pull request
//! targets. Pure: the rows are fixed when it opens, so a reload while it
//! is open cannot change what a row means.

/// What the picked row is for, which also titles the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickPurpose {
    /// The remote whose repository a new pull request goes to.
    TargetRemote,
    /// The branch a new pull request merges into.
    BaseBranch,
    /// The failing check whose page opens in the browser.
    CheckDetails,
    /// How a pull request is merged; a question follows.
    MergeMethod,
    /// The whole pull request or one of its commits, for review.
    ReviewCommit,
    /// How a submitted review judges the pull request; its summary follows.
    ReviewEvent,
}

impl PickPurpose {
    pub fn title(self) -> &'static str {
        match self {
            PickPurpose::TargetRemote => "Target remote",
            PickPurpose::BaseBranch => "Base branch",
            PickPurpose::CheckDetails => "Check details",
            PickPurpose::MergeMethod => "Merge method",
            PickPurpose::ReviewCommit => "Review commit",
            PickPurpose::ReviewEvent => "Review type",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickList {
    pub purpose: PickPurpose,
    /// Each row's text, already terminal-safe.
    pub rows: Vec<String>,
    pub cursor: usize,
}

impl PickList {
    /// `rows` with the cursor on `cursor`, or the last row when past it.
    pub fn new(purpose: PickPurpose, rows: Vec<String>, cursor: usize) -> Self {
        let cursor = cursor.min(rows.len().saturating_sub(1));
        Self {
            purpose,
            rows,
            cursor,
        }
    }

    /// Move the cursor by `delta`, stopping at either end.
    pub fn move_cursor(&mut self, delta: isize) {
        self.cursor = self
            .cursor
            .saturating_add_signed(delta)
            .min(self.rows.len().saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cursor_starts_inside_the_rows_and_stops_at_either_end() {
        let rows = vec!["a".to_string(), "b".into(), "c".into()];
        let mut list = PickList::new(PickPurpose::BaseBranch, rows, 9);
        assert_eq!(list.cursor, 2);
        list.move_cursor(1);
        assert_eq!(list.cursor, 2);
        list.move_cursor(-5);
        assert_eq!(list.cursor, 0);
        assert_eq!(list.purpose.title(), "Base branch");
        assert_eq!(PickPurpose::TargetRemote.title(), "Target remote");
    }
}
