#!/bin/bash
# Regenerates the patch fixtures with real git. Run from anywhere.
set -e
OUT=$(cd "$(dirname "$0")" && pwd)
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 LC_ALL=C
R=$(mktemp -d)
cd "$R"
g() { git -c user.name=t -c user.email=t@t -c commit.gpgsign=false "$@"; }
D="diff --no-color --no-ext-diff --no-textconv --no-renames -U3 --src-prefix=a/ --dst-prefix=b/"
g init -q
g config core.fileMode true
g config core.autocrlf false
for i in $(seq 1 20); do echo "line $i"; done > two.txt
printf 'keep\nlast' > eol_old.txt
printf 'keep\nlast\n' > eol_new.txt
printf 'keep\nold' > eol_both.txt
for i in $(seq 1 20); do printf 'row %d\r\n' $i; done > crlf.txt
printf 'a\n\nb\nc\nd\n' > blank.txt
echo gone > gone.txt
printf 'one\ntwo\nthree\nfour\nfive\n' > old.txt
printf 'x\0y\n' > bin.dat
printf 'a\nb\nc\n' > "tab	here.txt"
g add -A; g commit -q -m init
# two hunks + mode change: line 1 becomes two lines, line 20 changes
{ echo "line 1a"; echo "line 1b"; for i in $(seq 2 19); do echo "line $i"; done; echo "line twenty"; } > two.txt
chmod +x two.txt
g $D -- two.txt > $OUT/two_hunks.diff
printf 'keep\nlast\n' > eol_old.txt
g $D -- eol_old.txt > $OUT/no_eol_old.diff
printf 'keep\nlast' > eol_new.txt
g $D -- eol_new.txt > $OUT/no_eol_new.diff
printf 'keep\nnew' > eol_both.txt
g $D -- eol_both.txt > $OUT/no_eol_both.diff
{ printf 'row 1 changed\r\n'; for i in $(seq 2 19); do printf 'row %d\r\n' $i; done; printf 'row 20 changed\r\n'; } > crlf.txt
g $D -- crlf.txt > $OUT/crlf.diff
printf 'a\n\nB\nc\nd\n' > blank.txt
g -c diff.suppressBlankEmpty=true $D -- blank.txt > $OUT/blank_context.diff
rm gone.txt
g $D -- gone.txt > $OUT/deleted.diff
printf 'fresh\nfile\n' > new.txt
g $D --no-index -- /dev/null new.txt > $OUT/new_file.diff || true
printf 'x\0z\n' > bin.dat
g $D -- bin.dat > $OUT/binary.diff
printf 'a\nB\nc\n' > "tab	here.txt"
g $D -- "tab	here.txt" > $OUT/quoted_path.diff
g mv old.txt moved.txt
printf 'one\ntwo\nthree\nfour\nFIVE\n' > moved.txt
g add moved.txt
g diff --no-color --no-ext-diff -M --cached -- old.txt moved.txt > $OUT/rename.diff
g reset -q --hard
# submodule gitlink change
A=$(git rev-parse HEAD)
g commit -q --allow-empty -m second
B=$(git rev-parse HEAD)
g update-index --add --cacheinfo 160000,$A,sub
g commit -q -m sub
g update-index --cacheinfo 160000,$B,sub
g $D --cached -- sub > $OUT/submodule.diff
g reset -q --hard
# conflict -> combined diff
g checkout -q -b other
echo theirs > c.txt; g add c.txt; g commit -q -m theirs
g checkout -q -
echo ours > c.txt; g add c.txt; g commit -q -m ours
g merge -q other >/dev/null 2>&1 || true
g $D -- c.txt > $OUT/combined.diff
cd /; rm -rf "$R"
