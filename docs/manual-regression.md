# Kika manual regression test

A checklist of every user flow in Kika. Each item has an ID, the key or short
sequence to press, and the expected result after the colon. Default keys are
used throughout; a remapped key in your config changes only the key, not the
result.

## How to run

1. Build a sandbox (it refuses an existing folder unless you add `--force`):

   ```sh
   scripts/make-sandbox.sh ~/kika-sandbox
   ```

2. Point Kika at the sandbox's config, state and cache, so your real config,
   recent-repository list, pending reviews and forge cache are not touched,
   then start Kika in the sandbox repo:

   ```sh
   source ~/kika-sandbox/env.sh    # sets XDG_CONFIG_HOME, XDG_STATE_HOME, XDG_CACHE_HOME and cd's into repo/
   kika-dev
   ```

   `env.sh` also sets two things the tests need:

   - `GH_CONFIG_DIR` (to `~/.config/gh` unless you set it already). `gh`
     reads its login from `XDG_CONFIG_HOME` too, so without this it says
     "not logged in" and the `[github-read]` items cannot run.
   - `protocol.file.allow=always` for every git call, through
     `GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_0` and `GIT_CONFIG_VALUE_0`. Git
     refuses to clone a submodule over a `file://` URL without it, so
     `u` on `vendor/extra` (SUB-06) would fail with "transport 'file' not
     allowed".

   Without `env.sh`: set the same variables by hand and run Kika in
   `~/kika-sandbox/repo`.

3. Start again from a fresh sandbox at any time with
   `scripts/make-sandbox.sh ~/kika-sandbox --force`.

Helpers (run against an existing sandbox):

| Command | What it does |
|---|---|
| `scripts/make-sandbox.sh <dir> conflict` | Stashes the working changes, checks out `conflict/ours` and merges `conflict/theirs`, which stops on two conflict blocks in `conflict.txt` |
| `scripts/make-sandbox.sh <dir> push-other` | Pushes one new commit to `origin/main` and one to `origin/release` from `other/`, so `repo/` is behind after a fetch |
| `scripts/make-sandbox.sh <dir> fixup` | Stages a one-line change to `src/app.txt` line 3 (for `ctrl-b` and `ctrl-f` in Commits) |

What the sandbox holds (`repo/`):

- **Working tree:** `src/util.txt` staged and unstaged (`MM`), `src/app.txt`
  unstaged, `multi.txt` unstaged with three hunks, `to-delete.txt` deleted,
  `old-name.txt` renamed to `new-name.txt` (staged), `image.bin` binary
  change, untracked `scratch.txt` and `notes/todo.txt`.
- **History on `main`:** 19 commits by Alice Example, Bob Builder and
  Carol Remote, with fixed dates; a merge of `feature/merged` near the bottom;
  two `fixup!` commits (`fixup! Add util helpers`, `fixup! Tune app line 10`);
  `notes.txt` moved to `docs/notes.txt`; `lib/paths/deep.txt`; a binary
  `image.bin`. `src/app.txt` is changed by several commits (good for blame).
- **Branches:** `main` (tracks `origin/main`, ahead 3), `release` (tracks
  `origin/release`; behind after `push-other` and a fetch), `topic/tracked`
  (ahead 1, behind 1), `feature/merged` (merged), `feature/unmerged` (two
  unmerged commits, no upstream), `conflict/ours` and `conflict/theirs`,
  `to-rename` (on the first commit), `wt/side` (one commit of its own, checked out in the linked worktree `wt-side/`). `release` sits on "Touch app and deep path", inside `main`'s line, so rewrites below it move it.
  `origin/remote-only` has no local branch.
- **Remotes:** `origin` (`remote.git`) and `backup` (`backup.git`, empty).
- **Tags:** `v0.1.0` and `pushed-tag` (lightweight; `pushed-tag` is on
  origin), `v0.2.0` and `v1.0-rc` (annotated).
- **Stashes:** `stash@{0}` "WIP readme tweak", `stash@{1}` "WIP with
  untracked file" (holds `stashed-untracked.txt`).
- **Submodules:** `vendor/lib` (checked out) and `vendor/extra` (recorded,
  not initialized). The submodule URLs are `file://` URLs; `env.sh` sets
  `protocol.file.allow=always` so git may clone them.
- **Hook:** `.git/hooks/pre-commit` refuses a commit whose staged text
  contains `DO-NOT-COMMIT`; everything else passes. Because a hook exists,
  commits hand the terminal over unless `[commit] hooks_detached = true`.
- **Config** (`config/kika/config.toml`): theme `default`, and custom
  commands: Files `ctrl-t` (echo path, popup), `ctrl-u` (two prompts, popup),
  `ctrl-k` (`sleep 30`, none), `ctrl-g` (always fails, none), `ctrl-o`
  (terminal); Diff `ctrl-g` (echo path and line, popup); Blame `ctrl-y`
  (`git show --stat`, popup). Commented-out sections show how to turn on
  `[watch] fetch`, `[diff]`, `[commit]`, `[display]`, `[clipboard]` and
  `[forge]`. Most config changes need a restart of Kika.

**A clean worktree.** Some flows need one: undo/redo, bisect, paste
(cherry-pick) and the custom-patch rewrites. Before those, press `s` in Files
and stash everything with untracked files, or run `git stash -u`. Rebuild the
sandbox when an item leaves it in a state you do not want.

**Tags** (how an item can be tested):

| Tag | Meaning |
|---|---|
| `[sandbox]` | The local throwaway repo is enough |
| `[remote]` | Needs the sandbox's local bare remote (`origin`, `backup`) |
| `[github-read]` | Reads a real GitHub repo; no writes. Needs `[forge] github`, `gh` logged in, and a clone of a repo you can read |
| `[github-write]` | Would post, merge, approve or mark something on real GitHub. **Must not be run automatically.** Use a test repo you own |
| `[gitlab]` | Needs a GitLab host, `glab` logged in, `[forge] gitlab` and `gitlab_ask`. Items that say "(writes)" change GitLab and must not be run automatically |
| `[mouse]` | Needs real mouse input |
| `[visual]` | Needs eyes on colours or layout |
| `[editor]` | Opens `$EDITOR`, git's editor or a merge tool |

---

## Startup & CLI

- [ ] CLI-01 `kika-dev` in `repo/` `[sandbox]`: the workspace opens with Status, Files, Branches, Commits and Stash panels and the Diff pane; Files lists the sandbox's working-tree changes
- [ ] CLI-02 `kika-dev --help` `[sandbox]`: prints usage with the `review` command and `-h`/`-V`
- [ ] CLI-03 `kika-dev --version` `[sandbox]`: prints the version and exits
- [ ] CLI-04 `kika-dev review --help` `[sandbox]`: prints `Usage: kika-dev review <NUMBER>`
- [ ] CLI-05 `kika-dev review abc` `[sandbox]`: refused with an argument error; no screen opens
- [ ] CLI-06 `kika-dev review 1` in the sandbox (no `[forge]`) `[sandbox]`: review mode says no host is listed and asks nobody; `esc` goes to the review queue (with the same message), `esc` again to the workspace
- [ ] CLI-07 `kika-dev review <n>` in a GitHub clone with `[forge] github` `[github-read]`: starts on pull request `<n>`'s detail
- [ ] CLI-08 `kika-dev` in a folder that is not a repository `[sandbox]`: Kika starts without a repository and offers the recent repositories; `i` there makes the folder a repository
- [ ] CLI-09 No config at all (`XDG_CONFIG_HOME` pointing at an empty folder) `[sandbox]`: Kika starts with defaults and shows no error
- [ ] CLI-10 Global config with an unknown field (e.g. `[watch] refsx = 2`) `[sandbox]`: Kika shows the error, ignores the whole file and uses the defaults
- [ ] CLI-11 Global `[keys] "files.commit" = ["C"]` `[sandbox]`: `C` opens the commit message in Files; `c` no longer does
- [ ] CLI-12 Global `[keys] "workspace.whitespace" = []` `[sandbox]`: `w` does nothing; the help no longer lists it
- [ ] CLI-13 Global `[keys] "global.quit" = []` `[sandbox]`: the whole file is ignored with an error; `q` still quits
- [ ] CLI-14 Global `[keys] "files.stage" = ["s"]` `[sandbox]`: only that line is dropped with `key "s" is bound to both "files.stage" and "files.stash"…`; `space` still stages and `s` still opens the Stash menu
- [ ] CLI-15 `repo/.kika.toml` with `[keys] "files.stage" = ["x"]` `[sandbox]`: `x` stages in Files
- [ ] CLI-16 `repo/.kika.toml` that binds a locked binding (e.g. `"files.discard" = ["x"]`) `[sandbox]`: the repo file is ignored as a whole with an error; the global keys apply
- [ ] CLI-17 `repo/.kika.toml` with `[keys] "files.stage" = ["q"]` `[sandbox]`: refused (it would hide `global.quit`); the repo file is ignored
- [ ] CLI-18 `repo/.kika.toml` with a `[watch]`, `[diff]`, `[commit]`, `[forge]`, `[display]`, `[clipboard]`, `[theme]` or `[[commands]]` table `[sandbox]`: an error for each; the repo file is ignored as a whole
- [ ] CLI-19 `repo/.kika.toml` as a symbolic link `[sandbox]`: refused with an error
- [ ] CLI-20 Edit `[keys]` in another window, then come back to Kika `[sandbox]`: the new keys apply once Kika regains focus, without a restart
- [ ] CLI-21 Global config larger than 64 KiB `[sandbox]`: refused with an error; defaults apply

## Global and workspace navigation

### Panel focus, zoom, help, quit

- [ ] NAV-01 `1`, `2`, `3`, `4`, `5` `[sandbox]`: focus Status, Files, Branches, Commits, Stash; the focused border changes colour
- [ ] NAV-02 `0`, `l` and `right` from Files `[sandbox]`: focus the Diff pane on the selected file's hunks
- [ ] NAV-03 `tab` / `shift-tab` `[sandbox]`: move focus to the next / previous panel, wrapping round
- [ ] NAV-04 `z` three times `[sandbox]` `[visual]`: the focused panel goes normal, half, full, then normal again
- [ ] NAV-05 `?` in Files `[sandbox]`: "Keys: Files" lists the panel's keys, then the Workspace and Global keys
- [ ] NAV-06 In help: `j`/`k`, `pgdn`/`pgup` `[sandbox]`: the list scrolls a line / a page; `?` or `esc` closes it
- [ ] NAV-07 `?` in each panel (Status, Branches, Remotes tab, Tags tab, Commits, Reflog tab, Stash, Diff, Blame) `[sandbox]`: each shows that panel's own keys
- [ ] NAV-08 `q` in the workspace `[sandbox]`: Kika quits and the terminal is restored
- [ ] NAV-09 `ctrl-c` in the workspace `[sandbox]`: Kika quits
- [ ] NAV-10 `esc` in Files with no range, filter or search `[sandbox]`: Kika quits
- [ ] NAV-11 `r` `[sandbox]`: the file list and branches reload (change a file outside Kika first)
- [ ] NAV-12 `J` / `K` / `pgdn` / `pgup` with Files focused on `multi.txt` `[sandbox]`: the diff scrolls a line / a page, focus stays on Files
- [ ] NAV-13 `+` (or `=`) then `-` on `multi.txt` `[sandbox]`: more context lines, then fewer
- [ ] NAV-14 `w` on a file with a whitespace-only change `[sandbox]`: the whitespace-only lines hide; `w` again shows them
- [ ] NAV-15 `bksp` in the top repository `[sandbox]`: nothing happens (there is no parent); see SUB-04 for a submodule

### Watching for outside changes

- [ ] NAV-16 Edit `README.md` in another terminal and wait `[sandbox]`: it shows in Files within about 5 seconds
- [ ] NAV-17 `git branch extra` in another terminal `[sandbox]`: it shows in Branches within about 2 seconds
- [ ] NAV-18 Default config, watch the Command log for a few minutes `[remote]`: no fetch runs by itself (auto-fetch is off)
- [ ] NAV-19 `[watch] fetch = 60`, restart `[remote]`: a background fetch shows in the Command log about every minute; after `push-other` the branches show behind without pressing `f`
- [ ] NAV-20 `[watch] files = 0` `[sandbox]`: refused with an error (each value must be at least 1)

### Command log

- [ ] NAV-21 `@` `[sandbox]`: the Command log lists git commands this session, oldest first, with how they ended and how long they took
- [ ] NAV-22 In the log: `j`/`k`, `pgdn`/`pgup` `[sandbox]`: the cursor moves a row / a page
- [ ] NAV-23 In the log: `/status` `[sandbox]`: only matching rows show; `esc` clears the filter; a second `esc` closes the log
- [ ] NAV-24 In the log: `q` `[sandbox]`: closes the log (it does not quit Kika)
- [ ] NAV-25 In the log: `ctrl-c` `[sandbox]`: Kika quits
- [ ] NAV-26 Commit a message, then open the log `[sandbox]`: the commit line does not show the message body; a push to a URL with a password never shows the password

