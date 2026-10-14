//! The commit graph beside the log, worked out one row at a time as pages
//! arrive, so it never needs the whole history. Pure: the view turns the
//! cells into glyphs, and only for the rows on screen.

use kika_git::log::CommitRow;

/// Lanes kept open at most. A parent listed before its child (clock skew
/// in git's default order) leaves a lane waiting for a commit that already
/// went by; the cap keeps such lanes from piling up.
pub const MAX_LANES: usize = 32;

/// What one column of a row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphCell {
    /// The row's commit.
    Commit,
    /// A lane passing by, waiting for a later commit.
    Pipe,
    /// A lane this merge commit opens for one of its further parents.
    MergeIn,
    /// A lane that ends here: it was waiting for this commit, which another
    /// lane already holds, so the branch forked off here.
    BranchOut,
    Empty,
}

/// One row's columns, left to right.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphRow {
    pub cells: Vec<GraphCell>,
}

/// The lanes open after the rows pushed so far. Each holds the commit it
/// waits for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphBuilder {
    pub lanes: Vec<Option<String>>,
}

impl GraphBuilder {
    /// The graph row for `row`, the next commit of the log.
    pub fn push(&mut self, row: &CommitRow) -> GraphRow {
        let waiting = |lane: &Option<String>| lane.as_deref() == Some(row.oid.as_str());
        let col = match self.lanes.iter().position(waiting) {
            Some(col) => col,
            None => self.free_lane(),
        };
        let mut cells: Vec<GraphCell> = self
            .lanes
            .iter()
            .enumerate()
            .map(|(i, lane)| match lane {
                _ if i == col => GraphCell::Commit,
                Some(oid) if *oid == row.oid => GraphCell::BranchOut,
                Some(_) => GraphCell::Pipe,
                None => GraphCell::Empty,
            })
            .collect();
        for (i, cell) in cells.iter().enumerate() {
            if *cell == GraphCell::BranchOut {
                self.lanes[i] = None;
            }
        }
        self.lanes[col] = row.parents.first().cloned();
        for parent in row.parents.iter().skip(1) {
            let held = self
                .lanes
                .iter()
                .position(|lane| lane.as_deref() == Some(parent.as_str()));
            let lane = match held {
                Some(lane) => Some(lane),
                // A lane freed on this very row still shows its end here.
                None => match self
                    .lanes
                    .iter()
                    .zip(&cells)
                    .position(|(lane, cell)| lane.is_none() && *cell == GraphCell::Empty)
                {
                    Some(lane) => Some(lane),
                    None if self.lanes.len() < MAX_LANES => {
                        self.lanes.push(None);
                        cells.push(GraphCell::Empty);
                        Some(self.lanes.len() - 1)
                    }
                    None => None,
                },
            };
            if let Some(lane) = lane.filter(|&lane| lane != col) {
                self.lanes[lane] = Some(parent.clone());
                cells[lane] = GraphCell::MergeIn;
            }
        }
        while self.lanes.last().is_some_and(Option::is_none) {
            self.lanes.pop();
        }
        GraphRow { cells }
    }

    /// The first empty lane, or a new one. At the cap, the last lane is
    /// reused: its wait is dropped rather than the graph growing.
    fn free_lane(&mut self) -> usize {
        if let Some(free) = self.lanes.iter().position(Option::is_none) {
            return free;
        }
        if self.lanes.len() < MAX_LANES {
            self.lanes.push(None);
        }
        self.lanes.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use GraphCell::*;

    fn row(oid: &str, parents: &[&str]) -> CommitRow {
        CommitRow {
            oid: oid.into(),
            short_oid: oid.into(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            author: "a".into(),
            time: 0,
            subject: oid.into(),
            subject_altered: false,
        }
    }

    fn graph(rows: &[CommitRow]) -> (Vec<Vec<GraphCell>>, GraphBuilder) {
        let mut builder = GraphBuilder::default();
        let cells = rows.iter().map(|r| builder.push(r).cells).collect();
        (cells, builder)
    }

    #[test]
    fn a_linear_history_uses_one_lane() {
        let (cells, builder) = graph(&[row("c", &["b"]), row("b", &["a"]), row("a", &[])]);
        assert_eq!(cells, [vec![Commit], vec![Commit], vec![Commit]]);
        assert!(builder.lanes.is_empty());
    }

    #[test]
    fn a_merge_opens_a_second_lane_that_closes_at_the_fork_point() {
        // m merges s into b; both come from a.
        let (cells, builder) = graph(&[
            row("m", &["b", "s"]),
            row("s", &["a"]),
            row("b", &["a"]),
            row("a", &[]),
        ]);
        assert_eq!(
            cells,
            [
                vec![Commit, MergeIn],
                vec![Pipe, Commit],
                vec![Commit, Pipe],
                vec![Commit, BranchOut],
            ]
        );
        assert!(builder.lanes.is_empty());
    }

    #[test]
    fn a_lane_closed_on_a_row_is_not_reused_on_that_row() {
        // x is waited for by two lanes and is itself a merge.
        let (cells, _) = graph(&[
            row("m", &["x", "y"]),
            row("y", &["x"]),
            row("x", &["p", "q"]),
        ]);
        assert_eq!(cells[2], vec![Commit, BranchOut, MergeIn]);
    }

    #[test]
    fn an_octopus_merge_does_not_panic_and_closes_its_lanes() {
        let (cells, builder) = graph(&[
            row("o", &["a", "b", "c", "d"]),
            row("d", &["r"]),
            row("c", &["r"]),
            row("b", &["r"]),
            row("a", &["r"]),
            row("r", &[]),
        ]);
        assert_eq!(cells[0], vec![Commit, MergeIn, MergeIn, MergeIn]);
        assert_eq!(cells[5], vec![Commit, BranchOut, BranchOut, BranchOut]);
        assert!(builder.lanes.is_empty());
    }

    #[test]
    fn parents_seen_before_their_children_keep_the_lanes_bounded() {
        let mut builder = GraphBuilder::default();
        // Each parent is listed before its child, so every child's lane
        // waits for a commit that never comes again.
        for i in 0..1_000 {
            let parent = format!("p{i}");
            builder.push(&row(&parent, &[]));
            let child = format!("c{i}");
            builder.push(&row(&child, &[&parent, &format!("x{i}")]));
            assert!(builder.lanes.len() <= MAX_LANES);
        }
        let wide: Vec<String> = (0..100).map(|i| format!("w{i}")).collect();
        let parents: Vec<&str> = wide.iter().map(String::as_str).collect();
        let cells = builder.push(&row("octopus", &parents)).cells;
        assert!(cells.len() <= MAX_LANES);
        assert!(builder.lanes.len() <= MAX_LANES);
    }

    #[test]
    fn a_hundred_thousand_linear_rows_stay_on_one_lane() {
        let mut builder = GraphBuilder::default();
        let n = 100_000;
        for i in (0..n).rev() {
            let parent = if i == 0 {
                vec![]
            } else {
                vec![format!("{}", i - 1)]
            };
            let row = CommitRow {
                parents: parent,
                ..row(&i.to_string(), &[])
            };
            assert_eq!(builder.push(&row).cells, [Commit]);
            assert!(builder.lanes.len() <= 1);
        }
    }
}
