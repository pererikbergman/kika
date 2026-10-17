#!/usr/bin/env bash
# make-sandbox.sh - build a throwaway git setup for Kika's manual regression test.
#
# Usage:
#   make-sandbox.sh <dir> [--force]     build the sandbox under <dir>
#   make-sandbox.sh <dir> conflict      start a merge in <dir>/repo that stops on a conflict
#   make-sandbox.sh <dir> push-other    push one more commit to origin/main and origin/release from <dir>/other
#   make-sandbox.sh <dir> fixup         stage a small change in src/app.txt (for ctrl-b / ctrl-f fixup tests)
#
# The build refuses to touch an existing <dir> unless --force is given; with
# --force the old <dir> is deleted first.
#
# What it makes:
#   <dir>/remote.git   bare repo, "origin" of repo/ and other/
#   <dir>/backup.git   second bare repo, remote "backup" of repo/
#   <dir>/sub.git      bare repo used as a submodule
#   <dir>/repo         the working repo to run kika-dev in
#   <dir>/other        a second clone of remote.git, to push from
#   <dir>/wt-side      a linked worktree of repo/ on branch wt/side
#   <dir>/config/kika/config.toml   Kika config (point XDG_CONFIG_HOME at <dir>/config)
#   <dir>/env.sh       source this to set XDG_*, GH_CONFIG_DIR and protocol.file.allow for the sandbox

set -euo pipefail

usage() {
  sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
}

[ $# -ge 1 ] || usage
DIR_ARG=$1
shift
MODE=build
FORCE=0
for a in "$@"; do
  case "$a" in
    --force) FORCE=1 ;;
    conflict|push-other|fixup) MODE=$a ;;
    -h|--help) usage ;;
    *) echo "unknown argument: $a" >&2; usage ;;
  esac
done

# ---- stable, isolated git -------------------------------------------------
# Ignore the user's global and system git config while building, so hooks
# paths, signing, default branch names and templates cannot change the result.
EMPTY_CFG=$(mktemp "${TMPDIR:-/tmp}/kika-sandbox-gitconfig.XXXXXX")
trap 'rm -f "$EMPTY_CFG"' EXIT
export GIT_CONFIG_GLOBAL="$EMPTY_CFG"
export GIT_CONFIG_NOSYSTEM=1
export GIT_TEMPLATE_DIR=""
export GIT_EDITOR=true
export LANG=C LC_ALL=C
export TZ=UTC
# Let git clone submodules over file:// URLs (git refuses the file transport
# for submodules by default). env.sh sets the same for Kika's own git calls.
export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=protocol.file.allow GIT_CONFIG_VALUE_0=always
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE || true

ALICE_NAME="Alice Example";  ALICE_EMAIL="alice@example.com"
BOB_NAME="Bob Builder";      BOB_EMAIL="bob@example.com"
CAROL_NAME="Carol Remote";   CAROL_EMAIL="carol@example.com"

BASE_EPOCH=1704103200   # 2024-01-01 10:00:00 UTC
TICK=0

# who <alice|bob|carol>: set author and committer for the next git command
who() {
  case "$1" in
    alice) n=$ALICE_NAME; e=$ALICE_EMAIL ;;
    bob)   n=$BOB_NAME;   e=$BOB_EMAIL ;;
    carol) n=$CAROL_NAME; e=$CAROL_EMAIL ;;
  esac
  export GIT_AUTHOR_NAME="$n" GIT_AUTHOR_EMAIL="$e"
  export GIT_COMMITTER_NAME="$n" GIT_COMMITTER_EMAIL="$e"
}

# tick: move the fixed clock one hour on
tick() {
  TICK=$((TICK + 1))
  local d="$((BASE_EPOCH + TICK * 3600)) +0000"
  export GIT_AUTHOR_DATE="$d" GIT_COMMITTER_DATE="$d"
}

# commit <who> <message>: commit what is staged, at the next tick
commit() {
  who "$1"; tick
  git commit -q --no-verify -m "$2"
}

lines() { # lines <prefix> <count>: print numbered lines
  local i
  for ((i = 1; i <= $2; i++)); do echo "$1 line $i"; done
}

