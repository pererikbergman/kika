//! Everything that talks to Git. Commands go through a [`GitRunner`] so tests
//! can swap the real `git` binary for a fake.

pub mod diff;
#[cfg(test)]
pub mod fake;
pub mod head;
pub mod ops;
pub mod patch;
pub mod path;
pub mod repo_root;
pub mod repo_state;
pub mod runner;
pub mod status;

pub use runner::{CliRunner, GitRunner};
pub use status::FileEntry;
