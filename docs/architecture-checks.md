# Architecture checks

CI runs [noupling](https://github.com/pererikbergman/noupling) over Kika's Rust sources
in the `architecture` job of `.github/workflows/ci.yml`. It runs only in CI and on a
developer's machine; Kika's binary and `Cargo.toml` do not depend on it.

## What is checked

noupling parses the imports between source files and reports nine Issue kinds:
coupling violations (sibling directories that import each other), cycles, rule
violations, layer violations, gravity wells, red flags, stability violations, zone flags
and low cohesion. It also computes a health score out of 100 from the coupling
violations and cycles.

The job writes the score and Issue list to the GitHub job summary. It fails only when
the scan finds an Issue that the committed baseline does not list. The score on its own
never fails the build.

## Where the config lives

| File | Purpose |
|---|---|
| `.noupling/settings.json` | Scan settings: only `.rs` files; `target`, `factory` and `docs` are ignored. |
| `.noupling/baseline.json` | Accepted Issues, one fingerprint each (for example `coupling_violation:src/model -> src/git`). |

Everything else noupling writes under `.noupling/` (`history.db`, `pr.md`, reports) is
git-ignored.

## Running it locally

Install noupling at the same version as CI (see `NOUPLING_VERSION` in `ci.yml`), then
from the repo root:

```sh
noupling scan .               # parse the sources and store a snapshot
noupling audit . --baseline   # same gate as CI: exit 1 on Issues not in the baseline
noupling report . --format pr --baseline   # the Markdown CI puts in the job summary
```

## Accepting an Issue on purpose

If a change adds an Issue that you mean to keep:

```sh
noupling scan .
noupling baseline save .
```

Commit the updated `.noupling/baseline.json` in the same PR and say in the PR why the
Issue is accepted. The `timestamp` line changes on every save; that is expected. When
an Issue is fixed, save the baseline again so the resolved fingerprint is dropped.

## Bumping noupling

1. Pick the new release tag and set `NOUPLING_VERSION` in the `architecture` job.
2. Get the SHA-256 of that release's `noupling-linux-x86_64` asset, for example with
   `gh release view <tag> -R pererikbergman/noupling --json assets` (the `digest`
   field), and check it against a local download with `shasum -a 256`.
3. Set `NOUPLING_SHA256` to that digest. Do not make the workflow fetch the release's
   `.sha256` file: a checksum from the same place as the binary would not catch a
   replaced asset.
4. Run the local commands above with the new version. If the new version reports
   different Issues, update the baseline in the same PR.