abspath() { (cd "$1" && pwd -P); }

# ---- helper modes ---------------------------------------------------------
if [ "$MODE" != build ]; then
  [ -d "$DIR_ARG/repo/.git" ] || { echo "no sandbox at $DIR_ARG (run without a mode first)" >&2; exit 1; }
  ROOT=$(abspath "$DIR_ARG")
  case "$MODE" in
    conflict)
      cd "$ROOT/repo"
      if [ -n "$(git status --porcelain --untracked-files=all --ignore-submodules=all)" ]; then
        who alice; tick
        git stash push -q --include-untracked -m "sandbox: work put aside before the conflict"
        echo "Stashed the working changes as stash@{0} (\"sandbox: work put aside before the conflict\")."
      fi
      git checkout -q conflict/ours
      who alice; tick
      if git merge --no-edit conflict/theirs >/dev/null 2>&1; then
        echo "merge did not conflict - the sandbox is not as expected" >&2; exit 1
      fi
      echo "Merge of conflict/theirs into conflict/ours stopped on conflicts:"
      git diff --name-only --diff-filter=U | sed 's/^/  /'
      echo "Run kika-dev in $ROOT/repo; the Status panel shows the merge."
      echo "Afterwards: abort or commit, then 'git checkout main && git stash pop' to get the work back."
      ;;
    push-other)
      cd "$ROOT/other"
      git checkout -q main
      git pull -q --ff-only
      echo "Change from another clone ($TICK)" >> REMOTE-CHANGES.md
      git add REMOTE-CHANGES.md
      commit carol "Remote change on main (pushed from other/)"
      git push -q origin main
      git checkout -q release
      git pull -q --ff-only
      echo "release note from other/" >> RELEASE.md
      git add RELEASE.md
      commit carol "Remote change on release (pushed from other/)"
      git push -q origin release
      git checkout -q main
      echo "Pushed one commit each to origin/main and origin/release."
      echo "In repo/, press f: main and release now show they are behind."
      ;;
    fixup)
      cd "$ROOT/repo"
      sed -i.bak 's/^app line 3$/app line 3 (fixed later)/' src/app.txt && rm -f src/app.txt.bak
      git add src/app.txt
      echo "Staged a one-line change to src/app.txt line 3."
      ;;
  esac
  exit 0
fi

# ---- build ----------------------------------------------------------------
if [ -e "$DIR_ARG" ]; then
  if [ "$FORCE" = 1 ]; then
    # Only delete something that looks like a sandbox, or is empty.
    if [ -d "$DIR_ARG/repo/.git" ] || [ -z "$(ls -A "$DIR_ARG" 2>/dev/null)" ]; then
      rm -rf "$DIR_ARG"
    else
      echo "refusing: $DIR_ARG exists and does not look like a sandbox (no repo/.git)" >&2
      exit 1
    fi
  else
    echo "refusing: $DIR_ARG already exists (use --force to rebuild it)" >&2
    exit 1
  fi
fi
mkdir -p "$DIR_ARG"
ROOT=$(abspath "$DIR_ARG")
cd "$ROOT"

git init -q --bare -b main remote.git
git init -q --bare -b main backup.git
git init -q --bare -b main sub.git
git init -q --bare -b main extra.git

# -- submodule sources --------------------------------------------------------
for name in sub extra; do
  git clone -q "file://$ROOT/$name.git" "seed-$name" 2>/dev/null
  (
    cd "seed-$name"
    echo "# $name library" > README.md
    lines "$name" 5 > lib.txt
    git add . ; commit bob "Initial $name library"
    echo "$name v2" >> lib.txt
    git add . ; commit bob "Second $name commit"
    git push -q origin main
  )
  rm -rf "seed-$name"
done

# -- main history -----------------------------------------------------------
git init -q -b main repo
cd repo
git config core.autocrlf false
git config protocol.file.allow always   # the submodule clone does not read this; env.sh sets it for every git call

# 1
cat > README.md <<'EOF'
# Sandbox

A throwaway repository for Kika's manual regression test.
EOF
git add README.md; commit alice "Initial commit"

