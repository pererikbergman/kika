# Configuration

Kika works without any configuration. Two optional TOML files let you remap
keys, bind your own programs to keys, tune how often Kika looks for changes
made outside it, and choose a program to draw the Diff pane.

## Files and locations

| File | Where | Holds |
|---|---|---|
| Global | `$XDG_CONFIG_HOME/kika/config.toml`, else `~/.config/kika/config.toml` (Linux, macOS); `%APPDATA%\kika\config.toml` (Windows) | `[keys]`, `[watch]`, `[diff]` and `[[commands]]` |
| Repository | `.kika.toml` at the top level of the repository | `[keys]` only |

- Neither file is required. A missing file is not an error.
- An entered submodule uses its parent repository's `.kika.toml`, not its own.
  Outside a repository there is no repository file.
- Each file must be a regular file of at most 64 KiB, in UTF-8.
- The repository file must not be a symbolic link. The global file may be one.

A global file that uses every setting:

```toml
[keys]
"files.stage" = ["s", "ctrl-s"]   # replace this binding's keys
"files.discard" = []              # disable it

[watch]
refs = 2        # seconds
files = 5
fetch = 300     # leave out to keep auto-fetch off

[diff]
pager = "delta --paging=never"   # a program that draws the Diff pane
tool = "difft --color=always"    # a display-only diff tool
```

## Layering

Settings are applied in three layers, each on top of the one before:

1. the built-in defaults,
2. the global file,
3. the repository's `.kika.toml`.

A file with any error is ignored as a whole, and the layer below it applies.
Kika shows the error so you can fix it. Unknown fields and unknown binding ids
count as errors, so a typo is reported instead of silently doing nothing.

## `[keys]`

