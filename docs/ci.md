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
merged directly. Its merge box keeps showing `check (macos-self-hosted)` as "Expected,
waiting for status". That is normal. Do not wait for it or re-run CI.

To merge it, push the exact commit you reviewed to a branch in this repository.
**Pushing that branch is what starts the code on the Mac**, so read the whole diff
first, with extra care for `.github/`, `build.rs`, `tests/` and anything else that runs
at build or test time. Do not use `gh pr checkout` and push `HEAD`: that takes whatever
the PR head is now. The contributor can push a new commit between your review and your
push, and that commit would then run on the Mac with no further gate.

1. Record the head commit you are reviewing:
   `sha=$(gh pr view <n> --json headRefOid --jq .headRefOid)`.
2. Review the PR at that commit.
3. Fetch it: `git fetch origin pull/<n>/head`. Check that `git rev-parse FETCH_HEAD`
   prints `$sha`. If it does not, the contributor pushed again: stop and review the new
   commits.
4. Diff it against `main` once more, `git diff origin/main...$sha`, and confirm it is
   what you reviewed.
5. Push that exact commit: `git push origin "$sha:refs/heads/contrib/<n>"`.
6. Open a PR from `contrib/<n>` (or update an existing one). The contributor's commits
   and authorship stay as they are.
7. CI runs there as an own PR, so `check (macos-self-hosted)` runs on the Mac and the
   required check is satisfied.
8. Merge that PR normally and close the fork PR with a link to it. Never merge with
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

### When going public, in the same session

The fork approval setting below and the owner's review are the only things that stop a
fork that edits the workflow. Set it right after changing the visibility, in the same
session, before the repository is announced anywhere. Until it is set, GitHub's default
for public repositories applies, which only asks for approval from first-time
contributors.

- Settings → Actions → General → "Fork pull request workflows from outside
  collaborators": choose "Require approval for all outside collaborators". No fork PR
  workflow then runs until the owner approves it, after reading the diff.
- Optional: set the repository variable `HOSTED_RUNNERS` to `true`. GitHub-hosted
  minutes are free on public repositories, so ready PRs and pushes can add the Linux
  and Windows legs again.

### Strongly recommended: a guard on the runner itself

A fork can edit the workflow but not the runner's own configuration, so this is the
only guard a fork cannot change and a reviewer cannot miss. The runner can run a script
before every job:

1. Save the script below on the Mac as a `.sh` file outside the runner's work folder,
   for example `~/actions-runner/refuse-forks.sh`. The runner starts `.sh` hooks with
   bash, so `chmod +x` is not needed (it does no harm).
2. Add `ACTIONS_RUNNER_HOOK_JOB_STARTED=/full/path/to/refuse-forks.sh` to the `.env`
   file in the runner folder.
3. Restart the runner service.

The script needs `jq` (`brew install jq`). If `jq` is missing it refuses every job,
own PRs and pushes included, so a sudden failure in the hook step on every run means
`jq` is not on the runner's `PATH`.

```sh
#!/bin/sh
# Refuse any job whose pull request head is not in this repository.
command -v jq >/dev/null || { echo "jq not found: refusing the job." >&2; exit 1; }
head=$(jq -r '.pull_request.head.repo.full_name // empty' "$GITHUB_EVENT_PATH")
if [ "$GITHUB_EVENT_NAME" = "pull_request" ] && [ "$head" != "$GITHUB_REPOSITORY" ]; then
  echo "Refusing a fork pull request on the self-hosted runner." >&2
  exit 1
fi
```

This is not checked into the repository. Install it on the runner by hand.
