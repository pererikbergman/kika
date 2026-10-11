//! Everything that talks to Git. Commands go through a [`GitRunner`] so tests
//! can swap the real `git` binary for a fake.

pub mod askpass;
pub mod attr;
pub mod blame;
pub mod branches;
pub mod commit_files;
pub mod diff;
#[cfg(test)]
pub mod fake;
pub mod fetch;
pub mod head;
mod joiner;
pub mod log;
pub mod ops;
pub mod patch;
pub mod path;
pub mod prompts;
pub mod rebase_plan;
pub mod refs;
pub mod remote;
pub mod remotes;
pub mod repo_root;
pub mod repo_state;
pub mod retry;
pub mod runner;
pub mod status;
pub mod submodule;
pub mod version;
pub mod worktrees;

pub use runner::{CliRunner, GitRunner};
pub use status::FileEntry;
