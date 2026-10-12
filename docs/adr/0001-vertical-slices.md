# ADR-0001: Feature folders (vertical slices) over a thin MVI core

- Status: Accepted
- Date: 2026-10-12
- Scope: the layout of `src/`. Behaviour, key bindings, config format and MVI rules do not change.

## Context

Kika is split by layer. Every feature lives partly in each of five hub files:

| File | Lines | Code | Tests | What a feature puts there |
|---|---:|---:|---:|---|
| `src/model.rs` | 21,513 | 6,296 | 15,138 | `AppState` fields, an `update` arm, `impl AppState` handlers, tests |
| `src/keymap.rs` | 11,583 | 6,463 | 5,087 | 349 bindings in one `REGISTRY` (lines 2711-6544), ~1,550 lines of guard predicates |
| `src/view.rs` | 6,727 | 2,263 | 4,429 | panel renderers and tests |
| `src/effect.rs` | 5,023 | 1,898 | 3,064 | an `Effect` variant and its `Runtime::run` arm |
| `src/git/ops.rs` | 4,938 | 1,309 | 3,606 | a `GitOp` variant, its argv builder, tests |

On top of that, each feature has side files: about 60 top-level `*_view.rs` and state files, plus 11 files in `src/model/`. One feature change touches 5 to 8 files. The factory runs three jobs at once, and they collide in the same hubs. There are 2,817 tests, and 642 of them sit in one `mod tests` in `model.rs` that shares 210 helper functions.

## Decision

### Layout

```
src/
  main.rs app.rs cli.rs           core: loop, CLI
  model.rs                        core: AppState (composes feature state), update() dispatch
  intent.rs effect.rs             core: AppIntent, Effect, Runtime dispatch
  op.rs prompts.rs                core: GitOp, op_command dispatch, terminal decision (moved from src/git/ops.rs, src/git/prompts.rs)
  keymap.rs                       core: key engine, shared guards, the ordered chunk list, registry()
  view.rs layout.rs popup*.rs menu.rs rows.rs pick_list.rs list_filter.rs list_input.rs ...
                                  core: frame composition and shared widgets
  test_support.rs                 core: shared #[cfg(test)] builders
  features/
    mod.rs                        the rules below, as module docs
    files/  diff/{renderers/}  commit/  stash/  submodules/  worktrees/  repos/
    command_log/  custom_commands/  blame/  conflicts/  sync/  operation/
    pull_requests/  review/
    branches/{remotes/, tags/}
    commits/{reflog/, rewrite/, copy/, custom_patch/, bisect/, compare/}
  git/                            shared git plumbing (unchanged role)
  forge/                          shared forge plumbing (unchanged role)
  e2e/                            end-to-end tests (unchanged)
```

Core is the top-level files of `src/`, not a `src/core/` directory. This is measured, not a matter of taste. noupling does not flag imports between a parent directory and its children. It does flag imports between sibling directories, and two sibling directories that import each other count as a scored cycle. Features need `AppState`, `AppIntent` and `Effect`. Core needs feature state types. So `src/core/` next to `src/features/` would be a permanent `cycle:src/core -> src/features -> src/core`.

A feature folder uses these files, each only when it has something to put there:

| File | Holds |
|---|---|
| `mod.rs` | Module docs, and the feature's `pub(crate)` API: its state types and its `AppState` query methods |
| `state.rs` (or a named file such as `list.rs`) | The feature's state types, which `AppState` holds as fields |
| `update.rs` | `impl AppState { pub(crate) fn ... }` handlers that `update()` calls. They are pure, as now. |
| `keys.rs` | The feature's binding chunks (`pub(crate) static ...: &[Binding]`) and the guard predicates they use |
| `view.rs` | Its render functions. They read `&AppState` only. |
| `effects.rs` | The I/O behind its `Effect` variants: free functions that `Runtime::run` calls |
| `git.rs` | git reads and `GitOp` argv builders that only this feature uses |

Tests go next to the code they test, in `#[cfg(test)] mod tests`, as the project conventions require.

MVI does not change. `update(state, intent)` stays pure and returns effects. I/O happens only in `effects.rs` functions that `Runtime` calls. Views only read.

### Dependency rules

| From | May import | Must not import |
|---|---|---|
| core (`src/*.rs`) | anything: it is the composition root | none |
| a feature | core; `src/git`; `src/forge`; its own subtree; its parent features (for example `commits/reflog` may import `commits`) | a sibling feature, or anything inside one (for example `stash` importing `commits`, or `commits/rewrite` importing `commits/copy`) |
| `src/git`, `src/forge` | core files they already use (`program.rs`, `sequence_editor.rs`, `worktree_file.rs`, `effect.rs`, `intent.rs`) | any feature |
| `src/e2e` | anything | none |

