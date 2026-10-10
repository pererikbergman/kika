//! Everything that talks to Git. Commands go through a [`GitRunner`] so tests
//! can swap the real `git` binary for a fake.

#[cfg(test)]
pub mod fake;
pub mod runner;
pub mod status;

pub use runner::{CliRunner, GitRunner};
pub use status::FileEntry;