# 1b - feature/merged, merged with a merge commit (kept low, so every rewrite above it works)
git checkout -q -b feature/merged
echo "merged feature" > feature.txt
git add feature.txt; commit bob "Add merged feature"
git checkout -q main
who alice; tick
git merge -q --no-ff --no-edit -m "Merge branch 'feature/merged'" feature/merged

# 2
mkdir -p src
lines app 20 > src/app.txt
git add src/app.txt; commit alice "Add app"

# 3
lines util 12 > src/util.txt
git add src/util.txt; commit bob "Add util helpers"

# 5
lines notes 6 > notes.txt
git add notes.txt; commit alice "Add notes"

# 6 - rename across commits (for the path filter that follows renames)
mkdir -p docs
git mv notes.txt docs/notes.txt; commit alice "Move notes into docs"

# 7
sed -i.bak 's/^app line 5$/app line 5 changed by bob/' src/app.txt && rm src/app.txt.bak
git add src/app.txt; commit bob "Tune app line 5"

# 8
lines multi 80 > multi.txt
printf 'old name\n' > old-name.txt
printf 'this file will be deleted in the working tree\n' > to-delete.txt
git add multi.txt old-name.txt to-delete.txt; commit alice "Add multi, old-name and to-delete"

# 9 - binary
printf '\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR\x00\x01\x02\x03\x04\x05' > image.bin
git add image.bin; commit bob "Add a binary file"

# 10 - fixup of 3
echo "util line 13" >> src/util.txt
git add src/util.txt; commit bob "fixup! Add util helpers"

# 11
sed -i.bak 's/^app line 10$/app line 10 changed by alice/' src/app.txt && rm src/app.txt.bak
git add src/app.txt; commit alice "Tune app line 10"

# 12 - deep path for the path filter
mkdir -p lib/paths
echo "deep" > lib/paths/deep.txt
git add lib/paths; commit alice "Add lib/paths/deep.txt"

# 13 - fixup of 11
sed -i.bak 's/^app line 11$/app line 11 (with line 10)/' src/app.txt && rm src/app.txt.bak
git add src/app.txt; commit alice "fixup! Tune app line 10"

# 14
echo "lib/paths again" >> lib/paths/deep.txt
sed -i.bak 's/^app line 15$/app line 15 changed by bob/' src/app.txt && rm src/app.txt.bak
git add lib/paths src/app.txt; commit bob "Touch app and deep path"

# release branch here (pushed, tracks origin, made behind by push-other).
# It sits above both fixups' moves, so autosquash is not refused for it, and a
# reword below it moves it (--update-refs).
git branch release

# 15
printf 'conflict base\nshared line\nkeep 1\nkeep 2\nkeep 3\nkeep 4\nkeep 5\nsecond shared line\nend\n' > conflict.txt
git add conflict.txt; commit alice "Add conflict.txt"

# first push: origin gets main up to here
git remote add origin "$ROOT/remote.git"
git remote add backup "$ROOT/backup.git"
git push -q -u origin main
git push -q -u origin release

# tags
git tag v0.1.0 HEAD~10                       # lightweight
who alice; tick
git tag -a v0.2.0 -m "Release 0.2.0" HEAD~5  # annotated
who bob; tick
git tag -a v1.0-rc -m "Release candidate" HEAD
git tag pushed-tag HEAD~2
git push -q origin pushed-tag

# 16, 17 - local only: main is ahead of origin/main by 2
sed -i.bak 's/^app line 18$/app line 18 local/' src/app.txt && rm src/app.txt.bak
git add src/app.txt; commit alice "Local change one"
echo "util line 14" >> src/util.txt
git add src/util.txt; commit bob "Local change two"

# -- other branches ---------------------------------------------------------
# feature/unmerged: two commits not on main
git checkout -q -b feature/unmerged HEAD~3
echo "unmerged work 1" > unmerged.txt
git add unmerged.txt; commit bob "Unmerged work 1"
echo "unmerged work 2" >> unmerged.txt
git add unmerged.txt; commit bob "Unmerged work 2"

