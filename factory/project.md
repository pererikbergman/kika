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

## Architecture: strict MVI
| File | Role | Rule |
|---|---|---|
| `src/intent.rs` | Intent | `AppIntent` enum. Raw terminal or task events map to intents here. |
| `src/model.rs` | Model | `AppState` + `update(intent)`. Pure: no I/O, no rendering. |
| `src/view.rs` | View | `render(&AppState, &mut Frame)`. Reads state only. |
| `src/app.rs` | Loop | draw → wait for intent on the `mpsc` channel → update. |
| `src/cli.rs` | CLI | `clap` commands map to a starting `Context`. |

- Git, network and filesystem work runs in tasks that send an `AppIntent` back.
  Nothing in `update` or `render` blocks.
- Git access: the `git` CLI is the default (argv only, never shell strings);
  `git2` only for measured hot reads. See "Repository and Version Control" in the spec.

## Conventions
- Unit tests live next to the code in `#[cfg(test)] mod tests`.
- `update` is tested by applying intents to a state and asserting on fields.
- Views are tested by drawing into `ratatui::backend::TestBackend` and reading the buffer.
- Doc comments on public items say *why*, briefly. Match the existing comment density.
- Keep modules small; one concern per file.
