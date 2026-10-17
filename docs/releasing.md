# Releasing Kika

A release is a pushed tag `vX.Y.Z`. The tag must match the `version` of the root
`kika` crate in `Cargo.toml`. `.github/workflows/release.yml` builds the binaries,
publishes the GitHub release and updates the Homebrew tap
(`pererikbergman/homebrew-tap`). Every release job runs on GitHub-hosted runners,
never on the self-hosted Mac.

## Steps

1. Bump `version` in the root `Cargo.toml`, then run `cargo build` so `Cargo.lock`
   picks up the new version. Open a PR and merge it to `main`.
2. Tag the merge commit on `main` and push the tag:

   ```sh
   git switch main && git pull
   git tag v0.2.0
   git push origin v0.2.0
   ```

3. Watch the **Release** workflow in the Actions tab.

## What the workflow does

| Job | Runs on | Does |
|---|---|---|
| `version` | `ubuntu-latest` | Reads the `kika` crate version with `cargo metadata`. Fails with `tag vA does not match Cargo.toml version B` when the tag differs, and then nothing builds. |
| `build` | one hosted runner per target | `cargo build --release --locked --target <t>`, strips the binary, checks that `kika --version` prints `kika <version>`, and packages it. |
| `release` | `ubuntu-latest` | Checks there are exactly 5 archives and 5 `.sha256` files and that each checksum matches. Then runs `gh release create <tag> --verify-tag --generate-notes` with all ten files. |
| `tap` | `ubuntu-latest` | Renders `Formula/kika.rb` from `packaging/homebrew/kika.rb.tmpl` with `packaging/homebrew/render-formula.sh`, checks it with `ruby -c`, and pushes only that file to the tap's `main` as commit `kika <version>`. |

Build targets:

| Target | Runner | Smoke test |
|---|---|---|
| `aarch64-apple-darwin` | `macos-latest` | native |
| `x86_64-apple-darwin` | `macos-latest` (cross-compiled) | under Rosetta; skipped with a notice if Rosetta is missing |
| `x86_64-unknown-linux-gnu` | `ubuntu-22.04` | native |
| `aarch64-unknown-linux-gnu` | `ubuntu-22.04-arm` | native |
| `x86_64-pc-windows-msvc` | `windows-latest` | native |

The Linux builds use Ubuntu 22.04 so the binaries also run on older glibc.

Each archive is `kika-<version>-<target>.tar.gz` (`.zip` for Windows). It holds one
folder `kika-<version>-<target>/` with the binary (`kika` or `kika.exe`),
`LICENSE-MIT`, `LICENSE-APACHE` and `README.md`. Next to it is
`<archive>.sha256`, in `sha256sum` format.

**No partial releases.** `release` needs every `build` leg, so if one target fails
there is no GitHub release and no tap update.

## The tap token

The `tap` job is the only job that sees a secret, and it uses only
`TAP_GITHUB_TOKEN`. To create it:

1. GitHub, Settings, Developer settings, Personal access tokens, **Fine-grained
   tokens**, Generate new token.
2. Resource owner: `pererikbergman`. Repository access: **Only select repositories**,
   then `pererikbergman/homebrew-tap`.
3. Repository permissions: **Contents: Read and write**. Leave everything else at
   no access.
4. In `pererikbergman/kika`, Settings, Secrets and variables, Actions, add a
   repository secret named `TAP_GITHUB_TOKEN` with the token.

Fine-grained tokens expire, so renew it before it does.

If the secret is missing, the `tap` job fails with
`TAP_GITHUB_TOKEN is not set; the release is published but the tap was not updated.`
The GitHub release stays published.

## Recovery

- **The tap job failed** (missing or expired token, or a push error): fix the secret,
  then open the workflow run and use **Re-run failed jobs**. The release is not
  created again. If the tap already has this version's formula, the job exits with a
  notice and pushes nothing.
- **A build failed:** nothing was published. Fix the cause on `main`, then move the
  tag (`git push --delete origin vX.Y.Z`, retag, push), or bump to a new version.
- **The release step failed after creating the release** (for example during
  upload): delete the release (`gh release delete vX.Y.Z`), then re-run the failed
  jobs. The step does not reuse an existing release.
- **Wrong tag** (bad commit or version): delete both the release and the tag:

  ```sh
  gh release delete vX.Y.Z --cleanup-tag
  git tag -d vX.Y.Z
  ```

  If the tap was already updated, the next release overwrites the formula.

## Installing

macOS and Linux:

```sh
brew install pererikbergman/tap/kika
```

Windows: download `kika-<version>-x86_64-pc-windows-msvc.zip` from the
[release page](https://github.com/pererikbergman/kika/releases). Unpack it and put
`kika.exe` in a folder on your `PATH`. Kika runs the `git` CLI, so git must be
installed and on `PATH` too.

Other platforms can download the matching `.tar.gz` from the release, or build from
source with `cargo install --locked --path .`.