Each entry names a binding by its id (see the [binding reference](#binding-reference))
and lists the keys it should answer to:

```toml
[keys]
"files.commit" = ["C"]            # replaces the default keys
"workspace.whitespace" = []       # disables the binding
```

- The list replaces that binding's default keys; it does not add to them.
- `[]` disables the binding.
- `global.quit` must keep at least one key.
- One key cannot be bound twice in the same scope (the same table below).
  A binding in a panel may reuse a key from an enclosing scope, such as
  Global; the panel's binding wins while that panel has focus.

Key names:

- one printable character, such as `a`, `A`, `?` or `+`. Letters are
  case-sensitive: `J` is Shift+J;
- or one of `space`, `esc`, `enter`, `tab`, `shift-tab`, `bksp`, `up`, `down`,
  `left`, `right`, `pgup`, `pgdn`, `home`, `end`;
- either with an optional `ctrl-` prefix, written in lower case: `ctrl-s`.

Key changes are picked up when Kika regains focus and when you switch
repository, so you can edit a file in another window and come back.

## `[watch]` (global file only)

Kika polls the repository for changes made outside it.

| Setting | Meaning | Default |
|---|---|---|
| `refs` | Seconds between checks of branches and tags | 2 |
| `files` | Seconds between checks of the working tree | 5 |
| `fetch` | Seconds between background fetches from remotes | not set: auto-fetch is off |

Each value must be at least 1.

**Changes to `[watch]` take effect after a restart.** Kika reads them once,
when it starts.

## Fetch, pull and push

`f`, `p` and `P` run plain `git fetch --all`, `git pull` and `git push`, so
your upstream, `push.default`, `pull.rebase`, credential helpers, ssh-agent
and ssh config apply as they do on the command line. They reach the network
only when you press them; nothing here changes auto-fetch.

When git or ssh still needs an answer (a username, password, 2FA code, key
passphrase or a new host key), Kika shows it in a popup and hands back what
you type. A password or passphrase is drawn as dots. Kika never logs, stores
or shows it; it goes only to the process that asked. Git asks your credential
helper and ssh asks ssh-agent first, so the popup only appears when they had
nothing. This works by making Kika git's and ssh's askpass program:

- If you set `GIT_ASKPASS` or `core.askPass` yourself, git keeps using yours.
- OpenSSH 8.4 and later always ask through Kika. Older ssh versions use an
  askpass program only when `DISPLAY` is set; without it, a passphrase or
  host-key question fails with ssh's message instead. Load the key into
  ssh-agent, or answer the host-key question once in a terminal.
- On Windows there is no popup: git and ssh fail instead of asking, so use a
  credential helper or ssh-agent there.

A push the remote rejects because it has commits your branch lacks offers to
push again with `--force-with-lease`, which replaces them only if nobody
pushed since your last fetch. Nothing is forced unless you press `y`.

## `[diff]` (global file only)

`pager` names a program that draws the Diff pane in its own style, such as a
diff highlighter. Press `R` in the Diff pane to cycle from the built-in view
to each renderer and back. Hunk and line staging still work there: each
rendered row is matched to the change it shows, and when the selection
cannot be matched, the built-in view is drawn and the title says
`(unmapped)`.

```toml
[diff]
pager = "delta --paging=never"
```

- Kika runs the program itself, never through a shell. The value is split on
  spaces into the program and its arguments. Quotes, `|`, `;`, `$`, `~`, `*`
  and other shell syntax are refused, and the whole file is ignored with an
  error.
- The program must be a bare name, found on your `PATH`, or an absolute path.
  A relative path such as `./delta` is refused.
- The program reads the file's plain diff on standard input and gets the
  pane's width in the `COLUMNS` environment variable. If it ignores
  `COLUMNS`, give it a width option of its own.
- It may run for at most 10 seconds and print at most 16 MiB.
- After `pager`, `R` also offers your global git `pager.diff`, then
  `core.pager`, under the same rules. Plain viewers (`less`, `more`, `most`,
  `cat`) and `true`/`false` values are skipped, from either source. A
  repository's own git config is never read for this.

### `tool`: a display-only diff tool

`tool` names a structural diff tool, such as difftastic, that compares the
old and new file instead of reading a diff. `R` offers it after the pagers.

```toml
[diff]
tool = "difft --color=always"
```

- Kika reads both sides of the shown file into two new temp files in a
  private directory, runs the tool as `<tool> -- OLD NEW`, and removes the
  directory afterwards. For unstaged changes OLD is the index and NEW the
  working tree file; for staged changes OLD is `HEAD` and NEW the index; for
  an untracked file OLD is empty. The temp files are named `old` and `new`,
  plus the file's extension when it is 1 to 16 letters or digits, so your
  file names never reach the tool's arguments.
- The value follows the same rules as `pager`: run directly, never through a
  shell, split on spaces, with a bare name or an absolute path.
- The tool gets the pane's width in both `COLUMNS` and `DFT_WIDTH`, and runs
  under the same limits: at most 10 seconds and 16 MiB of output, and at
  most 16 MiB per side. Contents are the raw bytes, with no textconv or
  clean filter applied.
- Only regular files are shown. A symlink, a submodule or a conflicted file
  says so instead.
- **The tool's view is display-only.** Its rows cannot be matched to the
  changes they show, so hunk and line staging, discard and hunk editing are
  refused there, and the title says `(display only)`. Press `R` to get back
  to the built-in view. Staging and discarding whole files from the Files
  panel work as usual.
- **Only Kika's global file sets `tool`.** Git's own external diff settings
  (`diff.external`, `diff.tool`, `difftool.*` and `GIT_EXTERNAL_DIFF`) are
  never read for it, since git runs those through a shell and a cloned
  repository can bring them.

**Changes to `[diff]` take effect after a restart.**

## Custom commands

`[[commands]]` binds a key to a program of your own. Each entry works in one
panel and can pass the selected file, line or commit to the program.

```toml
[[commands]]
context = "files"
keys = ["ctrl-t"]
description = "Copy the file to…"
command = ["cp", "--", "{path}", "{prompt1}"]
prompts = ["Copy to"]
output = "none"

[[commands]]
context = "diff"
keys = ["ctrl-g"]
description = "Open the file in vim at this line"
command = ["vim", "+{line}", "--", "{path}"]

[[commands]]
context = "blame"
keys = ["ctrl-y"]
description = "Show the files this commit changed"
command = ["git", "show", "--stat", "{commit}"]
output = "popup"
```

| Field | Required | Meaning |
|---|---|---|
| `context` | yes | The panel the keys work in: `"files"`, `"diff"` or `"blame"` |
| `keys` | yes | At least one key, named as in `[keys]` |
| `description` | yes | The text shown for it in the help menu (`?`) |
| `command` | yes | The program and its arguments, one list item per argument |
| `prompts` | no | Titles of text prompts, asked in order before it runs |
| `output` | no | Where the output goes, see below; `"terminal"` if left out |

**Commands are read from the global file only.** A `.kika.toml` that holds
`[[commands]]` is an error and is ignored as a whole, because a file that
arrives with a clone must never be able to run a program.

### Placeholders

An argument may contain placeholders, replaced when you press the key:

| Placeholder | Replaced by | Allowed in |
|---|---|---|
| `{path}` | The selected file, relative to the repository's top level | files, diff, blame |
| `{line}` | The line: the selected change in the diff, the cursor's line in the blame | diff, blame |
| `{commit}` | The full id of the commit that last changed the blame cursor's line | blame |
| `{prompt1}`, `{prompt2}`, … | Your answer to that prompt, counted from 1 | files, diff, blame |

- Write `{{` and `}}` for literal braces. Any other `{` or `}`, an unknown
  placeholder, or a prompt number with no matching entry in `prompts` is an
  error.
- **Each list item is always exactly one argument.** A value is put into it
  as it is: never split at spaces, never expanded as a glob, never given to a
  shell. A file named `a b;$(rm -rf x)` arrives as that one argument.
  `"--file={path}"` is one argument too.
- When `{path}` would begin an argument with `-` (it comes first, or only
  empty answers come before it), it is given as `./-…`, so a file name can
  never be read as an option.
- The program, the first item of `command`, must be plain text. A
  placeholder there is an error.
- Without a selection the key does nothing and says why: no file selected, or
  for `{line}` and `{commit}`, no line here or a line not committed yet.

The program runs in the repository's top level (or the submodule you
entered), so `{path}` names the file from there. That is also where a
program path containing `/`, such as `./scripts/fmt.sh` or `bin/tool`, is
looked up: it runs whatever file that repository ships, including one that
came with a clone. Use a bare program name found on your `PATH`, or an
absolute path, for anything you have not checked.

### Output

| Mode | What happens |
|---|---|
| `"terminal"` | Kika hands the terminal to the program and comes back when it exits. For interactive programs. |
| `"popup"` | The program runs in the background. Its output, stdout and stderr together, is shown in a popup when it ends. |
| `"none"` | The program runs in the background. Its output is shown only if it fails. |

In `"popup"` and `"none"` mode the program gets no input, and only one runs
at a time. At most 256 KiB of output is kept; a program that prints more is
stopped. A program that exits with a non-zero status, or cannot start, is
shown as failed with its output. After any command Kika reloads the file
list, since the command may have changed the repository.

### Shell commands

Kika never runs `command` through a shell. If you need one, for a pipe or a
redirection, call the shell yourself, and pass each value as a positional
parameter after the script. Inside the script, read it as `"$1"`, `"$2"`, and
so on, always in double quotes:

```toml
[[commands]]
context = "files"
keys = ["ctrl-l"]
description = "Page the file's history"
command = ["sh", "-c", "git log --follow -- \"$1\" | less", "sh", "{path}"]
```

The `"sh"` after the script becomes `$0`; `{path}` becomes `$1`.

In csh and tcsh the words after the script start at `$1`, so the name after
the script becomes `$1` and the first value is `$2`:

```toml
[[commands]]
context = "files"
keys = ["ctrl-l"]
description = "Page the file's history"
command = ["tcsh", "-c", "git log --follow -- $2:q | less", "tcsh", "{path}"]
```

The name is still required, and it must not start with `-`: csh and tcsh keep
reading options after the script until the first word that is not one, so a
value in that slot could turn into an option such as `-c`. Kika rejects a
placeholder there, as it does for every shell. Read the values as `$2:q`,
`$3:q` and so on, not as `"$2"`, which fails with `Unmatched '"'.` on a value
that contains a newline. `$2:q` passes the value as one word, with no glob,
`$` or backtick expansion; an empty prompt answer becomes no word at all. In
csh and tcsh `-c` takes the very next argument as the script, so put any
other options before it: `["csh", "-f", "-c", "…", "csh", "{path}"]`.

**Never put a placeholder inside the script text.** The shell would read a
file name such as `$(rm -rf ~)` as code. When `command[0]` is a shell (`sh`,
`bash`, `rbash`, `zsh`, `dash`, `ash`, `ksh`, `ksh93`, `mksh`, `lksh`, `oksh`,
`pdksh`, `yash`, `posh`, `fish`, `csh` or `tcsh`, or a versioned name of one
such as `bash5`, `zsh5`, `bash-5.2` or `/usr/local/bin/bash5`, with or without
`.exe` and with any trailing dots or spaces, which Windows drops when it starts
a program: `bash.`, `bash.exe.` and `bash ` all count), Kika rejects a
placeholder in the script, in any argument before it, or in the slot right
after it (`$0`, or `$1` in csh and tcsh).
The check is cautious: it assumes the last option before the first
placeholder takes a value, so write the script, then a name for that slot,
then the values. In the POSIX-style shells, options work in either order
around `-c`:

- `["bash", "-o", "pipefail", "-c", "… \"$1\" …", "bash", "{path}"]`
- `["bash", "-c", "-o", "pipefail", "… \"$1\" …", "bash", "{path}"]`

`["bash", "-o", "pipefail", "{path}"]` is rejected too: it would run the
selected file as a script.

The same check applies to a shell started through one of these launchers in
`command[0]`: `env`, `busybox`, `toybox`, `nice`, `nohup`, `timeout`, `xargs`,
`exec`, `command`, `stdbuf`, `setsid`, `ionice`, `chrt`, `taskset`, `time`,
`sudo`, `doas`, `caffeinate` and `arch` (named the same ways as a shell: any
path, any case, with or without `.exe`, with trailing dots or spaces). Kika does
not read the launcher's options. It takes the first shell name among the
launcher's arguments as the shell, rejects any placeholder before it, and
checks the rest as if that shell were `command[0]`. If that shell name may be
an option's value or an assignment, because the argument before it starts with
`-` or `+` (other than `--`) or because it contains `=` (`-u SH`,
`SHELL=/bin/bash`), Kika also checks the next shell name the same way, and so
on. So `["env", "sh", "-c", "… \"$1\" …", "sh", "{path}"]` is accepted, and
`["env", "bash", "-c", "x {path}"]` and `["env", "-u", "SH", "bash", "{path}"]`
are not.

This can refuse a safe command:

- In `["timeout", "5", "grep", "sh", "{path}"]` the pattern `sh` is taken as
  the shell, so `{path}` is rejected.
- In `["env", "-i", "sh", "-c", "… \"$1\" …", "sh", "{path}"]` the first `sh`
  might be the value of `-i`, so the second `sh` is checked as a shell too.
  Put `--` before the shell, where the launcher accepts it:
  `["env", "-i", "--", "sh", …]`.

Otherwise, call the launcher from a shell and pass the value as `"$1"`:
`["sh", "-c", "timeout 5 grep sh -- \"$1\"", "sh", "{path}"]`.

`env -S` (or `--split-string`) splits one argument into several. When any word
of it names a shell, or holds a `$` variable that could, Kika cannot tell where
the script ends, so it rejects every placeholder in that command. A placeholder
inside the `-S` string itself is rejected too, since env would split the value
into a program and its arguments.

**Not checked.** The guard cannot see a shell or interpreter started any other
way, so for these the rule is yours to keep:

- a launcher that is not on the list above, for example `ssh`, `su -c`,
  `find -exec`, `watch`, `flock` or `systemd-run`;
- `sudo -s`, `sudo -i` and `doas -s`, which start a shell without naming one;
- a placeholder that becomes the launcher's program, as in
  `["env", "{prompt1}", …]`;
- `cmd /c`, `pwsh -c` and `powershell` on Windows;
- interpreters such as `python -c`, `perl -e` and `node -e`.

Pass values as arguments after the code, and read them from there (`"$1"`,
`sys.argv`, `@ARGV`, `process.argv`).

### Keys

- A command's keys follow the `[keys]` rules: one key cannot be bound twice in
  the same scope, so a command key must be free of the panel's own bindings
  and of other commands in that panel.
- A command key may shadow a key of an enclosing scope, but not one of a
  locked binding: `q` in a `files` command would take over `global.quit`,
  and is an error.
- A repository's `.kika.toml` cannot move a binding onto a command's key.
- Commands are listed in the help menu (`?`) after the panel's own keys.
  They are not shown in the bottom bar.
- A `"popup"` or `"none"` command that is still running can be stopped with
  `ctrl-x` (`workspace.cancel_command`).

## Global-only settings and trust

- **Auto-fetch is off unless `fetch` is set in the global file.** Setting it
  is your explicit opt-in to network access.
- **`[watch]`, and so auto-fetch, is allowed only in the global file.** A
  `[watch]` table in `.kika.toml` is an error, and the whole file is ignored.
- **`[diff]` is allowed only in the global file**, since it names a program
  for Kika to run. A `[diff]` table in `.kika.toml` is an error, and the whole
  file is ignored.
- **`[[commands]]` is allowed only in the global file**, for the same reason:
  a cloned repository must not be able to run a program.
- **A repository's `.kika.toml` comes with a clone and is not trusted.** It
  may hold only `[keys]`. It cannot change any binding marked `locked` in the
  table below, and it cannot add a key that would hide a locked binding in an
  enclosing scope. For example, it cannot bind `files.stage` to `q`, because
  in the file list that key would take over `global.quit`.

Locked bindings quit, start other programs (your editor or a diff renderer),
reach the network, discard work or delete a branch, answer a confirmation, or create or switch
repositories. A cloned repository could otherwise trap you in Kika, or move
"discard" onto a key you press without thinking. Locked bindings can still be changed in the
global file.

## Binding reference

Each id is `<scope>.<action>`. Keys are looked up in the focused scope first,
then its enclosing scopes: the status, file list, branch list, commits, commit
files, diff, blame and conflict panels fall back to Workspace, and Workspace falls
back to Global. Popups, menus and the commit editor do not fall back, so `q` and
`?` do not quit or open help there; each popup that can stay open has its own `ctrl-c` to quit.

`(none)` means a binding has no default keys. `locked` in the last column
means a repository's `.kika.toml` cannot change it.

### Global (the main screens)

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `global.help` | `?` | Show the keys for this panel | |
| `global.quit` | `q`, `esc`, `ctrl-c` | Quit Kika | locked |

### Workspace (every panel: Status, Files, Branches, Commits, Stash, diff, blame and conflict)

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `workspace.refresh` | `r` | Reload the changed files and the branches | |
| `workspace.fetch` | `f` | Fetch every remote | locked |
| `workspace.pull` | `p` | Pull into the current branch | locked |
| `workspace.push` | `P` | Push the current branch | locked |
| `workspace.scroll_down` | `J` | Scroll the diff down a line | |
| `workspace.scroll_up` | `K` | Scroll the diff up a line | |
| `workspace.page_down` | `pgdn` | Scroll the diff down a page | |
| `workspace.page_up` | `pgup` | Scroll the diff up a page | |
| `workspace.parent` | `bksp` | Back to the parent repository | |
| `workspace.more_context` | `+`, `=` | Show more context lines in the diff | |
| `workspace.less_context` | `-` | Show fewer context lines in the diff | |
| `workspace.whitespace` | `w` | Hide or show whitespace-only changes | |
| `workspace.worktrees` | `W` | Show worktrees, to switch to, add or remove one | |
| `workspace.command_log` | `@` | Show the git commands Kika ran | |
| `workspace.recent` | `O` | Show recent repositories, to switch to one | |
| `workspace.focus_status` | `1` | Focus the Status panel | |
| `workspace.focus_files` | `2` | Focus the Files panel | |
| `workspace.focus_branches` | `3` | Focus the Branches panel | |
| `workspace.focus_commits` | `4` | Focus the Commits panel | |
| `workspace.focus_stash` | `5` | Focus the Stash panel | |
| `workspace.focus_diff` | `0`, `l`, `right` | Focus the diff, to work on the selected file's hunks | |
| `workspace.next_panel` | `tab` | Focus the next panel | |
| `workspace.prev_panel` | `shift-tab` | Focus the previous panel | |
| `workspace.zoom` | `z` | Zoom the focused panel: normal, half, full | |
| `workspace.cancel_command` | `ctrl-x` | Stop the running command | |
| `workspace.undo` | `ctrl-z` | Undo the last commit, checkout, reset or rebase, saying first what moves | locked |
| `workspace.redo` | `ctrl-r` | Redo what the last undo reversed, saying first what moves | locked |

`ctrl-z` and `ctrl-r` read `HEAD`'s reflog afresh, find the last action (a
commit, an amend, a merge, a pull, a cherry-pick, a revert, a reset, a
checkout or a whole rebase) and show what reversing it would do: which ref
moves, from which commit to which, and whether the files change. Nothing moves
until you press `y`. A branch moves with `git reset --keep`, so git itself
refuses when a local change or an untracked file is in the way; Kika also
refuses while tracked files have changes. Ignored files in the way are
replaced, as git always treats them, and the question says so. A rebase is
undone to where the rebased branch's own reflog says it was. If `HEAD` moves
while the question is open, `y` changes nothing and says so. Undo and redo refuse while `HEAD`
points at a ref that is not a local branch, such as a remote-tracking ref. A checkout is reversed with a
checkout, which carries uncommitted changes over. Undo and redo wait while an
operation (a merge, rebase, cherry-pick, revert or bisect) is in progress.
Kika marks its own steps in the reflog as `kika undo` and `kika redo`, so
pressing `ctrl-z` again goes one action further back, and any new action of
your own ends what `ctrl-r` can redo. Only the newest 1000 reflog entries are
read. A stash, a branch deletion and an aborted rebase cannot be undone.

