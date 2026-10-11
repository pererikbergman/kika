# Configuration

Kika works without any configuration. Two optional TOML files let you remap
keys, bind your own programs to keys, and tune how often Kika looks for
changes made outside it.

## Files and locations

| File | Where | Holds |
|---|---|---|
| Global | `$XDG_CONFIG_HOME/kika/config.toml`, else `~/.config/kika/config.toml` (Linux, macOS); `%APPDATA%\kika\config.toml` (Windows) | `[keys]`, `[watch]` and `[[commands]]` |
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
- or one of `space`, `esc`, `enter`, `tab`, `bksp`, `up`, `down`, `left`,
  `right`, `pgup`, `pgdn`, `home`, `end`;
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

**Never put a placeholder inside the script text.** The shell would read a
file name such as `$(rm -rf ~)` as code. When `command[0]` is `sh`, `bash`,
`zsh`, `dash`, `ksh` or `fish`, Kika rejects a placeholder in the script or in
any argument before it. Options with a value, such as
`["bash", "-o", "pipefail", "-c", "… \"$1\" …", "bash", "{path}"]`, work the
same way: the value goes after the script.

That check looks only at the program name in `command[0]`. It does not see a
shell started through another program, such as `env sh -c`, `busybox sh -c`,
`xargs sh -c`, `exec`, `nice` or `timeout`, or `cmd /c` and `pwsh -c` on
Windows, and it cannot check other interpreters either. For all of those,
and for `python -c`, `perl -e`, `node -e` and the like, the rule is yours to
keep: pass values as arguments after the code, and read them from there
(`"$1"`, `sys.argv`, `@ARGV`, `process.argv`).

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

## Global-only settings and trust

- **Auto-fetch is off unless `fetch` is set in the global file.** Setting it
  is your explicit opt-in to network access.
- **`[watch]`, and so auto-fetch, is allowed only in the global file.** A
  `[watch]` table in `.kika.toml` is an error, and the whole file is ignored.
- **`[[commands]]` is allowed only in the global file**, for the same reason:
  a cloned repository must not be able to run a program.
- **A repository's `.kika.toml` comes with a clone and is not trusted.** It
  may hold only `[keys]`. It cannot change any binding marked `locked` in the
  table below, and it cannot add a key that would hide a locked binding in an
  enclosing scope. For example, it cannot bind `files.stage` to `q`, because
  in the file list that key would take over `global.quit`.

Locked bindings quit, start other programs (your editor), reach the network,
discard work, answer a confirmation, or create or switch repositories. A
cloned repository could otherwise trap you in Kika, or move "discard" onto a
key you press without thinking. Locked bindings can still be changed in the
global file.

## Binding reference

Each id is `<scope>.<action>`. Keys are looked up in the focused scope first,
then its enclosing scopes: the file list, diff and blame panels fall back to
Workspace, and Workspace falls back to Global. Popups, menus and the commit
editor do not fall back, so `q` and `?` do not quit or open help there; each
popup that can stay open has its own `ctrl-c` to quit.

`(none)` means a binding has no default keys. `locked` in the last column
means a repository's `.kika.toml` cannot change it.

### Global (the main screens)

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `global.help` | `?` | Show the keys for this panel | |
| `global.quit` | `q`, `esc`, `ctrl-c` | Quit Kika | locked |

### Workspace (file list, diff and blame)

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `workspace.refresh` | `r` | Reload the list of changed files | |
| `workspace.scroll_down` | `J` | Scroll the diff down a line | |
| `workspace.scroll_up` | `K` | Scroll the diff up a line | |
| `workspace.page_down` | `pgdn` | Scroll the diff down a page | |
| `workspace.page_up` | `pgup` | Scroll the diff up a page | |
| `workspace.parent` | `bksp` | Back to the parent repository | |
| `workspace.more_context` | `+`, `=` | Show more context lines in the diff | |
| `workspace.less_context` | `-` | Show fewer context lines in the diff | |
| `workspace.whitespace` | `w` | Hide or show whitespace-only changes | |
| `workspace.recent` | `O` | Show recent repositories, to switch to one | |

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
| `files.hunks` | `enter` | Work on the hunks of the selected file | |
| `files.menu` | `m` | Open the actions menu | |

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
| `diff.back` | `esc`, `h`, `left` | Back to the file list | |

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

### Recent repositories

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `recent.next` | `j`, `down` | Select the next row | |
| `recent.prev` | `k`, `up` | Select the previous row | |
| `recent.open` | `enter` | Open the selected repository | locked |
| `recent.init` | `i` | Make the directory Kika started in a repository | locked |
| `recent.close` | `esc`, `q` | Close the list | |
| `recent.quit` | `ctrl-c` | Quit Kika | locked |
