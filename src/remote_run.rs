//! A fetch, pull or push in progress, as the screen shows it: a spinner on
//! the branch it runs for and what it is doing in the status bar.

use crate::git::branches::BranchName;
use crate::git::remote::RemoteOp;

/// The spinner's frames, one per tick.
pub const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRun {
    pub op: RemoteOp,
    /// The effect's sequence, which also holds `op_seq`.
    pub seq: u64,
    /// The branch `HEAD` was on when it started; its row gets the spinner.
    pub branch: Option<BranchName>,
    /// Counts spinner ticks; the frame drawn is this modulo the frames.
    pub frame: u8,
    /// What ssh last said that needed no answer, such as "touch your key".
    pub notice: Option<String>,
}

impl RemoteRun {
    pub fn new(op: RemoteOp, seq: u64, branch: Option<BranchName>) -> Self {
        Self {
            op,
            seq,
            branch,
            frame: 0,
            notice: None,
        }
    }

    pub fn tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    /// The spinner frame to draw now.
    pub fn spinner(&self) -> &'static str {
        SPINNER[usize::from(self.frame) % SPINNER.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_tick_shows_the_next_frame_and_wraps() {
        let mut run = RemoteRun::new(RemoteOp::Fetch, 1, None);
        let mut seen = Vec::new();
        for _ in 0..5 {
            seen.push(run.spinner());
            run.tick();
        }
        assert_eq!(seen, ["|", "/", "-", "\\", "|"]);
        run.frame = u8::MAX;
        run.tick();
        assert_eq!(run.spinner(), "|");
    }
}