### File list

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `files.next` | `j`, `down` | Select the next file | |
| `files.prev` | `k`, `up` | Select the previous file | |
| `files.stage` | `space` | Stage the selected file, or unstage it if fully staged | |
| `files.stage_all` | `a` | Stage everything, or unstage everything if all is staged | |
| `files.commit` | `c` | Commit the staged changes with a message | |
| `files.amend` | `A` | Amend the last commit with the staged changes, or reword it | |
| `files.discard` | `d` | Discard the selected file's unstaged changes, or delete it if untracked | locked |
| `files.open` | `o` | Open the selected file in your editor | locked |
| `files.submodules` | `S` | Show submodules | |
| `files.blame` | `b` | Blame the selected file | |
| `files.hunks` | `enter` | Work on the hunks of the selected file, or resolve its conflicts | |
| `files.mergetool` | `M` | Resolve the selected conflicted file in your merge tool | locked |
| `files.ignore` | `i` | Add to .gitignore | |
| `files.menu` | `m` | Open the actions menu | |
| `files.filter` | `/` | Filter the files by typed text | |
| `files.range` | `V` | Select a range of files from here, for stage and discard; again to clear it | |
| `files.escape` | `esc` | Clear the selection, else the filter, else quit Kika | locked |

The filter matches each word you type, in order but not necessarily side by
side, against the file's path; it ignores case until you type an uppercase
letter. `V` starts a range at the selected file, and moving the cursor grows
it. `space` then stages the whole range in one go (or unstages it, when
nothing in it is left to stage), skipping conflicted files, and `d` asks
once before discarding the range; only `y` answers that question. Keys that
act on one file are refused while a range is selected.

