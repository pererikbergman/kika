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
| `/factory next` | Run the oldest open issue that has no `factory:*` label. Stop and say so if there is none. |
| `/factory approve <job-id>` | Human sign-off on a `needs-human` job: merge its PR (step 7). |
| `/factory rework <job-id> <note>` | Send a job back to the builder with human feedback (see Rework). |

## Files

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
  "branch": "factory/003-rate-limiting", "pr": 12,
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
   Give it the job folder, branch and issue number. When done, check `build.md` exists
   and the branch is pushed, then open a draft PR:
   `gh pr create --draft --base main --head <branch> --title "<title>" --body "Closes #<n>. Built by the factory; see review rounds in the PR comments."`
   Record `pr`. Stage → `review`, `round: 1`, push a `pending` round.
4. **Review (WAIT FOR ALL).** `mkdir jobs/<id>/round-N`. Spawn all four reviewers **in
   one message**, each given the job folder, branch and round folder. As each finishes,
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
7. **Merge.** `gh pr ready <pr>`, `gh pr merge <pr> --squash --delete-branch`,
   `git switch main && git pull --ff-only`. Remove the `factory:*` labels (the PR's
   `Closes #n` closes the issue). Stage → `merged`.

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
- One job at a time: the builder and the code reviewer use git, so don't run two jobs
  in parallel in this checkout.
- Nothing goes to `main` except through a merged PR.
- The factory definition (`.claude/`, `factory/project.md`, `factory/dashboard.html`) is
  committed; change it through a PR like any code. Runtime state (`factory/board.json`,
  `factory/jobs/`) is gitignored. Never commit it.
