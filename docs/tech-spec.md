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

### Repository and Version Control
* **`git2`**: Rust bindings for `libgit2`. Used for programmatic, safe traversal of the commit graph, memory-based diff building, and hunk staging without shelling out to the git executable.

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