1. **Something two features both need** goes in the nearest common parent feature, or in core if they have none. Examples:
   - `with_stacked_branches` is used by `commits/rewrite` and `commits/custom_patch`, so it goes in `commits`.
   - `discard_disabled` is used by `files` and `diff`, so it goes in core.
   - `commit_file_line` is used by the `stash` and `commits` views, so it goes in `rows.rs`.
2. **A feature never calls another feature's behaviour directly.** It returns an `Effect`, or the core refresh plan (`refresh.rs`) reloads what an operation changed.
3. **Reading another feature's state** goes through that feature's `pub(crate)` query methods on `AppState`, such as `selected_file()`, `selected_commit()` or `local_branch_selected()`. The issues below are move-only, so direct field reads that exist today are kept as they are and listed in `slices.md`. New code follows this rule.
4. **noupling only sees `use` items.** It misses fully qualified paths written inline (`crate::features::x::f()`) and calls to `AppState` methods. So every slice PR also runs this check, which must print nothing:
   `grep -rnE 'crate::features::' src/features/<f> | grep -v 'crate::features::<f>\b'`
   Hits that point at a parent feature are allowed.

### Shared git plumbing

`src/git` keeps every module that more than one feature imports, or that other plumbing or `src/forge` imports:
- runner, fake, path, command_line, recording, retry, askpass
- diff, patch, emoji, joiner
- status, repo_state, repo_root, refs, head, version
- log, rebase_plan, commit_files, branches, remote, remotes, tags, submodule, bisect, blame, forge_remote

A git module moves into a feature as `git.rs` only when that feature and core are its only importers:

| git module | Moves into |
|---|---|
| attr | `conflicts` |
| comment_prefix | `commit` |
| branch_tips | `commits` |
| reflog, undo | `commits/reflog` |
| fixup_base, left_behind | `commits/rewrite` |
| cherry_pick | `commits/copy` |
| custom_patch | `commits/custom_patch` |
| fetch | `sync` |
| stash | `stash` |
| worktrees, worktree_trust | `worktrees` |
| pr_objects | `review` |

`GitOp` leaves `src/git` for core `src/op.rs`. It names feature types such as `CustomPatch`, `StashRef` and `PatchAction`, and the one-op-at-a-time slot (`op_seq`) is a core concept. If it stayed in `src/git`, then `git -> features` imports would form a cycle. `git/prompts.rs` goes with it, as top-level `src/prompts.rs`: it inspects a `GitOp` to decide whether git needs the terminal. `op.rs` keeps the `GitOp` enum, `op_command` and `run_op`. The match arms call `features/<f>/git.rs` builders, such as the stash `store_command` and `rename_stash`.

### How the keymap registry is composed

Binding order is behaviour, within a scope:
- `resolve` takes the first match in the focused scope.
- `available` lists bindings "in table order within a scope", and the help popup and bottom bar are drawn from it.
- `Keymap::build` walks the registry to report clashes.

So:

1. `Binding` derives `Clone`. Every field already allows that.
2. The skeleton issue cuts today's `REGISTRY` into 79 contiguous chunks without reordering anything. A new chunk starts wherever the owning feature changes. Each chunk is named after its owner and scope. Core keeps `static CHUNKS: &[&[Binding]]` in exactly today's order.
3. `registry()` keeps its signature, `fn registry() -> &'static [Binding]`. It returns a slice of a `std::sync::LazyLock<Vec<Binding>>` that flattens `CHUNKS`. This is std only and needs no new dependency.
4. A golden test, `registry_order_is_pinned`, asserts that the full ordered id list equals the 349 ids on `main` today. Moving a feature moves its chunk definitions and changes only the paths in `CHUNKS`. A new binding is added to its chunk and to the pinned list in the same PR.
5. Guard predicates move with the chunk that uses them. Measured: only `never` and `no_branch_selected` are bound by more than one feature, and both stay in core. Five helper predicates are called by two features:
   - `commit_action_disabled` and `with_stacked_branches` go to the common parent, `commits`.
   - `branch_action_disabled`, `discard_disabled` and `open_disabled` stay in core.
6. `REPO_LOCKED`, `KeyPattern`, `KeyScope`, `Keymap`, `resolve`, `available`, `available_commands` and custom-command resolution stay in core `keymap.rs`.

### How `AppIntent` and `Effect` are composed

