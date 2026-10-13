//! The repository's worktrees: listing them, adding one for a local branch
//! and removing a linked one. Paths come from git or from the user and the
//! commands write to the filesystem, so every path reaches argv as raw
//! bytes after `--`, a typed target is checked before git sees it, and
//! nothing here ever forces.

use std::path::{Component, Path, PathBuf};

use super::trust::mark_untrusted;
use crate::git::branches::BranchName;
use crate::git::commit_files::CommitId;
use crate::git::error::GitError;
use crate::git::path::{RepoPath, os_string};
use crate::git::runner::{GitCommand, GitRunner};

/// One worktree as `git worktree list --porcelain` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: RepoPath,
    pub head: Option<CommitId>,
    /// The checked-out local branch; `None` when detached or on another ref.
    pub branch: Option<BranchName>,
    pub detached: bool,
    pub bare: bool,
    pub locked: bool,
    /// Its directory is gone; `git worktree prune` would forget it.
    pub prunable: bool,
    /// The first record: the repository's own worktree, never removable.
    pub main: bool,
    /// Read without `-z`, the record had a line that is no known key, which
    /// is what a path holding a newline looks like, or its `.git` does not
    /// lead back to this repository. Its path is not trusted.
    pub unsure: bool,
}

impl Worktree {
    /// A record for `path` with nothing else known yet.
    fn at(path: &[u8], main: bool) -> Self {
        Self {
            path: RepoPath::from_bytes(path),
            head: None,
            branch: None,
            detached: false,
            bare: false,
            locked: false,
            prunable: false,
            main,
            unsure: false,
        }
    }

    /// The path as the filesystem names it, from the raw bytes.
    pub fn path_buf(&self) -> PathBuf {
        PathBuf::from(self.path.as_os_str())
    }
}

/// A read: no optional locks, and English output so the keys are fixed.
/// It names no path and runs no hook, so literal pathspecs change nothing.
/// `-z` (git 2.36+) ends fields with NUL, so any path survives whole.
pub fn list_command(z: bool) -> GitCommand {
    let mut cmd = GitCommand::new("worktree").arg("list").arg("--porcelain");
    if z {
        cmd = cmd.arg("-z");
    }
    cmd.standard_env()
}

/// Every worktree, the main one first. `-z` is tried first; a git too old
/// for it fails, and the list is read again with newlines. Only that
/// second failure is reported. A record read with newlines could be forged
/// by a path holding one, so each is checked against the shared git dir.
pub fn read_worktrees(runner: &dyn GitRunner) -> Result<Vec<Worktree>, String> {
    if let Ok(output) = runner.run(&list_command(true))
        && output.success
    {
        return Ok(parse_list(&output.stdout, 0));
    }
    let output = runner
        .run_checked(&list_command(false))
        .map_err(|e| e.message_or("git worktree list failed"))?;
    let mut list = parse_list(&output.stdout, b'\n');
    match git_dirs(runner, &runner.workdir()) {
        Ok((_, common)) => mark_untrusted(&mut list, &common),
        Err(_) => list.iter_mut().for_each(|w| w.unsure = true),
    }
    Ok(list)
}

/// Parse porcelain output whose fields end at `sep`. An empty field ends a
/// record; a record with no `worktree` field is dropped. Unknown keys are
/// skipped, except with newlines, where such a line marks the record
/// `unsure` because a path holding a newline would split into one.
pub fn parse_list(bytes: &[u8], sep: u8) -> Vec<Worktree> {
    let mut list = Vec::new();
    let mut current: Option<Worktree> = None;
    // Whether the record being read has any field yet, and whether one
    // came before it; only the very first record is the main worktree.
    let (mut started, mut first) = (false, true);
    let mut stray = false;
    for field in bytes.split(|&b| b == sep) {
        if field.is_empty() {
            if let Some(mut worktree) = current.take() {
                worktree.unsure |= stray;
                list.push(worktree);
            }
            if started {
                first = false;
            }
            (started, stray) = (false, false);
            continue;
        }
        started = true;
        if let Some(path) = field.strip_prefix(b"worktree ") {
            match current {
                Some(_) => stray = true,
                None => current = Some(Worktree::at(path, first)),
            }
            continue;
        }
        let Some(worktree) = current.as_mut() else {
            stray = true;
            continue;
        };
        if !apply_field(worktree, field) && sep == b'\n' {
            stray = true;
        }
    }
    if let Some(mut worktree) = current {
        worktree.unsure |= stray;
        list.push(worktree);
    }
    if sep == b'\n' && looks_split(&list) {
        for worktree in &mut list {
            worktree.unsure = true;
        }
    }
    list
}

