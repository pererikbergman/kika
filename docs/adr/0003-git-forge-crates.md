# ADR-0003: The git and forge plumbing as workspace crates

- Status: Proposed
- Date: 2026-10-14
- Scope: where `src/git` and `src/forge` live and what they may import. Behaviour, key bindings, binding ids and order, intents and the config format do not change.

## Context

ADR-0001 ("noupling") named a possible follow-up: move `src/git` and `src/forge` out of the `kika` binary crate. Features import both trees, and noupling scores every import between sibling directories, so the informational health score fell to 4.9 when the refactor closed. It is 10.7 on main (dd37be7), measured with noupling 0.9.2. Most of the lost points come from `src/features -> src/git` and `src/features -> src/forge`.

In separate crates, two things change. noupling treats an import of another crate as external, so those imports stop scoring. And Cargo enforces the direction: a binary crate cannot be a dependency, so the plumbing can never import the app.

To move, `src/git` and `src/forge` must first stop importing the rest of the main crate. Measured on dd37be7 with `grep -rnE 'crate::' src/git src/forge | grep -vE 'crate::(git|forge)\b'`:

| Dependency | Where | How it is removed |
|---|---|---|
| `crate::program::command` | `git/runner.rs`, `forge/github.rs` | `program.rs` moves to `src/git/program.rs`. Its decoy and spawn-scan tests use features and forge, so they move up into `src/spawn_guard.rs`. |
| `crate::sequence_editor::PLAN_ENV` | `git/runner.rs`, `forge/github.rs`, tests in `git/runner.rs` and `git/command_line.rs` | Defined in `src/git/rebase_plan.rs`, which already owns the plan encoding. No re-export: the callers name the new path. |
| `crate::op::{GitOp, op_command}` (tests only) | `git/command_line.rs` `no_command_kika_builds_shows_a_secret`, `git/runner.rs` `every_command_kind_runs_in_the_workdir` | Both tests move into `src/op.rs`'s `mod tests` with their assertions unchanged. The runner test reaches the private `CliRunner::command` through a test-only `CliRunner::test_process`, which only calls it. |
| `crate::test_support::{STANDARD_ENV, env_list}` (tests only) | 17 use sites in 15 git test modules | Both move into `src/git/fake.rs`. `test_support.rs` re-exports them, so main-crate callers do not change. |
| `git/env_guard.rs` | reads `src/` and names main-crate files | It guards the whole tree, so it moves to `src/env_guard.rs`. |
| `forge::review::tests::{with_title, hostile_detail}` | used by `features/review/view.rs` | They move, with `HOSTILE`, into `forge::review::fixtures`. |
| `git/fixtures/reflog/*.bin` | used only by main-crate files | They move to `src/features/commits/reflog/fixtures/`. |
| `worktree_file::read_file` | nowhere in `src/git` or `src/forge` | No work. Only `sequence_editor.rs`, `features/worktrees/trust.rs` and `features/commits/rewrite/effects.rs` use it, and they stay in the main crate. |
| `Effect` / `AppIntent` in forge | nowhere | No work: #381 inverted it. |
| `safe_text`, `stop_work`, `error`, `ForgeContext` | already inside `src/git` or `src/forge` | Nothing moves. |

A trap, measured: if `src/git` imports `crate::forge` even once, noupling adds `cycle:src/forge -> src/git -> src/forge`. So the decoy test, which uses the forge runner, cannot stay in `src/git/program.rs`.

## Decision (proposed)

### Two steps

1. **The moves down, in one crate.** Every row of the table above. After it, `grep -rnE 'crate::' src/git src/forge | grep -vE 'crate::(git|forge)\b'` and `grep -rn 'crate::forge' src/git` print nothing.
2. **The workspace crates.** The two trees become member crates. This step is mechanical once step 1 holds.

### Crates and direction

Proposed names and layout (see "Open questions"):

```
Cargo.toml                 [workspace] members = ["crates/kika-git", "crates/kika-forge"]; the kika binary
crates/kika-git/           lib kika_git: today's src/git plus program.rs
crates/kika-forge/         lib kika_forge: today's src/forge
src/                       the kika binary, depends on both
```

- `kika-git` depends on no Kika crate. Its third-party deps are `ratatui` (only for `Span::width` in `diff.rs`, so emoji widths stay exactly as drawn) and `libc` on unix, both already in the tree.
- `kika-forge` depends on `kika-git`, `serde` and `serde_json`.
- `kika` depends on both. No crate depends on `kika`, and Cargo cannot let one.
- No new third-party dependency. `Cargo.lock` gains only the two path packages.