`i` asks for a pattern to add to the `.gitignore` at the top of the worktree
(or of the entered submodule), filled with one that matches exactly the
selected untracked file: anchored with `/`, with glob characters escaped.
Accept it as it is, or edit it into any pattern. The file is created if there
is none, and a line already there is not added twice. Only `.gitignore`
changes: no git command runs and nothing is staged. Kika will not write
through a `.gitignore` that is a symlink, and tracked files cannot be ignored
this way.

### Branches

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `branches.next` | `j`, `down` | Select the next branch | |
| `branches.prev` | `k`, `up` | Select the previous branch | |
| `branches.checkout` | `space` | Check out the selected branch | |
| `branches.new` | `n` | Create a branch at the selected one and check it out | |
| `branches.rename` | `R` | Rename the selected branch | |
| `branches.delete` | `d` | Delete the selected branch, asking first | locked |
| `branches.merge` | `m` | Merge the selected branch into the current one, or rebase onto it | locked |
| `branches.set_upstream` | `u` | Set the remote branch the selected branch tracks | |
| `branches.unset_upstream` | `U` | Stop the selected branch tracking its upstream | |
| `branches.switch_tab` | `]` | Show the remotes and their branches | |
| `branches.prev_tab` | `[` | Show the tags | |

While the Branches panel has focus, the diff pane shows the selected branch's
commits, and `J`, `K`, `pgdn` and `pgup` scroll them.