### Recent repositories

- [ ] NAV-27 `O` `[sandbox]`: Recent repositories lists repos opened before (open `other/` once first)
- [ ] NAV-28 In the list: `j`/`k`, `enter` on `other` `[sandbox]`: Kika switches to `other/`; the copied-commit set is cleared
- [ ] NAV-29 In the list: `esc` or `q` `[sandbox]`: closes it; `ctrl-c` quits Kika

### Worktrees

- [ ] NAV-30 `W` `[sandbox]`: lists `repo` (main) and `wt-side` (wt/side)
- [ ] NAV-31 In the list: `enter` on `wt-side` `[sandbox]`: Kika switches to the linked worktree; `W` and `enter` on `repo` switches back
- [ ] NAV-32 Select `feature/unmerged` in Branches, then `W`, `a` `[sandbox]`: a worktree is added for that branch and listed
- [ ] NAV-33 In the list: `d` on a clean linked worktree, then `y` `[sandbox]`: `Remove worktree <path>? Git refuses if it has uncommitted or untracked changes.`; `y` removes it and its folder
- [ ] NAV-34 `d` on a worktree with uncommitted changes, `y` `[sandbox]`: git refuses; Kika shows git's message and never forces
- [ ] NAV-35 `d` on the main worktree `[sandbox]`: refused (only a linked worktree can be removed)
- [ ] NAV-36 In the list: `/side` `[sandbox]`: filters by branch or path; `esc` clears the filter, then closes the list; `q` closes it

### Undo and redo

- [ ] NAV-37 With a clean worktree, commit something, then `ctrl-z` `[sandbox]`: "Undo or redo?" says which ref moves, from which commit to which, and whether files change; nothing moves yet
- [ ] NAV-38 Then `y` `[sandbox]`: the branch moves back; the reflog shows `kika undo`
- [ ] NAV-39 Then `ctrl-r`, `y` `[sandbox]`: the commit is back; the reflog shows `kika redo`
- [ ] NAV-40 `ctrl-z` twice (with `y`) `[sandbox]`: each press goes one action further back
- [ ] NAV-41 After an undo, make a new commit, then `ctrl-r` `[sandbox]`: nothing to redo
- [ ] NAV-42 `ctrl-z` after a checkout (`space` on a branch) `[sandbox]`: the question says a checkout; `y` checks out the previous branch
- [ ] NAV-43 `ctrl-z` after a whole rebase (e.g. a reword) `[sandbox]` `[editor]`: the branch goes back to where its own reflog says it was before the rebase
- [ ] NAV-44 `ctrl-z` with the sandbox's tracked changes in place `[sandbox]`: refused while tracked files have changes; nothing moves
- [ ] NAV-45 `ctrl-z` during the merge from the `conflict` helper `[sandbox]`: waits; says an operation is in progress
- [ ] NAV-46 `ctrl-z`, then move HEAD in another terminal, then `y` `[sandbox]`: nothing changes and Kika says HEAD moved
- [ ] NAV-47 `git symbolic-ref HEAD refs/remotes/origin/main` outside Kika, then `ctrl-z` `[sandbox]`: refused with "HEAD is on refs/remotes/origin/main, not a branch; nothing is undone". (A plain `git checkout origin/main` is an ordinary detached HEAD: `ctrl-z` offers to undo that checkout)

## Status

