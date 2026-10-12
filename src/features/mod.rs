//! Feature folders: one folder per feature, holding its state, update handlers,
//! key bindings, views and effects, around the thin MVI core in the top-level
//! files of `src/`. See `docs/adr/0001-vertical-slices.md`.
//!
//! A feature folder uses these files, each only when it has something to put there:
//!
//! | File | Holds |
//! |---|---|
//! | `mod.rs` | Module docs, and the feature's `pub(crate)` API: its state types and its `AppState` query methods |
//! | `state.rs` (or a named file such as `list.rs`) | The feature's state types, which `AppState` holds as fields |
//! | `update.rs` | `impl AppState` handlers that `update()` calls. They stay pure. |
//! | `keys.rs` | The feature's binding chunks and the guard predicates they use |
//! | `view.rs` | Its render functions. They read `&AppState` only. |
//! | `effects.rs` | The I/O behind its `Effect` variants: free functions that `Runtime::run` calls |
//! | `git.rs` | git reads and `GitOp` argv builders that only this feature uses |
//!
//! Dependency rules:
//!
//! | From | May import | Must not import |
//! |---|---|---|
//! | core (`src/*.rs`) | anything: it is the composition root | none |
//! | a feature | core; `src/git`; `src/forge`; its own subtree; its parent features | a sibling feature, or anything inside one |
//! | `src/git`, `src/forge` | the core files they already use | any feature |
//! | `src/e2e` | anything | none |
//!
//! 1. Something two features both need goes in their nearest common parent
//!    feature, or in core if they have none.
//! 2. A feature never calls another feature's behaviour directly. It returns an
//!    `Effect`, or the core refresh plan (`refresh.rs`) reloads what an operation
//!    changed.
//! 3. A feature reads another feature's state only through that feature's
//!    `pub(crate)` query methods on `AppState`. Direct field reads kept by the
//!    move-only slices are listed in each feature's `mod.rs`.
//! 4. noupling only sees `use` items, not inline `crate::features::...` paths or
//!    `AppState` method calls. So every slice also runs this check, which must
//!    print nothing (hits that point at a parent feature are allowed):
//!    `grep -rnE 'crate::features::' src/features/<f> | grep -v 'crate::features::<f>\b'`

pub(crate) mod blame;
pub(crate) mod command_log;
pub(crate) mod conflicts;
pub(crate) mod custom_commands;
pub(crate) mod operation;
pub(crate) mod repos;
pub(crate) mod submodules;
pub(crate) mod worktrees;
