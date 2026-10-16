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
* **`syntect`**: The planned syntax highlighter for the diff viewer, parsing large hunks in real time. It is not a dependency yet; it gets added when highlighting is built.

#### Displaying repository text
* **Strict by default:** names, paths, refs, authors, headers and git's messages are drawn through `sanitize`. Control characters become `U+FFFD`, and bidi controls and invisible characters become a visible `<U+XXXX>` escape.
* **File content is a little more lenient:** diff and blame body lines and the external renderer's output go through `sanitize_body`. It keeps a ZWJ/ZWNJ only where it changes what is drawn. The rules follow UTS #39 "Unicode Security Mechanisms", section 3.1.1.1 "Limited Contexts for Joining Controls" (A1, A2 and B, the same rules IDNA uses):
  * a ZWNJ that breaks a cursive join between two Arabic-script, Syriac, N'Ko or Mongolian letters of the same script (dual-joining before it, dual- or right-joining after it);
  * a joiner after an Indic virama or Myanmar asat.

  Every other joiner is escaped, including those in Latin, Greek, Cyrillic and CJK text. There are two documented deviations from the standard:
  * **Extension:** a ZWJ between a consonant and a virama is kept, but only in Devanagari, Bengali and Sinhala, where common fonts draw it distinctly (eyelash ra, ya-phala, touching letters).
  * **Tighter, as the standard allows:** after a virama, only a consonant of the same script may follow. Myanmar asat and Sinhala al-lakuna act only with a ZWJ. Myanmar's invisible stacker (U+1039) does not count as a virama. A ZWNJ next to a transparent mark or a join-causing character such as tatweel is escaped.
* **Known limit:** a terminal that does not shape these scripts draws a kept joiner as nothing. There, two words that differ only by it look the same. Kika cannot detect shaping support.
* **Emoji ZWJ sequences stay escaped by default:** a terminal without grapheme clustering draws each part, wider than the two columns the layout reserves. Emoji joiners cannot disguise text (a dropped one visibly splits the picture), so the only question is width, answered per terminal: the global-only `[display] emoji_zwj_terminals` lists the terminals that draw a sequence as one picture, matched exactly against `TERM_PROGRAM` (else `TERM`) once at startup. There is no auto-detection. In a listed terminal, diff and blame body lines (`EmojiZwj::Keep`) keep a ZWJ that joins two emoji elements (an Extended_Pictographic character, optionally with U+FE0F or a skin-tone modifier, that measures two columns on its own, so a one-column text-presentation symbol such as `©` or `→` never counts) when the whole sequence, of at most four elements, measures two columns; anything else stays escaped. A diff-search highlight widens to whole glyphs, so it never splits a kept sequence. `view::clip` counts such a sequence as one glyph, as wide as the whole cluster measures. Headers, names, the conflict view and the external renderer's output always escape.

### Repository and Version Control
Kika uses a hybrid model: `git2` for fast reads, the `git` executable for everything that mutates the repository or talks to the network.

**The `git` CLI is the default.** All git access goes through one interface, implemented first with the CLI. A read moves to `git2` only when profiling shows a measured need (likely candidates: commit-graph paging and blob reads for PR review). `git status` always stays on the CLI, because `libgit2` status can disagree with git (untracked cache, fsmonitor, sparse checkout).

