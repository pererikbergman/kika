//! Everything that talks to Git. Commands go through a [`GitRunner`] so tests
//! can swap the real `git` binary for a fake.

pub mod blame;
pub mod diff;
#[cfg(test)]
pub mod fake;
pub mod fetch;
pub mod head;
pub mod ops;
pub mod patch;
pub mod path;
pub mod prompts;
pub mod refs;
pub mod repo_root;
pub mod repo_state;
pub mod retry;
pub mod runner;
pub mod status;
pub mod submodule;
pub mod version;

pub use runner::{CliRunner, GitRunner};
pub use status::FileEntry;
