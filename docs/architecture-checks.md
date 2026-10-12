# Architecture checks

CI runs [noupling](https://github.com/pererikbergman/noupling) over Kika's Rust sources
in the `architecture` job of `.github/workflows/ci.yml`. It runs only in CI and on a
developer's machine; Kika's binary and `Cargo.toml` do not depend on it.

## What is checked

noupling parses the imports between source files and reports nine Issue kinds:
coupling violations (sibling directories that import each other), cycles, rule
violations, layer violations, gravity wells, red flags, stability violations, zone flags
and low cohesion. It also computes a health score out of 100. The audit prints each
Issue's "Score impact" and a "Points lost" line per kind. On Kika, coupling violations
and cycles cost points; gravity wells, red flags, zone flags and low cohesion are marked
"does not score".

The job writes the score and Issue list to the GitHub job summary. It fails only when
the scan finds an Issue that the committed baseline does not list. The score on its own
never fails the build. The mark next to the score in the summary (a red cross today) only
rates the score against the thresholds in `settings.json`. It does not mean the gate
failed. The job's own status tells you that.

## Where the config lives

| File | Purpose |
|---|---|
| `.noupling/settings.json` | Scan settings: only `.rs` files; `target`, `factory` and `docs` are ignored. |
| `.noupling/baseline.json` | Accepted Issues, one fingerprint each (for example `coupling_violation:src/model -> src/git`). |

Everything else noupling writes under `.noupling/` (`history.db`, `pr.md`, reports) is
git-ignored.

## Running it locally

Install noupling at the same version as CI (see `NOUPLING_VERSION` in `ci.yml`). Use
Homebrew (`brew install pererikbergman/noupling/noupling`) or download a binary for
your platform from the
[release page](https://github.com/pererikbergman/noupling/releases). Check it with
`noupling --version`.

Run `scan` first: `audit` and `report` read the latest snapshot that `scan` stored, so
without a fresh scan they show stale results, or stop with "No database found" if
nothing was ever scanned. From the repo root:

```sh
noupling scan .               # parse the sources and store a snapshot
noupling audit . --baseline   # same gate as CI: exit 1 on Issues not in the baseline
noupling report . --format pr --baseline   # the Markdown CI puts in the job summary
```

## When CI fails

1. Open the `architecture` job. The summary lists the Issues, and the failing step
   ("Fail on Issues not in the baseline") logs the full audit. Issues not in the
   baseline are the ones without "(baselined)".
2. Reproduce with the local commands above.
3. Fix the import if the Issue is real. Otherwise accept it on purpose (below).

## Accepting an Issue on purpose

If a change adds an Issue that you mean to keep:

```sh
noupling scan .
noupling baseline save .
```

Commit the updated `.noupling/baseline.json` in the same PR and say in the PR why the
Issue is accepted. The `timestamp` line changes on every save; that is expected. When
an Issue is fixed, save the baseline again so the resolved fingerprint is dropped.

A new import can fail the gate on Issues that seem unrelated to it. Red flags and zone
flags are measured against peer directories, so one more import can move an accepted
Issue to a different directory pair, or add a cycle or stability violation somewhere
else. The audit then reports the moved Issue as not in the baseline and the old one as
resolved. Read the list in the job log. If the Issues are acceptable, save the baseline
on purpose as above and explain the change in the PR.

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