* **`git2`**: Rust bindings for `libgit2`. Used for read-only queries where avoiding process spawns pays off: commit graph traversal with lazy pagination, refs and reflog, blob/tree reads, merge-base and ahead/behind counts, and cheap ref snapshots for detecting external changes. `git2` objects are `!Send`, so each blocking task opens its own `Repository`. `git2` is not a dependency yet; it gets added together with the first read that profiling justifies.
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
* **GitHub through the `gh` CLI**: GitHub uses the GitHub CLI (`gh api graphql --include --hostname <host> --input -`) as its forge transport, instead of `reqwest`. It is run with argv only, and the query goes on stdin. `gh` holds the user's login, proxy and TLS settings, so Kika handles no tokens and needs no TLS crate. The GitHub CLI must be installed: at startup, in the background, the GitHub forge adapter's `check` runs `gh --version` (argv only, no host, no network, no token) and the answer (`Ready`, `NotInstalled`, `Unusable`, or `TooOld` against an adapter minimum, none set yet) goes into a per-forge-kind gate in the forge client. While the gate holds anything but `Ready`, `ForgeContext::forge` hands out no adapter, so no other `gh` call runs, and the Branches panel says why in GitHub's words. No refresh asks the forge before the first check answers. `B` runs the check again. Login is not checked up front (`gh auth status` contacts the host); it still comes from the first real call. The feature is off unless the global config's `[forge] github` names the hosts Kika may contact (see `docs/configuration.md`).
* **ETag probe and rate limits**: GraphQL has no conditional requests, so a refresh first sends a cheap REST probe (`gh api --include ... repos/<owner>/<repo>/pulls?...&per_page=1` with `If-None-Match`). A 304 is free against the primary limit; when the cached head set matches, no open pull request waits on CI and the last GraphQL answer is under 15 minutes old (a 304 does not reset that age), the cached badges are kept and no GraphQL query runs. The status line from `--include`, not `gh`'s exit code, decides the outcome. A rate-limit answer (403/429 with `x-ratelimit-remaining: 0`, `retry-after`, or a GraphQL `RATE_LIMITED` error) stops the fetch and blocks every `gh` call to that host, in memory, until the reset (clamped to 1–60 minutes); the next open, fetch, pull or push after it asks again. A fork skips the probe: its own pull-request list says nothing about its parent's, so once a GraphQL answer has shown a `parent`, each refresh runs only the query.
* **Head matching**: each local branch is asked about by its head, read from local git only: the ref name its upstream has on its remote and the owner from that remote's URL (on the same host), or else its own name under the badge repository's owner. Only the ref name is sent; the query asks the same aliases of the repository and its `parent` (null unless a fork), with each pull request's `headRepositoryOwner`, and the first pull request whose owner matches the head's wins, the repository's before the parent's.
* **GitLab through the `glab` CLI**: GitLab uses the GitLab CLI (`glab api graphql --include --hostname <host> --input -`) in the same argv-and-stdin shape as `gh`: argv only, the query and branch names on stdin, the status line from `--include` deciding the outcome, `glab` holding the login. It has no probe or ETag, so each refresh runs the query. Its adapter checks `glab --version` the same way, into the same gate, only when a `gitlab_ask` host is set. It is opt-in per host with the global config's `[forge] gitlab_ask`, which must name `[forge] gitlab` hosts; `gitlab` alone only builds URLs. Review mode reads GitLab the same way: the four queues (`currentUser`'s assigned, review-requested and authored merge requests, and the project's open ones) in one query, a merge request's detail with its head pipeline's jobs as checks in another, and its discussions in a third. They fill the same queue, detail and discussion types GitHub's answers do, and each string is made terminal-safe while it is parsed. A discussion whose first note has a diff position is a review thread; GitLab's old and new line numbers map to a side and line (added on the new side, removed on the old, context by its new number), so threads anchor on the diff as GitHub's do. It is outdated when it was written on another head. Other discussions, less system notes, are the conversation. `diffRefs.startSha` (the target's tip) is the base, so the merge base is still computed locally, and the head and base commits are fetched by id from the matching remote. Approvals show as approving reviews. Writing (draft notes, replies, resolving), merging and viewed marks are not available on GitLab yet. GitLab keeps viewed marks in the browser only, so that one may never be.
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

# Async & Serialization
tokio = { version = "1.53", features = ["full"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

# CLI & Errors
clap = { version = "4.6", features = ["derive"] }
color-eyre = "0.6"

# Config
toml = "1.1"

[target.'cfg(unix)'.dependencies]
# Swap the SIGINT action around a hand-off and put the previous one back.
libc = "0.2"

[dev-dependencies]
# Paused virtual time, so timer tests never wait for real.
tokio = { version = "1.53", features = ["full", "test-util"] }
```