`m` opens a menu: fast-forward only, a merge commit, a squash (the changes are
staged for you to commit), or rebasing the current branch onto the selected
one. A merge commit, a rebase, and continuing or skipping run git on the
terminal, so hooks, signing and git's editor for the message work as on the
command line.

`u` asks for the upstream as `remote/branch`, starting from the current one,
or else the same name on `origin` (or the first remote). It must name a
remote branch Kika has listed; fetch first if it is new.

### Remotes

The Branches panel has three tabs: Local, Remotes and Tags. `]` shows the
next one and `[` the one before, wrapping round. A `switch_tab` key you
remapped still goes forward.

`]` from the Local tab shows the Remotes tab: each remote with its
URL (any user name and password in it left out), and its remote-tracking
branches below it.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `remotes.next` | `j`, `down` | Select the next remote or remote branch | |
| `remotes.prev` | `k`, `up` | Select the previous remote or remote branch | |
| `remotes.checkout` | `space` | Check out the selected remote branch as a new branch tracking it | |
| `remotes.add` | `a` | Add a remote; fetch to see its branches | |
| `remotes.rename` | `R` | Rename the selected remote | |
| `remotes.remove` | `d` | Remove the selected remote and its remote-tracking branches, asking first | locked |
| `remotes.switch_tab` | `]` | Show the tags | |
| `remotes.prev_tab` | `[` | Show the local branches | |

None of these reaches the network. A remote you add shows no branches until
you fetch with `f`. Kika refuses a remote URL that uses the `ext` or `fd`
transport (`ext::…`, `ext://…`, `fd::…`, `fd://…`), since those run a command
or use a file descriptor on every fetch; add such a remote with git directly. A remote named like an option
(starting with `-`), or refs left under a name no remote has, cannot be
renamed or removed from Kika.

### Tags

The Tags tab lists every tag on a commit, newest first: its name, the commit
it points at, its date and subject, with `◆` before an annotated tag. The
diff pane shows the selected tag's commits.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `tags.next` | `j`, `down` | Select the next tag | |
| `tags.prev` | `k`, `up` | Select the previous tag | |
| `tags.delete` | `d` | Delete the selected tag here, not on any remote, asking first | locked |
| `tags.push` | `P` | Push the selected tag to the upstream's remote, or the only remote | locked |
| `tags.switch_tab` | `]` | Show the local branches | |
| `tags.prev_tab` | `[` | Show the remotes and their branches | |

`d` deletes only the local tag, and only if it still points where it did when
it was listed. `P` pushes the one tag, never forced, to the current branch's
upstream remote, or to the only remote there is; with several remotes and no
upstream it does nothing and says so. On this tab `P` pushes the tag, not the
branch. Pushing is the only key here that reaches the network.

### Status

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `status.continue` | `c` | Continue the merge, rebase, cherry-pick or revert once conflicts are resolved | locked |
| `status.skip` | `s` | Skip the commit the rebase, cherry-pick or revert stopped on, or the one a bisect is testing | locked |
| `status.abort` | `a` | Abort the operation in progress, or reset the bisect, asking first | locked |
| `status.bisect_good` | `g` | Mark the commit the bisect is testing good | locked |
| `status.bisect_bad` | `b` | Mark the commit the bisect is testing bad | locked |

These act on the merge, rebase, cherry-pick or revert the Status panel shows
in progress. `cherry-pick --skip` and `revert --skip` need git 2.23 or newer;
an older git refuses them and changes nothing.

