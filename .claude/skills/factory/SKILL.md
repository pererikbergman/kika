---
name: factory
description: Run Kika's software factory. Takes a GitHub issue through spec → build → four parallel reviews → approval → PR merge, tracked on factory/board.json. Use for /factory <issue#|feature>, /factory next, /factory approve <job-id>, /factory rework <job-id> <note>.
---

# Factory orchestrator

The session running this skill is the **orchestrator**. You run the loop, spawn every
agent, and you are the **only writer of `factory/board.json`**. You never write code,
specs or reviews yourself; agents do, and they get their context from files.

## The loop (edit this to change the factory)

```
                   ┌──────────────────┐
                   │   GitHub issue   │
                   └─────────┬────────┘
                             ▼
                   ┌──────────────────┐
                   │   spec-writer    │
                   │       opus       │
                   └─────────┬────────┘
                             ▼
                   ┌──────────────────┐
                   │     builder      │◄────────────────────┐
                   │  opus · draft PR │                     │
                   └─────────┬────────┘                     │
        ┌─────────────┬──────┴──────┬─────────────┐         │
        ▼             ▼             ▼             ▼         │
  ┌───────────┐ ┌───────────┐ ┌───────────┐ ┌───────────┐   │
  │  security │ │     ux    │ │ ui design │ │    code   │   │
  │    opus   │ │   sonnet  │ │   sonnet  │ │   sonnet  │   │
  └─────┬─────┘ └─────┬─────┘ └─────┬─────┘ └─────┬─────┘   │
        └─────────────┴──────┬──────┴─────────────┘         │
                             ▼                              │
                   ┌──────────────────┐   any CHANGES:      │
                   │   WAIT FOR ALL   ├─────────────────────┘
                   │   4 reviews in   │   resume builder
                   └─────────┬────────┘   (max 2 times)
                             ▼ all PASS
                   ┌──────────────────┐
                   │     approver     │
                   │       opus       │
                   └─┬───────────────┬┘
             APPROVE │               │ ESCALATE
                     ▼               ▼
              ┌─────────────┐ ┌─────────────┐
              │  merge PR   │ │ needs-human │ ◄── /factory approve
              └─────────────┘ └─────────────┘
```

Agents (in `.claude/agents/`): `spec-writer`, `builder`, `reviewer-security`,
`reviewer-ux`, `reviewer-ui`, `reviewer-code`, `approver`.

## Commands

| Command | What you do |
|---|---|
| `/factory <issue-number>` | Run that GitHub issue through the loop. |
| `/factory <feature text>` | `gh issue create` with the text as title (and body), then run it. |
| `/factory next` | Run the next **free** issue (see Picking the next issue), then keep going (see Continuous run). Stop and say so if there is none. |
| `/factory approve <job-id>` | Human sign-off on a `needs-human` job: update from main and merge its PR (steps 7–8). |
| `/factory rework <job-id> <note>` | Send a job back to the builder with human feedback (see Rework). |

## Picking the next issue

`/factory next` picks the lowest-numbered open GitHub issue that is **free**:

1. List candidates: `gh issue list --state open --label ready-for-agent --limit 200 --json number,title,labels,body`.
2. Skip any issue that:
   - has a `factory:*` label (already in progress or waiting on a human);
   - has a title starting with `PRD:` (a parent document, not a work item);
   - has an open blocker: parse the `## Blocked by` section for `#<n>` references and check
     each with `gh issue view <n> --json state`. Any blocker not `CLOSED` means skip.
3. Take the lowest remaining issue number. Tell the user which issue was picked and why the
   ones before it were skipped (one line each).

### Continuous run

`/factory next` does not stop after one job. It keeps **3 active jobs** running in parallel
whenever enough free issues exist, each in its own worktree (see Parallel jobs). Active
means any stage except `merged` and `needs-human`: a job waiting on the user does not hold
a slot. After **every** event (an agent finishing, a merge, an escalation, a user answer),
count active jobs and immediately start new ones until there are 3 or no free issue is left.
When a job reaches `merged`, give the user a one-line note (job id, PR link).
When a job reaches `needs-human`, report it and its questions, then keep filling slots.
Stop only when no free issue is left: say so, and list what is blocked and by what.