- [ ] STATUS-01 Status panel on `main` with nothing in progress `[sandbox]`: shows the branch and no operation
- [ ] STATUS-02 After the `conflict` helper `[sandbox]`: Status shows the merge in progress (branch, upstream and "merging"); Files shows the conflicted file as `! UU conflict.txt`
- [ ] STATUS-03 Resolve all blocks (see CONFLICT), stage, then `1`, `c` `[sandbox]` `[editor]`: the merge commit is made on the terminal (git's editor for the message); Status shows nothing in progress
- [ ] STATUS-04 `c` while conflicts remain `[sandbox]`: refused, git's message shown
- [ ] STATUS-05 `a` during the merge, then `y` `[sandbox]`: "Abort the operation in progress?"; `y` aborts the merge and the conflicts are gone
- [ ] STATUS-06 During a rebase stopped by `E` (see LOG-31), `c` `[sandbox]`: the rebase continues and finishes
- [ ] STATUS-07 During a rebase stopped on a conflict with no work since the stop, `s` `[sandbox]`: the stopped commit is skipped right away
- [ ] STATUS-08 Same, but after editing a conflicted file, `s` `[sandbox]`: "Skip the stopped commit?" is asked first
- [ ] STATUS-09 During a cherry-pick that stops (paste a commit that conflicts), `s` / `a` / `c` `[sandbox]`: skip, abort and continue work as for a rebase
- [ ] STATUS-10 During a revert that stops on a conflict, `c` after resolving `[sandbox]` `[editor]`: the revert commit is made
- [ ] STATUS-11 Bisect in progress (see LOG-63): Status shows good, bad and skipped counts, the commit being tested and where it started `[sandbox]`
- [ ] STATUS-12 During a bisect: `g`, `b` `[sandbox]`: the tested commit is marked good / bad and git checks out the next one
- [ ] STATUS-13 During a bisect: `s` `[sandbox]`: the tested commit is skipped
- [ ] STATUS-14 Once git names the first bad commit `[sandbox]`: Status shows it
- [ ] STATUS-15 During a bisect: `a`, then `y` `[sandbox]`: the bisect is reset; HEAD goes back to where it started
- [ ] STATUS-16 Bisect started on the command line with `--term-old=fixed --term-new=broken` `[sandbox]`: shown and can be reset, but `g`/`b` do not mark it

## File list

- [ ] FILES-01 `2` `[sandbox]`: Files lists `image.bin` (M), `multi.txt` (M), `old-name.txt → new-name.txt` (R), `src/app.txt` (M), `src/util.txt` (MM), `to-delete.txt` (D), `notes/todo.txt` and `scratch.txt` (untracked)
- [ ] FILES-02 `j`/`k` (and arrows) `[sandbox]`: the selection moves and the Diff pane shows the selected file's change
- [ ] FILES-03 Select `image.bin` `[sandbox]`: the diff says the binary files differ
- [ ] FILES-04 Select `new-name.txt` `[sandbox]`: shown as a rename from `old-name.txt`
- [ ] FILES-05 Select `to-delete.txt` `[sandbox]`: the diff shows every line removed
- [ ] FILES-06 Select `scratch.txt` `[sandbox]`: the diff shows the whole file added
- [ ] FILES-07 `space` on `src/app.txt` (unstaged) `[sandbox]`: the file moves to staged and the diff shows the staged change
- [ ] FILES-08 `space` again on the fully staged `src/app.txt` `[sandbox]`: it is unstaged
- [ ] FILES-09 `space` on `src/util.txt` (`MM`) `[sandbox]`: the rest is staged; the file is fully staged
- [ ] FILES-10 `space` on `to-delete.txt` `[sandbox]`: the deletion is staged
- [ ] FILES-11 `space` on `scratch.txt` `[sandbox]`: the untracked file is added
- [ ] FILES-12 `a` `[sandbox]`: everything is staged; `a` again unstages everything
- [ ] FILES-13 `d` on `src/app.txt`, then `y` `[sandbox]`: "Discard all unstaged changes to this file?"; `y` discards its unstaged change
- [ ] FILES-14 `d` on `scratch.txt`, then `y` `[sandbox]`: the untracked file is deleted
- [ ] FILES-15 `d` on `to-delete.txt`, then `y` `[sandbox]`: the file comes back
- [ ] FILES-16 `d` on `src/util.txt` (`MM`), then `y` `[sandbox]`: only the unstaged part is discarded; the staged change stays
- [ ] FILES-17 `o` on `src/app.txt` `[sandbox]` `[editor]`: the file opens in your editor; Kika comes back when it closes
- [ ] FILES-18 `b` on `src/app.txt` `[sandbox]`: Blame opens (see BLAME)
- [ ] FILES-19 `enter` on `multi.txt` `[sandbox]`: the Diff pane takes focus on its hunks (see HUNK)
- [ ] FILES-20 `i` on `scratch.txt` `[sandbox]`: a prompt is filled with `/scratch.txt`; `enter` adds it to `.gitignore` (created if missing) and the file leaves the list
- [ ] FILES-21 `i` on `notes/todo.txt`, edit the pattern to `notes/`, `enter` `[sandbox]`: the edited pattern is written; `i` again with the same pattern does not add a second line
- [ ] FILES-22 `i` on a tracked file `[sandbox]`: refused (tracked files cannot be ignored this way)
- [ ] FILES-23 `i` when `.gitignore` is a symlink `[sandbox]`: refused; nothing is written
- [ ] FILES-24 `m` `[sandbox]`: the actions menu opens; `esc` closes it
- [ ] FILES-25 `/app` `[sandbox]`: only `src/app.txt` shows; `enter` keeps the filter; `esc` clears it
- [ ] FILES-26 `/s txt` `[sandbox]`: words match in order, not side by side (e.g. `src/util.txt`, `scratch.txt`)
- [ ] FILES-27 `/App` `[sandbox]`: an uppercase letter makes the filter case-sensitive; nothing matches
- [ ] FILES-28 `V`, `j`, `j`, `space` `[sandbox]`: a range of three files is staged in one go; conflicted files are skipped
- [ ] FILES-29 `V` over files that are all staged, `space` `[sandbox]`: the range is unstaged
- [ ] FILES-30 `V` over two files, `d` `[sandbox]`: asks once for the range; only `y` discards
- [ ] FILES-31 While a range is set: `o`, `b`, `enter` `[sandbox]`: refused (they act on one file)
- [ ] FILES-32 `V` again `[sandbox]`: clears the range
- [ ] FILES-33 `esc` order with a range, a filter and a diff search all set `[sandbox]`: the first `esc` clears the range, the next the filter, the next the search, the next quits
- [ ] FILES-34 `ctrl-f` on `multi.txt`, type `edited` `[sandbox]`: the diff beside the list is searched as you type, focus stays on Files; `enter` scrolls to the match
- [ ] FILES-35 Then `n` / `N` `[sandbox]`: next / previous match
- [ ] FILES-36 `L` on `src/app.txt` `[sandbox]`: the Commits panel shows only that file's history
- [ ] FILES-37 `s` `[sandbox]`: the Stash menu offers all tracked changes, with untracked files, staged only, or this file
- [ ] FILES-38 `s`, "with untracked files", message `all work`, `enter` `[sandbox]`: the worktree is clean; Stash shows `stash@{0}` "all work"
- [ ] FILES-39 `s`, "staged only", empty message `[sandbox]`: only the staged changes are stashed
- [ ] FILES-40 `s`, "this file" on `multi.txt` `[sandbox]`: only `multi.txt` is stashed
- [ ] FILES-41 `s` while conflicts exist (after the `conflict` helper) `[sandbox]`: refused; nothing changes
- [ ] FILES-42 `S` `[sandbox]`: the Submodule list opens (see SUB)
- [ ] FILES-43 `M` on a conflicted file `[sandbox]` `[editor]`: git's merge tool opens for it
- [ ] FILES-44 `M` on a file that is not conflicted `[sandbox]`: refused with a message

## Commit message and commit flow

- [ ] COMMIT-01 With staged changes, `c` `[sandbox]`: the commit message popup opens on the subject
- [ ] COMMIT-02 Type a subject, `tab`, type a body, `tab` `[sandbox]`: focus moves to the body and back to the subject
- [ ] COMMIT-03 `ctrl-s` `[sandbox]`: the commit is made; the screen switches to git and back (the sandbox has a hook); the new commit is at the top of Commits
- [ ] COMMIT-04 Same with `[commit] hooks_detached = true` (restart) `[sandbox]`: the commit runs in the background; the screen does not switch
- [ ] COMMIT-05 With `commit.gpgsign = true` and `hooks_detached = true` `[sandbox]`: the commit still hands the terminal over
- [ ] COMMIT-06 With nothing staged, `c` `[sandbox]`: "Nothing is staged. Stage all changes and commit?"; `y` opens the message without staging yet; the commit (`ctrl-s`) stages and commits everything, untracked files too; leaving the message stages nothing
- [ ] COMMIT-07 Same, `enter` `[sandbox]`: acts like `y`: opens the message, and the commit stages everything
- [ ] COMMIT-08 Same, `n` or `esc` `[sandbox]`: no commit; nothing is staged
- [ ] COMMIT-09 Type a subject, `esc` `[sandbox]`: "Discard the typed message?"; `n` or `esc` goes back to the message with the text kept
- [ ] COMMIT-10 Same, `y` `[sandbox]`: the message is thrown away and the popup closes
- [ ] COMMIT-11 Same, `enter` `[sandbox]`: does nothing (only `y` discards)
- [ ] COMMIT-12 `esc` with an empty message `[sandbox]`: the popup closes without asking
- [ ] COMMIT-13 Stage a line containing `DO-NOT-COMMIT`, `c`, subject, `ctrl-s` `[sandbox]`: the hook fails; "Commit failed" shows the hook's message
- [ ] COMMIT-14 Then `esc` or `enter` `[sandbox]`: back to the message with the text kept
- [ ] COMMIT-15 Then `ctrl-n`, `ctrl-s` `[sandbox]`: the hooks are skipped for this commit and it succeeds
- [ ] COMMIT-16 `ctrl-e` in the message `[sandbox]` `[editor]`: the message opens in your editor; closing it commits
- [ ] COMMIT-17 `q` and `?` in the message `[sandbox]`: they are typed, not quit or help
- [ ] COMMIT-18 `ctrl-c` in the message `[sandbox]`: the popup closes and Kika stays; with a typed message it asks "Discard the typed message?" first
- [ ] COMMIT-19 `A` with staged changes `[sandbox]`: "Loading the last commit" briefly, then the message holds the last commit's message; `ctrl-s` amends with the staged changes
- [ ] COMMIT-20 `A` with nothing staged, change the subject, `ctrl-s` `[sandbox]`: the last commit is reworded
- [ ] COMMIT-21 `A`, then `esc` while it loads `[sandbox]`: does not amend
- [ ] COMMIT-22 `ctrl-c` while the last commit loads `[sandbox]`: Kika quits
- [ ] COMMIT-23 Hook that waits for input with `hooks_detached = true` `[sandbox]`: the commit gets no terminal (it fails or waits); Kika stays usable

## Diff hunks

- [ ] HUNK-01 `enter` on `multi.txt` `[sandbox]`: the Diff pane has focus; the first of three hunks is selected
- [ ] HUNK-02 `j` / `k` `[sandbox]`: the next / previous hunk is selected
- [ ] HUNK-03 `space` on the second hunk `[sandbox]`: only that hunk is staged; `multi.txt` becomes `MM`
- [ ] HUNK-04 `t` `[sandbox]`: switches to the staged view, showing the staged hunk; `t` switches back
- [ ] HUNK-05 In the staged view, `space` `[sandbox]`: the hunk is unstaged
- [ ] HUNK-06 `d` on a hunk, then `y` `[sandbox]`: asks first; `y` discards that hunk from the working tree
- [ ] HUNK-07 `d`, then `n` or `esc` `[sandbox]`: the hunk stays
- [ ] HUNK-08 `o` `[sandbox]` `[editor]`: the editor opens at the selected hunk's first change. The line is passed only to editors Kika knows (vi, vim, nvim, nano, emacs, kak, micro, code, subl, zed, hx); any other `$EDITOR` gets just the file
- [ ] HUNK-09 `e` `[sandbox]` `[editor]`: the hunk opens in your editor; on close, the edited hunk is staged
- [ ] HUNK-10 `v` `[sandbox]`: line mode (see LINE)
- [ ] HUNK-11 `/edited`, `enter` `[sandbox]`: scrolls to the first match; `n` / `N` step through matches
- [ ] HUNK-12 `esc` with a search set `[sandbox]`: clears the search; `esc` again goes back to the side panel
- [ ] HUNK-13 `h` or `left` `[sandbox]`: back to the side panel last focused
- [ ] HUNK-14 `enter` on `scratch.txt` (untracked), `space` `[sandbox]`: the whole new file is one hunk; staging adds it
- [ ] HUNK-15 `enter` on `image.bin` `[sandbox]`: no hunks to pick; staging a hunk is refused or not offered
- [ ] HUNK-16 `R` without `[diff]` set and no git pager `[sandbox]`: stays on the built-in view (no renderer to cycle to)
- [ ] HUNK-17 `[diff] pager = "delta --paging=never"` (restart), `R` `[sandbox]` `[visual]`: the pane is drawn by the pager; `space` still stages the matched hunk; `R` again returns to the built-in view
- [ ] HUNK-18 With the pager view on a selection it cannot match `[sandbox]` `[visual]`: the built-in view is drawn and the title says `(unmapped)`
- [ ] HUNK-19 `[diff] tool = "difft --color=always"`, `R` until the tool shows `[sandbox]` `[visual]`: the title says `(display only)`; `space`, `d`, `e` and `v` are refused
- [ ] HUNK-20 `[diff] pager = "delta | less"` `[sandbox]`: the whole file is ignored with an error (shell syntax refused)
- [ ] HUNK-21 `[diff] pager = "./delta"` `[sandbox]`: refused (relative path)
- [ ] HUNK-22 Git `pager.diff` set in your global git config, `R` `[sandbox]` `[visual]`: offered after `pager`; `less` or `cat` there are skipped
- [ ] HUNK-23 `tool` on a symlink or a submodule `[sandbox]`: says so instead of running the tool

## Diff lines

- [ ] LINE-01 In a hunk of `multi.txt`, `v` `[sandbox]`: line mode; the first changed line is selected
- [ ] LINE-02 `j` / `k` `[sandbox]`: move to the next / previous changed line
- [ ] LINE-03 `J` / `K` `[sandbox]`: the selection grows down / up a changed line
- [ ] LINE-04 `space` `[sandbox]`: only the selected lines are staged; the rest of the hunk stays unstaged
- [ ] LINE-05 `t` to the staged view, `v`, select a line, `space` `[sandbox]`: that line is unstaged
- [ ] LINE-06 `d`, then `y` `[sandbox]`: only the selected lines are discarded from the working tree
- [ ] LINE-07 `o` `[sandbox]` `[editor]`: the editor opens at the selected line (same editor list as HUNK-08)
- [ ] LINE-08 `esc` or `v` `[sandbox]`: back to whole hunks

## Blame

- [ ] BLAME-01 `b` on `src/app.txt` `[sandbox]`: each line shows the commit, author (Alice or Bob) and date that last changed it
- [ ] BLAME-02 Line 1 (changed in the working tree) `[sandbox]`: shown as not committed yet
- [ ] BLAME-03 `j`/`k`, `pgdn`/`pgup` `[sandbox]`: the cursor moves a line / a page
- [ ] BLAME-04 `enter` on line 5 `[sandbox]`: shows the commit "Tune app line 5"
- [ ] BLAME-05 In that commit: `j`/`k`, `pgdn`/`pgup` `[sandbox]`: it scrolls
- [ ] BLAME-06 In that commit: `/line 5`, `n`, `N` `[sandbox]`: its own search finds the matches
- [ ] BLAME-07 In that commit: `esc` with a search `[sandbox]`: clears it; `esc` again (or `h`/`left`) goes back to the blame at the same line
- [ ] BLAME-08 In the blame: `/changed by` `[sandbox]`: matches line text only, not commit, author or date; `n`/`N` step
- [ ] BLAME-09 `enter` on the bar after a search, then `enter` again `[sandbox]`: the first puts the cursor on the match; the second shows its commit
- [ ] BLAME-10 Search in the blame, open a commit, search there, go back `[sandbox]`: the blame's search is still there
- [ ] BLAME-11 `esc` with no search, or `h`/`left` `[sandbox]`: back to the file list
- [ ] BLAME-12 `b` on an untracked file `[sandbox]`: refused or every line shown as not committed yet

## Conflict view

Run `scripts/make-sandbox.sh <dir> conflict` first.

- [ ] CONFLICT-01 `2` `[sandbox]`: `conflict.txt` shows as conflicted
- [ ] CONFLICT-02 `enter` on `conflict.txt` `[sandbox]`: the conflict view opens on the first of two blocks, ours and theirs side by side or stacked
- [ ] CONFLICT-03 `j` / `k` `[sandbox]`: move to the next / previous conflict
- [ ] CONFLICT-04 `o` on the first block `[sandbox]`: our side is kept and the file is written at once; the second block keeps its markers
- [ ] CONFLICT-05 `t` on the second block `[sandbox]`: their side is kept and written
- [ ] CONFLICT-06 Once every block is picked `[sandbox]`: Kika offers to stage the file; yes stages it
- [ ] CONFLICT-07 Fresh conflict, `b` `[sandbox]`: both sides are kept, ours first
- [ ] CONFLICT-08 Pick one block, `esc` `[sandbox]`: back to the file list; the pick stays in the file
- [ ] CONFLICT-09 `M` `[sandbox]` `[editor]`: the file opens in your merge tool instead
- [ ] CONFLICT-10 `V` over the files including `conflict.txt`, `space` `[sandbox]`: the conflicted file is skipped
- [ ] CONFLICT-11 `[diff] tool` set, select `conflict.txt` in the tool view `[sandbox]`: says the file is conflicted instead of running the tool

## Branches

- [ ] BRANCH-01 `3` `[sandbox]`: the Local tab lists branches with their upstreams and ahead/behind (`main origin/main ↑3`, `topic/tracked` ↑1 ↓1)
- [ ] BRANCH-02 Select a branch `[sandbox]`: the diff pane shows its commits; `J`/`K`/`pgdn`/`pgup` scroll them
- [ ] BRANCH-03 `space` on `feature/unmerged` `[sandbox]`: it is checked out; the working changes come along when git allows
- [ ] BRANCH-04 `space` on `wt/side` `[sandbox]`: git refuses (it is checked out in `wt-side/`); the message shows
- [ ] BRANCH-05 `n` on `main`, name `new-one`, `enter` `[sandbox]`: a branch is made at `main` and checked out
- [ ] BRANCH-06 `R` on `to-rename`, new name `renamed`, `enter` `[sandbox]`: the branch is renamed
- [ ] BRANCH-07 `d` on `feature/merged`, then `y` `[sandbox]`: "Delete this branch?"; `y` deletes it
- [ ] BRANCH-08 `d` on `feature/unmerged` `[sandbox]`: asks in stronger words; `y` does nothing; `D` deletes it with its commits
- [ ] BRANCH-09 `d`, then `n` or `esc` `[sandbox]`: the branch stays
- [ ] BRANCH-10 `d` on the current branch `[sandbox]`: refused
- [ ] BRANCH-11 `m` on `feature/unmerged` while on `main` `[sandbox]`: the merge menu offers fast-forward only, a merge commit, a squash and rebase onto
- [ ] BRANCH-12 "Fast-forward only" on a branch that cannot fast-forward `[sandbox]`: git refuses; nothing changes
- [ ] BRANCH-13 "Merge commit" `[sandbox]` `[editor]`: git runs on the terminal; its editor opens for the message; a merge commit is made
- [ ] BRANCH-14 "Squash" `[sandbox]`: the branch's changes are staged for you to commit; no commit is made
- [ ] BRANCH-15 "Rebase onto" (on `topic/tracked`, rebase onto `main`) `[sandbox]`: the current branch is rebased on the terminal
- [ ] BRANCH-16 `m` on `conflict/theirs` while on `conflict/ours`, "Merge commit" `[sandbox]`: stops on the conflict; Status shows the merge
- [ ] BRANCH-17 `u` on `feature/unmerged` `[sandbox]`: a prompt starts with `origin/feature/unmerged`; type `origin/release`, `enter`: the branch now tracks `origin/release`
- [ ] BRANCH-18 `u` with a name Kika has not listed (`origin/nope`) `[sandbox]`: refused; says to fetch first if it is new
- [ ] BRANCH-19 `U` on a tracking branch `[sandbox]`: it no longer tracks its upstream
- [ ] BRANCH-20 `]` / `[` `[sandbox]`: Local → Remotes → Tags → Local, and back the other way
- [ ] BRANCH-21 `M` on `feature/unmerged`, then `enter` on `main` `[sandbox]`: the files changed from the mark to `main` show in place of the log; the marked row shows `◆`
- [ ] BRANCH-22 `M` on the marked branch `[sandbox]`: clears the mark
- [ ] BRANCH-23 `/feat` `[sandbox]`: only matching branches show; each tab keeps its own filter
- [ ] BRANCH-24 `V` over `feature/merged`, `feature/unmerged` and `main`, `d` `[sandbox]`: "Delete the selection?" counts the kept ones (current and unmerged); `y` deletes only `feature/merged`
- [ ] BRANCH-25 While a range is set, `space` / `R` `[sandbox]`: wait until `esc` clears the range
- [ ] BRANCH-26 Switch tabs with a range set `[sandbox]`: the range is cleared, the filter stays
- [ ] BRANCH-27 `esc` order: range, then filter, then quit `[sandbox]`
- [ ] BRANCH-28 `B` with no `[forge]` `[sandbox]`: says no host is listed; nothing is asked
- [ ] BRANCH-29 `o` with no `[forge]` `[sandbox]`: the Pull request menu shows its items off, each saying why
- [ ] BRANCH-30 Select `to-rename`, run `git branch -D to-rename` in another terminal, then `r` and at once `space` `[sandbox]`: `to-rename` is never checked out; `space` acts on the row the reloaded list shows

## Remotes

- [ ] REMOTE-01 `]` from Local `[remote]`: lists `origin` and `backup` with their URLs, and `origin`'s branches (`main`, `release`, `remote-only`, `topic/tracked`) below
- [ ] REMOTE-02 `j`/`k` `[remote]`: move over remotes and remote branches
- [ ] REMOTE-03 `space` on `origin/remote-only` `[remote]`: a local `remote-only` tracking it is made and checked out
- [ ] REMOTE-04 `a`, name `third`, URL `../backup.git` `[remote]`: the remote is added; it shows no branches until `f`
- [ ] REMOTE-05 `a` with URL `ext::sh -c touch% /tmp/x` `[sandbox]`: refused (`ext` transport); nothing is added
- [ ] REMOTE-06 `a` with URL `fd::3` `[sandbox]`: refused
- [ ] REMOTE-07 `R` on `backup`, new name `spare` `[remote]`: renamed; its tracking refs follow
- [ ] REMOTE-08 `d` on `backup`, then `y` `[remote]`: "Remove this remote?"; `y` removes it and its remote-tracking branches
- [ ] REMOTE-09 `d`, then `enter` `[remote]`: nothing happens (`enter` does not answer); `n`/`esc` keep it
- [ ] REMOTE-10 `/rel` `[remote]`: `origin/release` shows, with its remote's row
- [ ] REMOTE-11 `V` over both remote rows and some branch rows, `d`, `y` `[remote]`: only the remotes are removed; branch rows are left alone
- [ ] REMOTE-12 A remote added with git as `-odd` `[sandbox]`: cannot be renamed or removed from Kika
- [ ] REMOTE-13 A remote URL with a user and password (`https://u:p@example.com/x.git`) `[sandbox]`: the URL shows without the user and password
- [ ] REMOTE-14 None of the Remotes keys reach the network (check the Command log) `[remote]`
- [ ] REMOTE-15 Select `origin/remote-only`, run `git update-ref -d refs/remotes/origin/remote-only` in another terminal, then `r` and at once `space` `[remote]`: no `remote-only` branch is made; `space` acts on the row the reloaded list shows

## Tags

- [ ] TAG-01 `[` from Local `[sandbox]`: lists `v1.0-rc`, `v0.2.0`, `pushed-tag`, `v0.1.0` newest first (by tagger date for annotated tags, commit date for the others), with commit, date and subject; `◆` before the annotated ones
- [ ] TAG-02 Select a tag `[sandbox]`: the diff pane shows its commits
- [ ] TAG-03 `d` on `v0.1.0`, then `y` `[sandbox]`: the local tag is deleted; remotes are untouched
- [ ] TAG-04 `d`, then `enter`, `n`, `esc` `[sandbox]`: `enter` does nothing; `n` and `esc` keep the tag
- [ ] TAG-05 `d` after the tag was moved in another terminal (`git tag -f`) `[sandbox]`: refused (it no longer points where it did)
- [ ] TAG-06 `P` on `v1.0-rc` while on `main` `[remote]`: the tag is pushed to `origin` (never forced); `git ls-remote --tags ../remote.git` shows it
- [ ] TAG-07 Check out `feature/unmerged` (no upstream, two remotes), `[`, `P` `[remote]`: nothing is pushed; says so
- [ ] TAG-08 `/v0` `[sandbox]`: filters the tags
- [ ] TAG-09 `V` over two tags, `d`, `y` `[sandbox]`: both are deleted after one question
- [ ] TAG-10 On the Tags tab, `P` pushes the tag, not the branch `[remote]`
- [ ] TAG-11 Select `v0.1.0`, run `git tag -d v0.1.0` in another terminal, then `r` and at once `d` `[sandbox]`: the question names the tag the reloaded list shows, never `v0.1.0`

## Fetch, pull and push

- [ ] NET-01 Run `scripts/make-sandbox.sh <dir> push-other`, then `f` `[remote]`: `git fetch --all` runs; `main` shows ↑3 ↓1 and `release` ↓1
- [ ] NET-02 Check out `release`, `p` `[remote]`: it fast-forwards to `origin/release`
- [ ] NET-03 On `main` after NET-01 (diverged), `p` `[remote]`: git's own message shows (divergent branches) unless `pull.rebase` is set
- [ ] NET-04 With `git config pull.rebase true`, `p` on `main` `[remote]`: rebases onto `origin/main`
- [ ] NET-05 On `main` before any `push-other`, `P` `[remote]`: the 3 commits are pushed; `main` shows no ahead count
- [ ] NET-06 On `topic/tracked` (ahead 1, behind 1), `P` `[remote]`: rejected; Kika offers to push again with `--force-with-lease`
- [ ] NET-07 Then `n` or `esc` `[remote]`: nothing is forced
- [ ] NET-08 Then `y` `[remote]`: pushed with `--force-with-lease`; `origin/topic/tracked` now equals the local branch
- [ ] NET-09 Push again after someone pushed to the branch since your last fetch, `y` `[remote]`: the lease refuses; nothing is replaced
- [ ] NET-10 `P` on a branch with no upstream `[remote]`: git's own message (or `push.default` behaviour) shows
- [ ] NET-11 `f` with `origin` pointing at a missing path `[sandbox]`: git's error shows in a popup
- [ ] NET-12 A fetch from an https remote that asks for a username and password, with no credential helper `[github-read]`: "Git is asking" shows; the password is drawn as dots; `enter` sends it
- [ ] NET-13 Same, `esc` `[github-read]`: the question is cancelled and the fetch fails
- [ ] NET-14 Same, `ctrl-c` `[github-read]`: the question is cancelled and Kika quits
- [ ] NET-15 An ssh remote to a new host `[github-read]`: the host-key question shows ssh's words and is typed, not `y`/`n` keys: `yes`/`no` show as typed, other text shows as dots, `enter` sends; typing `no` and `enter` fails the fetch with "Host key verification failed."
- [ ] NET-16 With `GIT_ASKPASS` set to your own program `[sandbox]`: git keeps using yours; no Kika popup
- [ ] NET-17 Check the Command log after `f`, `p`, `P` `[remote]`: each shows as the plain git command with how it ended

## Commits

### Moving and opening

- [ ] LOG-01 `4` `[sandbox]`: the log shows the graph, short hashes, authors, dates and subjects, newest first
- [ ] LOG-02 `j`/`k` `[sandbox]`: the selection moves (the Diff pane keeps what it showed; the log has no preview); `enter` on a commit shows its files and their diff
- [ ] LOG-03 Move to the end of the log `[sandbox]`: more commits load, down to "Initial commit"
- [ ] LOG-04 `enter` on "Touch app and deep path" `[sandbox]`: its files show (see PATCH)
- [ ] LOG-05 `space` on an older commit `[sandbox]`: asks first (detached HEAD); `y` checks it out detached
- [ ] LOG-06 `space` with local changes that would be overwritten `[sandbox]`: git refuses; the message shows
- [ ] LOG-07 On a detached HEAD, `N`, name `from-detached` `[sandbox]`: a branch is made there and checked out
- [ ] LOG-08 Detached, make a commit, then `space` on another commit `[sandbox]`: "Leave commits behind?"; `enter` does nothing; `n`/`esc` stay; `y` moves HEAD

### Rewrites

- [ ] LOG-09 `s` on "Local change two" `[sandbox]` `[editor]`: it is squashed into "Local change one"; git's editor opens with both messages; the sandbox's uncommitted changes come back (staged ones unstaged)
- [ ] LOG-10 `F` on "Local change two" `[sandbox]`: folded into "Local change one", keeping that one's message; no editor
- [ ] LOG-11 `e` on "Tune app line 5" `[sandbox]` `[editor]`: git's editor opens; the new message is saved; `release` (which points above it) moves to the new copy
- [ ] LOG-12 `d` on "Local change two" `[sandbox]`: dropped without a question; its change is gone; it stays in the reflog
- [ ] LOG-13 `alt-k` on "Local change one" `[sandbox]`: it moves above "Local change two"
- [ ] LOG-14 `alt-j` on "Local change two" `[sandbox]`: it moves below "Local change one"; `alt-up`/`alt-down` do the same
- [ ] LOG-15 `shift-up`/`shift-down` in Commits `[sandbox]`: nothing happens
- [ ] LOG-16 `s` on "Add app" (the commit below it is a merge) `[sandbox]`: refused (no merges allowed)
- [ ] LOG-17 `s` / `F` on "Initial commit" `[sandbox]`: refused (first commit)
- [ ] LOG-18 `s` on a commit whose parent is not loaded yet `[sandbox]`: refused until you move down to the end of the log
- [ ] LOG-19 Git 2.38+, `alt-j` on "Add conflict.txt" (just above `release`'s commit) `[sandbox]`: refused, since it would change which commits `release` holds
- [ ] LOG-20 A rewrite where git's todo does not match what Kika asked for (edit the todo by hand while it runs) `[sandbox]`: git aborts; nothing changes
- [ ] LOG-21 `alt-j` twice on "fixup! Tune app line 10" (the second move puts it below "Tune app line 10") `[sandbox]`: the second rebase stops on a conflict in `src/app.txt`; Status shows it with continue, skip and abort
- [ ] LOG-22 While stopped, look above the log `[sandbox]`: "Rebase todo" lists the remaining steps, last to run at the top
- [ ] LOG-23 `j`/`k` `[sandbox]`: move through the todo rows and on into the log
- [ ] LOG-24 On a todo row: `alt-k` / `alt-j` `[sandbox]`: the row moves; git's todo list on disk changes at once
- [ ] LOG-25 On a todo row: `s`, `F`, `d`, `e`, `E`, `u` `[sandbox]`: the action changes to squash, fixup, drop, reword, edit, pick
- [ ] LOG-26 On a todo row: `b` `[sandbox]`: a stop is added after it; `b` again removes it
- [ ] LOG-27 Todo list holding an `exec` line (from `git rebase -i --exec true`) `[sandbox]`: shown under "Rebase todo (read-only)"; an edit key names the line and its number
- [ ] LOG-28 Change the todo on disk, then edit a row in Kika `[sandbox]`: refused (the list is not the one shown)
- [ ] LOG-29 While stopped: `c` in Status `[sandbox]`: continue
- [ ] LOG-30 While stopped: `a` in Status, `y` `[sandbox]`: abort; history as before
- [ ] LOG-31 `E` on "Tune app line 10" `[sandbox]`: the rebase stops there; amend with `A`, then `c` in Status finishes it
- [ ] LOG-32 `b` on "Tune app line 10" `[sandbox]`: the rebase stops right after it; `c` in Status finishes
- [ ] LOG-33 Run `scripts/make-sandbox.sh <dir> fixup`, unstage the rest (`git restore --staged src/util.txt new-name.txt old-name.txt`), then `ctrl-b` in Commits `[sandbox]`: `Fixup base: <hash> Add app`, and the cursor moves to "Add app" (the commit that last changed line 3). Without the unstage it names the 3 commits the staged lines come from and stays
- [ ] LOG-34 Stage lines last changed by several commits, `ctrl-b` `[sandbox]`: names the newest three, counts the rest, leaves the cursor
- [ ] LOG-35 Stage only a new file, `ctrl-b` `[sandbox]`: says there is no such commit
- [ ] LOG-36 Then `ctrl-f` on "Add app" `[sandbox]`: a commit `fixup! Add app` is made with `git commit --fixup`
- [ ] LOG-37 `S` `[sandbox]`: "Fold every fixup! commit?"; `enter` does nothing; `n`/`esc` leave the history
- [ ] LOG-38 `S`, `y` `[sandbox]`: every `fixup!` commit is folded into its target in one rewrite; no `fixup!` subjects remain
- [ ] LOG-39 `S` when a fixup's target is not loaded `[sandbox]`: the whole autosquash is refused
- [ ] LOG-40 `S` with `rebase.autoSquash=false` and a `squash!` commit present `[sandbox]`: fixups still fold; `squash!` stays as it is

### Ranges

- [ ] LOG-41 `V`, `j`, `s` (range of two above "Local change one") `[sandbox]` `[editor]`: the whole range folds into the commit just below it, in one rebase
- [ ] LOG-42 `V` over two commits, `F` `[sandbox]`: both fold into the commit below, keeping that one's message
- [ ] LOG-43 `V` over two commits, `d` `[sandbox]`: "Drop the selected commits?" names how many; `enter` does nothing; `y` drops them
- [ ] LOG-44 `V` with a `/` filter hiding a commit inside the range, `d` `[sandbox]`: refused; clear the filter first
- [ ] LOG-45 While a range is set: `e`, `alt-k`, `E`, `b` `[sandbox]`: wait until `esc` clears the range

### Revert and reset

- [ ] LOG-46 `R` on "Tune app line 5" `[sandbox]` `[editor]`: `git revert` runs on the terminal; git's editor opens; a revert commit is made
- [ ] LOG-47 `R` on the merge commit `[sandbox]`: refused (a merge commit cannot be reverted)
- [ ] LOG-48 `X` on "Local change one", "soft" `[sandbox]`: the branch moves; the later changes stay staged
- [ ] LOG-49 `X`, "mixed" `[sandbox]`: the branch moves; the changes stay in the worktree, unstaged
- [ ] LOG-50 `X`, "hard" `[sandbox]`: "Reset hard?"; `enter` does nothing; `n`/`esc` keep the changes; `y` resets and throws away every uncommitted change
- [ ] LOG-51 `R` or `X` during a merge in progress `[sandbox]`: they wait until it is finished or aborted

### Tags, copy and paste

- [ ] LOG-52 `t` on "Add notes", name `lw-tag` `[sandbox]`: a lightweight tag is made there; it shows in the Tags tab
- [ ] LOG-53 `T`, name `ann-tag`, message `a note` `[sandbox]`: an annotated tag (`◆`) is made
- [ ] LOG-54 `t` with `-bad`, `@`, `HEAD` or `a..b` `[sandbox]`: refused before git runs; the prompt stays open
- [ ] LOG-55 `c` on two commits of `feature/unmerged` (checked out), check out `main`, `v` `[sandbox]`: copied rows show `+`; the title and bar count them; `v` cherry-picks them parents first, on the terminal
- [ ] LOG-56 `c` again on a copied commit `[sandbox]`: takes it out of the set
- [ ] LOG-57 `v` again after pasting `[sandbox]`: git cherry-pick runs again (the branch holds copies, not the copied commits) and stops on a conflict or an empty pick, handled from Status. Copying a commit that is on the current branch (e.g. "Local change two" on `main`) and pressing `v` is refused: `<hash> is already on the current branch`
- [ ] LOG-58 `v` with tracked changes `[sandbox]`: waits for a clean worktree (untracked files may stay)
- [ ] LOG-59 `c` on the merge commit `[sandbox]`: refused
- [ ] LOG-60 `C` `[sandbox]`: the copied set is cleared
- [ ] LOG-61 Paste a commit that conflicts (`Theirs: change the shared line` onto `conflict/ours`) `[sandbox]`: stops; resolve, then continue, skip or abort from Status
- [ ] LOG-62 After a paste of two commits, `ctrl-z`, `y` `[sandbox]`: undoes one picked commit (`Undo: cherry-pick: <subject>`), newest first; a second `ctrl-z`, `y` undoes the other

### Bisect

- [ ] LOG-63 With a clean worktree, `B` on HEAD, "bad" `[sandbox]`: a bisect starts; nothing moves yet
- [ ] LOG-64 `B` on "Add app", "good" `[sandbox]`: git checks out a commit to test; Status takes over (see STATUS-11)
- [ ] LOG-65 `B`, "skip" `[sandbox]`: the selected commit is skipped
- [ ] LOG-66 `B`, "reset" `[sandbox]`: the bisect is reset
- [ ] LOG-67 `B` with tracked changes `[sandbox]`: refused
- [ ] LOG-68 `B` during a merge in progress `[sandbox]`: refused

### Filters and marks

- [ ] LOG-69 `/util` `[sandbox]`: only loaded commits matching the text show; `esc` clears it
- [ ] LOG-70 `L`, `docs/notes.txt`, `enter` `[sandbox]`: only "Move notes into docs" and "Add notes" show (followed across the rename); the title names the path; the graph is hidden
- [ ] LOG-71 `L`, `lib/paths`, `enter` `[sandbox]`: a directory works too
- [ ] LOG-72 `A`, `bob`, `enter` `[sandbox]`: only Bob Builder's commits show (name or email, ignoring case)
- [ ] LOG-73 `A`, `Bob` `[sandbox]`: an uppercase letter makes it case-sensitive
- [ ] LOG-74 `L` and `A` both set `[sandbox]`: both apply; an empty answer to one clears only that half
- [ ] LOG-75 While `L` or `A` is set: `s`, `F`, `d`, `e`, `alt-k`, `E`, `b` `[sandbox]`: they wait until the filter is cleared
- [ ] LOG-76 While `/` is set: `alt-k`/`alt-j` `[sandbox]`: they do not move log commits; todo rows stay editable
- [ ] LOG-77 `M` on "Add notes", `enter` on HEAD `[sandbox]`: the files changed from the mark to HEAD show in place of the log; the marked row shows `◆`
- [ ] LOG-78 `esc` order with a range, `/` filter, mark and `L` filter `[sandbox]`: clears the range, then the filter, then the mark, then the log filter, then quits
- [ ] LOG-79 `y` on a commit `[sandbox]`: the Copy menu offers full hash, short hash, subject, full message and Forge URL (see CLIP)
- [ ] LOG-80 Run `git commit --allow-empty -m gone` in another terminal, `r`, select "gone", run `git reset --soft HEAD~1` there, then `r` and at once `space` `[sandbox]`: nothing names the "gone" commit; `space` acts on the row the reloaded log shows

## Reflog

- [ ] REFLOG-01 `]` (or `[`) in Commits `[sandbox]`: the Reflog tab lists every place HEAD has been, newest first, with what moved it
- [ ] REFLOG-02 `j`/`k` `[sandbox]`: move through the entries
- [ ] REFLOG-03 `/checkout` `[sandbox]`: only matching rows show; `enter` keeps the filter; `esc` clears it; `esc` again quits
- [ ] REFLOG-04 `L` or `A` `[sandbox]`: does not filter; says path and author filters work on the log tab
- [ ] REFLOG-05 `ctrl-z` from the Reflog tab with a filter set `[sandbox]`: undo works on the last action, whatever the filter hides
- [ ] REFLOG-06 `]` again `[sandbox]`: back to the log
- [ ] REFLOG-07 On the Reflog tab, cursor on the second row, run `git checkout -q - && git checkout -q -` in another terminal, then `r` and at once `j` `[sandbox]`: the cursor ends one row below the entry it was on, in the reloaded list

## Commit files and custom patches

- [ ] PATCH-01 `enter` on "Touch app and deep path" `[sandbox]`: lists `lib/paths/deep.txt` and `src/app.txt`; the diff shows the selected file's change against the first parent
- [ ] PATCH-02 `j`/`k` `[sandbox]`: the next / previous file and its change show
- [ ] PATCH-03 `J`/`K`/`pgdn`/`pgup`, `+`/`-`, `w` `[sandbox]`: scroll, context and whitespace work on that change
- [ ] PATCH-04 `ctrl-f`, `app`, `n`, `N` `[sandbox]`: searches the selected file's change
- [ ] PATCH-05 `/deep` `[sandbox]`: filters the commit's files
- [ ] PATCH-06 `esc` order: filter, then search, then back to the log at the same commit `[sandbox]`
- [ ] PATCH-07 `h`/`left` `[sandbox]`: back to the log at the same commit
- [ ] PATCH-08 Focusing another panel `[sandbox]`: the Commits panel goes back to the log
- [ ] PATCH-09 `space` on `src/app.txt` `[sandbox]`: picked whole, shown with `●`; the Commits title names the patch's commit and counts its files
- [ ] PATCH-10 `space` again `[sandbox]`: the pick is dropped
- [ ] PATCH-11 `l` on `src/app.txt`, `space` on a hunk `[sandbox]`: that hunk's lines are picked; the file shows `◐`, the lines a `●`
- [ ] PATCH-12 In the diff, `v`, select one line, `space` `[sandbox]`: only that line is picked; `space` again drops it
- [ ] PATCH-13 In a commit's change: `t`, `d`, `e` `[sandbox]`: do nothing
- [ ] PATCH-14 Open another commit and pick from it `[sandbox]`: refused until the patch is reset
- [ ] PATCH-15 Pick from "Initial commit", the merge commit, or a comparison (`M` + `enter`) `[sandbox]`: refused
- [ ] PATCH-16 Pick from "Move notes into docs" (a rename) `[sandbox]`: refused
- [ ] PATCH-17 `ctrl-p` `[sandbox]`: the menu offers Remove from commit, Move to selected commit, Move to index, New commit after source…, Reset patch
- [ ] PATCH-18 "Reset patch" `[sandbox]`: the picks are forgotten; the repository is unchanged
- [ ] PATCH-19 With tracked changes, "Remove from commit" `[sandbox]`: refused (needs a clean worktree)
- [ ] PATCH-20 Clean worktree, "Remove from commit" `[sandbox]`: "Rewrite with the custom patch?"; `enter` does nothing; `n`/`esc` leave the history; `y` takes the change out of the commit; the patch is dropped
- [ ] PATCH-21 "Move to selected commit" with the cursor on a newer commit, `y` `[sandbox]`: the change moves into that commit
- [ ] PATCH-22 Same with an older commit, `y` `[sandbox]`: the change moves into the older commit and drops out of its source
- [ ] PATCH-23 "Move to index", `y` `[sandbox]`: the change leaves the commit and is staged
- [ ] PATCH-24 "New commit after source…", message, `y` `[sandbox]`: the change becomes its own commit right after its source
- [ ] PATCH-25 Pick from "Add a binary file", "Remove from commit", `y` `[sandbox]`: refused when the rewrite starts; nothing changes
- [ ] PATCH-26 A patch whose lines sit next to other changes of their commit, rewrite `[sandbox]`: the pick conflicts; the rebase is aborted; HEAD, index and worktree are as before and the patch is kept
- [ ] PATCH-27 Quit and restart Kika `[sandbox]`: the patch is gone (it lives only while Kika runs)

## Stash and stash files

- [ ] STASH-01 `5` `[sandbox]`: lists `stash@{0}` "WIP readme tweak" and `stash@{1}` "WIP with untracked file"
- [ ] STASH-02 `enter` on `stash@{1}` `[sandbox]`: its files show; `stashed-untracked.txt` shows as added
- [ ] STASH-03 In stash files: `j`/`k` `[sandbox]`: each file's change against the commit it was made on
- [ ] STASH-04 In stash files: `ctrl-f`, `stashed`, `n`, `N` `[sandbox]`: searches the change
- [ ] STASH-05 In stash files: `esc` with a search `[sandbox]`: clears it; `esc` again (or `bksp`) goes back to the list at the same stash
- [ ] STASH-06 `space` on `stash@{0}` `[sandbox]`: applied to the worktree; it stays in the list
- [ ] STASH-07 `g` on `stash@{0}` (clean worktree) `[sandbox]`: applied and dropped
- [ ] STASH-08 `g` on a stash that conflicts with the worktree `[sandbox]`: the conflict shows; the stash is kept
- [ ] STASH-09 `d`, then `y` `[sandbox]`: "Drop stash?"; `enter` does nothing; `n`/`esc` keep it; `y` drops it
- [ ] STASH-10 `R` on `stash@{1}`, new message `renamed` `[sandbox]`: it moves to `stash@{0}` with the new message
- [ ] STASH-11 `R` on a stash that shares its commit with `stash@{0}` `[sandbox]`: refused; nothing changes
- [ ] STASH-12 `/readme` `[sandbox]`: only matching rows show; never reaches git
- [ ] STASH-13 `V` over both, `d` `[sandbox]`: "Delete the selection?" names how many and which; `y` drops both, deepest first
- [ ] STASH-14 While a range is set: `enter`, `space`, `g`, `R` `[sandbox]`: wait until `esc` clears the range
- [ ] STASH-15 Drop a stash in another terminal, then `d` in Kika on a stale row `[sandbox]`: refused; the list reloads
- [ ] STASH-16 `ctrl-z` after a stash `[sandbox]`: a stash cannot be undone; says so or offers the previous action
- [ ] STASH-17 Select `stash@{1}`, run `git stash drop stash@{1}` in another terminal, then `r` and at once `space` `[sandbox]`: "WIP with untracked file" is never applied; `space` acts on the row the reloaded list shows

## Submodules

- [ ] SUB-01 `S` in Files `[sandbox]`: lists `vendor/lib` (checked out) and `vendor/extra` (not initialized)
- [ ] SUB-02 `j`/`k`, `esc` or `q` `[sandbox]`: move; close the list; `ctrl-c` quits
- [ ] SUB-03 `enter` on `vendor/lib` `[sandbox]`: Kika works in the submodule (its files, branches, commits)
- [ ] SUB-04 In the submodule, `bksp` `[sandbox]`: back to the parent repository
- [ ] SUB-05 `i` on `vendor/extra` `[sandbox]`: its URL is recorded without cloning
- [ ] SUB-06 `u` on `vendor/extra` `[sandbox]`: it is cloned and checked out at the recorded commit (needs `protocol.file.allow`, which `env.sh` sets)
- [ ] SUB-07 Change the submodule's commit (commit inside `vendor/lib`), back in the parent `[sandbox]`: Files shows `vendor/lib` changed; `tool` refuses it (see HUNK-23)
- [ ] SUB-08 In the submodule, a `files` custom command `[sandbox]`: runs in the submodule's top level
- [ ] SUB-09 In the submodule, `i` on an untracked file `[sandbox]`: writes the submodule's `.gitignore`
- [ ] SUB-10 In the submodule with `repo/.kika.toml` remapping a key `[sandbox]`: the parent's `.kika.toml` applies

## Custom commands

- [ ] CMD-01 `ctrl-t` on `src/app.txt` `[sandbox]`: a popup titled "Echo the file path" shows `path: src/app.txt`; `esc` closes it
- [ ] CMD-02 `ctrl-u`, answer `hello` and `world` `[sandbox]`: two prompts, then a popup shows `hello world`
- [ ] CMD-03 `ctrl-u`, `esc` at the first prompt `[sandbox]`: nothing runs
- [ ] CMD-04 `ctrl-k` (sleep 30), then `ctrl-x` `[sandbox]`: the bar shows it running; `ctrl-x` stops it
- [ ] CMD-05 `ctrl-k`, then `ctrl-k` again while it runs `[sandbox]`: only one runs at a time; the second is refused
- [ ] CMD-06 `ctrl-g` in Files (`false`) `[sandbox]`: shown as failed
- [ ] CMD-07 `ctrl-o` `[sandbox]`: Kika hands the terminal over; it shows `file: <path>`; `enter` comes back to Kika
- [ ] CMD-08 In the Diff pane on a hunk, `ctrl-g` `[sandbox]`: a popup shows the path and the selected change's line
- [ ] CMD-09 In Blame on a committed line, `ctrl-y` `[sandbox]`: a popup shows `git show --stat` for that line's commit
- [ ] CMD-10 In Blame on line 1 (not committed), `ctrl-y` `[sandbox]`: nothing runs; says the line is not committed yet
- [ ] CMD-11 `ctrl-t` with no file selected (clean worktree) `[sandbox]`: nothing runs; says no file is selected
- [ ] CMD-12 `?` in Files `[sandbox]`: the commands are listed after the panel's own keys; they are not in the bottom bar
- [ ] CMD-13 A command that creates a file (`["touch", "{prompt1}"]`) `[sandbox]`: the file list reloads and shows the new file
- [ ] CMD-14 A file named `a b;$(touch x)` and `ctrl-t` `[sandbox]`: the name arrives as one argument; no file `x` is made
- [ ] CMD-15 A file named `-n` and `ctrl-t` `[sandbox]`: the path is passed as `./-n`
- [ ] CMD-16 Global `[[commands]]` on `q` in `files` `[sandbox]`: error; the whole global file is ignored
- [ ] CMD-17 Global `[[commands]]` with `["sh", "-c", "echo {path}"]` `[sandbox]`: rejected (placeholder in the script); the whole file is ignored
- [ ] CMD-18 Global `[[commands]]` with `["sh", "-c", "echo \"$1\"", "sh", "{path}"]` `[sandbox]`: accepted and works
- [ ] CMD-19 Global `[keys] "files.stage" = ["ctrl-t"]` with the `ctrl-t` command `[sandbox]`: the command is dropped with `key "ctrl-t" is bound to both "files.stage" and command 1 (…); command 1 was dropped`
- [ ] CMD-20 `repo/.kika.toml` binding `"files.stage" = ["ctrl-t"]` `[sandbox]`: that entry is dropped; the command keeps `ctrl-t`
- [ ] CMD-21 Global `[[commands]]` with an unknown placeholder `{foo}` `[sandbox]`: error; the whole file is ignored
- [ ] CMD-22 A `{prompt2}` with only one entry in `prompts` `[sandbox]`: error
- [ ] CMD-23 A command printing more than 256 KiB in popup mode `[sandbox]`: it is stopped (no process left); the popup, titled with the command's description (not "… failed"), ends with "[output truncated]"

## Menus, questions, prompts, filters and errors

- [ ] POPUP-01 In any menu: `j`/`k` `[sandbox]`: the cursor moves
- [ ] POPUP-02 `enter` `[sandbox]`: runs the highlighted item
- [ ] POPUP-03 `esc` in a menu opened from another menu `[sandbox]`: back to the menu below
- [ ] POPUP-04 `q` and `?` in a menu `[sandbox]`: do not quit or open help
- [ ] POPUP-05 `ctrl-c` in a menu `[sandbox]`: Kika quits
- [ ] POPUP-06 A plain yes/no question (e.g. `space` on a commit): `y` and `enter` answer yes; `n` and `esc` no `[sandbox]`
- [ ] POPUP-07 A text prompt (e.g. `n` in Branches): `enter` uses the text; `esc` closes without it; `ctrl-c` quits `[sandbox]`
- [ ] POPUP-08 A filter on the bottom bar (`/` in Files): `q` and `?` are typed; the list follows each key `[sandbox]`
- [ ] POPUP-09 In the filter: `enter` keeps the text; `esc` clears it and closes; `ctrl-c` quits `[sandbox]`
- [ ] POPUP-10 An error popup (e.g. NET-11): `esc` or `enter` closes it; `ctrl-c` quits `[sandbox]`
- [ ] POPUP-11 Pick list (e.g. merge method, base branch): `j`/`k`, `enter` takes the row; `esc` or `q` closes and stops; `ctrl-c` quits `[github-read]`
- [ ] POPUP-12 Repository text with control or bidi characters (a file named with U+202E, a commit subject with `\x1b`) `[sandbox]` `[visual]`: shown as `U+FFFD` or a visible `<U+XXXX>`; the layout is not broken

## Destructive confirms

Each question below answers only to `y` (or `D` where noted). Check all four
keys on each: `enter` does nothing, `n` cancels, `esc` cancels, `y` does it.

- [ ] CONFIRM-01 Discard this work? (`d` on a file in Files) `[sandbox]`: `enter` nothing; `n`/`esc` keep; `y` discards
- [ ] CONFIRM-02 Discard this work? (`d` on a hunk, and on lines) `[sandbox]`: same
- [ ] CONFIRM-03 Discard this work? (`V` range, `d`) `[sandbox]`: same, once for the range
- [ ] CONFIRM-04 Delete this branch? (merged `feature/merged`) `[sandbox]`: `enter` nothing; `n`/`esc` keep; `y` deletes
- [ ] CONFIRM-05 Delete this branch? (unmerged `feature/unmerged`) `[sandbox]`: `enter` and `y` nothing; `n`/`esc` keep; `D` deletes
- [ ] CONFIRM-06 Remove this remote? (`backup`) `[remote]`: `enter` nothing; `n`/`esc` keep; `y` removes
- [ ] CONFIRM-07 Delete this tag? (`v0.1.0`) `[sandbox]`: `enter` nothing; `n`/`esc` keep; `y` deletes
- [ ] CONFIRM-08 Delete the selection? (branches range) `[sandbox]`: `enter` nothing; `n`/`esc` keep all; `y` deletes the merged ones
- [ ] CONFIRM-09 Delete the selection? (stash range) `[sandbox]`: same; `y` drops them
- [ ] CONFIRM-10 Reset hard? `[sandbox]`: `enter` nothing; `n`/`esc` keep the changes; `y` resets
- [ ] CONFIRM-11 Leave commits behind? `[sandbox]`: `enter` nothing; `n`/`esc` stay; `y` moves HEAD
- [ ] CONFIRM-12 Drop stash? `[sandbox]`: `enter` nothing; `n`/`esc` keep; `y` drops
- [ ] CONFIRM-13 Abort the operation in progress? (merge, rebase, cherry-pick, revert, bisect reset) `[sandbox]`: `enter` nothing; `n`/`esc` keep going; `y` aborts
- [ ] CONFIRM-14 Skip the stopped commit? `[sandbox]`: `enter` nothing; `n`/`esc` keep the work and the stop; `y` skips
- [ ] CONFIRM-15 Fold every fixup! commit? `[sandbox]`: `enter` nothing; `n`/`esc` leave the history; `y` folds
- [ ] CONFIRM-16 Rewrite with the custom patch? `[sandbox]`: `enter` nothing; `n`/`esc` leave the history; `y` rewrites
- [ ] CONFIRM-17 Drop the selected commits? `[sandbox]`: `enter` nothing; `n`/`esc` leave the history; `y` drops; if the range changed meanwhile, `y` drops nothing
- [ ] CONFIRM-18 Undo or redo? `[sandbox]`: `enter` nothing; `n`/`esc` leave everything; `y` moves
- [ ] CONFIRM-19 `Remove worktree <path>?` `[sandbox]`: `enter` nothing; `n`/`esc` keep; `y` removes it and its folder
- [ ] CONFIRM-20 Discard the typed message? `[sandbox]`: `enter` nothing; `n`/`esc` back to the message; `y` discards
- [ ] CONFIRM-21 Force push with lease? (after a rejected push) `[remote]`: `n`/`esc` force nothing; only `y` pushes
- [ ] CONFIRM-22 Create this pull request? `[github-write]`: `enter` nothing; `n`/`esc` create nothing; `y` creates
- [ ] CONFIRM-23 Merge this pull request? `[github-write]`: `enter` nothing; `n`/`esc` merge nothing; `y` merges
- [ ] CONFIRM-24 Check out this pull request? `[github-read]`: `enter` nothing; `n`/`esc` check out nothing and fetch nothing; `y` checks out `pr/<n>`
- [ ] CONFIRM-25 Delete this comment? / Discard this comment? (review) `[github-read]`: `enter` nothing; `n`/`esc` keep; `y` deletes or discards
- [ ] CONFIRM-26 Post this reply / comment? (review) `[github-write]`: `enter` nothing; `n`/`esc` keep the text unposted; `y` posts
- [ ] CONFIRM-27 `ctrl-c` on any of the questions above `[sandbox]`: Kika quits without doing it

## Display, theme, NO_COLOR and mouse

### Theme and colour

- [ ] DISPLAY-01 `[theme] name = "default"` `[sandbox]` `[visual]`: the usual colours on a dark background; plain text keeps the terminal's colour
- [ ] DISPLAY-02 `name = "light"` on a light terminal `[sandbox]` `[visual]`: every role readable on white (256-colour indexes)
- [ ] DISPLAY-03 `name = "high-contrast"` `[sandbox]` `[visual]`: bright colours, white text, gray dim text
- [ ] DISPLAY-04 `[theme.colors] added = "light_green"`, `match_bg = 229`, `hash = "#005f87"` `[sandbox]` `[visual]`: each role changes, the rest of the theme stays
- [ ] DISPLAY-05 `[theme.colors] text = "reset"` on `light` `[sandbox]` `[visual]`: plain text in the terminal's own colour
- [ ] DISPLAY-06 `added = "Green"`, `"#fff"`, `256`, `name = "dark"` or an unknown role `[sandbox]`: error naming the key and what to write; the whole file is ignored
- [ ] DISPLAY-07 Selected rows `[sandbox]` `[visual]`: reverse video in every theme
- [ ] DISPLAY-08 Search matches in the diff `[sandbox]` `[visual]`: every match on `match_bg`, the current one on `current_match_bg`
- [ ] DISPLAY-09 `NO_COLOR=1 kika-dev` `[sandbox]` `[visual]`: no colour anywhere; statuses, marks and badges still show as letters and symbols
- [ ] DISPLAY-10 `NO_COLOR=1` with `[theme] name = "light"` and `[theme.colors]` `[sandbox]` `[visual]`: still no colour (NO_COLOR wins)
- [ ] DISPLAY-11 `NO_COLOR=1` with a `[diff] pager` `[sandbox]` `[visual]`: the pager's output has no colour either
- [ ] DISPLAY-12 `NO_COLOR=1` in the commit editor and a review detail `[sandbox]` `[visual]`: no colour; review mode's neutral rows are dimmed, not coloured
- [ ] DISPLAY-13 `NO_COLOR=` (empty) `[sandbox]` `[visual]`: colour stays
- [ ] DISPLAY-14 Set or clear `NO_COLOR` while Kika runs `[sandbox]`: no change until restart
- [ ] DISPLAY-15 A diff line with an emoji ZWJ sequence, default config `[sandbox]` `[visual]`: the joiner shows as `<U+200D>`
- [ ] DISPLAY-16 `[display] emoji_zwj_terminals = ["<your $TERM_PROGRAM>"]` `[sandbox]` `[visual]`: diff and blame body lines show the picture; file names and headers still escape
- [ ] DISPLAY-17 `emoji_zwj_terminals = [""]` `[sandbox]`: refused; the whole file is ignored
- [ ] DISPLAY-18 Narrow terminal (80 columns, then 60) `[sandbox]` `[visual]`: panels fit; nothing runs past a pane's edge

### Mouse (`[display] mouse`, on by default)

- [ ] MOUSE-01 Click a row in Files `[mouse]`: it is selected
- [ ] MOUSE-02 Click rows in Branches (Local, Remotes incl. a remote's own row, Tags), Commits (Log, Reflog, an opened commit's files), Stash (list, an opened stash's files) `[mouse]`: each is selected
- [ ] MOUSE-03 Click another panel or its border `[mouse]`: it takes focus; a collapsed panel keeps its selection
- [ ] MOUSE-04 Click the Diff pane `[mouse]`: it takes focus
- [ ] MOUSE-05 Wheel over the expanded list `[mouse]`: the selection moves 3 rows a notch; focus does not move
- [ ] MOUSE-06 Wheel over the Diff pane `[mouse]`: scrolls 3 lines a notch; focus does not move
- [ ] MOUSE-07 Click a row in a menu, the pick list, Submodules, Worktrees, Recent repositories or Command log `[mouse]`: the cursor moves there; nothing runs until `enter`
- [ ] MOUSE-08 Wheel inside those popups `[mouse]`: the cursor moves 3 rows a notch
- [ ] MOUSE-09 Click outside the popup, or on a popup below the top one `[mouse]`: nothing
- [ ] MOUSE-10 Mouse while a popup's filter is being typed `[mouse]`: nothing
- [ ] MOUSE-11 In help: wheel scrolls 3 lines; a click does nothing `[mouse]`
- [ ] MOUSE-12 Click while a question, prompt, error, password prompt, commit message or filter is open `[mouse]`: nothing; a click never answers
- [ ] MOUSE-13 Help's last line `[sandbox]`: says the mouse clicks and scrolls
- [ ] MOUSE-14 Shift-drag (Option-drag on macOS) `[mouse]`: the terminal selects text anyway
- [ ] MOUSE-15 `[display] mouse = false`, restart `[mouse]`: clicks and wheel do nothing in Kika; plain drag selects text
- [ ] MOUSE-16 Open the editor (`o`) or a terminal custom command, then click and scroll `[mouse]` `[editor]`: the other program gets no mouse codes from Kika; Kika captures again when it comes back
- [ ] MOUSE-17 Quit Kika, then click in the shell `[mouse]`: no mouse codes printed
- [ ] MOUSE-18 Review queue: click a row `[mouse]` `[github-read]`: the cursor moves; clicking the tabs line does nothing
- [ ] MOUSE-19 Review files: click a file, click the diff, wheel over each `[mouse]` `[github-read]`: the file is selected and the list gets the keys; clicking the diff gives it the keys; the wheel moves the list 3 rows and scrolls the diff 3 lines
- [ ] MOUSE-20 Review detail and conversation: wheel `[mouse]` `[github-read]`: scrolls 3 lines a notch
- [ ] MOUSE-21 Pending list, thread reader, line pick and comment box: click and wheel `[mouse]` `[github-read]`: ignored

## Clipboard

- [ ] CLIP-01 `y` in Commits, "Full hash" `[sandbox]` `[visual]`: "Copied" shows; pasting in the terminal gives the full hash (OSC 52)
- [ ] CLIP-02 "Short hash", "Subject", "Full message" `[sandbox]` `[visual]`: each pastes the right text
- [ ] CLIP-03 Inside tmux without `set -g set-clipboard on` `[visual]`: "Copied" shows but nothing reaches the clipboard
- [ ] CLIP-04 "Forge URL" with no `[forge]` `[sandbox]`: off, says why
- [ ] CLIP-05 `[clipboard] command = ["pbcopy"]`, restart, "Full hash" `[sandbox]`: `pbpaste` prints the hash; no OSC 52 is sent
- [ ] CLIP-06 Then `@` `[sandbox]`: the Command log lists `pbcopy` with its arguments, never the text
- [ ] CLIP-07 `command = ["false"]` `[sandbox]`: "Could not copy"
- [ ] CLIP-08 `command = ["sleep", "10"]` `[sandbox]`: after 5 seconds it is stopped and "Could not copy" shows; Kika never waits while you type
- [ ] CLIP-09 `command = "pbcopy"` (a string), `[]`, or `["bin/copy"]` `[sandbox]`: `clipboard.command` error; the whole file is ignored
- [ ] CLIP-10 Copy a commit message longer than 64 KB `[sandbox]`: refused
- [ ] CLIP-11 Copy a message with control characters `[sandbox]`: they are cleaned first

## Forge on the workspace

Use a clone of a GitHub repository you can read, with `[forge] github =
["github.com"]` and `browser = ["open"]`.

- [ ] FORGE-01 Open the clone `[github-read]`: Branches shows badges such as `#123 open ✓`, `draft`, `merged`, `closed`, with `✓`, `✗` or `●`
- [ ] FORGE-02 `@` `[github-read]`: each `gh` call has its own line; the ETag shows as `<etag>`
- [ ] FORGE-03 `B` `[github-read]`: the badges are asked for again
- [ ] FORGE-04 `B` with no change on GitHub `[github-read]`: only the probe runs (no GraphQL call) unless a check is running or failed or 15 minutes passed
- [ ] FORGE-05 Restart Kika offline `[github-read]`: cached badges show at once, marked `· cached`; the bottom line says `GitHub: offline`
- [ ] FORGE-06 `gh auth logout` (or a fresh `GH_CONFIG_DIR`) `[github-read]`: `GitHub: not logged in` and, if wide enough, `run gh auth login --hostname github.com`
- [ ] FORGE-07 `PATH` without `gh` `[github-read]`: `GitHub: gh not installed · install the GitHub CLI to see pull requests`; no `gh` call is made
- [ ] FORGE-08 Install `gh` (fix `PATH`) and press `B` `[github-read]`: the check runs again and the badges load without a restart
- [ ] FORGE-09 `[forge.allow] "github.com" = ["someone-else"]` `[github-read]`: no badges; `B` says the repo is not allowed; no `gh` call in the Command log
- [ ] FORGE-10 Sandbox (origin is a local path) with `[forge] github` set, `B` `[sandbox]`: says the remote is on no listed host
- [ ] FORGE-11 `[forge] github = ["https://github.com"]` or `["GitHub.com"]` `[sandbox]`: error; the whole file is ignored
- [ ] FORGE-12 `f`, `p` or `P` in the clone `[github-read]`: the badges are read again afterwards
- [ ] FORGE-13 `o` on a branch with a pull request, "Open in browser" `[github-read]`: the pull request page opens; the browser command shows in the Command log
- [ ] FORGE-14 `o`, "Open in browser" with no `browser` `[github-read]`: off, says so
- [ ] FORGE-15 `o`, "Copy URL" `[github-read]`: the pull request's `https://` URL is copied, with no user, token or port
- [ ] FORGE-16 `y` in Commits, "Forge URL" `[github-read]`: the commit's page URL is copied
- [ ] FORGE-17 `o`, "Create pull request…" on a branch already with a pull request `[github-read]`: off
- [ ] FORGE-18 "Create pull request…" on a pushed branch with none, one candidate remote `[github-write]`: asks the base branch (starting on `main`), then the title (starting from the branch name), then "Create this pull request?" naming repo, `head → base` and title; `y` creates it; the new badge shows at once
- [ ] FORGE-19 Same with several candidate remotes (your own copy of the repo plus the upstream) `[github-write]`: Target remote is asked first; a head pushed to another repository is sent as `owner:branch`; a pull request on another remote's repo gets no badge and Kika names its number
- [ ] FORGE-20 "Create pull request…" on a branch that is not pushed `[github-read]`: refused; asks nothing
- [ ] FORGE-21 GitHub refuses the create (e.g. no commits between) `[github-write]`: GitHub's message shows
- [ ] FORGE-22 Rate limited `[github-read]`: `GitHub: rate limited · until HH:MM UTC`; `B` is refused with a notice; no `gh` call to that host until the reset

## Review mode

Use a GitHub repository with open pull requests. Items that post, approve,
merge or mark viewed are `[github-write]`; run them only on a test repo you
own, by hand.

### Queues

- [ ] REVIEW-01 `G` in the workspace `[github-read]`: review mode lists four queues: assigned to you, review requested, authored by you, all open
- [ ] REVIEW-02 `tab` / `shift-tab` `[github-read]`: next / previous queue
- [ ] REVIEW-03 `j`/`k` `[github-read]`: move through the pull requests
- [ ] REVIEW-04 `/` then a number, title word or author `[github-read]`: filters the queue; `esc` clears the filter; `esc` again goes back to the workspace
- [ ] REVIEW-05 `r` `[github-read]`: the queues are asked for again (one `gh api graphql` line in the Command log)
- [ ] REVIEW-06 A queue with more than 50 open pull requests `[github-read]`: only the first 50 show
- [ ] REVIEW-07 `G` with the remote on an unlisted host `[sandbox]`: says so and asks nobody

### Detail and checks

- [ ] REVIEW-08 `enter` on a pull request `[github-read]`: the detail shows title, author, state, `base ← head`, mergeability, labels, reviewers, CI checks, description and the number of unresolved threads
- [ ] REVIEW-09 `j`/`k`, `pgdn`/`pgup` `[github-read]`: the detail scrolls
- [ ] REVIEW-10 A description with headings, lists, quotes, code, emphasis and links `[github-read]` `[visual]`: rendered; images, tables and HTML show as text
- [ ] REVIEW-11 Under mergeability `[github-read]`: `Merge: merge commit · squash · rebase` (only allowed methods) or `Cannot merge: …`
- [ ] REVIEW-12 `o` with one failing check `[github-read]`: its page opens with `browser`
- [ ] REVIEW-13 `o` with several failing checks `[github-read]`: a pick list asks which
- [ ] REVIEW-14 `o` with no failing check or no `browser` `[github-read]`: says why
- [ ] REVIEW-15 `r` on the detail `[github-read]`: the detail and then the threads are read again
- [ ] REVIEW-16 `esc` `[github-read]`: back to the queue

### Conversation

- [ ] REVIEW-17 `C` `[github-read]`: comments and review bodies, oldest first, each with author, time and `commented`, `approved` or `requested changes`
- [ ] REVIEW-18 `j`/`k`, `pgdn`/`pgup` `[github-read]`: scroll
- [ ] REVIEW-19 A pull request with more than 200 conversation items `[github-read]`: ends with `More on GitHub (showing the first N)`
- [ ] REVIEW-20 `esc` `[github-read]`: back to the pull request
- [ ] REVIEW-21 `a` on the conversation, type, `ctrl-s` `[github-write]`: asks to post; only `y` posts the comment on the pull request

### Files and diff

- [ ] REVIEW-22 `f` `[github-read]`: changed files with status, viewed mark (`✓`, `~`, blank, `?`) and added/removed counts; the selected file's diff from the merge base
- [ ] REVIEW-23 `f` when the head is not local `[github-read]`: fetched once by id from the matching remote; no ref or tag is written
- [ ] REVIEW-24 `f` with a remote that has several URLs `[github-read]`: says so; fetches nothing
- [ ] REVIEW-25 `j`/`k` in the list `[github-read]`: pick a file; its diff shows
- [ ] REVIEW-26 `tab` `[github-read]`: the keys move to the diff; `j`/`k`, `pgdn`/`pgup` scroll it; `tab` back
- [ ] REVIEW-27 In the diff: `/word`, `n`, `N`, `esc` `[github-read]`: search the loaded lines; `esc` clears, then goes back to the list
- [ ] REVIEW-28 `c` `[github-read]`: offers "Whole pull request" and each commit, oldest first, as `<id> <subject>`
- [ ] REVIEW-29 Pick a commit `[github-read]`: files and diff against its first parent; the title says `commit 2/5 <id>`; the selected file stays when present
- [ ] REVIEW-30 `a` in a commit's view `[github-read]`: refused; works only in the whole pull request view
- [ ] REVIEW-31 `esc` chain `[github-read]`: files → pull request → queue → workspace

### Threads

- [ ] REVIEW-32 A file with threads `[github-read]`: each shows under its line with author, time and resolved state; resolved ones folded to one line; outdated ones at the top of the file
- [ ] REVIEW-33 Diff title `[github-read]`: `threads loading…`, then the unresolved count; `+` when GitHub has more than were read
- [ ] REVIEW-34 `]` / `[` `[github-read]`: next / previous unresolved thread across files; stop at the first and last; title says `thread 2/5`
- [ ] REVIEW-35 A thread marked **addressed** `[github-read]` `[visual]`: green; it is unresolved and its lines changed in a later push
- [ ] REVIEW-36 `enter` `[github-read]`: the selected (or the file's first) thread opens in a box with every comment in full
- [ ] REVIEW-37 In the thread: `j`/`k`, `pgdn`/`pgup`, `]`/`[` `[github-read]`: scroll; step through all of the file's threads, resolved ones too
- [ ] REVIEW-38 In the thread: `q` and `?` `[github-read]`: do nothing; `esc` closes; `]` on the diff goes on from the thread read last
- [ ] REVIEW-39 In the thread: `a`, type, `ctrl-s`, `y` `[github-write]`: the reply is posted
- [ ] REVIEW-40 In the thread: `a`, type, `ctrl-s`, `n` `[github-read]`: nothing is posted; the text is kept
- [ ] REVIEW-41 In the thread: `R` `[github-write]`: resolves it on GitHub; `R` again unresolves it
- [ ] REVIEW-42 A commit's view with threads `[github-read]`: the file's threads show above its diff as "on the whole pull request"
- [ ] REVIEW-43 Threads fail to load `[github-read]`: title says `threads unavailable`; `esc`, `r` tries again

### Pending comments

- [ ] REVIEW-44 `a` on the files screen `[github-read]`: line pick starts on the first numbered line in view
- [ ] REVIEW-45 In the pick: `j`/`k` `[github-read]`: move over numbered lines only (file and `@@` headers skipped)
- [ ] REVIEW-46 In the pick: `v`, `j`, `j` `[github-read]`: a range; it stays inside one hunk; `v` again drops it
- [ ] REVIEW-47 In the pick: `q` and `?` do nothing; `esc` stops picking `[github-read]`
- [ ] REVIEW-48 `enter` (or `a`) on an added line `[github-read]`: the comment box opens for the new side
- [ ] REVIEW-49 On a removed line `[github-read]`: the comment is on the old side
- [ ] REVIEW-50 In the box: `enter` is a new line; `tab` types a tab; `q` and `?` are typed `[github-read]`
- [ ] REVIEW-51 `ctrl-s` `[github-read]`: kept in the pending review; the diff shows `✎ pending: <first line>`; the list title counts pending comments; nothing is posted
- [ ] REVIEW-52 `esc` in an empty box `[github-read]`: closes; with text, "Discard this comment?" asks first, only `y` discards
- [ ] REVIEW-53 `ctrl-e` in the box `[github-read]` `[editor]`: the comment opens in git's editor; closing it saves, as `ctrl-s`
- [ ] REVIEW-54 In the pick: `s` on a range `[github-read]`: the box opens with a code suggestion for those lines
- [ ] REVIEW-55 `P` `[github-read]`: pending comments listed as `path:L12` or `path:R10-R14` with their first line
- [ ] REVIEW-56 In the list: `j`/`k`, `enter` or `e` `[github-read]`: edit the selected comment in the same box
- [ ] REVIEW-57 In the list: `d`, then `enter`, `n`, `y` `[github-read]`: `enter` nothing, `n` keeps, `y` deletes
- [ ] REVIEW-58 In the list: `esc` closes; `ctrl-c` quits `[github-read]`
- [ ] REVIEW-59 Quit and restart, open the same pull request `[github-read]`: the pending comments are still there (file in `$XDG_STATE_HOME/kika/reviews`)
- [ ] REVIEW-60 Replace the pending file with a malformed one or a symlink `[github-read]`: comments on that pull request are off; Kika names the file's full path
- [ ] REVIEW-61 Writing comments `[github-read]`: asks GitHub nothing (no new Command log lines)

### Submit, viewed marks, line links

- [ ] REVIEW-62 `S` on the files screen (or in `P`) `[github-write]`: offers approve, comment, request changes and a summary; the pending comments go out as one review; the pending review is cleared
- [ ] REVIEW-63 `S`, then cancel at the method or summary `[github-read]`: nothing is sent; pending comments stay
- [ ] REVIEW-64 `v` on a file `[github-write]`: dim `…` until GitHub answers, then `✓`; `v` again clears it
- [ ] REVIEW-65 `v` in a commit's view `[github-write]`: marks the file for the whole pull request
- [ ] REVIEW-66 A file changed since you viewed it `[github-read]`: shows `~`
- [ ] REVIEW-67 `o` on the files screen `[github-read]`: opens the top numbered line on GitHub's files page; the bar says `Opening R12 on GitHub: <path>`
- [ ] REVIEW-68 `]` to a thread, then `o` `[github-read]`: opens that thread's line
- [ ] REVIEW-69 `o` in a commit's view `[github-read]`: the URL is `…/pull/<n>/commits/<id>#diff-…`
- [ ] REVIEW-70 `o` with no `browser` or no numbered line in view `[github-read]`: off, says why

### Merge and checkout

- [ ] REVIEW-71 `M` on a mergeable pull request with several methods `[github-write]`: asks the method, then `Merge #N <title> into <base> by <method>?`; `enter` nothing; `y` merges; the detail says merged; it leaves the queues; the badge turns `merged`
- [ ] REVIEW-72 `M` where a non-required check failed `[github-write]`: the question adds "Some checks have not passed"
- [ ] REVIEW-73 `M` on a draft, closed, merged, conflicting or behind pull request `[github-read]`: refused, saying why in Kika's own words
- [ ] REVIEW-74 `M`, `n` or `esc` `[github-read]`: nothing is merged
- [ ] REVIEW-75 Merge after the head moved on GitHub `[github-write]`: GitHub refuses; its message is shown cut short and escaped
- [ ] REVIEW-76 `b` on the detail `[github-read]`: "Check out this pull request?" names `pr/<n>`, what happens to it and the fetch remote; `y` checks it out and goes to the workspace
- [ ] REVIEW-77 `b` when `pr/<n>` has commits the pull request lacks, or is checked out in another worktree `[github-read]`: never moved; says why
- [ ] REVIEW-78 `b` with local changes that block the switch `[github-read]`: git refuses; the branch stays where it was
- [ ] REVIEW-79 `b` when `pr/<n>` is behind `[github-read]`: switched to, then fast-forwarded
- [ ] REVIEW-80 Detach HEAD and make a commit, then `b` and `y` on the detail `[github-read]`: "Leave commits behind?" names `pr/<n>`, and its hint says to press `n`, `esc` back to the workspace, then `N` in Commits to create a branch at the detached commit; `enter` does nothing; `n`/`esc` stay in review with HEAD where it was; `y` checks out `pr/<n>` and goes to the workspace

## GitLab

Needs a GitLab host with `[forge] gitlab = ["<host>"]` and `gitlab_ask =
["<host>"]`, `glab` logged in, and a clone of a project you can read. Items
marked "(writes)" change GitLab: run them only on a test project, by hand.

- [ ] GITLAB-01 `gitlab` only (no `gitlab_ask`) `[gitlab]`: no badges; no `glab` call in the Command log; `o` → Copy URL still builds the merge-request list URL
- [ ] GITLAB-02 With `gitlab_ask`: Branches shows `!12 open ✓` style badges; `locked` shows as open `[gitlab]`
- [ ] GITLAB-03 Pipelines `[gitlab]`: `✓` passed, `✗` failed or cancelled, `●` running/pending/scheduled, nothing for manual or skipped
- [ ] GITLAB-04 `@` `[gitlab]`: each refresh is one `glab api graphql --include --hostname <host> --input -` line; no probe
- [ ] GITLAB-05 `B` `[gitlab]`: the query runs again
- [ ] GITLAB-06 `PATH` without `glab` `[gitlab]`: `GitLab: glab not installed · install the GitLab CLI to see merge requests`; `B` checks again
- [ ] GITLAB-07 Logged out `[gitlab]`: `GitLab: not logged in · run glab auth login --hostname <host>`
- [ ] GITLAB-08 Offline `[gitlab]`: `GitLab: offline`; cached badges stay
- [ ] GITLAB-09 A project in nested groups (`acme/sub/project`) `[gitlab]`: badges, links and review mode work as for a top-level group
- [ ] GITLAB-10 `[forge.allow] "<host>" = ["acme"]` `[gitlab]`: allows `acme/sub/project`; `["acmecorp"]` does not allow `acme/...`
- [ ] GITLAB-11 `[forge.allow]` for a host only in `gitlab` `[sandbox]`: error; the whole file is ignored
- [ ] GITLAB-12 `gitlab_ask` host also in `github`, or not in `gitlab` `[sandbox]`: error; the whole file is ignored
- [ ] GITLAB-13 `o` in Branches, "Open in browser" `[gitlab]`: opens the merge-request list filtered to the branch (any state)
- [ ] GITLAB-14 `o`, "Create pull request…" on a GitLab branch `[gitlab]`: off, says creating is GitHub only
- [ ] GITLAB-15 `G` `[gitlab]`: the four queues (assigned, review requested, authored, all open)
- [ ] GITLAB-16 `enter` on a merge request `[gitlab]`: detail with the head pipeline's jobs as checks; approvals as approving reviews
- [ ] GITLAB-17 `C` `[gitlab]`: discussions without a diff position, system notes left out
- [ ] GITLAB-18 `f` `[gitlab]`: files and diff from local git; diff discussions anchor on their lines (added on new side, removed on old)
- [ ] GITLAB-19 `f` when GitLab will not serve the head by id `[gitlab]`: Kika fetches `refs/merge-requests/<n>/head` instead; no local ref moves
- [ ] GITLAB-20 `v` on the files screen `[gitlab]`: says viewed marks are not available on GitLab
- [ ] GITLAB-21 `S` with pending comments `[gitlab]` (writes): offers Approve and Comment only (no Request changes); drafts are made and published in one go; an approval follows
- [ ] GITLAB-22 `S` when the head moved since the comments were written `[gitlab]`: refused; nothing is sent
- [ ] GITLAB-23 `S` while you have your own draft notes on the merge request `[gitlab]`: refused
- [ ] GITLAB-24 Approval fails after the comments went out `[gitlab]` (writes): the pending review is cleared; told to approve again with `S`
- [ ] GITLAB-25 A draft or the publish fails `[gitlab]` (writes): drafts made are deleted; the pending review is kept
- [ ] GITLAB-26 Thread `a`, type, `ctrl-s`, `y` `[gitlab]` (writes): the reply is posted to the discussion
- [ ] GITLAB-27 Thread `R` `[gitlab]` (writes): resolves; `R` again unresolves
- [ ] GITLAB-28 `a` on the conversation, `y` `[gitlab]` (writes): a top-level comment is posted
- [ ] GITLAB-29 `s` suggestion on a 3-line range, submit `[gitlab]` (writes): posted as `suggestion:-2+0`
- [ ] GITLAB-30 A suggestion over more than 101 lines `[gitlab]`: refused
- [ ] GITLAB-31 `M` `[gitlab]` (writes): offers "merge" and, when the project allows, "squash"; never rebase; `y` merges with the head Kika showed
- [ ] GITLAB-32 `M` after the head moved `[gitlab]` (writes): GitLab refuses (409); the message shows escaped
- [ ] GITLAB-33 `b` on a merge request `[gitlab]`: checks out `pr/<n>`, with the `refs/merge-requests/<n>/head` fallback when needed
- [ ] GITLAB-34 `o` on the files screen `[gitlab]`: opens `/-/merge_requests/<n>/diffs` at the line; with `?commit_id=` in a commit's view
- [ ] GITLAB-35 Rate limited `[gitlab]`: `GitLab: rate limited · until HH:MM UTC`; `B` says `GitLab is rate limited; refresh after the limit resets`
- [ ] GITLAB-36 A project not found or no access `[gitlab]`: `GitLab: repository not found or no access`
- [ ] GITLAB-37 The detail's merge line `[gitlab]`: names the methods as the `M` pick list does (`Merge: merge · squash`), never "merge commit"
- [ ] GITLAB-38 A retried job `[gitlab]` (writes): retry a failed job on a real merge request, let it finish, reopen the detail (enter): one row for that job, showing the retried result and linking to the new job's page