While only a bisect is in progress, the panel shows its good, bad and skipped
counts, the commit being tested, where the bisect started and, once git names
it, the first bad commit. `g`, `b` and `s` mark the commit being tested good,
bad or skipped, and `a` asks, then resets the bisect: its marks are dropped and
HEAD goes back to where it started. Only `y` answers that question, not Enter.
A merge, rebase, cherry-pick or revert in progress still comes first for `s`
and `a`. A bisect started on the command line with its own terms is shown and
can be reset, but not marked from Kika.

### Commits

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `commits.next` | `j`, `down` | Select the next commit | |
| `commits.prev` | `k`, `up` | Select the previous commit | |
| `commits.open` | `enter` | Show the files the selected commit changed | |
| `commits.checkout` | `space` | Check out the selected commit as a detached HEAD, asking first | |
| `commits.revert` | `R` | Make a commit that undoes the selected one | locked |
| `commits.reset` | `X` | Reset the current branch to the selected commit: soft, mixed or hard | locked |
| `commits.squash` | `s` | Squash the selected commit into the one below, combining their messages | locked |
| `commits.fixup` | `F` | Fold the selected commit into the one below, keeping that one's message | locked |
| `commits.drop` | `d` | Drop the selected commit and its change from the history | locked |
| `commits.reword` | `e` | Write a new message for the selected commit | locked |
| `commits.switch_tab` | `[`, `]` | Show HEAD's reflog: everywhere HEAD has been | |
| `commits.tag` | `t` | Tag the selected commit with a lightweight tag | |
| `commits.tag_annotated` | `T` | Tag the selected commit with an annotated tag and a message | |
| `commits.copy` | `c` | Add the selected commit to the copied set, or take it out | |
| `commits.paste` | `v` | Cherry-pick the copied commits onto the current branch, oldest first | locked |
| `commits.clear_copied` | `C` | Clear the copied set | |
| `commits.filter` | `/` | Filter the loaded commits by typed text | |
| `commits.range` | `V` | Select a range of commits from here, for copy; again to clear it | |
| `commits.escape` | `esc` | Clear the selection, else the filter, else quit Kika | locked |
| `commits.bisect` | `B` | Mark the selected commit good, bad or skipped for a bisect, or reset it | locked |
| `commits.copy_menu` | `y` | Copy hash or message (the terminal must allow OSC 52; in tmux, set-clipboard on) | |

`space` asks first, since a detached HEAD is on no branch: commits made there
belong to no branch until you create one. Git refuses the checkout when it
would overwrite local changes. `R` runs `git revert` on the terminal, so git's
editor for the message and the commit hooks work as on the command line; a
merge commit cannot be reverted. `X` opens a menu: soft keeps the index and
the worktree, mixed keeps the worktree and resets the index, and hard throws
away every uncommitted change, so it asks first. Revert and reset wait until
an operation in progress is finished or aborted.

`B` opens the Bisect menu: mark the selected commit bad or good, skip it, or
reset the bisect. With no bisect in progress, the first good or bad mark
starts one; it moves nothing until there is both a good and a bad commit, then
git checks out a commit to test and the Status panel takes over. A bisect
does not start while tracked files have changes, or while a merge, rebase,
cherry-pick or revert is in progress. Kika never runs `git bisect run`.

`y` opens the Copy menu: the selected commit's full hash, short hash, subject
or full message. Kika copies with the OSC 52 terminal escape, so it needs no
clipboard program and works over SSH. Some terminals do not support OSC 52 or
turn it off by default, and none says whether a copy worked, so "Copied" means
it was sent. Inside tmux it needs `set -g set-clipboard on`; GNU screen is not
supported. Text is cleaned of control characters first, and more than 64 KB
is refused.

`t` asks for a tag name and tags the selected commit; `T` then asks for a
one-line message too and makes an annotated tag. The commit is the one
selected when you pressed the key. A name git would refuse, one starting with
`-`, or `@` or `HEAD`, is refused before git runs, and the prompt stays open.

`s`, `F`, `d` and `e` rewrite history with `git rebase -i --autostash` on the
terminal: uncommitted changes are stashed and put back (as git's autostash
does, a staged change to a tracked file comes back unstaged), git's editor opens for
a squash's combined message or a new message, and hooks and signing work as on
the command line. Squash and fixup fold the selected commit into the one below
it. Kika acts as the rebase's sequence editor and checks git's todo list
against what you asked for; on any difference git aborts and nothing changes.
The commits from `HEAD` down to the one rewritten must be one line of history:
no merges, not the first commit, at most 1000 commits (400 on Windows), and
for squash and fixup the commit below must be loaded: moving down to the end of
the log loads it. A rebase that stops on conflicts can be
continued, skipped or aborted from the Status panel. Drop does not ask first;
the dropped commit stays in git's reflog.

`c` copies the selected commit, or takes it out of the copied set; mark each
commit you want in turn. Copied rows show `+` before their hash, and the
panel's title and the bottom bar count them; in the Commits panel the bar
puts `v` and `C` first while anything is copied. Check out another branch and press `v` to
cherry-pick the copied commits onto it with one `git cherry-pick` on the
terminal, so hooks and signing work as on the command line. They are picked
parents first, whatever order you copied them in. A merge commit cannot be
copied, a commit already on the current branch is refused before git runs, and
`v` waits for a clean worktree (untracked files may stay) and for an operation
in progress to finish. A pick that stops on conflicts, or on a commit the
branch already has (an empty pick), is resolved, continued, skipped or aborted
from the Status panel. The set stays after a paste, so it
can be pasted onto another branch too; `C` clears it, and so does switching
to another repository. The commits a paste makes can be undone with `ctrl-z`.

### Reflog (the Commits panel's Reflog tab)

Each row is one place `HEAD` has been, newest first: its commit, what moved
it, and git's message. The list is read-only; `ctrl-z` and `ctrl-r` undo and
redo from it.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `reflog.next` | `j`, `down` | Select the next reflog entry | |
| `reflog.prev` | `k`, `up` | Select the previous reflog entry | |
| `reflog.switch_tab` | `[`, `]` | Show the log | |

