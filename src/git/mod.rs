//! Everything that talks to Git. Commands go through a [`GitRunner`] so tests
//! can swap the real `git` binary for a fake.

pub mod askpass;
pub mod attr;
pub mod bisect;
pub mod blame;
pub mod branch_tips;
pub mod branches;
pub mod cherry_pick;
pub mod command_line;
pub mod commit_files;
pub mod diff;
pub(crate) mod emoji;
#[cfg(test)]
pub mod fake;
pub mod fetch;
pub mod forge_remote;
pub mod head;
mod joiner;
pub mod log;
pub mod ops;
pub mod patch;
pub mod path;
pub mod prompts;
pub mod rebase_plan;
pub mod recording;
pub mod reflog;
pub mod refs;
pub mod remote;
pub mod remotes;
pub mod repo_root;
pub mod repo_state;
pub mod retry;
pub mod runner;
pub mod stash;
pub mod status;
pub mod submodule;
pub mod tags;
pub mod undo;
pub mod version;
pub mod worktrees;

pub use runner::{CliRunner, GitRunner};
pub use status::FileEntry;
