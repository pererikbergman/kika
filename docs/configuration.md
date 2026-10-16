# Configuration

Kika works without any configuration. Two optional TOML files let you remap
keys, bind your own programs to keys, tune how often Kika looks for changes
made outside it, choose a program to draw the Diff pane, keep commits whose
hooks never ask anything in the background, show GitHub pull requests
beside your branches, say which terminals draw emoji sequences as one
picture, and choose the colours Kika draws with.

## Files and locations

| File | Where | Holds |
|---|---|---|
| Global | `$XDG_CONFIG_HOME/kika/config.toml`, else `~/.config/kika/config.toml` (Linux, macOS); `%APPDATA%\kika\config.toml` (Windows) | `[keys]`, `[watch]`, `[diff]`, `[commit]`, `[forge]`, `[display]`, `[clipboard]`, `[theme]` and `[[commands]]` |
| Repository | `.kika.toml` at the top level of the repository | `[keys]` only |

- Neither file is required. A missing file is not an error.
- An entered submodule uses its parent repository's `.kika.toml`, not its own.
  Outside a repository there is no repository file.
- Each file must be a regular file of at most 64 KiB, in UTF-8.
- The repository file must not be a symbolic link. The global file may be one.

A global file that uses every setting:

```toml
[keys]
"files.stage" = ["x", "ctrl-s"]   # replace this binding's keys
"files.discard" = []              # disable it

[watch]
refs = 2        # seconds
files = 5
fetch = 300     # leave out to keep auto-fetch off

[diff]
pager = "delta --paging=never"   # a program that draws the Diff pane
tool = "difft --color=always"    # a display-only diff tool

[commit]
hooks_detached = true   # commits that only run hooks stay in the background

[forge]
github = ["github.com"]   # leave out to keep pull requests off
gitlab = ["gitlab.com"]   # builds URLs; gitlab alone never contacts a host
gitlab_ask = ["gitlab.com"]  # asks glab for merge requests; leave out to keep it off
browser = ["open"]        # opens forge pages; leave out to keep it off

[forge.allow]
"github.com" = ["acme", "me/kika"]   # leave out to allow every repository on the host
"gitlab.com" = ["acme"]              # a gitlab_ask host is limited the same way

[display]
emoji_zwj_terminals = ["WezTerm"]   # leave out to keep emoji joiners escaped

[clipboard]
command = ["pbcopy"]   # copies instead of OSC 52; leave out to keep it off

[theme]
name = "light"   # leave out to keep the default colours

[theme.colors]
added = "light_green"   # one role's colour, over the theme
```

Earlier versions of this example bound `files.stage` to `s`. `s` now opens
the Stash menu in the Files panel (`files.stash`), so in a file that still
binds `s` to `files.stage` that one line is dropped: Kika shows `key "s" is
bound to both "files.stash" and "files.stage"; the change to "files.stage" was
dropped`, `files.stage` keeps `space`, `s` keeps opening the Stash menu, and
the rest of the file applies. Pick another key, as the example does with `x`,
or move `files.stash` off `s` in the same file.

## Layering

Settings are applied in three layers, each on top of the one before:

1. the built-in defaults,
2. the global file,
3. the repository's `.kika.toml`.