### Commit files (a commit opened from the Commits panel)

The diff pane shows the selected file's change in that commit, against its
first parent. `J`, `K`, `pgdn` and `pgup` scroll it, and `+`, `-` and `w`
change its context and whitespace as for any diff. Focusing another panel
goes back to the log.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `commit_files.next` | `j`, `down` | Select the next file and show its change | |
| `commit_files.prev` | `k`, `up` | Select the previous file and show its change | |
| `commit_files.back` | `esc`, `h`, `left` | Back to the log, at the same commit | |

### Diff (hunks)

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `diff.next` | `j`, `down` | Select the next hunk | |
| `diff.prev` | `k`, `up` | Select the previous hunk | |
| `diff.stage` | `space` | Stage the selected hunk, or unstage it in the staged view | |
| `diff.lines` | `v` | Pick changed lines inside the selected hunk | |
| `diff.side` | `t` | Switch between the unstaged and the staged changes | |
| `diff.discard` | `d` | Discard the selected hunk from the working tree | locked |
| `diff.open` | `o` | Open the file in your editor at the selected hunk | locked |
| `diff.edit_hunk` | `e` | Edit the selected hunk in your editor, then stage it | locked |
| `diff.renderer` | `R` | Cycle diff renderer | locked |
| `diff.back` | `h`, `left` | Back to the side panel last focused | |
| `diff.search` | `/` | Search the diff for typed text | |
| `diff.search_next` | `n` | Go to the next match of the search | |
| `diff.search_prev` | `N` | Go to the previous match of the search | |
| `diff.escape` | `esc` | Clear the search, else go back to the side panel | |

### Diff (lines)

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `lines.next` | `j`, `down` | Move to the next changed line | |
| `lines.prev` | `k`, `up` | Move to the previous changed line | |
| `lines.extend_down` | `J`, `down` | Grow the selection down a changed line | |
| `lines.extend_up` | `K`, `up` | Grow the selection up a changed line | |
| `lines.stage` | `space` | Stage the selected lines, or unstage them in the staged view | |
| `lines.back` | `esc`, `v` | Back to whole hunks | |
| `lines.discard` | `d` | Discard the selected lines from the working tree | locked |
| `lines.open` | `o` | Open the file in your editor at the selected line | locked |

### Blame

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `blame.next` | `j`, `down` | Move to the next line | |
| `blame.prev` | `k`, `up` | Move to the previous line | |
| `blame.page_down` | `pgdn` | Move down a page | |
| `blame.page_up` | `pgup` | Move up a page | |
| `blame.commit` | `enter` | Show the commit that last changed this line | |
| `blame.back` | `esc`, `h`, `left` | Back to the file list | |

### Commit opened from a blame line

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `blame_commit.scroll_down` | `j`, `down` | Scroll the commit down a line | |
| `blame_commit.scroll_up` | `k`, `up` | Scroll the commit up a line | |
| `blame_commit.page_down` | `pgdn` | Scroll the commit down a page | |
| `blame_commit.page_up` | `pgup` | Scroll the commit up a page | |
| `blame_commit.back` | `esc`, `h`, `left` | Back to the blame, at the same line | |

### Conflict view

Enter on a conflicted file opens it here. Each pick writes the file at once; blocks not
picked yet keep their markers. Once every block is picked, Kika offers to stage the file.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `conflict.next` | `j`, `down` | Move to the next conflict | |
| `conflict.prev` | `k`, `up` | Move to the previous conflict | |
| `conflict.ours` | `o` | Keep our side of this conflict and write the file | locked |
| `conflict.theirs` | `t` | Keep their side of this conflict and write the file | locked |
| `conflict.both` | `b` | Keep both sides of this conflict, ours first, and write the file | locked |
| `conflict.mergetool` | `M` | Resolve this file in your merge tool instead | locked |
| `conflict.back` | `esc` | Back to the file list; picks already written stay | |

### Stage all and commit?

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `commit_confirm.stage_all` | `y`, `enter` | Stage all changes, then write the message | |
| `commit_confirm.cancel` | `n`, `esc` | Do not commit | |

### Commit message

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `commit_editor.commit` | `ctrl-s` | Commit with this message | |
| `commit_editor.switch_field` | `tab` | Switch between subject and body | |
| `commit_editor.skip_hooks` | `ctrl-n` | Skip the pre-commit and commit-msg hooks for this commit | |
| `commit_editor.edit_externally` | `ctrl-e` | Write the message in your editor, then commit when it closes | locked |
| `commit_editor.cancel` | `esc` | Close without committing | |
| `commit_editor.quit` | `ctrl-c` | Quit Kika, stopping a commit that is running | locked |

### Discard the typed message?

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `commit_discard.discard` | `y` | Throw the typed message away | locked |
| `commit_discard.keep` | `n`, `esc` | Back to the message | |

### Commit failed

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `commit_error.back` | `esc`, `enter` | Back to the message | |

### Loading the last commit

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `commit_loading.cancel` | `esc` | Do not amend | |
| `commit_loading.quit` | `ctrl-c` | Quit Kika | locked |

### Help menu

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `help.close` | `?`, `esc` | Close this menu | |
| `help.down` | `j`, `down` | Scroll the list down | |
| `help.up` | `k`, `up` | Scroll the list up | |
| `help.page_down` | `pgdn` | Scroll the list down a page | |
| `help.page_up` | `pgup` | Scroll the list up a page | |

### Menus

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `menu.next` | `j`, `down` | Move down the menu | |
| `menu.prev` | `k`, `up` | Move up the menu | |
| `menu.select` | `enter` | Run the highlighted item | |
| `menu.back` | `esc` | Close this menu and go back to the one below | |
| `menu.quit` | `ctrl-c` | Quit Kika | locked |

