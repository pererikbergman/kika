# Kika

Kika is a terminal Git client and pull request / merge request review tool. It
shows the working tree, commits, branches and diffs of a repository in one
keyboard-driven screen, and lets you read and review open pull requests (GitHub)
and merge requests (GitLab) without leaving the terminal.

## Install

With Homebrew:

```sh
brew install pererikbergman/tap/kika
```

Or build from source with a Rust toolchain (1.85 or newer):

```sh
cargo install --path .
```

## Configuration

Settings, key bindings and forge options are described in
[docs/configuration.md](docs/configuration.md).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