### The `test-support` feature

Each crate has a `test-support` cargo feature; `kika-forge`'s turns on `kika-git/test-support`. It exposes the test builders the main crate's tests use: `fake`, and every `#[cfg(test)]` item outside a `mod tests` that the main crate or forge tests use. Those items become `#[cfg(any(test, feature = "test-support"))]`. Only `[dev-dependencies]` turn the feature on, so the release binary does not change.

### Imports

Rewritten explicitly, with no alias: in `kika`, `crate::git::` becomes `kika_git::` and `crate::forge::` becomes `kika_forge::`; inside `kika-git`, `crate::git::` becomes `crate::`; inside `kika-forge`, `crate::forge::` becomes `crate::` and `crate::git::` becomes `kika_git::`. Items the main crate uses go from `pub(crate)` to `pub`.

### Tests

Unit tests stay next to their code, inside each crate. A test that needs a `kika` type (`AppState`, `op`, a feature) stays in the main crate. Two guards read or run the whole tree, so they live in the main crate as `#[cfg(test)]` modules:
- `src/spawn_guard.rs`: the scan that no production code builds a process by hand, and the run of each spawn site from a directory of decoys. `handoff.rs` and `effect.rs` ask it `in_decoy_child`.
- `src/env_guard.rs`: the scan that no builder sets a standard git env var by hand.

In step 2 both walk `src/` and `crates/*/src/`. Their skip and allow lists name the moved files. Those are changes to scanned paths and test names, not to assertions.

No assertion changes in either step. The test count stays the same (3048 on dd37be7); only the moved tests' paths change.

### noupling

Settings stay `strict` with no layers. In step 2 the two `dependency_rules` from ADR-0001 (`src/git/** -> src/features/**` and `src/forge/** -> src/features/**`) are deleted: they no longer match any file, and Cargo enforces the direction. noupling does not resolve imports between crates, so a rule pointed at `crates/**` would never fire. It still checks the imports inside each crate.

Measured with noupling 0.9.2 on copies of the tree:

| Tree | Health score | Issues | Fingerprint change |
|---|---:|---:|---|
| main (dd37be7) | 10.7 | 19 | none |
| Step 1, the moves down | 11.2 | 19 | none |
| Step 2, the workspace crates | 99.1 | 12 | see below |

Expected fingerprint diff for step 2:
- Gone: `coupling_violation:src/e2e -> src/forge`, `coupling_violation:src/e2e -> src/git`, `coupling_violation:src/features -> src/forge`, `coupling_violation:src/features -> src/git`, `coupling_violation:src/forge -> src/git`, `red_flag:fused_sibling:src/features -> src/git`, `zone_flag:src/forge`, `zone_flag:src/git`.
- New: `zone_flag:src/features/custom_commands`. It does not score; noupling's median moved.

Never allowed, as in ADR-0001: any `cycle:*` or `rule_violation:*`.

## Open questions

The owner has not answered these yet. The decision above assumes the first option in each.

1. Crate names and place: `kika-git` and `kika-forge` under `crates/`. Other names are possible.
2. Explicit `kika_git::` paths (about 1,200 rewrites in 192 files), rather than a `use kika_git as git;` alias at the crate root. Both score 99.1. Explicit keeps the boundary visible at each use.
3. `kika-git` keeps its `ratatui` dependency for one width call in `diff.rs`. The alternative, a direct `unicode-width` dependency, would be a new third-party dependency.
4. The noupling deny rules are deleted rather than re-pointed, because a rule across crates would never fire.
5. Two PRs, one per step. If the owner wants one PR, the steps merge in this order.
6. ADR-0001's "possible follow-up" sentence stays as it is. This ADR supersedes it once accepted.

## Consequences

Good:
- The compiler, not a grep, keeps the plumbing from importing the app.
- The health score goes from about 11 to about 99, because feature-to-plumbing imports stop scoring.
- `src/git` and `src/forge` no longer depend on any other main-crate file after step 1.

Bad:
- Step 2 is a wide diff: every `crate::git::` and `crate::forge::` path in the main crate changes.
- Test-only builders need a cargo feature to cross the crate line.
- The workspace commands (`--workspace`, `fmt --all`) replace the plain ones in CI and in `factory/project.md`.