### Yes/no questions

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `confirm.yes` | `y`, `enter` | Yes | locked |
| `confirm.no` | `n`, `esc` | No | locked |

### Text prompts

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `prompt.ok` | `enter` | Use the typed text | |
| `prompt.cancel` | `esc` | Close without using the text | |
| `prompt.quit` | `ctrl-c` | Quit Kika | locked |

### Filter or search (typing on the bottom bar)

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `input.accept` | `enter` | Keep the typed text; a search goes to its match | |
| `input.cancel` | `esc` | Clear the typed text and close | |
| `input.quit` | `ctrl-c` | Quit Kika | locked |

Every other key is typed, `q` and `?` included. The list or the search
follows each key as you type.

### Git is asking (a credential or other question)

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `credential.answer` | `enter` | Send the typed answer to git | locked |
| `credential.cancel` | `esc` | Cancel the question, which fails the operation | locked |
| `credential.quit` | `ctrl-c` | Cancel the question and quit Kika | locked |

A yes/no question from ssh uses the yes/no keys above instead.

### Error messages

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `error.close` | `esc`, `enter` | Close this message | |
| `error.quit` | `ctrl-c` | Quit Kika | locked |

### Discard this work?

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `discard_confirm.yes` | `y` | Yes, throw it away | locked |
| `discard_confirm.no` | `n`, `esc` | No, keep it | locked |
| `discard_confirm.quit` | `ctrl-c` | Quit Kika | locked |

### Delete this branch?

A branch whose commits are all merged is deleted with `y`. One with commits
that are not merged asks in stronger words, and only `D` deletes it.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `delete_branch.yes` | `y` | Yes, delete the merged branch | locked |
| `delete_branch.force` | `D` | Yes, delete the branch and its unmerged commits | locked |
| `delete_branch.no` | `n`, `esc` | No, keep the branch | locked |
| `delete_branch.quit` | `ctrl-c` | Quit Kika | locked |

### Remove this remote?

Removing a remote also deletes its remote-tracking branches, so `enter` does
not answer it.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `remove_remote.yes` | `y` | Yes, remove the remote and its remote-tracking branches | locked |
| `remove_remote.no` | `n`, `esc` | No, keep the remote | locked |
| `remove_remote.quit` | `ctrl-c` | Quit Kika | locked |
| `delete_tag.yes` | `y` | Yes, delete the tag here | locked |
| `delete_tag.no` | `n`, `esc` | No, keep the tag | locked |
| `delete_tag.quit` | `ctrl-c` | Quit Kika | locked |

### Reset hard?

A hard reset throws away every uncommitted change, staged and unstaged, so
`enter` does not answer it.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `reset_hard.yes` | `y` | Yes, reset and throw away every uncommitted change | locked |
| `reset_hard.no` | `n`, `esc` | No, keep the changes | locked |
| `reset_hard.quit` | `ctrl-c` | Quit Kika | locked |

### Undo or redo?

The question lists what would happen. An undo can move a branch away from
commits, so `enter` does not answer it.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `travel.yes` | `y` | Yes, do what the question says | locked |
| `travel.no` | `n`, `esc` | No, leave everything where it is | locked |
| `travel.quit` | `ctrl-c` | Quit Kika | locked |

### Submodule list

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `submodules.next` | `j`, `down` | Select the next submodule | |
| `submodules.prev` | `k`, `up` | Select the previous submodule | |
| `submodules.enter` | `enter` | Work in the selected submodule | |
| `submodules.update` | `u` | Clone or check out the selected submodule at the recorded commit | locked |
| `submodules.init` | `i` | Record the selected submodule's URL without cloning it | locked |
| `submodules.close` | `esc`, `q` | Close the list | |
| `submodules.quit` | `ctrl-c` | Quit Kika | locked |

### Worktree list

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `worktrees.next` | `j`, `down` | Select the next worktree | |
| `worktrees.prev` | `k`, `up` | Select the previous worktree | |
| `worktrees.open` | `enter` | Switch to the selected worktree | locked |
| `worktrees.add` | `a` | Add a worktree for the local branch selected in the Branches panel | locked |
| `worktrees.remove` | `d` | Remove the selected linked worktree | locked |
| `worktrees.close` | `esc`, `q` | Close the list | |
| `worktrees.quit` | `ctrl-c` | Quit Kika | locked |

### Command log

Every git command Kika ran this session, oldest first, with how it ended and
how long it took. Lines never show env, stdin, credentials in URLs, secret
config values or message bodies. `@` opens it with any modifiers, since
layouts type it with Shift, AltGr or Option.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `command_log.next` | `j`, `down` | Select the next command | |
| `command_log.prev` | `k`, `up` | Select the previous command | |
| `command_log.page_down` | `pgdn` | Move a page down | |
| `command_log.page_up` | `pgup` | Move a page up | |
| `command_log.close` | `esc`, `q` | Close the log | |
| `command_log.quit` | `ctrl-c` | Quit Kika | locked |

### Remove this worktree?

Removing a worktree deletes its directory, so `enter` does not answer it. Git
refuses a worktree with uncommitted or untracked changes, or a locked one, and
Kika never forces it.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `remove_worktree.yes` | `y` | Yes, remove the worktree and its directory | locked |
| `remove_worktree.no` | `n`, `esc` | No, keep the worktree | locked |
| `remove_worktree.quit` | `ctrl-c` | Quit Kika | locked |

### Recent repositories

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `recent.next` | `j`, `down` | Select the next row | |
| `recent.prev` | `k`, `up` | Select the previous row | |
| `recent.open` | `enter` | Open the selected repository | locked |
| `recent.init` | `i` | Make the directory Kika started in a repository | locked |
| `recent.close` | `esc`, `q` | Close the list | |
| `recent.quit` | `ctrl-c` | Quit Kika | locked |
