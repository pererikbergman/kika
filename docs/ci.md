# CI and fork pull requests

CI is `.github/workflows/ci.yml`. It has two jobs: `check` (fmt, clippy and tests on a
matrix of operating systems) and `architecture` (noupling, see
`docs/architecture-checks.md`). The required check for merging into `main` is
`check (macos-self-hosted)`.

The `macos-self-hosted` leg runs on the owner's own Mac, a self-hosted runner with the
labels `self-hosted` and `mac`. It is not billed. Code that runs there runs on the
owner's machine, so it must never be code from a stranger. Once the repository is
public, anyone can open a pull request from a fork. This page says how CI keeps those
PRs off the Mac, and which settings the owner has to set by hand.

## Which runner each event uses

A run is **own** when it is a push, or a pull request whose head branch is in this
repository. In the workflow that is this expression, written out the same way wherever
a runner is picked:

```
github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository
```

If the fork has been deleted, `head.repo` is null, so the PR counts as a fork. The rule
fails safe.

`check` matrix legs:

| Case | Own push or PR | Fork PR |
|---|---|---|
| Draft PR, or the repository variable `HOSTED_RUNNERS` is not `true` | `macos-self-hosted` | `macos-latest` |
| Otherwise | `macos-self-hosted`, `ubuntu-latest`, `windows-latest` | `macos-latest`, `ubuntu-latest`, `windows-latest` |

`runs-on` maps `macos-self-hosted` to `[self-hosted, mac]` only when the run is also
own. Every other leg runs on the GitHub-hosted runner of the same name. So the guard
has two layers: if the matrix ever offered `macos-self-hosted` to a fork, the job would
ask for a runner labelled `macos-self-hosted`, which no runner has, and would not start.

`architecture` runs on `[self-hosted, mac]` when the run is own, and on `macos-latest`
for a fork PR. `macos-latest` is Apple silicon too, so the pinned
`noupling-macos-aarch64` binary and its SHA-256 check work on both.

The workflow never uses `pull_request_target`. That event runs with the base
repository's token and secrets, so checking out PR code under it would hand them to
the fork.

`tests/ci_fork_safety.rs` reads `ci.yml` as text and fails if a `runs-on` that names
`self-hosted` lacks the own expression, if the `os` matrix offers `macos-self-hosted`
without it, if `runs-on` is a bare label list, or if `pull_request_target` appears.

## How a fork PR gets its required check

A fork PR runs `check (macos-latest)` (and Linux and Windows when hosted runners are
on), never `check (macos-self-hosted)`. That is on purpose: the required check means
"passed on the owner's Mac", and a fork's code never gets there. So a fork PR cannot be
merged directly.

After reviewing the fork PR:

1. Fetch the contributor's commits and push them to a branch in this repository, for
   example `gh pr checkout <n>` and then `git push origin HEAD:contrib/<n>`.
2. Open a PR from that branch (or update an existing one). Keep the contributor's
   commits and authorship.
3. CI runs there as an own PR, so `check (macos-self-hosted)` runs on the Mac and the
   required check is satisfied.
4. Merge that PR normally and close the fork PR with a link to it. Never merge with
   `--admin` to skip the check.

## What the guard does not protect against

For `pull_request`, GitHub runs the workflow file **from the fork's head**, not from
`main`. A fork can edit `ci.yml` to put any job on `[self-hosted, mac]`. The `if:` and
`runs-on` guard and the test protect against mistakes in this repository. They do not
stop an attacker.

The real gate is the manual settings below, together with reading every change under
`.github/` (and anything else that runs during the build, such as `build.rs`) before
approving a fork PR's workflow run.

## Manual repository settings

These are set by hand in the GitHub web UI. CI cannot set them.

### Before going public

- Settings → Actions → General → Workflow permissions: choose "Read repository contents
  and packages permissions" (a read-only `GITHUB_TOKEN`).
- On the same page, turn off "Allow GitHub Actions to create and approve pull
  requests".
- Settings → Actions → Runners: confirm the Mac runner is registered on this
  repository, not on an account or organisation level, so no other repository can use
  it. Runner groups and their "selected workflows" limit exist only for organisations.
  On a personal account, the repository-level registration is the limit.

### After going public

- Settings → Actions → General → "Fork pull request workflows from outside
  collaborators": choose "Require approval for all outside collaborators". No fork PR
  workflow then runs until the owner approves it, after reading the diff.
- Optional: set the repository variable `HOSTED_RUNNERS` to `true`. GitHub-hosted
  minutes are free on public repositories, so ready PRs and pushes can add the Linux
  and Windows legs again.

### Optional: a guard on the runner itself

A fork can edit the workflow but not the runner's own configuration. The runner can
run a script before every job: set `ACTIONS_RUNNER_HOOK_JOB_STARTED` in the runner's
`.env` file to the script's path and restart the runner. A script that refuses fork PRs
(it needs `jq`):

```sh
#!/bin/sh
# Refuse any job whose pull request head is not in this repository.
head=$(jq -r '.pull_request.head.repo.full_name // empty' "$GITHUB_EVENT_PATH")
if [ "$GITHUB_EVENT_NAME" = "pull_request" ] && [ "$head" != "$GITHUB_REPOSITORY" ]; then
  echo "Refusing a fork pull request on the self-hosted runner." >&2
  exit 1
fi
```

This is not checked into the repository. Install it on the runner by hand if wanted.