/// Whether newline output shows signs of a path that held a newline:
/// git prints `HEAD` for every worktree that is not bare, and never lists
/// a path twice. A blank line inside a path can fake a whole record, so
/// then no record is trusted.
fn looks_split(list: &[Worktree]) -> bool {
    let headless = list.iter().any(|w| !w.bare && w.head.is_none());
    let repeated = list
        .iter()
        .enumerate()
        .any(|(i, w)| list[..i].iter().any(|seen| seen.path == w.path));
    headless || repeated
}

/// Fill in one non-`worktree` field; `false` when the key is unknown.
fn apply_field(worktree: &mut Worktree, field: &[u8]) -> bool {
    if let Some(oid) = field.strip_prefix(b"HEAD ") {
        worktree.head = std::str::from_utf8(oid).ok().and_then(CommitId::parse);
    } else if let Some(name) = field.strip_prefix(b"branch ") {
        worktree.branch = name
            .strip_prefix(b"refs/heads/")
            .filter(|short| !short.is_empty())
            .map(BranchName::from_bytes);
    } else if field == b"detached" {
        worktree.detached = true;
    } else if field == b"bare" {
        worktree.bare = true;
    } else if field == b"locked" || field.starts_with(b"locked ") {
        worktree.locked = true;
    } else if field == b"prunable" || field.starts_with(b"prunable ") {
        worktree.prunable = true;
    } else {
        return false;
    }
    true
}

/// `git worktree add -- <abs path> <branch>`. The short name is passed,
/// since `refs/heads/x` would make git detach. A name that git could read
/// as an option, or `-` (git's `@{-1}`), is refused. Only the locale: it
/// takes a path and runs the `post-checkout` hook.
pub fn add_command(abs_path: &Path, branch: &BranchName) -> Result<GitCommand, String> {
    if branch.0.is_empty() || branch.0.starts_with(b"-") {
        return Err(format!(
            "Refusing to add a worktree for branch {}",
            branch.display()
        ));
    }
    Ok(GitCommand::new("worktree")
        .arg("add")
        .arg("--")
        .arg(abs_path.as_os_str())
        .arg(branch.as_os_string())
        .locale_env())
}

/// `git worktree remove -- <path>`. Never forced: git refuses a worktree
/// with changes or a lock, and that refusal is what the user sees. Only
/// the locale, since it takes a path.
pub fn remove_command(path: &RepoPath) -> GitCommand {
    GitCommand::new("worktree")
        .arg("remove")
        .arg("--")
        .arg(path.as_os_str())
        .locale_env()
}

/// What a failed `worktree remove` says. Before git 2.17 there is no
/// `remove`, and git answers with its usage text.
pub fn remove_error(error: &GitError) -> String {
    let message = error.message_or("git worktree remove failed");
    if message.starts_with("usage: git worktree") {
        return "Removing a worktree needs Git 2.17 or newer".into();
    }
    message
}

/// Remove the linked worktree at `path`.
pub fn remove_worktree(runner: &dyn GitRunner, path: &RepoPath) -> Result<(), String> {
    // Git removes the worktree it runs in, so the check that keeps Kika's
    // own one is made here too, on resolved paths.
    let workdir = runner.workdir();
    if !workdir.as_os_str().is_empty()
        && canonical(Path::new(path.as_os_str())) == canonical(&workdir)
    {
        return Err("Switch to another worktree first".into());
    }
    runner
        .run_checked(&remove_command(path))
        .map(drop)
        .map_err(|e| remove_error(&e))
}