`/factory <issue-number>` runs that one issue only and does not continue.

`ready-for-human` issues are never picked by `next`. Running one explicitly with
`/factory <issue-number>` is allowed, but its job always ends in `needs-human` (tell the
approver it is a HITL issue; it must ESCALATE).

## Parallel jobs

Each job builds in its own git worktree, so jobs never share a checkout:

- **Worktree:** `<repo-parent>/kika-wt/<job-id>`, on branch `factory/<job-id>`, created by
  you in step 3 with `git worktree add -b <branch> <worktree> origin/main`. Record it on the
  job as `"worktree"`. The builder and the code reviewer work only there.
- **Main checkout:** always stays on a clean `main`. Only you touch it (`git pull`, the
  board, job folders). `factory/jobs/` lives here, so agents get its **absolute** path.
- **Cap:** at most 3 active jobs. Each worktree has its own `target/`, so first builds are
  cold; don't raise the cap without asking the user.
- **Picking:** when choosing among free issues for a parallel slot, prefer the lowest
  number, but skip one whose issue text clearly overlaps an active job's area (same panel or
  same feature) and take the next; mention the skip.
- **Agents in flight:** track each job's agents separately; a reviewer or builder finishing
  only advances its own job. Never resume a builder mid-round of its own job.


```
factory/
  project.md              repo context every agent reads first
  board.json              durable state (you write it), drives dashboard.html
  dashboard.html          `cd factory && python3 -m http.server`, open http://localhost:8000/dashboard.html
  jobs/<job-id>/
    issue.md              written by you from the GitHub issue
    spec.md               spec-writer
    build.md              builder (one section per round)
    round-N/review-{security,ux,ui,code}.md   reviewers
    decision.md           approver
```

**Job id:** issue number zero-padded to 3 digits plus a short kebab slug of the title,
e.g. issue #3 "Rate limiting" → `003-rate-limiting`. **Branch:** `factory/<job-id>`.

## board.json

```json
{ "repo": "pererikbergman/kika", "jobs": [ {
  "id": "003-rate-limiting", "issue": 3, "title": "Rate limiting",
  "branch": "factory/003-rate-limiting", "worktree": "../kika-wt/003-rate-limiting", "pr": 12,
  "stage": "review", "round": 2,
  "rounds": [
    { "security": "CHANGES", "ux": "PASS", "ui": "PASS", "code": "PASS" },
    { "security": "pending", "ux": "PASS", "ui": "pending", "code": "PASS" }
  ],
  "decision": null,
  "files": ["issue.md", "spec.md", "build.md", "round-1/review-security.md"],
  "updated": "2026-10-10T12:00:00Z"
} ] }
```

- `repo` is the GitHub `owner/name` (the dashboard builds PR links from it).
- `stage` is one of `spec`, `build`, `review`, `approval`, `needs-human`, `merged`.
- `rounds[N-1]` holds round N's verdicts: `pending`, `PASS` or `CHANGES`.
- `files` lists every file in the job folder, relative to it (the dashboard reads them).
- Update the job (and `updated`) at **every** step below, by rewriting the whole file
  (read, modify, write). Keep it valid JSON.

## Running a job

0. **Setup.** Working tree must be clean and on `main` (`git status`); stop if not.
   If `factory/board.json` is missing, create it as `{ "repo": "<owner/name>", "jobs": [] }`
   (from `gh repo view --json nameWithOwner`).
   `git pull --ff-only`. Ensure labels exist:
   `gh label create factory:in-progress --color FBCA04 --force` and
   `gh label create factory:needs-human --color D93F0B --force`.
   Label the issue `factory:in-progress`.
1. **Issue.** `gh issue view <n> --json number,title,body,labels,comments` → write
   `jobs/<id>/issue.md`. Add the job to the board with `stage: "spec"`.
