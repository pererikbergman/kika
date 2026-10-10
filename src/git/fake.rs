//! A scripted [`GitRunner`] for tests: each call must match the next expected
//! argv, and gets the canned output back.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::sync::Mutex;

use super::runner::{GitCommand, GitOutput, GitRunner};

#[derive(Default)]
pub struct FakeRunner {
    expected: Mutex<VecDeque<(Vec<String>, GitOutput)>>,
    envs: Mutex<Vec<Vec<(OsString, OsString)>>>,
}

impl FakeRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Expect a call with exactly `argv`; answer it with `output`.
    pub fn expect(self, argv: &[&str], output: GitOutput) -> Self {
        let argv = argv.iter().map(|s| s.to_string()).collect();
        self.expected.lock().unwrap().push_back((argv, output));
        self
    }

    /// Environment of every call so far, in order.
    pub fn envs(&self) -> Vec<Vec<(OsString, OsString)>> {
        self.envs.lock().unwrap().clone()
    }

    pub fn assert_done(&self) {
        let left = self.expected.lock().unwrap();
        assert!(left.is_empty(), "expected git calls never made: {left:?}");
    }
}

impl GitRunner for FakeRunner {
    fn run(&self, cmd: &GitCommand) -> io::Result<GitOutput> {
        let actual: Vec<String> = cmd
            .args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let (expected, output) = self
            .expected
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected git call: {actual:?}"));
        assert_eq!(actual, expected, "git argv mismatch");
        self.envs.lock().unwrap().push(cmd.envs().to_vec());
        Ok(output)
    }
}

/// A successful run that printed `stdout`.
pub fn ok(stdout: &[u8]) -> GitOutput {
    GitOutput {
        success: true,
        stdout: stdout.to_vec(),
        stderr: Vec::new(),
    }
}

/// A failed run that printed `stderr`.
pub fn fail(stderr: &str) -> GitOutput {
    GitOutput {
        success: false,
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    }
}