A `[keys]` entry that binds a key already taken in the same scope is dropped
on its own (see [`[keys]`](#keys)), and so is a global `[[commands]]` entry
on a key the global file's own `[keys]` gives a binding (see
[Keys](#keys-1) under Commands); the rest of the file applies. A file with
any other error is ignored as a whole, and the layer below it applies. Kika
shows the error so you can fix it. Unknown fields and unknown binding ids
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
- An entry that breaks the rule above is dropped, not the whole file. Its
  binding keeps the keys it has in the layer below: the built-in keys for the
  global file, and the global file's keys (else the built-in ones) for
  `.kika.toml`. If the file moves two bindings onto one key, both entries are
  dropped. A key the layer below holds, such as a default binding, a global
  binding or a global command when the clash is in `.kika.toml`, always stays
  where it is. Kika shows one error per dropped entry, naming the key and both
  bindings. In the global file, a key it gives both a binding and one of its
  own `[[commands]]` stays with the binding: the command is dropped instead.
  Every other problem, such as an unknown id, a key that does not parse,
  `global.quit = []` or a `.kika.toml` that touches a locked binding, still
  ignores the whole file, even when the same file has a droppable clash.

Key names:

- one printable character, such as `a`, `A`, `?` or `+`. Letters are
  case-sensitive: `J` is Shift+J;
- or one of `space`, `esc`, `enter`, `tab`, `shift-tab`, `bksp`, `up`, `down`,
  `left`, `right`, `pgup`, `pgdn`, `home`, `end`;
- either with an optional `ctrl-` or `alt-` prefix, written in lower case:
  `ctrl-s`, `alt-k`. Some macOS terminals send Option as Alt only once
  "Use Option as Meta" is turned on.

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

## `[commit]` (global file only)

By default a commit in a repository with an installed commit hook hands the
terminal to git, so a hook can ask you something. The screen switches away
and back on every such commit. If your hooks never ask anything, set
`hooks_detached` to keep those commits in the background, like a commit with
no hooks.

```toml
[commit]
hooks_detached = true   # default false: any commit hook hands the terminal over
```

- Only a commit whose sole reason to hand over is a hook runs detached. A
  commit that signs (`commit.gpgsign`) still hands the terminal over, since
  signing may ask for a passphrase.
- Rebase, cherry-pick, revert, a merge commit, continue and skip still run on
  the terminal, since they may also open git's editor.
- **Risk:** a hook that waits for input gets no terminal. It may fail, or wait
  until you quit Kika.

**Changes to `[commit]` take effect after a restart.**

## `[forge]` (global file only)

Kika can show each local branch's GitHub pull request (or GitLab merge
request, see [GitLab merge requests](#gitlab-merge-requests)) in the
Branches panel:
its number, its state (`open`, `draft`, `merged` or `closed`) and its checks
(`✓` passed, `✗` failed, `●` running, nothing when there are none), as in
`main  origin/main  #123 open ✓`. **It is off by default.** It turns on only
when you name the hosts Kika may ask:

```toml
[forge]
github = ["github.com", "github.example.com"]
```

- Leave the table out, or give an empty list, and the feature is off. Kika
  then never runs `gh`, never reads or writes the cache below, and never
  reaches the network for it.
- Each host is written as lower-case ASCII: letters, digits and `-`, in
  labels joined by dots, such as `github.com`. No scheme (`https://`), port,
  user name, path, wildcard (`*`) or trailing dot. Write an international
  domain name in its `xn--` form. A host that breaks these rules is an
  error, and the whole file is ignored.
- A repository is matched by its `origin` remote, or by its only remote when
  there is no `origin`. Its URL must be `https://host/owner/repo`,
  `ssh://host/owner/repo` or `user@host:owner/repo` (with or without `.git`),
  on the standard port, and the host must equal one you listed. Any other
  repository shows nothing. A branch shows the newest pull request
  into that repository whose head branch has the same name.
- Opening any clone whose origin is on a listed host runs read-only `gh`
  calls under your account, for that clone's owner/repo and branch names,
  unless `[forge.allow]` (below) leaves that repository out.
- Kika asks GitHub through the [GitHub CLI](https://cli.github.com), `gh`,
  which must be installed and logged in for the host (`gh auth login
  --hostname <host>`, or `GH_TOKEN`, `GITHUB_TOKEN` or `GH_ENTERPRISE_TOKEN`
  in the environment). `gh` holds your login: Kika never reads, stores,
  passes or shows a token. Each call shows in the Command log under its own
  line. A refresh first asks whether the pull requests changed:

  ```
  gh api --include --hostname <host> -H "Accept: application/vnd.github+json" -H "If-None-Match: <etag>" repos/<owner>/<repo>/pulls?state=all&sort=updated&direction=desc&per_page=1
  ```

  and then, when needed, reads the badges with
  `gh api graphql --include --hostname <host> --input -` (one call per 50
  branches). The `If-None-Match` part is left out when nothing is cached,
  and the log shows `<etag>`, never the value.
- Pull requests load when the repository opens, and again after a fetch,
  pull or push you start, or when you press `B` in the Branches panel.
  There is no timer. `B` says why when it cannot ask: no host listed, the
  remote on none of them or not in `[forge.allow]`, a refresh already
  running, or a rate limit.
- When the pull requests have not changed, GitHub answers that first
  conditional request for free (it does not count against your rate
  limit), and the cached badges are kept without the GraphQL query. The
  query still runs when the branches differ from the ones last asked about,
  when an open pull request's checks were still running or had failed, or
  when the last query is 15 minutes old or more, since checks can finish or
  re-run without changing the pull request. So a check that turns red on
  the same commit shows at the first refresh after those 15 minutes, at
  the latest.
- The last result is cached so it shows at once on the next start, in
  `$XDG_CACHE_HOME/kika/forge`, else `~/Library/Caches/kika/forge` (macOS),
  `~/.cache/kika/forge` (Linux) or `%LOCALAPPDATA%\kika\cache\forge`
  (Windows). It holds the host, owner, repository name and path, the
  branch names asked about, GitHub's ETag for the pull-request list, and
  each branch's number, state and checks, nothing else. Only you can read
  it.
- When GitHub cannot be reached, a line at the bottom of the Branches panel
  says why (`GitHub: offline`, `GitHub: not logged in`, `GitHub: gh not
  installed`, …), and the badges already shown stay, marked `· cached`.
  Where the panel is wide enough, the line also says what to do, such as
  `run gh auth login --hostname github.com`. Nothing else waits on it.
- When Kika starts, it checks once, in the background, that `gh` is
  installed and works, by running `gh --version`. This runs locally: it
  names no host, reaches no network and reads no token. Your login is not
  checked here; the first real call says when you are not logged in.
  Before the check answers, only the cache is read. When `gh` is not
  ready, the line says so before anything is asked, and no `gh` call is
  made for GitHub until it is:
  - `GitHub: gh not installed · install the GitHub CLI to see pull
    requests`: `gh` is not on `PATH`.
  - `GitHub: gh not working · check that gh --version runs`: it exited
    with an error, did not answer within 5 seconds, or printed no version.
  - `GitHub: gh 1.2.0 is too old · update to gh 1.5.0 or newer`: older than
    a version Kika needs. No minimum is set yet, so this does not show
    today.

  `B` runs the check again before it refreshes, so a `gh` installed after
  Kika started is picked up without a restart. The check is only run for a
  forge whose hosts you opted in: with no `[forge] github` host, `gh` is
  never run.
- When GitHub rate limits you, the line says `GitHub: rate limited`, and,
  where it fits, `until HH:MM UTC` (the badges shown are the cached ones,
  though the line leaves out `· cached` to keep room for the time). Kika then runs no `gh` call to that
  host until the limit resets (at least a minute, at most an hour), even
  for another repository. It does not retry by itself: the next open,
  fetch, pull, push or `B` after the reset asks again. Until then `B` is
  refused with a notice. The wait is kept only while Kika runs.

### Limiting which repositories are asked about

Listing a host lets every clone on it ask `gh` about its own repository. To
narrow that, give a host a list of owners and `owner/repo` names in
`[forge.allow]`:

```toml
[forge]
github = ["github.com", "github.example.com"]

[forge.allow]
"github.com" = ["acme", "pererikbergman/kika"]
```

- A host with no key in `[forge.allow]` (or no `[forge.allow]` at all)
  allows every repository on it, as above.
- A host with a key allows only the repositories its list names: `acme`
  allows every repository of the owner `acme`, `pererikbergman/kika` only
  that one. An empty list (`"github.com" = []`) allows none.
- Names are compared ignoring case, as GitHub does: `acme` also allows
  `ACME/widgets`. There are no patterns: `*`, a third `/` part (on a
  GitHub host), an empty part, a part of dots only or a name ending in
  `.git` is an error, as is any character other than letters, digits, `.`,
  `_` and `-`. A GitLab host takes group paths too; see
  [GitLab merge requests](#gitlab-merge-requests).
- Each key must be one of the hosts in `github` or `gitlab_ask`. A key for
  a host that is only in `gitlab`, or for a host not listed, is an error.
  Any error ignores the whole file.
- It gates every `gh` call for a repository: the badges, review mode, and
  creating a pull request. On a `gitlab_ask` host it gates every `glab`
  call the same way. A clone it leaves out shows no badges, its cache
  is neither read nor written, and `gh` is never run for it. Only the
  repository a call is about is checked: you can open a pull request from
  your own fork, whatever its owner, into a repository the list allows,
  and only allowed repositories are offered as its target.
- Opening or copying a forge page does not run `gh`, so it works
  for any repository on a listed host.

### GitLab merge requests

`gitlab` alone never contacts a host; `gitlab_ask` does. Name a GitLab host
in both, and its branches show their merge request, as in
`topic  origin/topic  !12 open ✓`:

```toml
[forge]
gitlab = ["gitlab.com", "gitlab.example.com"]
gitlab_ask = ["gitlab.example.com"]

[forge.allow]
"gitlab.example.com" = ["acme"]
```

- **It is off by default.** A host only in `gitlab` builds URLs and is
  never contacted. Each `gitlab_ask` host must also be in `gitlab`, and must
  not be in `github`; it is written as any host is (see above). A host that
  breaks a rule is an error, and the whole file is ignored.
- `[forge.allow]` takes a `gitlab_ask` host as a key, and limits which of
  its repositories `glab` is asked about, as it does for `gh`.
- A project may sit in nested groups, as `acme/sub/project`, up to 21
  parts in all (GitLab's 20 group levels and the project). Its remote URL
  is matched like any other (`https://host/acme/sub/project.git`,
  `git@host:acme/sub/project.git`, …), on a host in `gitlab` or
  `gitlab_ask`, and it gets the same badges, links and pages as a project
  in a top-level group. A part that is only `-` is not a project path, as
  GitLab keeps `/-/` for its own pages. A GitHub host never matches such a
  URL: there a repository is always `owner/repo`.
- An entry in a GitLab host's `[forge.allow]` list is a group or project
  path of up to 21 parts, compared ignoring case:

  ```toml
  [forge.allow]
  "gitlab.example.com" = ["acme", "beta/team", "gamma/sub/project"]
  ```

  - `acme` allows every project under the group `acme`, at any depth:
    `acme/widgets`, `acme/team/widgets`, `acme/a/b/c/widgets`. Paths are
    compared whole part by whole part, so `acme` never allows
    `acmecorp/widgets`.
  - Naming a subgroup instead of its group narrows it: `beta/team` allows
    the project `beta/team` and every project under the group `beta/team`
    (`beta/team/x`, `beta/team/deep/y`), but not `beta/other/x` or
    `beta/widgets`.
  - `gamma/sub/project` allows that project (GitLab never lets a group and
    a project share a path).

  The part rules are those above, and a part that is only `-` is an error
  too.
- Kika asks GitLab through the [GitLab CLI](https://gitlab.com/gitlab-org/cli),
  `glab`, found as `glab` on your `PATH`. It must be installed and logged in
  for the host (`glab auth login --hostname <host>`, or `GITLAB_TOKEN` in the
  environment). `glab` holds your login: Kika never reads, stores, passes or
  shows a token. Each refresh runs, once per 50 branches,

  ```
  glab api graphql --include --hostname <host> --input -
  ```

  with the query and branch names on its input, never on the command line.
  It runs with telemetry off (`GLAB_SEND_TELEMETRY=0`), no prompts and no
  update check, and without `GITLAB_HOST`, `GITLAB_URI`, `GITLAB_API_HOST`,
  `GITLAB_URL` or `GITLAB_REPO`, so only the command line picks the host.
  The answer is judged by its HTTP status line, not by `glab`'s exit code,
  so a GraphQL answer is read even when `glab` exits with an error.
- There is no probe: GitLab has no cheap "has anything changed" request
  here, so each refresh (open, fetch, pull, push, or `B`) runs the query.
  There is no timer, as on GitHub, and the last answer is cached the same
  way.
- A badge is `!` and the merge request's number, then its state: `open`
  (a `locked` merge request shows as open), `draft`, `merged` or `closed`.
  Its pipeline shows `✓` passed, `✗` failed or cancelled, `●` running,
  pending or scheduled, and nothing for a manual pipeline (it waits for a
  person), a skipped one, an unknown status, or none.
- `glab` is checked at start the same way, with `glab --version`, and only
  when a `gitlab_ask` host is set: `GitLab: glab not installed · install
  the GitLab CLI to see merge requests`, `GitLab: glab not working · check
  that glab --version runs`, or `GitLab: glab 1.2.0 is too old · update to
  glab 1.5.0 or newer`. While it shows, no `glab` call is made; `B` checks
  again.
- Problems show on the Branches panel's bottom line in GitLab's words:
  `GitLab: updating…`, `GitLab: not logged in · run glab auth login
  --hostname <host>`, `GitLab: offline`, `GitLab: repository not found or
  no access`, `GitLab: could not load merge requests`, and `GitLab: rate
  limited · until HH:MM UTC` or `GitLab: rate limit over · fetch to retry`.
  `B` during a rate limit says `GitLab is rate limited; refresh after the
  limit resets`.
- Review mode, creating and merging are GitHub only. On a GitLab host
  **Create pull request…** is off and says so, and the page `o` opens stays
  the filtered list of merge requests.

### Opening, copying and creating pull requests

`o` in the Branches panel opens the Pull request menu for the selected
branch:

- **Open in browser** starts the program `browser` names with the page's
  URL as its last argument. There is no built-in or guessed program: until
  you set `browser`, this item is off and says so.
- **Copy URL** puts the page's URL on the clipboard.
- **Create pull request…** opens a pull request on GitHub (see below).

The page of a GitHub branch is its pull request, so it needs the badge.
A GitLab branch's page is the list of its merge requests, in any state,
filtered to the branch, so it needs no number, even where a `gitlab_ask`
host shows the `!N` badge. Every URL is built by Kika from the remote's host, owner and
repository name, always `https://`, never with the user name, password,
token or port the remote URL may hold.

```toml
[forge]
github = ["github.com"]
gitlab = ["gitlab.com", "gitlab.example.com"]   # builds URLs
browser = ["open"]                             # macOS; ["xdg-open"] on Linux
```

- `gitlab` lists GitLab hosts the same way `github` lists GitHub ones, to
  build URLs. `gitlab` alone never contacts a host; `gitlab_ask` does, and
  turns the merge-request badges on (see
  [GitLab merge requests](#gitlab-merge-requests)).
- `browser` is the program and its arguments as a list, run directly with
  no shell. The program must be a bare name (looked up on your `PATH`) or
  an absolute path; anything else is an error, and the whole file is
  ignored. It runs in the background with no input or output, and shows in
  the Command log.
- Creating needs a GitHub branch with no pull request yet, pushed to a
  remote: the head is its upstream. Only remotes on the same GitHub host
  whose repository `[forge.allow]` allows can take it; with none, Kika says
  so and asks nothing. When more than one could, Kika asks which (Target
  remote), then which of
  its branches to merge into (Base branch, starting on `main`, else
  `master`). It then asks for the title, starting from the branch name, and
  shows the repository, `head → base` and the title. Only `y` creates it;
  `enter` does nothing there. The call is
  `gh api --include --hostname <host> --method POST repos/<owner>/<repo>/pulls --input -`,
  with the title and branch names on its input, never on the command line.
  A head pushed to another repository (a fork) is sent as `owner:branch`.
  The new badge shows at once, and the badges are read again. A pull
  request made on another remote's repository gets no badge, since badges
  come from `origin`; Kika says its number instead. When GitHub refuses,
  its message is shown.
- Kika does not push the branch, fill in a body, mark drafts, or ask for
  reviewers or labels.

The Commits panel's Copy menu (`y`) has **Forge URL**, the selected commit's
page.

### Reviewing pull requests

`G` anywhere in the workspace enters review mode for the repository's
GitHub pull requests; `kika review <number>` starts on one pull request.
Review mode lists four queues of the repository's open pull requests:
assigned to you, review requested from you, authored by you, and all open
(`tab` and `shift-tab` move between them, `/` filters by number, title or
author). `enter` opens one: its title, author, state, `base ← head`,
whether it can be merged, labels, reviewers, CI checks and description.
`o` opens a failing check's page with `browser`, asking which when several
fail. `f` shows the pull request's changed files: each with its status,
whether you marked it viewed on GitHub (`✓` viewed, `~` changed since you
viewed it, blank not viewed, `?` unknown) and its added and removed lines,
and the selected file's diff from the merge base of base and head, with
`/` search once `tab` moves the keys to the diff. `esc` goes back to the
pull request, then to the queue, then to the workspace.

- The files and diff come from local git: `git merge-base`, then
  `git diff-tree` from the merge base to the head. GitHub's diff is never
  read.
- When the head or base commit is not in the repository, `f` fetches the
  missing ones once, by commit id only:
  `git -c fetch.writeFetchHEAD=false fetch --quiet --no-tags --no-recurse-submodules -- <remote> <id>…`.
  No ref and no tag is written. It goes only to the remote (`origin`, or
  the only remote) whose one URL names the reviewed repository on a
  `github` host; with none, or with a remote that has several URLs, Kika
  says so and fetches nothing. A head is
  fetched at most once a session, so if it is still missing after that,
  Kika says so and leaves it to you. A password or passphrase is asked as
  for a fetch from the workspace.
- Viewed marks are read with the pull request, 100 files per
  `gh api graphql --include --hostname <host> --input -` call (the
  repository, number and page cursor go on its input), for at most 1000
  files (10 calls). Each call shows in the Command log. Paging stops at a
  rate limit or a failed page. Files past the cap or past such a stop, and
  files whose name is not UTF-8, show `?`.
- `v` on the files screen marks the selected file viewed on GitHub, or
  not viewed when it is already. It is one
  `gh api graphql --include --hostname <host> --input -` call, shown in
  the Command log; the pull request's id and the file's path travel on its
  input. It is asked only when you press `v`, only on an allowed `github`
  host, and not while that host is rate limited. GitHub keeps the mark per
  file for the whole pull request. While GitHub has not answered yet, the
  file's mark shows a dim `…`. A file whose mark is `?` can still be
  marked; its `?` turns to the mark once GitHub takes it. A file whose
  name is not UTF-8 cannot be marked.
- `c` on the files screen picks what it shows: "Whole pull request", or
  one of its commits, oldest first, as `<id> <subject>`. A commit's files
  and diff are against its first parent, so a merge commit shows what it
  brought in from that side. The selected file stays selected when the
  new view has it; otherwise the first file is. The commits are read once
  per files screen with `git log --no-show-signature --no-color --format=%H%x00%P%x00%s -n 251 <merge-base>..<head> --`;
  only the latest 250 are offered, and the list then ends with a note
  saying so. The list title shows the view, `whole pull request` or
  `commit 2/5 <id>`, and its viewed count is of the files listed. Viewed
  marks stay the pull request's: a commit's files show the same marks,
  and `v` there marks the file for the whole pull request. It is local git only and asks GitHub nothing.
- Opening a pull request (or `r`) then reads its review threads, comments
  and reviews in one more
  `gh api graphql --include --hostname <host> --input -` call, with the
  repository and number on its input. The detail says how many threads
  are unresolved. The files screen shows each thread under the line it is
  on, old or new side, with its author, time and whether it is resolved;
  this is in the whole pull request view only, and a commit's view shows
  the file's threads above its diff as "on the whole pull request".
  Resolved threads are folded to one line there; `enter` reads them in
  full (see below). Outdated threads show at the top of the file with
  the lines they were written on, as do comments on the whole file and
  threads whose line is not in the diff; that block takes at most half the
  diff and counts the threads it has no room for. `]` and `[` jump to the
  next and previous unresolved thread, across the files; they stop at the
  first and last, and the thread jumped to is drawn first. The diff's
  title says `threads loading…`, `threads unavailable` (go back with
  `esc` and press `r` to try again), how many are unresolved, or
  `thread 2/5` after a jump; a `+` there means GitHub has more threads
  than were read. At most 100 threads, 50 comments in each, and 200
  conversation items (100 comments and 100 reviews) are read.
- `C` on a pull request shows its conversation: the comments on the pull
  request and the review bodies, oldest first, each with its author, time
  and what it did (`commented`, `approved`, `requested changes`). `j`/`k`
  and `pgdn`/`pgup` scroll it (`pgdn`/`pgup` scroll the pull request too),
  and `esc` goes back to the pull request. It ends with
  `More on GitHub (showing the first N)` when GitHub has more than were
  read. `enter` on the files screen opens the selected thread (the one
  `]`/`[` went to), or the file's first, in a box over the screen with
  every comment in full, resolved threads included, and the lines an
  outdated thread was written on. In it `]`/`[` step through all of the
  file's threads, resolved ones too, `j`/`k`/`pgdn`/`pgup` scroll, and
  `esc` closes it; `]` on the diff then goes on from the thread read
  last. Neither one asks GitHub anything: both show what was read with
  the pull request.
- `a` on the files screen writes a comment on a line or a range of lines
  of the diff, old or new side. It starts a pick on the first numbered line
  in view: `j`/`k` move over the numbered lines (file and `@@` headers are
  skipped), `v` starts a range at the line picked, `enter` (or `a` again)
  writes the comment on the line or the range, and `esc` stops. A range
  stays inside one hunk. An added or unchanged line takes the comment on
  the new side, a removed line on the old side. The comment is typed in a
  box over the screen, markdown, as many lines as you like: `enter` is a
  new line, `tab` types a tab (it does not move the focus), `q` and `?`
  are typed, `ctrl-s` keeps it and `esc` drops it
  (asking first when it has text). `a` works in the whole pull request
  view only, not in a commit's (`c` goes back).
- A comment is not posted. It goes into the pending review, which only you
  see until you submit it; under its line the diff shows
  `✎ pending: <first line>`, and the file list's title counts the pending
  comments. `P` lists them as `path:L12` or `path:R10-R14` with their first
  line: `enter` or `e` edits one in the same box, `d` deletes one after a
  question only `y` answers, and `esc` closes the list. At most 500
  comments of up to 64 KiB each are kept per pull request.
- The pending review is kept on disk, so it is still there after Kika
  quits: one file per pull request and clone in
  `$XDG_STATE_HOME/kika/reviews`, else
  `~/Library/Application Support/kika/reviews` (macOS),
  `~/.local/state/kika/reviews` (Linux) or `%LOCALAPPDATA%\kika\reviews`
  (Windows). Each file is named by a hash, `<16 hex digits>.json`, and
  inside it names the host, owner, repository name, pull request number
  and path it belongs to, then each comment's file, lines, text and the
  head it was written on. Only you can read it. A file there that Kika did
  not write (a link, too large, malformed, of another version or for
  another pull request) is never read or overwritten: comments on that
  pull request are off, and Kika says why with the file's full path, so
  you can remove it to start again. Writing comments asks GitHub nothing
  and starts no other program.
- Each change is written as soon as you make it, one write at a time. If
  you quit while a write is still running and you changed something after
  it started, that newest change can be lost.
- `o` on the files screen opens the top line of the diff on GitHub's page
  of the pull request, with `browser`: the first numbered row at the top,
  or the selected thread's line (after `]` or `[`) while its row is in
  view. The bar then names the side and line opened, such as
  `Opening R12 on GitHub: src/cache.rs`. The page is
  `https://<host>/<owner>/<name>/pull/<n>/files#diff-<sha256 of the path><L|R><line>`,
  or `…/pull/<n>/commits/<id>#diff-…` in a commit's view; `L` is the old
  side and `R` the new. It asks GitHub nothing: the address is built
  locally. It is off, saying why, with no `browser`, no numbered line in
  view, or a file whose name is not UTF-8.

- GitHub is asked only when you enter review mode, open a pull request
  (which reads its viewed marks in up to 10 calls), press `r` or answer
  `y` to a merge, or press `v` on the files screen; opening a pull request
  or `r` asks for its detail, then its threads. Nothing refreshes by
  itself, and nothing GitHub sends is cached on disk.
- Only a `github` host is asked. With none, or with the remote on another
  host, review mode says so and asks nobody; a rate-limited host is not
  asked until its limit resets.
- Each request is one
  `gh api graphql --include --hostname <host> --input -`, shown in the
  Command log; the repository, searches and number travel on its input.
- Only the first 50 pull requests of each queue show. Descriptions show
  headings, lists, quotes, code, emphasis and links; images, tables and
  HTML show as text.

#### Merging a pull request

Under the mergeability line the detail says how it may be merged
(`Merge: merge commit · squash · rebase`, only the methods the repository
allows) or why it may not (`Cannot merge: …`). `M` merges it: when the
repository allows several methods it first asks which, then asks
`Merge #N <title> into <base> by <method>?`, adding "Some checks have not
passed" when a check that is not required failed. Only `y` answers yes;
`enter` does not.

- A pull request may be merged only when it is open, not a draft,
  mergeable, and GitHub's merge state is clean, or held up only by checks
  that are not required. Already merged, closed, a draft, conflicts, a
  missing required review or check, a branch behind its base, a state
  GitHub is still working out (press `r`), no allowed method, or no head
  commit each say why, in Kika's own words.
- The merge is one
  `gh api --include --hostname <host> --method PUT repos/<owner>/<name>/pulls/<number>/merge --input -`,
  shown in the Command log. The method and the head commit you saw travel
  on its input, so GitHub refuses rather than merge a head that moved since.
  GitHub's default commit title and message apply. GitHub's own refusal is
  shown cut short and with control characters escaped.
- Nothing else is asked first, and the branch is not deleted. Once merged,
  the detail says merged, the pull request leaves the queues and the local
  branch's badge turns `merged`; the badges are then read again once.
- Only a `github` host is asked, and not while it is rate limited.

**Changes to `[forge]` take effect after a restart.**

## `[display]` (global file only)

Some emoji are a sequence of several emoji joined by a zero-width joiner
(ZWJ, U+200D): a family, a person at a laptop, a heart on fire. A terminal
that clusters them draws the sequence as one picture, two columns wide.
Others draw each part, which takes more columns than Kika reserved and
pushes the row out of its pane. Kika cannot tell which kind of terminal it
runs in, so by default it shows every such joiner as `<U+200D>`.

`emoji_zwj_terminals` lists the terminals that draw these sequences as one
picture. In a listed terminal, diff and blame body lines show the picture.

```toml
[display]
emoji_zwj_terminals = ["WezTerm", "ghostty"]
```

| Setting | Meaning | Default |
|---|---|---|
| `emoji_zwj_terminals` | Terminal names, each matched exactly (case counts) against `$TERM_PROGRAM`, or against `$TERM` when `$TERM_PROGRAM` is unset or empty | empty: emoji joiners stay escaped everywhere |

- To find the name to list, run `echo "$TERM_PROGRAM"` in the terminal, or
  `echo "$TERM"` if that prints nothing. An empty name is refused, and the
  whole file is ignored with an error.
- Inside tmux, `$TERM_PROGRAM` is `tmux`, and tmux decides the width there.
  List `tmux` only if your tmux draws these sequences as one picture.
- Only a well-formed sequence of at most four emoji that measures two
  columns is drawn as a picture, and each emoji in it must be two columns
  wide on its own. A longer chain stays escaped. A
  symbol drawn as text, one column wide, such as `©`, `™`, `→` or a `❤`
  without U+FE0F, never counts as an emoji here, so a joiner next to one
  stays escaped. So does a joiner next to a letter, at the start or end of a
  line, in a run of joiners, or next to a hidden or direction-changing
  character.
- If joiners still show as `<U+200D>`, check the name with the commands
  above, capitals included. If rows run past the pane's edge instead, the
  terminal does not draw these sequences as one picture: take it off the
  list.
- File names, authors, commit headers, hunk headers, the conflict view and a
  `[diff]` program's output always escape emoji joiners.

**Changes to `[display]` take effect after a restart.**

## `[theme]` (global file only)

`[theme]` picks the colours Kika draws with. `name` chooses a built-in
theme, and `[theme.colors]` changes single roles on top of it. Without a
`[theme]` table Kika draws as it always has.

```toml
[theme]
name = "light"

[theme.colors]
added = "light_green"   # a colour name
match_bg = 229          # a 256-colour index
hash = "#005f87"        # a true colour
```

| Setting | Meaning | Default |
|---|---|---|
| `name` | The built-in theme to start from: `default`, `light` or `high-contrast` | `default` |
| `colors` | A table of role = colour, each one replacing that role's colour in the theme | empty |

The built-in themes:

- `default`: the colours Kika has always drawn, for dark backgrounds. Plain
  text keeps the terminal's own foreground colour.
- `light`: for light backgrounds. It sets every role, using 256-colour
  indexes, because the basic yellow, cyan and grey are hard to read on
  white in most terminal schemes.
- `high-contrast`: for dark backgrounds. Each default colour moves to its
  bright form, plain text is `white`, and dimmed text is `gray`.

A colour in `[theme.colors]` is one of:

- one of these 16 names, written exactly so (lowercase, with `_`):

  | Name | ANSI colour |
  |---|---|
  | `black` | 0 |
  | `red` | 1 |
  | `green` | 2 |
  | `yellow` | 3 |
  | `blue` | 4 |
  | `magenta` | 5 |
  | `cyan` | 6 |
  | `gray` | 7 |
  | `dark_gray` | 8 |
  | `light_red` | 9 |
  | `light_green` | 10 |
  | `light_yellow` | 11 |
  | `light_blue` | 12 |
  | `light_magenta` | 13 |
  | `light_cyan` | 14 |
  | `white` | 15 |

- `reset`, the terminal's own colour for that role. On `light` this is how
  to get your terminal's default text colour back: `text = "reset"`;
- a 256-colour index from `0` to `255`, as a number (`17`) or a string
  (`"17"`);
- a true colour as `"#rrggbb"`, six hex digits in either case
  (`"#3a3a3a"`, `"#AbCdEf"`).

Anything else is an error, and the whole file is ignored: another spelling
such as `"Green"`, `"darkgray"` or `"Reset"`, a short `"#fff"`, a number
above 255, or a value that is not a number or string. So is a `name` that
is not a built-in theme, a role that does not exist, or an unknown key in
`[theme]`. The error names the key and says what to write instead.

The roles, and their colour in each built-in theme. A bare number is a
256-colour index, and `reset` is the terminal's own colour:

| Role | Meaning | `default` | `light` | `high-contrast` |
|---|---|---|---|---|
| `text` | Plain text that still takes a colour slot: a plain status letter, the rebase `pick` action | `reset` | `black` | `white` |
| `dim` | Unfocused borders, hints, empty states, dates and disabled rows | `dark_gray` | `240` | `gray` |
| `focused` | The focused panel's border and the Diff pane's cursor gutter | `cyan` | `25` | `light_cyan` |
| `added` | Added lines and counts, and the status `A` | `green` | `28` | `light_green` |
| `removed` | Removed lines and counts, and the status `D` | `red` | `124` | `light_red` |
| `modified` | The status `M` | `yellow` | `130` | `light_yellow` |
| `status_renamed` | The status `R` and `C` | `cyan` | `25` | `light_cyan` |
| `hunk_header` | Hunk headers in the diff and hunk views | `cyan` | `25` | `light_cyan` |
| `warning` | Notices and things to look at: the bar notice, next steps, markers | `yellow` | `130` | `light_yellow` |
| `error` | Failures: a git command's error, conflicted files, load errors | `red` | `124` | `light_red` |
| `success` | Done and fine: viewed, check passed, mergeable | `green` | `28` | `light_green` |
| `pending` | A check still running | `yellow` | `130` | `light_yellow` |
| `neutral` | A check neutral or skipped; mergeability unknown | `dark_gray` | `240` | `gray` |
| `hash` | A commit's short hash | `cyan` | `25` | `light_cyan` |
| `ref_name` | A stash name or reflog selector | `yellow` | `130` | `light_yellow` |
| `ref_label` | A reflog entry's label | `cyan` | `25` | `light_cyan` |
| `author` | A commit's author | `green` | `28` | `light_green` |
| `graph` | The commit graph's lanes | `magenta` | `90` | `light_magenta` |
| `mark` | The diff mark and the custom-patch mark | `magenta` | `90` | `light_magenta` |
| `copied_commit` | Copied commits and the bar's copied note | `cyan` | `25` | `light_cyan` |
| `location` | Where a review comment sits | `cyan` | `25` | `light_cyan` |
| `conflict_ours` | The "ours" side of a conflict | `green` | `28` | `light_green` |
| `conflict_theirs` | The "theirs" side of a conflict | `cyan` | `25` | `light_cyan` |
| `thread_open` | A review thread still open | `yellow` | `130` | `light_yellow` |
| `thread_addressed` | A review thread addressed | `green` | `28` | `light_green` |
| `pr_open` | The PR badge: open | `green` | `28` | `light_green` |
| `pr_draft` | The PR badge: draft | `dark_gray` | `240` | `gray` |
| `pr_merged` | The PR badge: merged | `magenta` | `90` | `light_magenta` |
| `pr_closed` | The PR badge: closed | `red` | `124` | `light_red` |
| `match_fg` | The text of every search match, the current one included | `black` | `black` | `black` |
| `match_bg` | The background of a search match | `yellow` | `229` | `light_yellow` |
| `current_match_bg` | The background of the current search match | `light_red` | `217` | `light_red` |

- `match_fg` is the text of both kinds of search match. It is drawn on
  `match_bg` for every match, and on `current_match_bg` for the current
  one, so pick it to read on both.
- `focused` also colours the Diff pane's cursor gutter.
- The selected row is drawn in reverse video, the text and background
  swapped. It takes no colour of its own, so no role changes it.
- Colour is only an extra signal. Every status, mark and badge also shows
  as a letter, symbol or word, so nothing is lost in any theme or in a
  terminal without colour.
- Colours inside a `[diff]` program's output are that program's own; a theme
  does not change them.

**Changes to `[theme]` take effect after a restart.**

## `[clipboard]` (global file only)

Kika copies with the OSC 52 terminal escape (see the Copy menu under
Commits). If your terminal does not support OSC 52, or has it turned off,
`command` names a program to copy with instead. It is off by default.

```toml
[clipboard]
command = ["pbcopy"]                           # macOS
# command = ["wl-copy"]                        # Linux, Wayland
# command = ["xclip", "-selection", "clipboard"]  # Linux, X11
```

| Setting | Meaning | Default |
|---|---|---|
| `command` | The program and its arguments as a list, run directly with no shell | unset: copies use OSC 52 |

- The copied text goes to the program on stdin, never in its arguments. It
  is cleaned of control characters first, and more than 64 KB is refused,
  as with OSC 52.
- When `command` is set it replaces OSC 52: Kika never sends both, since it
  cannot tell whether an OSC 52 copy worked.
- The program must be a bare name, looked up on `PATH`, or an absolute path.
  An empty list, a relative path such as `bin/copy`, a control character, or
  a single string instead of a list is refused, and the whole file is ignored
  with a `clipboard.command` error.
- "Copied" shows as soon as the program starts. If it cannot start, exits
  with an error, or has not finished after 5 seconds (it is then stopped),
  a "Could not copy" error says so. Kika never waits for it while you type.
- The program runs on the machine Kika runs on. Over SSH that fills the
  remote machine's clipboard, not yours; OSC 52 is the way to reach your own.
- Each run is listed in the Command log with its arguments, never the text.

**Changes to `[clipboard]` take effect after a restart.**

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
- A repository's `.kika.toml` cannot move a binding onto a command's key:
  that entry is dropped, and the command keeps the key.
- In the global file, a command on a key that the same file's `[keys]` gives
  a binding in the command's scope is dropped on its own: the binding keeps
  the key, and the other commands and the rest of the file apply. The whole
  command is dropped, even if its other keys are free or clash with a default
  binding. Kika shows one error per dropped command, naming every key of it
  that a binding holds, those bindings, and the command by its place in the
  file and its description, for example `key "ctrl-t" is bound to both
  "files.stage" and command 1 ("Copy the file to…"); command 1 was dropped`.
  If several commands share that key, all of them are dropped. A dropped
  command binds nothing, so it never takes a key from any binding.
- The commands that are left must still follow every rule: one that clashes
  with a default binding the file did not move, with another command that is
  left, or that would hide a locked binding still ignores the whole global
  file.
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
- **`[display]` is allowed only in the global file.** It is off by default.
  A `[display]` table in `.kika.toml` is an error, and the whole file is
  ignored.
- **`[commit]` is allowed only in the global file**, since it decides whether
  hooks get the terminal. A `[commit]` table in `.kika.toml` is an error, and
  the whole file is ignored.
- **`[[commands]]` is allowed only in the global file**, for the same reason:
  a cloned repository must not be able to run a program.
- **`[forge]` is allowed only in the global file**, since it lets Kika contact
  a host and names the browser it runs. This includes `[forge.allow]`: a
  clone cannot widen or change which of its repositories are asked about. A
  `[forge]` or `[forge.allow]` table in `.kika.toml` is an error, and the
  whole file is ignored.
- **`[clipboard]` is allowed only in the global file**, since it names a
  program for Kika to run. A `[clipboard]` table in `.kika.toml` is an error,
  and the whole file is ignored.
- **`[theme]` is allowed only in the global file**, since a cloned
  repository that could recolour Kika could make a warning or an error hard
  to see. A `[theme]` or `[theme.colors]` table in `.kika.toml` is an error,
  and the whole file is ignored.
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
files, stash, stash files, diff, blame and conflict panels fall back to Workspace, and Workspace falls
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
| `workspace.review` | `G` | Review this repository's pull requests on GitHub | locked |
| `workspace.focus_status` | `1` | Focus the Status panel | |
| `workspace.focus_files` | `2` | Focus the Files panel | |
| `workspace.focus_branches` | `3` | Focus the Branches panel | |
| `workspace.focus_commits` | `4` | Focus the Commits panel | |
| `workspace.focus_stash` | `5` | Focus the Stash panel | |
| `workspace.focus_diff` | `0`, `l`, `right` | Focus the diff, to work on the selected file's hunks, or pick lines of a commit's file | |
| `workspace.next_panel` | `tab` | Focus the next panel | |
| `workspace.prev_panel` | `shift-tab` | Focus the previous panel | |
| `workspace.zoom` | `z` | Zoom the focused panel: normal, half, full | |
| `workspace.patch_menu` | `ctrl-p` | Show what can be done with the custom patch: remove, move or split it off | locked |
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
| `files.escape` | `esc` | Clear the selection, else the filter, else the diff search, else quit Kika | locked |
| `files.search_diff` | `ctrl-f` | Search the selected file's diff for typed text | |
| `files.search_next` | `n` | Go to the next match of the diff search | |
| `files.search_prev` | `N` | Go to the previous match of the diff search | |
| `files.log` | `L` | Show the selected file's history in the Commits panel | |

The filter matches each word you type, in order but not necessarily side by
side, against the file's path; it ignores case until you type an uppercase
letter. `V` starts a range at the selected file, and moving the cursor grows
it. `space` then stages the whole range in one go (or unstages it, when
nothing in it is left to stage), skipping conflicted files, and `d` asks
once before discarding the range; only `y` answers that question. Keys that
act on one file are refused while a range is selected.

`ctrl-f` searches the diff beside the list without focusing it, as `/` does
in the Diff pane: each key you type searches again, Enter scrolls to the
match, and `n` and `N` step through the matches. A diff still streaming in is
searched as far as it has loaded; the pane's title says `of 7+` until the
rest arrives. The commit files and stash files lists search their diff the
same way. Esc clears the search before it does anything else.

| `files.stash` | `s` | Stash changes: all, with untracked files, staged only, or this file | locked |

`s` opens a menu that puts work aside without committing it: every tracked
change, every change with the untracked files too, only the staged changes,
or only the selected file. Each asks for an optional message. Stashing only
the staged changes needs git 2.35 or newer; an older git is refused with a
message and nothing changes. Nothing can be stashed while conflicts exist.

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
| `branches.pull_request` | `o` | Open, copy or create the selected branch's pull request | locked |
| `branches.refresh_pull_requests` | `B` | Ask GitHub again for the branches' pull requests | locked |
| `branches.set_upstream` | `u` | Set the remote branch the selected branch tracks | |
| `branches.unset_upstream` | `U` | Stop the selected branch tracking its upstream | |
| `branches.switch_tab` | `]` | Show the remotes and their branches | |
| `branches.prev_tab` | `[` | Show the tags | |
| `branches.mark` | `M` | Mark the selected branch to diff against (enter on another); again to clear | |
| `branches.diff` | `enter` | Show what changed from the diff mark to the selected branch | |
| `branches.filter` | `/` | Filter the local branches by typed text | |
| `branches.range` | `V` | Select a range from here, for delete; again to clear it | |
| `branches.escape` | `esc` | Clear the selection, else the filter, else quit Kika | locked |

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

`/` narrows the tab shown as you type, as in the Files panel, and each tab
keeps its own filter. `V` starts a range; `d` over it asks once about every
branch in it. `y` deletes only the merged ones: the current branch and any
branch with unmerged commits are kept and counted in the question, and `D`
still deletes one unmerged branch at a time. Every other row key waits until
`esc` clears the range. Switching tabs clears the range, not the filter.

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
| `remotes.filter` | `/` | Filter the remotes and their branches by typed text | |
| `remotes.range` | `V` | Select a range from here, for remove; again to clear it | |
| `remotes.escape` | `esc` | Clear the selection, else the filter, else quit Kika | locked |

None of these reaches the network. A remote you add shows no branches until
you fetch with `f`. Kika refuses a remote URL that uses the `ext` or `fd`
transport (`ext::…`, `ext://…`, `fd::…`, `fd://…`), since those run a command
or use a file descriptor on every fetch; add such a remote with git directly. A remote named like an option
(starting with `-`), or refs left under a name no remote has, cannot be
renamed or removed from Kika.

The filter matches a branch row as `remote/branch`, and a remote's row shows
whenever its name matches or one of its branches shows. `d` over a range
removes the configured remotes whose own rows are in it; branch rows in the
range are left alone, since deleting a branch on a remote needs the network.

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
| `tags.filter` | `/` | Filter the tags by typed text | |
| `tags.range` | `V` | Select a range from here, for delete; again to clear it | |
| `tags.escape` | `esc` | Clear the selection, else the filter, else quit Kika | locked |

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
an older git refuses them and changes nothing. `s` first checks for work done
since the stop, and asks before skipping when it finds some (see "Skip the
stopped commit?" below).

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
| `commits.open` | `enter` | Show the files the selected commit changed, or those changed from the diff mark to it | |
| `commits.checkout` | `space` | Check out the selected commit as a detached HEAD, asking first | |
| `commits.revert` | `R` | Make a commit that undoes the selected one | locked |
| `commits.reset` | `X` | Reset the current branch to the selected commit: soft, mixed or hard | locked |
| `commits.squash` | `s` | Squash the selected commit into the one below, combining their messages | locked |
| `commits.fixup` | `F` | Fold the selected commit into the one below, keeping that one's message | locked |
| `commits.drop` | `d` | Drop the selected commit and its change from the history | locked |
| `commits.reword` | `e` | Write a new message for the selected commit | locked |
| `commits.move_up` | `alt-k`, `alt-up` | Move the selected commit or todo row up, above the newer one | locked |
| `commits.move_down` | `alt-j`, `alt-down` | Move the selected commit or todo row down, below the older one | locked |
| `commits.edit` | `E` | Stop the rebase at the selected commit, to amend it, then continue from Status | locked |
| `commits.break` | `b` | Stop the rebase right after the selected commit, or toggle the stop after a todo row | locked |
| `commits.pick` | `u` | Pick the selected todo row as it is, undoing its other action | locked |
| `commits.create_fixup` | `ctrl-f` | Commit the staged change as a fixup! of the selected commit | locked |
| `commits.autosquash` | `S` | Fold every fixup! commit into the commit it fixes, after asking | locked |
| `commits.find_base` | `ctrl-b` | Select the commit that last touched the staged lines | |
| `commits.switch_tab` | `[`, `]` | Show HEAD's reflog: everywhere HEAD has been | |
| `commits.tag` | `t` | Tag the selected commit with a lightweight tag | |
| `commits.tag_annotated` | `T` | Tag the selected commit with an annotated tag and a message | |
| `commits.branch_at_head` | `N` | Create a branch at the detached HEAD and check it out | |
| `commits.copy` | `c` | Add the selected commit to the copied set, or take it out | |
| `commits.paste` | `v` | Cherry-pick the copied commits onto the current branch, oldest first | locked |
| `commits.clear_copied` | `C` | Clear the copied set | |
| `commits.filter` | `/` | Filter the loaded commits by typed text | |
| `commits.range` | `V` | Select a range of commits from here, for copy, squash, fixup and drop; again to clear it | |
| `commits.escape` | `esc` | Clear the selection, else the filter, else the diff mark, else the log filter, else quit Kika | locked |
| `commits.bisect` | `B` | Mark the selected commit good, bad or skipped for a bisect, or reset it | locked |
| `commits.copy_menu` | `y` | Copy hash or message (the terminal must allow OSC 52; in tmux, set-clipboard on) | |
| `commits.filter_path` | `L` | Show only the commits that changed a typed path, across renames | |
| `commits.filter_author` | `A` | Show only the commits by a typed author | |
| `commits.mark` | `M` | Mark the selected commit to diff against (enter on another); again to clear | |

`L` and `A` ask git for a narrower log, typed on the bottom bar and run when
you press `enter`: `L` takes one file or directory, relative to the
repository's top level, and follows the file across renames; `A` matches the
typed text anywhere in the author's name and email, as plain text, ignoring
case until you type an uppercase letter. Both can be set at once, and an empty
answer clears that half. The title names what the log is narrowed to, the
graph is hidden, and the rewrites (squash, fixup, drop, reword, moving a
commit, edit and break) wait until it is cleared, since the row below need not
be the commit's parent.

`M` marks a commit for diffing (`M` on a local branch in the Branches panel
marks the branch). `enter` on another commit, or on another branch, then lists
the files changed from the mark to it, in place of the log, and the diff pane
shows each one. The marked row shows `◆` in both panels. In the Commits
panel, `esc` clears a range, then the `/` filter, then the mark, then the log
filter, and only then quits; in the Branches panel, `M` on the marked branch
clears the mark.

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

`y` opens the Copy menu: the selected commit's full hash, short hash, subject,
full message, or its page on the forge (Forge URL; see `[forge]`). Kika copies with the OSC 52 terminal escape, so it needs no
clipboard program and works over SSH. Some terminals do not support OSC 52 or
turn it off by default, and none says whether a copy worked, so "Copied" means
it was sent. Inside tmux it needs `set -g set-clipboard on`; GNU screen is not
supported. Text is cleaned of control characters first, and more than 64 KB
is refused. If OSC 52 does not work in your terminal, set `[clipboard]
command` in the global file to copy with a program such as `pbcopy` instead.

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

`alt-k` and `alt-j` move the selected commit above the newer one or below the
older one, the same way (as do `alt-up` and `alt-down`; in this panel only
plain arrows move the cursor, so Shift+arrow does nothing). `E` stops the rebase at the selected commit so you
can amend it, and `b` stops it right after the selected commit (`b` needs Git
2.20 or newer); continue from the Status panel to finish. With Git 2.38 or
newer every rewrite passes `--update-refs`, so other branches that point at
rewritten commits move to their new copies; a move that would change which
commits such a branch holds is refused. With an older Git, a rewrite that
would leave another branch behind is refused and names the branch.

To fix an older commit, stage the fix and press `ctrl-b`: Kika blames the
lines the staged change touches in `HEAD` and moves the cursor to the commit
that last changed them. When they come from several commits it names the
newest three, counts the rest, and leaves the cursor; newly added files have
no such commit. `ctrl-f` then
commits the staged change as `fixup! <subject>` of the selected commit, with
`git commit --fixup`, so hooks and signing work as for any commit. `S` asks
first (only `y` answers, not Enter), then folds every `fixup!` commit into the
commit it fixes in one rewrite: each moves to just after its target, older
fixups first, and becomes a fixup. The target is the nearest older commit
whose subject is the fixup's, without its `fixup! ` prefixes. Only the line
of history from `HEAD` down to the first merge is searched; if a fixup's
target is not loaded yet, the whole autosquash is refused, and moving down
to the end of the log loads more. A fixup or target whose subject holds
control characters, or starts or ends with spaces, is refused rather than
matched, since Kika shows such subjects changed. Kika orders the fixups itself and passes
`--no-autosquash`, so `rebase.autoSquash` has no effect, and `squash!` and
`amend!` commits stay as they are. With `--update-refs`, a branch on a fixup
or on a commit a fixup moves past is refused, like a move.

While a rebase is stopped, its remaining steps show above the log under
"Rebase todo", the last to run at the top. `j` and `k` move through them and
on into the log. On a todo row, `alt-k` and `alt-j` move it, `s`, `F`, `d`,
`e`, `E` and `u` (pick) change its action, and `b` adds or removes a stop
after it. Each change is checked and written to git's todo list at once,
only if the list on disk is still the one shown; git's help comments are not
kept. A commit never moves past another branch's update line. A todo list
holding a line Kika does not edit, such as `exec` or a comment written with a
`core.commentChar` other than `#`, is shown as it is under "Rebase todo
(read-only)"; an edit key then names the line and its number. Edit such a list
with `git rebase --edit-todo`, or continue or abort from the Status panel.
Rebases Kika starts itself always use `#` for comments, whatever
`core.commentChar` says, so their todo list stays editable.

`s`, `F` and `d` also act on a range of log rows, in one rebase. `s` and `F`
turn every commit in the range into a squash or a fixup, so the whole range
folds into the commit just below it; to fold only the selected commits
together, leave the oldest of them out of the range. `d` drops every commit in
the range, and asks first, naming how many, when that is two or more; only `y`
answers. The whole range is checked before git runs, and anything a single
commit would be refused for refuses the range. A range the `/` filter has
gaps in, because it hides commits inside the selection, is refused rather
than widened to commits you cannot see; clear the filter first. If the range
changes while the drop question is open, `y` drops nothing.

Reword, moves, edit, break and pick act on one row: they wait while a range is
selected (`esc` clears it), and on a log row the filter hides. While the log is
filtered, `alt-k` and `alt-j` do not move log commits at all, since the commit
next to the selected one may be hidden; clear the filter first. The filter
leaves the todo rows above the log alone, so they stay editable.

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
redo from it, whatever the filter hides. `/` narrows the rows as you type,
matching each row's short id and message as drawn; `enter` keeps the filter
and `esc` clears it, then quits as everywhere else. The filter only narrows
what is shown and never reaches git. `L` and `A` do not filter the reflog:
they only tell you that path and author filters work on the log tab.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `reflog.next` | `j`, `down` | Select the next reflog entry | |
| `reflog.prev` | `k`, `up` | Select the previous reflog entry | |
| `reflog.switch_tab` | `[`, `]` | Show the log | |
| `reflog.filter` | `/` | Filter the reflog by typed text | |
| `reflog.escape` | `esc` | Clear the filter, else quit Kika | locked |
| `reflog.filter_path` | `L` | Does not filter here; only says that path filters work on the log tab | |
| `reflog.filter_author` | `A` | Does not filter here; only says that author filters work on the log tab | |

### Commit files (a commit opened from the Commits panel)

The diff pane shows the selected file's change in that commit, against its
first parent. `J`, `K`, `pgdn` and `pgup` scroll it, and `+`, `-` and `w`
change its context and whitespace as for any diff. Focusing another panel
goes back to the log.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `commit_files.next` | `j`, `down` | Select the next file and show its change | |
| `commit_files.prev` | `k`, `up` | Select the previous file and show its change | |
| `commit_files.patch` | `space` | Pick the selected file's whole change for the custom patch, or drop it | |
| `commit_files.back` | `h`, `left` | Back to the log, at the same commit | |
| `commit_files.search` | `ctrl-f` | Search the selected file's change for typed text | |
| `commit_files.search_next` | `n` | Go to the next match of the diff search | |
| `commit_files.search_prev` | `N` | Go to the previous match of the diff search | |
| `commit_files.filter` | `/` | Filter the commit's files by typed text | |
| `commit_files.escape` | `esc` | Clear the filter, else the diff search, else go back to the log, at the same commit | locked |

#### Custom patches

A custom patch is a set of changes picked from one commit, to take out of
it. In a commit's files, `space` picks the selected file's whole change, or
drops it. `l` (or `0`, `right`) focuses the diff on the file's change in the
commit, where `space` picks the selected hunk's changed lines, or in line
mode (`v`) the selected lines; picking the same lines again drops them.
Nothing is staged and git does not run. Picked files show `●` (whole) or `◐`
(some lines), picked lines a `●` beside them, and the Commits panel's title
names the patch's commit and counts its files. `t`, `d` and `e` do nothing in
a commit's change. A first commit, a merge commit and a comparison cannot be
picked from, nor renamed or copied files; a binary or submodule file is
refused when the rewrite starts, before anything changes. A patch holds
one commit's changes: picking from another commit is refused until the
patch is reset. It lives only while Kika runs.

`ctrl-p` shows what can be done with it:

- **Remove from commit** takes it out of its commit.
- **Move to selected commit** takes it out and puts it into the commit under
  the Commits panel's cursor, newer or older.
- **Move to index** takes it out and leaves it staged.
- **New commit after source…** asks for a message, takes it out, and commits
  it right after its commit.
- **Reset patch** forgets the picks; nothing in the repository changes.

Each rewrite asks first, and only `y` answers it. Kika rebases with a stop at
each commit the patch changes and applies it there with `git apply --index`,
amending with `--no-verify` as the rebase's own picks do; signing follows
your config. Moving to an older commit stops only there: when git picks the
patch's commit again, the change is already in place and drops out of it.
If the patch's lines sit right next to other changes of their commit, that
pick conflicts. A rewrite needs a clean worktree (untracked files may stay),
no operation in progress, the whole log in view and the patch's commit
loaded. If any step fails, including a conflict, the rebase is aborted (or,
once it has finished, the branch is reset with `--keep`), so `HEAD`, the
index and the worktree end as they were and the patch is kept. On success
the patch is dropped.

### Stash

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `stash.next` | `j`, `down` | Select the next stash | |
| `stash.prev` | `k`, `up` | Select the previous stash | |
| `stash.files` | `enter` | Show the files the selected stash holds | locked |
| `stash.apply` | `space` | Apply the selected stash to the worktree and keep it | locked |
| `stash.pop` | `g` | Apply the selected stash and drop it; a conflict keeps it | locked |
| `stash.drop` | `d` | Drop the selected stash, or the range, asking first | locked |
| `stash.rename` | `R` | Give the selected stash a new message; it moves to stash@{0} | locked |
| `stash.filter` | `/` | Filter the stashes by typed text | |
| `stash.range` | `V` | Select a range from here, for drop; again to clear it | |
| `stash.escape` | `esc` | Clear the selection, else the filter, else quit Kika | locked |

`/` narrows the list as you type, matching each row as drawn:
`stash@{N}` and the message. The filter only picks rows; it never reaches
git. `V` starts a range; `d` over it asks once, naming how many stashes go
and which, and only `y` answers. Kika checks every entry in the range is
still where it was listed before it drops any, then drops them from the
deepest up, so no drop renumbers one still to go. If the list changed, none
is dropped and the list reloads. While a range is set, `enter`, `space`, `g`
and `R` wait until `esc` clears it.

Git has no rename for a stash, so `R` stores the entry again with the new
message, as `stash@{0}`, and drops it from its old place. Git adds no entry
for the commit already on top, so a stash that shares its commit with
`stash@{0}`, or `stash@{0}` when `stash@{1}` is the same commit, cannot be
renamed: drop the copy first, then rename. Nothing changes when it is refused. Each action checks
the entry is still where it was when the key was pressed; if the list changed
under Kika, it is refused and the list reloads.

### Stash files (a stash opened from the Stash panel)

The diff pane shows the selected file's change in that stash, against the
commit it was made on. Untracked files the stash took show as added. Focusing
another panel goes back to the list.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `stash_files.next` | `j`, `down` | Select the next file and show its change | |
| `stash_files.prev` | `k`, `up` | Select the previous file and show its change | |
| `stash_files.close` | `bksp` | Back to the stash list, at the same stash | |
| `stash_files.search` | `ctrl-f` | Search the selected file's change for typed text | |
| `stash_files.search_next` | `n` | Go to the next match of the diff search | |
| `stash_files.search_prev` | `N` | Go to the previous match of the diff search | |
| `stash_files.escape` | `esc` | Clear the diff search, else go back to the stash list, at the same stash | locked |

### Diff (hunks)

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `diff.next` | `j`, `down` | Select the next hunk | |
| `diff.prev` | `k`, `up` | Select the previous hunk | |
| `diff.stage` | `space` | Stage the selected hunk, or unstage it in the staged view; in a commit's change, pick it for the custom patch or drop it | |
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
| `lines.stage` | `space` | Stage the selected lines, or unstage them in the staged view; in a commit's change, pick them for the custom patch or drop them | |
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
| `blame.back` | `h`, `left` | Back to the file list | |
| `blame.search` | `/` | Search the blamed lines' text for typed text | |
| `blame.search_next` | `n` | Go to the next match of the search | |
| `blame.search_prev` | `N` | Go to the previous match of the search | |
| `blame.escape` | `esc` | Clear the search, else go back to the file list | locked |

The search reads each line's text, not its commit, author or date. Enter on
the bar puts the cursor on the match, so a second Enter shows its commit.

### Commit opened from a blame line

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `blame_commit.scroll_down` | `j`, `down` | Scroll the commit down a line | |
| `blame_commit.scroll_up` | `k`, `up` | Scroll the commit up a line | |
| `blame_commit.page_down` | `pgdn` | Scroll the commit down a page | |
| `blame_commit.page_up` | `pgup` | Scroll the commit up a page | |
| `blame_commit.back` | `h`, `left` | Back to the blame, at the same line | |
| `blame_commit.search` | `/` | Search the commit for typed text | |
| `blame_commit.search_next` | `n` | Go to the next match of the search | |
| `blame_commit.search_prev` | `N` | Go to the previous match of the search | |
| `blame_commit.escape` | `esc` | Clear the search, else go back to the blame, at the same line | locked |

The commit's search is its own: the blame's search is still there when you
go back to it.

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

### Delete the selection?

`d` over a range in the Branches or Stash panel asks once, naming how many go
and the first few names. Only `y` answers it; `enter` does not.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `delete_range.yes` | `y` | Yes, delete what the question names | locked |
| `delete_range.no` | `n`, `esc` | No, keep them all | locked |
| `delete_range.quit` | `ctrl-c` | Quit Kika | locked |

### Reset hard?

A hard reset throws away every uncommitted change, staged and unstaged, so
`enter` does not answer it.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `reset_hard.yes` | `y` | Yes, reset and throw away every uncommitted change | locked |
| `reset_hard.no` | `n`, `esc` | No, keep the changes | locked |
| `reset_hard.quit` | `ctrl-c` | Quit Kika | locked |

### Leave commits behind?

Asked before a detached `HEAD` moves off commits that no branch, tag or
remote-tracking branch reaches. Afterwards only the reflog has them, so
`enter` does not answer it.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `leave_commits.yes` | `y` | Yes, move HEAD and leave the commits behind | locked |
| `leave_commits.no` | `n`, `esc` | No, stay where HEAD is | locked |
| `leave_commits.quit` | `ctrl-c` | Quit Kika | locked |

### Drop stash?

A dropped stash is gone from the stash list, so `enter` does not answer it.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `drop_stash.yes` | `y` | Yes, drop the stash | locked |
| `drop_stash.no` | `n`, `esc` | No, keep the stash | locked |
| `drop_stash.quit` | `ctrl-c` | Quit Kika | locked |

### Abort the operation in progress?

Aborting a merge, rebase, cherry-pick or revert loses resolved conflicts,
and resetting a bisect drops its marks, so `enter` does not answer it.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `abort_confirm.yes` | `y` | Yes, abort the operation | locked |
| `abort_confirm.no` | `n`, `esc` | No, keep the operation going | locked |
| `abort_confirm.quit` | `ctrl-c` | Quit Kika | locked |

### Skip the stopped commit?

Skipping a rebase, cherry-pick or revert commit loses the work done since
the stop, so `enter` does not answer it. It is only asked when there is such
work: a conflict resolved in the index, a conflicted file edited, in a rebase,
an unstaged change to a tracked file, or, at a rebase `edit` or `break` stop,
a staged change. If Kika cannot tell, it asks. With no work since the stop,
`s` skips right away.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `skip_confirm.yes` | `y` | Yes, skip the commit and lose the work since the stop | locked |
| `skip_confirm.no` | `n`, `esc` | No, keep the work and the stop | locked |
| `skip_confirm.quit` | `ctrl-c` | Quit Kika | locked |

### Fold every fixup! commit?

Autosquash rewrites commits you may not be looking at, so `enter` does not
answer it.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `autosquash_confirm.yes` | `y` | Yes, fold every fixup! commit into its target | locked |
| `autosquash_confirm.no` | `n`, `esc` | No, leave the history as it is | locked |
| `autosquash_confirm.quit` | `ctrl-c` | Quit Kika | locked |

### Rewrite with the custom patch?

A custom patch rewrites commits you may not be looking at, so `enter` does
not answer it.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `patch_confirm.yes` | `y` | Yes, rewrite the history with the custom patch | locked |
| `patch_confirm.no` | `n`, `esc` | No, leave the history as it is | locked |
| `patch_confirm.quit` | `ctrl-c` | Quit Kika | locked |

### Drop the selected commits?

Dropping a range throws away the changes of several commits at once, so
`enter` does not answer it. The question counts the commits; `y` drops them
only if the range still holds the same ones.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `drop_range_confirm.yes` | `y` | Yes, drop the selected commits and their changes | locked |
| `drop_range_confirm.no` | `n`, `esc` | No, leave the history as it is | locked |
| `drop_range_confirm.quit` | `ctrl-c` | Quit Kika | locked |

### Undo or redo?

The question lists what would happen. An undo can move a branch away from
commits, so `enter` does not answer it.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `travel.yes` | `y` | Yes, do what the question says | locked |
| `travel.no` | `n`, `esc` | No, leave everything where it is | locked |
| `travel.quit` | `ctrl-c` | Quit Kika | locked |

### Create this pull request?

The question names the repository, `head → base` and the title. Creating it
reaches GitHub, so `enter` does not answer it.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `create_pull_request.yes` | `y` | Yes, create the pull request on GitHub | locked |
| `create_pull_request.no` | `n`, `esc` | No, create nothing | locked |
| `create_pull_request.quit` | `ctrl-c` | Quit Kika | locked |

### Merge this pull request?

The question names the number, title, base and method. Merging reaches
GitHub and cannot be undone, so `enter` does not answer it.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `merge_pr.yes` | `y` | Yes, merge the pull request on GitHub | locked |
| `merge_pr.no` | `n`, `esc` | No, merge nothing | locked |
| `merge_pr.quit` | `ctrl-c` | Quit Kika | locked |

### Check out this pull request?

`b` on a pull request's detail checks its head out as the local branch
`pr/<number>` and goes to the workspace. It reads the branch with
`for-each-ref`'s `%(worktreepath)`, so it needs git 2.23 or newer; an
older git says why and nothing runs.

The question names the branch `pr/<number>`, what happens to it, and the
remote a missing head is fetched from first. Nothing is fetched before
`y`, and `enter` does not answer it. A branch with commits the pull
request does not have, or one another worktree has checked out, is never
moved. Right before it runs, the branch is read again: if it, or the
branch checked out here, changed while the question was open, nothing
runs. A branch that is behind is switched to first and only then
fast-forwarded, so a switch git refuses for local changes leaves it
where it was. Uncommitted changes come along, or git refuses.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `checkout_pr.yes` | `y` | Yes, check out the pull request's branch | locked |
| `checkout_pr.no` | `n`, `esc` | No, check out nothing | locked |
| `checkout_pr.quit` | `ctrl-c` | Quit Kika | locked |

### Pick one (a pull request's target remote or base branch, a failing check, or a merge method)

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `pick_list.next` | `j`, `down` | Select the next row | |
| `pick_list.prev` | `k`, `up` | Select the previous row | |
| `pick_list.pick` | `enter` | Take the selected row | locked |
| `pick_list.close` | `esc`, `q` | Close the list and stop | |
| `pick_list.quit` | `ctrl-c` | Quit Kika | locked |

### Review (GitHub pull request queues and one pull request)

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `review.next` | `j`, `down` | Select the next pull request or file, or scroll the open one down | |
| `review.prev` | `k`, `up` | Select the previous pull request or file, or scroll the open one up | |
| `review.open` | `enter` | Open the selected pull request | locked |
| `review.next_queue` | `tab` | Show the next queue | |
| `review.prev_queue` | `shift-tab` | Show the previous queue | |
| `review.filter` | `/` | Filter the queue by number, title or author | |
| `review.refresh` | `r` | Ask GitHub again for the queues or the open pull request | locked |
| `review.open_check` | `o` | Open a failing check's page in the browser | locked |
| `review.back` | `esc` | Back to the pull request from its files or conversation, else to the queue; in the queue, clear the filter, else back to the workspace | locked |
| `review.files` | `f` | Show the pull request's changed files and diff, fetching its commits once if they are not here | locked |
| `review.merge` | `M` | Merge the open pull request on GitHub, after a question only y answers | locked |
| `review.checkout` | `b` | Check out the open pull request as branch pr/<number> and go to the workspace, after a question only y answers | locked |
| `review.conversation` | `C` | Show the pull request's conversation: its comments and review bodies, oldest first | |
| `review.comment_general` | `a` | Write a comment on the pull request, posted to GitHub after a question only y answers | locked |
| `review.page_down` | `pgdn` | Scroll the pull request or its conversation down a page | |
| `review.page_up` | `pgup` | Scroll the pull request or its conversation up a page | |

### Review files (one pull request's changed files and diff)

The file list has the Review keys too: `j`/`k` pick a file and show its
diff, `esc` goes back to the pull request.

A review thread marked **addressed** (in green) is unresolved, and GitHub
says the lines it was written on changed in a later push. It says the
code moved, not that the comment was answered; resolved threads whose
lines changed still read `outdated`.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `review_files.focus` | `tab` | Move the keys between the file list and the diff | |
| `review_files.viewed` | `v` | Mark the selected file viewed on GitHub, or not viewed if it is | locked |
| `review_files.commit` | `c` | Review the whole pull request or one of its commits | |
| `review_files.next_thread` | `]` | Go to the next unresolved review thread | |
| `review_files.prev_thread` | `[` | Go to the previous unresolved review thread | |
| `review_files.thread` | `enter` | Read the selected review thread, or the file's first, with every comment in full | |
| `review_files.open_line` | `o` | Open the selected thread's line, else the top line, on GitHub's pull request page | locked |
| `review_files.comment` | `a` | Pick a diff line or range and write a comment on it, kept in the pending review | |
| `review_files.pending` | `P` | List the pending review's comments, to edit or delete them | |
| `review_files.submit` | `S` | Submit the pending review to GitHub: approve, request changes or comment, with a summary | locked |
| `review_diff.down` | `j`, `down` | Scroll the diff down a line | |
| `review_diff.up` | `k`, `up` | Scroll the diff up a line | |
| `review_diff.page_down` | `pgdn` | Scroll the diff down a page | |
| `review_diff.page_up` | `pgup` | Scroll the diff up a page | |
| `review_diff.search` | `/` | Search the lines of the diff loaded so far for typed text | |
| `review_diff.search_next` | `n` | Go to the next match of the search | |
| `review_diff.search_prev` | `N` | Go to the previous match of the search | |
| `review_diff.escape` | `esc` | Clear the search, else go back to the file list | |

### Review thread (one review thread read in full over the files screen)

`q` and `?` do nothing here; `esc` closes the thread.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `review_thread.down` | `j`, `down` | Scroll the thread down a line | |
| `review_thread.up` | `k`, `up` | Scroll the thread up a line | |
| `review_thread.page_down` | `pgdn` | Scroll the thread down a page | |
| `review_thread.page_up` | `pgup` | Scroll the thread up a page | |
| `review_thread.next` | `]` | Read the file's next review thread, resolved ones included | |
| `review_thread.prev` | `[` | Read the file's previous review thread, resolved ones included | |
| `review_thread.reply` | `a` | Write a reply to the thread, posted to GitHub after a question only y answers | locked |
| `review_thread.resolve` | `R` | Resolve the thread on GitHub, or unresolve it if it is resolved | locked |
| `review_thread.close` | `esc` | Close the thread and go back to the files | |
| `review_thread.quit` | `ctrl-c` | Quit Kika | locked |

### Pick lines to comment on (`a` on the files screen)

`q` and `?` do nothing here; `esc` stops picking.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `review_pick.down` | `j`, `down` | Move to the next numbered line | |
| `review_pick.up` | `k`, `up` | Move to the previous numbered line | |
| `review_pick.range` | `v` | Start a range at this line, or drop it | |
| `review_pick.confirm` | `enter`, `a` | Write a comment on the line or range picked | |
| `review_pick.suggest` | `s` | Write a code suggestion for the line or range picked | |
| `review_pick.cancel` | `esc` | Stop picking and go back to the files | |
| `review_pick.quit` | `ctrl-c` | Quit Kika | locked |

### Pending comment (the comment being typed)

Every other key is typed into the comment: `enter` is a new line, and `q`
and `?` are typed.

`ctrl-e` opens the comment in the editor git uses (`GIT_EDITOR`,
`core.editor`, `VISUAL`, `EDITOR`, then git's default), and closing it
counts as `ctrl-s`.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `review_comment.save` | `ctrl-s` | Keep a line comment in the pending review, or ask to post a reply or general comment | |
| `review_comment.edit_externally` | `ctrl-e` | Write the comment in your editor; closing it saves, as ctrl-s does | locked |
| `review_comment.cancel` | `esc` | Close the comment, asking first when it has text | |
| `review_comment.quit` | `ctrl-c` | Quit Kika | locked |

### Pending review (`P` on the files screen)

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `review_pending.down` | `j`, `down` | Select the next pending comment | |
| `review_pending.up` | `k`, `up` | Select the previous pending comment | |
| `review_pending.edit` | `enter`, `e` | Edit the selected comment | |
| `review_pending.delete` | `d` | Delete the selected comment, after a question only y answers | |
| `review_pending.submit` | `S` | Submit the pending review to GitHub: approve, request changes or comment, with a summary | locked |
| `review_pending.close` | `esc` | Close the list | |
| `review_pending.quit` | `ctrl-c` | Quit Kika | locked |

### Delete this comment? / Discard this comment?

The comment is lost, so `enter` does not answer it.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `review_comment_confirm.yes` | `y` | Yes: delete or discard the comment, or post it to GitHub | locked |
| `review_comment_confirm.no` | `n`, `esc` | No, keep the comment | locked |
| `review_comment_confirm.quit` | `ctrl-c` | Quit Kika | locked |

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
| `worktrees.filter` | `/` | Filter the worktrees by branch or path | |
| `worktrees.escape` | `esc` | Clear the filter, else close the list | locked |
| `worktrees.close` | `q` | Close the list | |
| `worktrees.quit` | `ctrl-c` | Quit Kika | locked |

### Command log

Every git command Kika ran this session, oldest first, with how it ended and
how long it took. Lines never show env, stdin, credentials in URLs, secret
config values or message bodies. `@` opens it with any modifiers, since
layouts type it with Shift, AltGr or Option. `/` narrows it to the rows that
match, as drawn: state, time, `[terminal]` marker and the cleaned line. Esc
clears the filter first and closes the log on the next press.

| Binding id | Default keys | Action | Repo file |
|---|---|---|---|
| `command_log.next` | `j`, `down` | Select the next command | |
| `command_log.prev` | `k`, `up` | Select the previous command | |
| `command_log.page_down` | `pgdn` | Move a page down | |
| `command_log.page_up` | `pgup` | Move a page up | |
| `command_log.filter` | `/` | Filter the commands shown | |
| `command_log.escape` | `esc` | Clear the filter, else close the log | locked |
| `command_log.close` | `q` | Close the log | |
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