# remote-only: a branch on origin with no local branch (for checking out a remote branch)
git push -q origin feature/unmerged:refs/heads/remote-only
git fetch -q origin

# topic/tracked: tracks origin, one commit ahead and one behind
git checkout -q -b topic/tracked main~2
echo "topic base" > topic.txt
git add topic.txt; commit alice "Topic base"
git push -q -u origin topic/tracked
echo "topic remote side" >> topic.txt
git add topic.txt; commit carol "Topic: commit only on origin"
git push -q origin topic/tracked
git reset -q --hard HEAD~1
echo "topic local side" > topic-local.txt
git add topic-local.txt; commit alice "Topic: commit only here"

# wt/side: checked out in a linked worktree, on a commit of its own (off main's
# line, so rewrites on main never have to move a branch checked out elsewhere)
git branch wt/side main
# to-rename: on the first commit, which no rewrite touches
git branch to-rename "$(git rev-list --max-parents=0 HEAD)"
git checkout -q main

# -- submodules -------------------------------------------------------------
who alice; tick
git -c protocol.file.allow=always submodule --quiet add "file://$ROOT/sub.git" vendor/lib
git -c protocol.file.allow=always submodule --quiet add "file://$ROOT/extra.git" vendor/extra
commit alice "Add submodules vendor/lib and vendor/extra"
# leave vendor/extra uninitialized, so init (i) and update (u) can be tried
git submodule --quiet deinit -f vendor/extra
rm -rf .git/modules/vendor/extra

# conflict pair (made after the submodules, so checking it out keeps them)
git checkout -q -b conflict/ours main
printf 'conflict base\nours changed this line\nkeep 1\nkeep 2\nkeep 3\nkeep 4\nkeep 5\nours changed the second line\nend\n' > conflict.txt
git add conflict.txt; commit alice "Ours: change the shared line"
git checkout -q -b conflict/theirs main
printf 'conflict base\ntheirs changed this line\nkeep 1\nkeep 2\nkeep 3\nkeep 4\nkeep 5\ntheirs changed the second line\nend\n' > conflict.txt
git add conflict.txt; commit bob "Theirs: change the shared line"

git checkout -q main

# the submodule commit makes main ahead by 3; keep it that way

# -- worktree -----------------------------------------------------------------
git worktree add -q "$ROOT/wt-side" wt/side
(
  cd "$ROOT/wt-side"
  echo "side work" > side.txt
  git add side.txt; commit bob "Side work in the linked worktree"
)

# -- second clone -----------------------------------------------------------
cd "$ROOT"
git clone -q "$ROOT/remote.git" other
(cd other && git checkout -q -b release origin/release && git checkout -q main)

# -- stashes ------------------------------------------------------------------
cd "$ROOT/repo"
# stash@{1}: tracked change plus an untracked file
echo "stashed app change" >> src/app.txt
echo "stashed untracked" > stashed-untracked.txt
who bob; tick
git stash push -q --include-untracked -m "WIP with untracked file"
# stash@{0}: a tracked change only
echo "stashed readme change" >> README.md
who alice; tick
git stash push -q -m "WIP readme tweak"

# -- pre-commit hook --------------------------------------------------------
# Refuses a commit whose staged text holds the word DO-NOT-COMMIT, so a failed
# commit and ctrl-n (skip hooks) can be tested. Everything else passes.
mkdir -p .git/hooks
cat > .git/hooks/pre-commit <<'EOF'
#!/bin/sh
if git diff --cached -U0 | grep -q '^+.*DO-NOT-COMMIT'; then
  echo "pre-commit: staged text contains DO-NOT-COMMIT" >&2
  exit 1
fi
exit 0
EOF
chmod +x .git/hooks/pre-commit

# -- working tree state -----------------------------------------------------
# staged change (partly staged: more unstaged on top)
sed -i.bak 's/^util line 2$/util line 2 staged/' src/util.txt && rm src/util.txt.bak
git add src/util.txt
sed -i.bak 's/^util line 9$/util line 9 unstaged/' src/util.txt && rm src/util.txt.bak
# unstaged change
sed -i.bak 's/^app line 1$/app line 1 unstaged/' src/app.txt && rm src/app.txt.bak
# several hunks in one file
sed -i.bak -e 's/^multi line 3$/multi line 3 edited/' \
           -e 's/^multi line 4$/multi line 4 edited/' \
           -e 's/^multi line 30$/multi line 30 edited/' \
           -e 's/^multi line 60$/multi line 60 edited/' \
           -e 's/^multi line 61$/multi line 61 edited/' multi.txt && rm multi.txt.bak