/// Check the typed target, then add a worktree for `branch` there.
/// Returns the absolute path it was made at.
pub fn add_worktree(
    runner: &dyn GitRunner,
    typed: &str,
    branch: &BranchName,
) -> Result<PathBuf, String> {
    let workdir = runner.workdir();
    let (git_dir, common_dir) = git_dirs(runner, &workdir)?;
    let path = prepare_add(&workdir, &git_dir, &common_dir, typed)?;
    runner
        .run_checked(&add_command(&path, branch)?)
        .map_err(|e| e.message_or("git worktree add failed"))?;
    Ok(path)
}

/// This worktree's git directory and the one all worktrees share, with
/// relative output taken from `workdir`, where the command ran. A plumbing
/// read that names no path.
fn git_dirs(runner: &dyn GitRunner, workdir: &Path) -> Result<(PathBuf, PathBuf), String> {
    let cmd = GitCommand::new("rev-parse")
        .arg("--git-dir")
        .arg("--git-common-dir")
        .standard_env();
    let output = runner.run_checked(&cmd).map_err(|e| e.to_string())?;
    let stdout = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
    let mut lines = stdout.split(|&b| b == b'\n');
    match (lines.next(), lines.next(), lines.next()) {
        (Some(git_dir), Some(common), None) if !git_dir.is_empty() && !common.is_empty() => {
            let dir = |bytes: &[u8]| workdir.join(PathBuf::from(os_string(bytes)));
            Ok((dir(git_dir), dir(common)))
        }
        _ => Err("Could not find the repository's git directory".into()),
    }
}

/// Turn the typed target into the absolute path to give git, refusing one
/// that is inside a git directory, names a `.git` component, or already
/// holds anything but an empty real directory. Blocking: run it in a task.
pub fn prepare_add(
    workdir: &Path,
    git_dir: &Path,
    common_dir: &Path,
    typed: &str,
) -> Result<PathBuf, String> {
    let joined = normalize(&workdir.join(typed));
    let refused = |why: &str| Err(format!("Refusing {}: {why}", joined.display()));
    if has_dot_git(&joined) {
        return refused("a .git directory is not a worktree");
    }
    match std::fs::symlink_metadata(&joined) {
        Ok(meta) if meta.is_dir() => {
            let empty = std::fs::read_dir(&joined)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false);
            if !empty {
                return refused("it is not empty");
            }
        }
        Ok(meta) if meta.file_type().is_symlink() => return refused("it is a symbolic link"),
        Ok(_) => return refused("it is a file"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return refused(&e.to_string()),
    }
    let path = canonical(&joined);
    if has_dot_git(&path) {
        return refused("a .git directory is not a worktree");
    }
    for dir in [git_dir, common_dir] {
        let dir = canonical(&normalize(dir));
        if path.starts_with(&dir) {
            return refused("it is inside the repository's git directory");
        }
    }
    Ok(path)
}

