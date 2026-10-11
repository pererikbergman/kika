# Kika - Technical Specification

## Core Architecture
Kika uses a strict Model-View-Intent (MVI) architecture to isolate terminal rendering, Git index manipulation, and async network calls.

* **Model (State):** Centralized state machine managing local workspace data and remote review contexts.
* **Intent (Input):** User actions and network events map to strongly typed intents handled by the message bus.
* **View (Render):** Deep modules that translate state into terminal UI without executing business logic.

## Technology Stack

### UI and Rendering
* **`ratatui`**: Immediate-mode UI framework handling layouts, widgets, and buffer rendering.
* **`crossterm`**: Terminal backend for cross-platform raw mode management and input event capturing.
* **`syntect`**: High-performance syntax highlighting for the diff viewer, parsing large hunks in real time.

#### Displaying repository text
* **Strict by default:** names, paths, refs, authors, headers and git's messages are drawn through `sanitize`. Control characters become `U+FFFD`, and bidi controls and invisible characters become a visible `<U+XXXX>` escape.
* **File content is a little more lenient:** diff and blame body lines and the external renderer's output go through `sanitize_body`. It keeps a ZWJ/ZWNJ only where it changes what is drawn. The rules follow UTS #39 "Unicode Security Mechanisms", section 3.1.1.1 "Limited Contexts for Joining Controls" (A1, A2 and B, the same rules IDNA uses):
  * a ZWNJ that breaks a cursive join between Arabic-script, Syriac, N'Ko or Mongolian letters;
  * a joiner after an Indic virama or Myanmar asat.

  Every other joiner is escaped, including those in Latin, Greek, Cyrillic and CJK text. There are two documented deviations from the standard:
  * **Extension:** a ZWJ between a consonant and a virama is kept, but only in Devanagari, Bengali and Sinhala, where common fonts draw it distinctly (eyelash ra, ya-phala, touching letters).
  * **Tighter, as the standard allows:** after a virama, only a consonant of the same script may follow. Myanmar asat and Sinhala al-lakuna act only with a ZWJ. Myanmar's invisible stacker (U+1039) does not count as a virama.
* **Known limit:** a terminal that does not shape these scripts draws a kept joiner as nothing. There, two words that differ only by it look the same. Kika cannot detect shaping support.
* **Emoji ZWJ sequences stay escaped:** a terminal without grapheme clustering draws each part, wider than the two columns the layout reserves.

### Repository and Version Control
Kika uses a hybrid model: `git2` for fast reads, the `git` executable for everything that mutates the repository or talks to the network.

**The `git` CLI is the default.** All git access goes through one interface, implemented first with the CLI. A read moves to `git2` only when profiling shows a measured need (likely candidates: commit-graph paging and blob reads for PR review). `git status` always stays on the CLI, because `libgit2` status can disagree with git (untracked cache, fsmonitor, sparse checkout).

* **`git2`**: Rust bindings for `libgit2`. Used for read-only queries where avoiding process spawns pays off: commit graph traversal with lazy pagination, refs and reflog, blob/tree reads, merge-base and ahead/behind counts, and cheap ref snapshots for detecting external changes. `git2` objects are `!Send`, so each blocking task opens its own `Repository`.
* **`git` CLI (via `tokio::process`)**: Used for all mutations and network operations: commit, stage/unstage, partial (hunk/line) staging, checkout, merge, rebase, stash, fetch, pull and push. The real `git` binary runs the user's hooks, commit signing (GPG/SSH), credential helpers, `.gitattributes` filters (LFS, CRLF) and pagers, none of which `libgit2` fully supports.

#### Git command conventions
* **Typed argument builder:** commands are built as argv (never shell strings) through one builder, with a fake runner in unit tests that asserts the exact argv sequence and returns canned output.
* **Machine-readable output:** NUL-separated formats everywhere (`status --porcelain=v2 -z`, `--pretty=format:` with `%x00`), and `LANG=C` when output must be parsed.
* **Lock hygiene:** background commands set `GIT_OPTIONAL_LOCKS=0`; commands that fail on `index.lock` are retried with backoff.
* **Partial staging:** parse the plain `git diff` for the file, keep only the selected change lines (unselected `-` lines become context, unselected `+` lines are dropped; mirrored when unstaging), then `git apply --cached [--reverse]`. If every change in the file is selected, use `git add` / `git reset` on the file instead.
* **Interactive rebase:** `git rebase -i` with `GIT_SEQUENCE_EDITOR` pointing at Kika itself, which rewrites the todo list and exits.
* **Concurrency:** subprocesses are bounded by a `tokio::sync::Semaphore`; each diff pane keeps at most one running command and kills it when superseded.

### Event Loop and State Management
* **`tokio`**: Async runtime preventing the UI thread from blocking during large diff fetches or Git graph parsing.
* **`tokio::sync::mpsc`**: The message bus channeling `AppIntent` events from crossterm listeners and network tasks to the main state loop.

### Forge Integration (GitHub / GitLab)
* **`reqwest`**: Async HTTP client for pulling PR metadata, inline comments, and CI/CD status.
* **`serde` & `serde_json`**: Serialization and deserialization of Forge API payloads.

### Utilities and Ergonomics
* **`clap` (derive)**: CLI argument parsing enabling direct jumps to contexts (e.g., `kika review 123`).
* **`color_eyre`**: Error handling that cleanly restores the terminal state before printing readable panic traces.

## Initial Cargo.toml

```toml
[package]
name = "kika"
version = "0.1.0"
edition = "2024"

[dependencies]
# UI & Terminal
ratatui = "0.30"
crossterm = "0.29"
syntect = "5.3"

# Git Operations
git2 = "0.21"

# Async & Networking
tokio = { version = "1.53", features = ["full"] }
reqwest = { version = "0.13", features = ["json"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

# CLI & Errors
clap = { version = "4.6", features = ["derive"] }
color-eyre = "0.6"
```