# deleted (unstaged)
rm to-delete.txt
# renamed (staged)
git mv old-name.txt new-name.txt
# binary change
printf '\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR\x09\x08\x07' > image.bin
# untracked files
echo "scratch" > scratch.txt
mkdir -p notes
echo "todo" > notes/todo.txt

# -- Kika config --------------------------------------------------------------
mkdir -p "$ROOT/config/kika" "$ROOT/state" "$ROOT/cache"
cat > "$ROOT/config/kika/config.toml" <<'EOF'
# Sandbox config for Kika's manual regression test. Harmless commands only.

[theme]
name = "default"

# Shows the selected file's path in a popup.
[[commands]]
context = "files"
keys = ["ctrl-t"]
description = "Echo the file path"
command = ["echo", "path:", "{path}"]
output = "popup"

# Asks two questions, then shows the answers.
[[commands]]
context = "files"
keys = ["ctrl-u"]
description = "Echo two answers"
command = ["echo", "{prompt1}", "{prompt2}"]
prompts = ["First word", "Second word"]
output = "popup"

# Runs for 30 seconds in the background; stop it with ctrl-x.
[[commands]]
context = "files"
keys = ["ctrl-k"]
description = "Sleep 30 seconds"
command = ["sleep", "30"]
output = "none"

# Always fails, to show a failed command.
[[commands]]
context = "files"
keys = ["ctrl-g"]
description = "Always fail"
command = ["false"]
output = "none"

# Takes the terminal until you press Enter.
[[commands]]
context = "files"
keys = ["ctrl-o"]
description = "Terminal: show path, wait for Enter"
command = ["sh", "-c", "echo \"file: $1\"; printf 'Press Enter to go back'; read x", "sh", "{path}"]
output = "terminal"

[[commands]]
context = "diff"
keys = ["ctrl-g"]
description = "Echo file and line"
command = ["echo", "{path}", "line", "{line}"]
output = "popup"

[[commands]]
context = "blame"
keys = ["ctrl-y"]
description = "Show the files this commit changed"
command = ["git", "show", "--stat", "--no-color", "{commit}"]
output = "popup"

# Uncomment to try the parts that need them (restart Kika after a change):
#
# [watch]
# fetch = 60
#
# [diff]
# pager = "delta --paging=never"
# tool = "difft --color=always"
#
# [commit]
# hooks_detached = true
#
# [display]
# mouse = false
#
# [clipboard]
# command = ["pbcopy"]
#
# [forge]
# github = ["github.com"]
# gitlab = ["gitlab.com"]
# gitlab_ask = ["gitlab.com"]
# browser = ["open"]
EOF

cat > "$ROOT/env.sh" <<EOF
# source this file: keeps Kika's config, state and cache inside the sandbox
export XDG_CONFIG_HOME="$ROOT/config"
export XDG_STATE_HOME="$ROOT/state"
export XDG_CACHE_HOME="$ROOT/cache"
# gh reads its login from XDG_CONFIG_HOME too; keep the real one
export GH_CONFIG_DIR="\${GH_CONFIG_DIR:-\$HOME/.config/gh}"
# let git (and Kika's submodule update) clone the sandbox's file:// submodules
export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=protocol.file.allow GIT_CONFIG_VALUE_0=always
cd "$ROOT/repo"
EOF

# -- summary ----------------------------------------------------------------
echo "Sandbox ready at $ROOT"
echo
echo "  source \"$ROOT/env.sh\" && kika-dev"
echo
git -C "$ROOT/repo" status --short
echo
git -C "$ROOT/repo" branch -vv --no-color | sed 's/^/  /'
echo
git -C "$ROOT/repo" stash list | sed 's/^/  /'
