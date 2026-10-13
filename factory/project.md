# Project context (read this first)

Every factory agent reads this file before doing anything else.

## What Kika is
A terminal Git client and PR/MR review tool, written in Rust. The full design is in
`docs/tech-spec.md`. Read it when a job touches architecture, git access or forge
integration.

## Language and toolchain
- Rust, edition 2024 (`Cargo.toml`). Toolchain 1.85 or newer.
- UI: `ratatui` 0.30 + `crossterm` 0.29. Async: `tokio`. CLI: `clap` (derive).
  Errors: `color-eyre` (`color_eyre::Result` at the edges).

## Commands
| Purpose | Command |
|---|---|
| Build | `cargo build` |
| Test | `cargo test` |
| Lint | `cargo clippy --all-targets -- -D warnings` |
| Format check | `cargo fmt --check` |

A change is only done when all four pass.

## Architecture: MVI core with feature folders
The layout is decided in `docs/adr/0001-vertical-slices.md` (ADR-0001). Core is the
top-level files of `src/`; each feature lives in its own folder under `src/features/`.

Core files:

| File | Role | Rule |
|---|---|---|
| `src/main.rs`, `src/app.rs` | Loop | draw → wait for intent on the `mpsc` channel → update. |
| `src/cli.rs` | CLI | `clap` commands map to a starting `Context`. |
| `src/intent.rs` | Intent | `AppIntent`, one flat enum with variants grouped by feature. Raw terminal or task events map to intents here. |
| `src/model.rs` | Model | `AppState` (composes feature state) + `update(intent)`, one `match` whose arms call feature handlers. Pure: no I/O, no rendering. |
| `src/effect.rs` | Effects | `Effect`, one flat enum, and the `Runtime::run` dispatch that calls each feature's `effects.rs`. |
| `src/keymap.rs` | Keys | Key engine, shared guards, the ordered `CHUNKS` list and `registry()`. Binding order is behaviour. |
| `src/view.rs`, `src/diff_pane_view.rs` (the Diff pane's drawing and search marks) and the shared widgets (`layout.rs`, `popup*.rs`, `menu.rs`, `pick_list.rs`, `list_filter.rs`, ...) | View | `render(&AppState, &mut Frame)` composes the frame. Reads state only. |
| `src/op.rs`, `src/prompts.rs` | Op slot | `GitOp`, `op_command` and `run_op`; whether an op needs the terminal. Feature `git.rs` files hold the argv builders the arms call. |
| `src/refresh.rs`, `src/reload.rs`, `src/watch.rs` | Refresh | The refresh plan from each `GitOp` to the data it makes stale, the reload executor (`apply_plan`, `reload_repo`) that turns a plan into each feature's loads, and the watch for outside changes. |
| `src/handoff.rs` | Hand-off | Blocking work that needs the real terminal (editor, attached ops, commands). The loop suspends the screen around it. |
| `src/list_input.rs`, `src/filtered_list.rs` | List input | List filters, ranges, pane search and Esc in a list, shared by every list. `FilteredList` owns one list's rows, cursor, filter, range and load in flight. |
| `src/bisect.rs`, `src/diff_pane.rs`, `src/hunk_view.rs`, `src/diff_search.rs`, `src/diff_mark.rs`, `src/range_delete.rs`, `src/remote_run.rs`, `src/model/confirm_guard.rs` | Shared state | State and guards that two or more features use. Each one's reason is in ADR-0001's Outcome. |
| `src/config.rs`, `src/config_docs.rs`, `src/clipboard.rs`, `src/program.rs`, `src/sequence_editor.rs`, `src/worktree_file.rs` | Plumbing | Config loading and its doc test, the clipboard, program lookup, Kika as git's sequence editor, and worktree file I/O. |
| `src/test_support.rs` | Tests | Shared `#[cfg(test)]` builders that tests in more than one area use. A helper only one feature's tests use stays with that feature. |

Feature-folder files (each only when the feature has something to put there):

| File | Holds |
|---|---|
| `mod.rs` | Module docs, and the feature's `pub(crate)` API: its state types and its `AppState` query methods |
| `state.rs` (or a named file such as `list.rs`) | The feature's state types, which `AppState` holds as fields |
| `update.rs` | `impl AppState` handlers that `update()` calls. Pure. |
| `keys.rs` | The feature's binding chunks and the guard predicates they use |
| `view.rs` | Its render functions. They read `&AppState` only. |
| `effects.rs` | The I/O behind its `Effect` variants: free functions that `Runtime::run` calls |
| `git.rs` | git reads and `GitOp` argv builders that only this feature uses |

Dependency rules:

| From | May import | Must not import |
|---|---|---|
| core (`src/*.rs`) | anything: it is the composition root | none |
| a feature | core; `src/git`; `src/forge`; its own subtree; its parent features (for example `commits/reflog` may import `commits`) | a sibling feature, or anything inside one |
| `src/git`, `src/forge` | the core files they already use | any feature |
| `src/e2e` | anything | none |

1. Something two features both need goes in their nearest common parent feature, or
   in core if they have none.
2. A feature never calls another feature's behaviour directly. It returns an `Effect`,
   or the core refresh plan (`refresh.rs`, run by `reload.rs`) reloads what an operation changed.
3. A feature reads another feature's state only through that feature's `pub(crate)`
   query methods on `AppState`. Direct field reads kept by the move-only slices are
   listed in the feature's `mod.rs`; new code follows the rule.
4. noupling only sees `use` items. Every slice also runs this check, which must print
   nothing (hits that point at a parent feature are allowed):
   `grep -rnE 'crate::features::' src/features/<f> | grep -v 'crate::features::<f>\b'`

The known exceptions to rule 2 (three direct calls to a sibling feature's `AppState`
methods) are listed in ADR-0001's Outcome section and in each caller's `mod.rs`.

MVI purity:
- `update` is pure and returns effects. Git, network and filesystem work runs in tasks
  or `effects.rs` functions that send an `AppIntent` back. Nothing in `update` or
  `render` blocks.
- Views only read state.
- Git access: the `git` CLI is the default (argv only, never shell strings);
  `git2` only for measured hot reads. See "Repository and Version Control" in the spec.

## Conventions
- Unit tests live next to the code in `#[cfg(test)] mod tests`.
- `update` is tested by applying intents to a state and asserting on fields.
- Views are tested by drawing into `ratatui::backend::TestBackend` and reading the buffer.
- Doc comments on public items say *why*, briefly. Match the existing comment density.
- Keep modules small; one concern per file.
- Never name other git clients (as precedent, comparison or otherwise) in code, comments,
  tests, commit messages, specs or reviews. Describe behaviour on its own terms.
- Security defaults for git commands: put `--` before paths, set `GIT_LITERAL_PATHSPECS=1`
  on any command that takes repo file names, and build argv from raw path bytes
  (`RepoPath`), never from the lossy display string.