Both stay single flat enums in core (`intent.rs`, `effect.rs`), with their variants grouped by feature under section comments. Payload types are imported from the features, which is a downward import.
- `update()` stays the one `match`. An arm becomes a one-line call to a `pub(crate)` feature handler. Inline arms longer than three lines move into a handler.
- `Runtime::run` stays one `match`. Each arm calls a `features/<f>/effects.rs` function with the runner, the sender and the payload.

Nesting (`AppIntent::Stash(StashIntent)`) was rejected for now. The 349 bindings build `AppIntent` values in a `static`, and nesting would rewrite every binding, menu item and test for no behaviour gain. It can be revisited after the move.

### Config docs and registry tests

- `config_docs.rs` stays a core `#[cfg(test)]` module. It keeps reading `registry()` and `REPO_LOCKED`.
- Ids, default keys and descriptions do not change, so `docs/configuration.md` must not change in any slice PR.
- `config.rs` remap tests keep calling `registry()`.
- The new golden order test is added once, by the skeleton issue.

### noupling

Settings stay as they are: `strict`, no `layers`. Two deny rules are added to `dependency_rules`:
- `src/git/** -> src/features/**`, message "git plumbing never imports a feature (ADR-0001)"
- `src/forge/** -> src/features/**`, with the same message

Layers were tried with the pinned binary and rejected. A layer list is strictly top-to-bottom, so core↔feature imports become per-file-pair `layer_violation` fingerprints that churn on every PR.

Expected baseline changes. Each slice PR runs `noupling scan . && noupling baseline save .` and lists the fingerprint diff in its description.

| Allowed to appear | Allowed to disappear |
|---|---|
| `coupling_violation:src/features -> src/git` | `coupling_violation:src/model -> src/git` |
| `coupling_violation:src/features -> src/forge` | `coupling_violation:src/model -> src/forge` |
| `coupling_violation:src/e2e -> src/features` | `coupling_violation:src/e2e -> src/model` |
| `red_flag:fused_sibling:src/features -> src/git` | `red_flag:fused_sibling:src/model -> src/git` |
| `red_flag:fused_sibling:src/features -> src/forge` | |
| `gravity_well:*`, `zone_flag:src/features/*`, `low_cohesion:src/features/*` (noupling says these do not score) | `low_cohesion:src/model`, and the remaining `src/model` fingerprints once that directory is empty |

The forge red flag comes from noupling's median threshold moving as `src/model` sheds git imports, not from new forge imports.

Never allowed: any `cycle:*`; any `rule_violation:*`; `coupling_violation:src/features/<a> -> src/features/<b>` for sibling features; `coupling_violation:src/git -> src/features`; `coupling_violation:src/forge -> src/features`.

Measured with noupling 0.9.2 on copies of the tree:

| Tree | Health score | New fingerprints or cycles |
|---|---:|---|
| Today | 66.8 | none |
| `GitOp` moved to `src/op.rs` | 69.6 | none |
| Pilot slice (`command_log`) | 67.8 | one new fingerprint, `coupling_violation:src/features -> src/git` |
| Every planned file moved | 18.0 | no feature↔feature fingerprint, no cycle |

The score falls because files that today sit at the top of `src/` keep importing `src/git` and `src/forge` once they are under `src/features/`, and noupling scores every sibling import. The gate is baseline-only and the score never fails CI (see `docs/architecture-checks.md`). Moving `src/git` and `src/forge` into workspace crates would score about 99 in the same simulation. That would first need `program.rs`, `PLAN_ENV` and `worktree_file::read_file` moved down, and forge's use of `Effect`/`AppIntent` inverted. It is a possible follow-up and not part of this decision.

## Consequences

Good:
- A feature change touches one folder plus one-line edits in core (a `CHUNKS` path, an `update` arm, a `Runtime` arm, an `AppState` field). Parallel factory jobs stop colliding in the hubs.
- Tests sit next to the feature they cover.
- Each hub shrinks to composition. Expected: `model.rs` keeps about 1,700 lines of code (state, dispatch, op slot, focus) plus core tests; `keymap.rs` about 1,400 plus tests.
- Feature↔feature coupling becomes visible to the gate for `use` imports.

Bad:
- About 40 PRs of mechanical moves. Each one needs a baseline save, and `baseline.json` conflicts on every rebase. Rule: never merge it by hand; rerun `scan` and `baseline save`.
- The informational health score drops as described above.
- Handlers become `pub(crate)` rather than `pub(super)`, because core dispatch is no longer their parent module.
- Cross-feature calls to `AppState` methods and direct field reads stay invisible to noupling. Rule 3 and the grep check are the guard.
- `factory/project.md` must replace its "Architecture: strict MVI" file table with the core and feature layout and these rules. The skeleton issue does that.