2. **Spec.** Spawn `spec-writer` with the job folder path. When done, check `spec.md`
   exists. Stage → `build`.
3. **Build.** Spawn `builder`, named `builder-<job-id>` (use the Agent `name` field if
   available; otherwise keep the returned agent id on the job as `"builder_agent"`).
   First create the worktree (see Parallel jobs). Give the builder the absolute job folder,
   the worktree path, branch and issue number. When done, check `build.md` exists
   and the branch is pushed, then open a draft PR:
   `gh pr create --draft --base main --head <branch> --title "<title>" --body "Closes #<n>. Built by the factory; see review rounds in the PR comments."`
   Record `pr`. Stage → `review`, `round: 1`, push a `pending` round.
4. **Review (WAIT FOR ALL).** `mkdir jobs/<id>/round-N`. Spawn all four reviewers **in
   one message**, each given the absolute job folder, branch, worktree and round folder. As each finishes,
   read the first line of its file and update its chip on the board.
   The round is done **only when all four files exist** in the round folder and each
   starts with `VERDICT: PASS` or `VERDICT: CHANGES`. If a reviewer finished without a
   valid file, re-run that reviewer. **Never resume the builder mid-round.**
   When the round is done, post one PR comment summarising the four verdicts
   (`gh pr comment <pr> --body ...`).
5. **Branch on verdicts.**
   - Any `CHANGES` and fewer than 2 builder resumes so far: resume `builder-<job-id>`
     with SendMessage: "Round N reviews are in `jobs/<id>/round-N/`. Address them,
     re-run checks, push, update build.md." Stage → `build`, then back to step 4 with
     N+1.
   - Any `CHANGES` after 2 resumes: stage → `needs-human`, label the issue
     `factory:needs-human`, `gh pr ready <pr>`. Stop.
   - All `PASS`: stage → `approval`.
6. **Approve.** Spawn `approver` with job folder, branch and PR. Read the first line of
   `decision.md` into `decision`.
   - `ESCALATE`: stage → `needs-human`, label `factory:needs-human`, `gh pr ready <pr>`,
     comment the escalation questions on the PR. Stop and tell the user.
   - `APPROVE`: go to 7.
7. **Update from main.** Another job may have merged since this branch was built. If
   `git merge-base --is-ancestor origin/main origin/<branch>` fails, resume the builder:
   "Merge origin/main into your branch in the worktree, resolve conflicts, re-run all
   checks, push, and add an `## Update from main` section to build.md saying whether any
   conflict needed a behaviour decision." If the builder reports a non-trivial conflict
   (behaviour or tests changed to resolve it), run one more review round (step 4); it does
   not count toward the 2-resume limit. A clean or purely mechanical merge goes straight on.
8. **Merge.** `gh pr ready <pr>`, `gh pr merge <pr> --squash --delete-branch`, then in the
   main checkout `git pull --ff-only`, `git worktree remove <worktree>` and
   `git branch -D <branch>`. Remove the `factory:*` labels (the PR's `Closes #n` closes the
   issue). Stage → `merged`.

Finish every command by telling the user the job id, stage, PR link, and anything
waiting on them.

## Rework

`/factory rework <job-id> <note>`: write the note to `jobs/<id>/round-N/review-human.md`
(N = the latest round) starting with `VERDICT: CHANGES`, add it to `files`. Resume the
builder pointing at that round folder (if the builder from the original session is
gone, spawn a fresh `builder` and tell it to read `build.md` and the round folder
first). Remove `factory:needs-human`. Then continue from step 4 with a new round.
Rework does not count toward the 2-resume limit.

## Rules

- Agents get context from files, not this conversation, and write only their own file.
- Jobs run in parallel only through separate worktrees (see Parallel jobs); nothing
  switches branches in the main checkout.
- Nothing goes to `main` except through a merged PR.
- The factory definition (`.claude/`, `factory/project.md`, `factory/dashboard.html`) is
  committed; change it through a PR like any code. Runtime state (`factory/board.json`,
  `factory/jobs/`) is gitignored. Never commit it.