/// `path` with `.` dropped and `..` taken back a component, by text alone.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The deepest existing ancestor of `path` resolved, with the rest put back.
pub(super) fn canonical(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut base = path;
    loop {
        if let Ok(real) = base.canonicalize() {
            let real = without_verbatim(real);
            return rest.iter().rev().fold(real, |dir, name| dir.join(name));
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                base = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Windows canonical paths start `\\?\`, which git does not expect; a
/// drive path without it means the same file.
#[cfg(windows)]
pub(super) fn without_verbatim(path: PathBuf) -> PathBuf {
    let stripped = path
        .to_str()
        .and_then(|text| text.strip_prefix(r"\\?\"))
        .filter(|rest| !rest.starts_with("UNC\\"))
        .map(PathBuf::from);
    stripped.unwrap_or(path)
}

#[cfg(not(windows))]
pub(super) fn without_verbatim(path: PathBuf) -> PathBuf {
    path
}

/// Whether any component is `.git`, in any letter case, as a
/// case-insensitive filesystem would read it.
fn has_dot_git(path: &Path) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|name| name.eq_ignore_ascii_case(".git"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::CliRunner;
    use crate::git::fake::{self, FakeRunner, TempDir, fail, ok};
    use crate::git::runner::EnvProfile;
    use crate::test_support::env_list;

    /// The env of a read.
    const STANDARD: [(&str, &str); 3] = [
        ("LC_ALL", "C"),
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("GIT_LITERAL_PATHSPECS", "1"),
    ];

    /// `stderr` as a failed `worktree remove` returns it.
    fn removal_failed(stderr: &[u8]) -> GitError {
        let output = crate::git::runner::GitOutput {
            success: false,
            stdout: Vec::new(),
            stderr: stderr.to_vec(),
        };
        GitError::failed(&remove_command(&"/r/x".into()), &output)
    }

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    /// Porcelain output for `records`, each a list of fields, with `sep`.
    fn porcelain(records: &[&[&[u8]]], sep: u8) -> Vec<u8> {
        let mut out = Vec::new();
        for record in records {
            for field in *record {
                out.extend_from_slice(field);
                out.push(sep);
            }
            out.push(sep);
        }
        out
    }

    fn head(oid: &str) -> Vec<u8> {
        format!("HEAD {oid}").into_bytes()
    }

    fn every_kind(sep: u8) -> Vec<Worktree> {
        let (ha, hb) = (head(A), head(B));
        let out = porcelain(
            &[
                &[b"worktree /r/main", &ha, b"branch refs/heads/main"],
                &[
                    b"worktree /r/linked",
                    &hb,
                    b"branch refs/heads/feature/x",
                    b"locked because",
                ],
                &[
                    b"worktree /r/detached",
                    &ha,
                    b"detached",
                    b"prunable gitdir gone",
                ],
                &[b"worktree /r/bare.git", b"bare"],
            ],
            sep,
        );
        parse_list(&out, sep)
    }

    #[test]
    fn every_kind_of_record_is_read_in_both_modes() {
        for sep in [0, b'\n'] {
            let list = every_kind(sep);
            assert_eq!(list.len(), 4, "{sep}");
            let main = &list[0];
            assert!(main.main && !main.locked && !main.unsure);
            assert_eq!(main.path.display(), "/r/main");
            assert_eq!(main.head, CommitId::parse(A));
            assert_eq!(main.branch, Some(BranchName::from("main")));

            let linked = &list[1];
            assert!(!linked.main && linked.locked && !linked.prunable);
            assert_eq!(linked.branch, Some(BranchName::from("feature/x")));
            assert_eq!(linked.head, CommitId::parse(B));

            let detached = &list[2];
            assert!(detached.detached && detached.prunable && detached.branch.is_none());

            let bare = &list[3];
            assert!(bare.bare && bare.head.is_none() && !bare.unsure);
        }
    }

    #[test]
    fn a_plain_locked_or_prunable_without_a_reason_is_read() {
        let out = porcelain(&[&[b"worktree /r", b"locked", b"prunable"]], 0);
        let list = parse_list(&out, 0);
        assert!(list[0].locked && list[0].prunable);
    }

    #[test]
    fn an_unknown_key_is_ignored_with_nul_and_marks_unsure_with_newlines() {
        let out = porcelain(&[&[b"worktree /r", b"shiny new-key"]], 0);
        let list = parse_list(&out, 0);
        assert_eq!(list.len(), 1);
        assert!(!list[0].unsure);

        let out = porcelain(&[&[b"worktree /r", b"shiny new-key"]], b'\n');
        assert!(parse_list(&out, b'\n')[0].unsure);
    }

    #[test]
    fn a_ref_that_is_not_a_local_branch_and_a_bad_head_are_left_out() {
        let out = porcelain(
            &[&[b"worktree /r", b"HEAD zz\x1b", b"branch refs/remotes/o/x"]],
            0,
        );
        let list = parse_list(&out, 0);
        assert_eq!(list[0].branch, None);
        assert_eq!(list[0].head, None);
    }

    #[test]
    fn a_record_with_no_worktree_line_is_dropped() {
        let ha = head(A);
        let out = porcelain(&[&[&ha, b"detached"], &[b"worktree /r/b", b"bare"]], 0);
        let list = parse_list(&out, 0);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].path.display(), "/r/b");
        assert!(!list[0].main, "only the first record is the main one");
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_utf8_keeps_its_bytes() {
        use std::os::unix::ffi::OsStrExt;

        let out = porcelain(&[&[b"worktree /r/x\xff", b"detached"]], 0);
        let list = parse_list(&out, 0);
        assert_eq!(list[0].path.as_os_str().as_bytes(), b"/r/x\xff");
        assert_eq!(list[0].path.display(), "/r/x\u{FFFD}");
    }

    #[test]
    fn an_escape_in_a_path_never_reaches_the_display() {
        for sep in [0, b'\n'] {
            let out = porcelain(&[&[b"worktree /r/\x1b[31mred", b"detached"]], sep);
            let list = parse_list(&out, sep);
            assert!(!list[0].path.display().contains('\x1b'), "{sep}");
            assert!(list[0].path.display().ends_with("[31mred"));
        }
    }

    #[test]
    fn a_newline_in_a_path_splits_into_a_stray_line_without_nul() {
        let out = porcelain(&[&[b"worktree /r/a\nb", b"detached"]], b'\n');
        let list = parse_list(&out, b'\n');
        assert_eq!(list.len(), 1);
        assert!(list[0].unsure);
        assert_eq!(list[0].path.display(), "/r/a");
    }

    #[cfg(unix)]
    #[test]
    fn a_newline_in_a_path_is_kept_whole_with_nul() {
        use std::os::unix::ffi::OsStrExt;

        let out = porcelain(&[&[b"worktree /r/a\nb", b"detached"]], 0);
        let list = parse_list(&out, 0);
        assert_eq!(list[0].path.as_os_str().as_bytes(), b"/r/a\nb");
        assert!(!list[0].unsure);
    }

    #[test]
    fn a_blank_line_in_a_path_without_nul_makes_every_record_unsure() {
        // `/x/a\n\nworktree /etc` split into a headless record and a fake one.
        let ha = head(A);
        let out = porcelain(
            &[
                &[b"worktree /r", &ha, b"branch refs/heads/main"],
                &[b"worktree /x/a"],
                &[b"worktree /etc", &ha, b"branch refs/heads/x"],
            ],
            b'\n',
        );
        let list = parse_list(&out, b'\n');
        assert_eq!(list.len(), 3);
        assert!(list.iter().all(|w| w.unsure), "{list:#?}");

        let out = porcelain(
            &[
                &[b"worktree /r", &ha, b"bare"],
                &[b"worktree /r/b", &ha, b"detached"],
                &[b"worktree /r/b", &ha, b"detached"],
            ],
            b'\n',
        );
        assert!(parse_list(&out, b'\n').iter().all(|w| w.unsure));

        // A bare main worktree has no HEAD, and that is not suspect.
        let out = porcelain(
            &[&[b"worktree /r", b"bare"], &[b"worktree /r/b", &ha]],
            b'\n',
        );
        assert!(parse_list(&out, b'\n').iter().all(|w| !w.unsure));
    }

    #[test]
    fn empty_output_is_no_worktrees() {
        assert_eq!(parse_list(b"", 0), vec![]);
        assert_eq!(parse_list(b"", b'\n'), vec![]);
    }

    const LIST_Z: &[&str] = &["worktree", "list", "--porcelain", "-z"];
    const LIST: &[&str] = &["worktree", "list", "--porcelain"];

    #[test]
    fn the_list_is_a_read_with_fixed_output() {
        for z in [true, false] {
            let cmd = list_command(z);
            let args: Vec<_> = cmd.args().iter().map(|a| a.to_str().unwrap()).collect();
            assert_eq!(args, if z { LIST_Z } else { LIST });
            assert_eq!(cmd.env_profile(), EnvProfile::Standard);
            assert_eq!(cmd.process_env(), env_list(&STANDARD));
        }
    }

    #[test]
    fn read_worktrees_uses_nul_when_git_has_it() {
        let runner = FakeRunner::new().expect(LIST_Z, ok(b"worktree /r\0bare\0\0"));
        let list = read_worktrees(&runner).unwrap();
        assert_eq!(list[0].path.display(), "/r");
        runner.assert_done();
    }

    #[test]
    fn read_worktrees_falls_back_to_newlines_and_reports_only_the_second_failure() {
        let runner = FakeRunner::new()
            .expect(LIST_Z, fail("error: unknown switch `z'\n"))
            .expect(LIST, ok(b"worktree /r\nbare\n\n"))
            .expect(GIT_DIRS, ok(b"/r\n/r\n"));
        assert_eq!(read_worktrees(&runner).unwrap()[0].path.display(), "/r");
        runner.assert_done();

        let runner = FakeRunner::new()
            .expect(LIST_Z, fail("error: unknown switch `z'\n"))
            .expect(LIST, fail("fatal: not a \x1b[31mrepo\n"));
        assert_eq!(
            read_worktrees(&runner),
            Err("fatal: not a \u{FFFD}[31mrepo".into())
        );
        runner.assert_done();
    }

    const GIT_DIRS: &[&str] = &["rev-parse", "--git-dir", "--git-common-dir"];

    #[test]
    fn read_worktrees_with_newlines_checks_each_record_against_the_common_dir() {
        let tmp = TempDir::new("read-trust");
        let main = tmp.path().join("main");
        std::fs::create_dir_all(main.join(".git")).unwrap();
        let common = format!("{}\n", main.join(".git").display());
        let listed = format!(
            "worktree {}\nHEAD {A}\n\nworktree {}\nHEAD {A}\n\n",
            main.display(),
            tmp.path().join("forged").display()
        );
        let runner = FakeRunner::new()
            .expect(LIST_Z, fail("error: unknown switch `z'\n"))
            .expect(LIST, ok(listed.as_bytes()))
            .expect(GIT_DIRS, ok(format!("{common}{common}").as_bytes()));
        let unsure: Vec<_> = read_worktrees(&runner)
            .unwrap()
            .iter()
            .map(|w| w.unsure)
            .collect();
        assert_eq!(unsure, [false, true]);
        runner.assert_done();
        assert_eq!(runner.process_envs()[2], env_list(&STANDARD));

        let runner = FakeRunner::new()
            .expect(LIST_Z, fail("error: unknown switch `z'\n"))
            .expect(LIST, ok(listed.as_bytes()))
            .expect(GIT_DIRS, fail("fatal: no\n"));
        let list = read_worktrees(&runner).unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.iter().all(|w| w.unsure), "{list:#?}");
        runner.assert_done();

        // With `-z` nothing is checked: the one call made is the list.
        let nul = listed.replace('\n', "\0");
        let runner = FakeRunner::new().expect(LIST_Z, ok(nul.as_bytes()));
        let list = read_worktrees(&runner).unwrap();
        assert!(list.iter().all(|w| !w.unsure), "{list:#?}");
        assert_eq!(runner.process_envs().len(), 1);
        runner.assert_done();
    }

    #[test]
    fn a_silent_git_dirs_failure_names_the_command() {
        let runner = FakeRunner::new().expect(GIT_DIRS, fail(""));
        assert_eq!(
            git_dirs(&runner, Path::new("/r")),
            Err("git rev-parse failed".into())
        );
        runner.assert_done();
    }

    #[test]
    fn a_real_linked_worktree_and_the_main_one_are_trusted_from_either() {
        let tmp = TempDir::new("real-trust");
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        fake::git(&repo, &["init", "-q"]);
        fake::git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let linked = tmp.path().join("wt");
        let linked_text = linked.to_str().unwrap();
        fake::git(
            &repo,
            &["worktree", "add", "-q", "-b", "side", "--", linked_text],
        );
        for dir in [&repo, &linked] {
            let runner = CliRunner::isolated(dir.as_path());
            let output = runner.run(&list_command(false)).unwrap();
            assert!(output.success);
            let mut list = parse_list(&output.stdout, b'\n');
            let (_, common) = git_dirs(&runner, dir).unwrap();
            mark_untrusted(&mut list, &common);
            assert_eq!(list.len(), 2, "{list:#?}");
            assert!(list.iter().all(|w| !w.unsure), "{list:#?}");
        }
    }

    fn args(cmd: &GitCommand) -> Vec<String> {
        cmd.args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn add_passes_the_absolute_path_and_the_short_branch_after_a_double_dash() {
        let cmd = add_command(Path::new("/r/repo-feature"), &BranchName::from("feature")).unwrap();
        assert_eq!(
            args(&cmd),
            ["worktree", "add", "--", "/r/repo-feature", "feature"]
        );
    }

    #[test]
    fn add_refuses_a_branch_git_could_read_as_an_option() {
        for name in ["-x", "-", ""] {
            assert!(
                add_command(Path::new("/r/x"), &BranchName::from(name)).is_err(),
                "{name:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn remove_passes_the_raw_path_after_a_double_dash_and_never_forces() {
        use std::os::unix::ffi::OsStrExt;

        let path = RepoPath::from_bytes(b"/r/-f\xff");
        let cmd = remove_command(&path);
        let raw: Vec<&[u8]> = cmd.args().iter().map(|a| a.as_bytes()).collect();
        assert_eq!(raw, [&b"worktree"[..], b"remove", b"--", b"/r/-f\xff"]);
        assert!(!raw.iter().any(|a| *a == b"--force" || *a == b"-f"));
        assert_eq!(cmd.env_profile(), EnvProfile::Locale);
        assert_eq!(cmd.process_env(), env_list(&[("LC_ALL", "C")]));
    }

    #[test]
    fn an_old_git_without_remove_says_which_git_is_needed() {
        let usage = b"usage: git worktree add [<options>] <path> [<commit-ish>]\n   or: git worktree list\n";
        assert_eq!(
            remove_error(&removal_failed(usage)),
            "Removing a worktree needs Git 2.17 or newer"
        );
        let dirty =
            b"fatal: '/r/x' contains modified or untracked files, use --force to delete it\n";
        assert_eq!(
            remove_error(&removal_failed(dirty)),
            "fatal: '/r/x' contains modified or untracked files, use --force to delete it"
        );
        assert_eq!(remove_error(&removal_failed(b"\x1b[2J")), "\u{FFFD}[2J");
        assert_eq!(
            remove_error(&removal_failed(b"")),
            "git worktree remove failed"
        );
    }

    /// A work tree with its `.git` directory, canonical, in a temp dir.
    fn layout() -> (TempDir, PathBuf, PathBuf) {
        let tmp = TempDir::new("worktrees");
        let workdir = tmp.path().join("repo");
        std::fs::create_dir_all(workdir.join(".git")).unwrap();
        let workdir = canonical(&workdir);
        let git_dir = workdir.join(".git");
        (tmp, workdir, git_dir)
    }

    #[test]
    fn a_relative_target_resolves_to_an_absolute_path() {
        let (_tmp, workdir, git_dir) = layout();
        let path = prepare_add(&workdir, &git_dir, &git_dir, "../wt").unwrap();
        assert!(path.is_absolute());
        assert_eq!(path, workdir.parent().unwrap().join("wt"));
    }

    #[test]
    fn a_target_that_looks_like_an_option_becomes_a_path_inside_the_workdir() {
        let (_tmp, workdir, git_dir) = layout();
        let path = prepare_add(&workdir, &git_dir, &git_dir, "-rf").unwrap();
        assert_eq!(path, workdir.join("-rf"));
        assert!(path.is_absolute());
    }

    #[test]
    fn a_target_naming_a_git_directory_is_refused() {
        let (_tmp, workdir, git_dir) = layout();
        for typed in [".git/wt", "sub/.GIT/x", "a/../.git/y"] {
            assert!(
                prepare_add(&workdir, &git_dir, &git_dir, typed).is_err(),
                "{typed}"
            );
        }
    }

    #[test]
    fn a_target_inside_the_common_dir_reached_through_dot_dot_is_refused() {
        let tmp = TempDir::new("worktrees-common");
        let workdir = tmp.path().join("wd");
        let store = tmp.path().join("store");
        std::fs::create_dir_all(&workdir).unwrap();
        std::fs::create_dir_all(&store).unwrap();
        let error = prepare_add(&workdir, &store, &store, "../store/wt").unwrap_err();
        assert!(error.contains("git directory"), "{error}");
    }

    #[test]
    fn an_existing_file_or_a_non_empty_directory_is_refused() {
        let (_tmp, workdir, git_dir) = layout();
        std::fs::write(workdir.join("file"), "x").unwrap();
        std::fs::create_dir(workdir.join("full")).unwrap();
        std::fs::write(workdir.join("full").join("a"), "x").unwrap();
        for typed in ["file", "full"] {
            assert!(
                prepare_add(&workdir, &git_dir, &git_dir, typed).is_err(),
                "{typed}"
            );
        }
        assert!(workdir.join("full").join("a").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_refused_even_to_an_empty_directory() {
        let (_tmp, workdir, git_dir) = layout();
        std::fs::create_dir(workdir.join("empty")).unwrap();
        std::os::unix::fs::symlink(workdir.join("empty"), workdir.join("link")).unwrap();
        let error = prepare_add(&workdir, &git_dir, &git_dir, "link").unwrap_err();
        assert!(error.contains("symbolic link"), "{error}");
    }

    #[test]
    fn an_empty_existing_directory_is_accepted() {
        let (_tmp, workdir, git_dir) = layout();
        std::fs::create_dir(workdir.join("empty")).unwrap();
        assert_eq!(
            prepare_add(&workdir, &git_dir, &git_dir, "empty"),
            Ok(workdir.join("empty"))
        );
    }

    #[test]
    fn add_worktree_checks_the_target_then_runs_git_add() {
        let (_tmp, workdir, _git_dir) = layout();
        let target = workdir.parent().unwrap().join("repo-feature");
        let target_text = target.to_str().unwrap().to_string();
        let runner = WorkdirRunner {
            fake: FakeRunner::new()
                .expect(GIT_DIRS, ok(b".git\n.git\n"))
                .expect(&["worktree", "add", "--", &target_text, "feature"], ok(b"")),
            workdir: workdir.clone(),
        };
        assert_eq!(
            add_worktree(&runner, "../repo-feature", &BranchName::from("feature")),
            Ok(target)
        );
        runner.fake.assert_done();
        // The rev-parse is a read; the add takes a path and runs a hook.
        assert_eq!(
            runner.fake.process_envs(),
            [env_list(&STANDARD), env_list(&[("LC_ALL", "C")])]
        );

        let runner = WorkdirRunner {
            fake: FakeRunner::new().expect(GIT_DIRS, ok(b".git\n.git\n")),
            workdir,
        };
        assert!(add_worktree(&runner, ".git/x", &BranchName::from("feature")).is_err());
        runner.fake.assert_done();
    }

    /// A [`FakeRunner`] that also has a working directory.
    struct WorkdirRunner {
        fake: FakeRunner,
        workdir: PathBuf,
    }

    impl GitRunner for WorkdirRunner {
        fn run(&self, cmd: &GitCommand) -> std::io::Result<crate::git::runner::GitOutput> {
            self.fake.run(cmd)
        }

        fn spawn(&self, cmd: &GitCommand) -> std::io::Result<crate::git::runner::GitChild> {
            self.fake.spawn(cmd)
        }

        fn workdir(&self) -> PathBuf {
            self.workdir.clone()
        }
    }

    #[test]
    fn remove_worktree_refuses_the_worktree_git_runs_in() {
        let (_tmp, workdir, _) = layout();
        let runner = WorkdirRunner {
            fake: FakeRunner::new(),
            workdir: workdir.join("."),
        };
        let path = RepoPath::from(workdir.to_str().unwrap());
        assert_eq!(
            remove_worktree(&runner, &path),
            Err("Switch to another worktree first".into())
        );
        runner.fake.assert_done();
    }

    #[test]
    fn remove_worktree_maps_gits_answer() {
        let runner = FakeRunner::new().expect(&["worktree", "remove", "--", "/r/x"], ok(b""));
        assert_eq!(remove_worktree(&runner, &"/r/x".into()), Ok(()));
        let runner = FakeRunner::new().expect(
            &["worktree", "remove", "--", "/r/x"],
            fail("usage: git worktree add <path>\n"),
        );
        assert_eq!(
            remove_worktree(&runner, &"/r/x".into()),
            Err("Removing a worktree needs Git 2.17 or newer".into())
        );
    }
}
