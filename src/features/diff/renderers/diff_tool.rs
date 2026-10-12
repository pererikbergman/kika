//! Running a display-only diff tool on one file: both sides of the shown
//! change are read into two private temp files, and the tool is run on
//! them as `argv -- OLD NEW`. Blocking; called from tasks.
//!
//! Contents are the raw index and `HEAD` blobs and worktree bytes. Repo
//! file names never reach the tool: the temp files are named `old` and
//! `new`, plus the file's extension when it is plainly one.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::external_diff::{self, ExternalRenderer, LIMITS, Limits, MAX_BYTES};
use crate::git::diff::{DiffRequest, DiffSide, terminal_safe};
use crate::git::path::RepoPath;
use crate::git::runner::{GitCommand, GitRunner};
use crate::worktree_file;

/// Run `renderer`, a tool, on the old and new contents of `request`'s
/// file at `cols` columns, in `workdir`, within the renderer caps.
pub fn render(
    runner: &dyn GitRunner,
    workdir: &Path,
    request: &DiffRequest,
    renderer: &ExternalRenderer,
    cols: u16,
) -> Result<Vec<u8>, String> {
    render_with(
        runner,
        workdir,
        request,
        renderer,
        cols,
        LIMITS,
        &std::env::temp_dir(),
    )
}

/// [`render`] with the caps and the temp base as parameters, so tests can
/// shorten the one and watch the other.
fn render_with(
    runner: &dyn GitRunner,
    workdir: &Path,
    request: &DiffRequest,
    renderer: &ExternalRenderer,
    cols: u16,
    limits: Limits,
    temp_base: &Path,
) -> Result<Vec<u8>, String> {
    let (old, new) = sides(runner, workdir, request, &renderer.label)?;
    let files = TempFiles::create(temp_base)?;
    let ext = extension(&request.path);
    let old = files.write("old", ext, &old)?;
    let new = files.write("new", ext, &new)?;
    let args = [OsString::from("--"), old.into(), new.into()];
    external_diff::run(renderer, workdir, &args, b"", cols, limits)
}

/// The old and new contents of the file `request` shows.
fn sides(
    runner: &dyn GitRunner,
    workdir: &Path,
    request: &DiffRequest,
    label: &str,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    if request.commit.is_some() {
        return Err(format!("{label} shows working tree changes only"));
    }
    let path = &request.path;
    match request.side {
        DiffSide::Worktree => {
            let old = blob(runner, index_entry(runner, path, label)?, label)?;
            Ok((old, worktree(workdir, path, label)?))
        }
        DiffSide::Index => {
            let head_path = request.orig_path.as_ref().unwrap_or(path);
            let old = blob(runner, head_entry(runner, head_path)?, label)?;
            let new = blob(runner, index_entry(runner, path, label)?, label)?;
            Ok((old, new))
        }
        DiffSide::Untracked => Ok((Vec::new(), worktree(workdir, path, label)?)),
    }
}

/// One file's entry in the index or a tree.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    mode: Vec<u8>,
    oid: ObjectId,
}

/// A full object id, checked to be 40 (SHA-1) or 64 (SHA-256) ASCII hex
/// digits, so the git output it came from can never put an option or other
/// text in a `cat-file` argv.
#[derive(Debug, PartialEq, Eq)]
struct ObjectId(String);

impl ObjectId {
    fn parse(bytes: &[u8]) -> Option<Self> {
        let full = matches!(bytes.len(), 40 | 64) && bytes.iter().all(u8::is_ascii_hexdigit);
        full.then(|| Self(String::from_utf8_lossy(bytes).into_owned()))
    }
}

/// A git read of a repo file name: the name is literal, never pathspec
/// magic or a glob, and the read takes no lock.
fn path_command(subcommand: &str) -> GitCommand {
    GitCommand::new(subcommand)
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
}

fn failure(stderr: &[u8], what: &str) -> String {
    let message = terminal_safe(stderr);
    if message.is_empty() {
        format!("{what} failed")
    } else {
        message
    }
}

/// The space-separated fields before the tab of each NUL-ended record
/// of `stdout` that names exactly `path`. A pathspec can match more than
/// the one name, such as files under a directory of that name.
fn records_of<'a>(stdout: &'a [u8], path: &'a RepoPath) -> impl Iterator<Item = Vec<&'a [u8]>> {
    stdout.split(|&b| b == 0).filter_map(move |record| {
        let tab = record.iter().position(|&b| b == b'\t')?;
        let (meta, name) = (&record[..tab], &record[tab + 1..]);
        (name == path_bytes(path)).then(|| meta.split(|&b| b == b' ').collect())
    })
}

impl Entry {
    /// The entry `source` listed. A bad id fails the whole read: skipping
    /// the record would show the file as absent and give a wrong diff.
    /// The message names the file in its terminal-safe form and never
    /// echoes the id, which is untrusted bytes.
    fn new(mode: &[u8], oid: &[u8], path: &RepoPath, source: &str) -> Result<Self, String> {
        let oid = ObjectId::parse(oid).ok_or_else(|| {
            format!(
                "{source} gave a bad object id for {}; the repository may be damaged",
                path.display()
            )
        })?;
        Ok(Self {
            mode: mode.to_vec(),
            oid,
        })
    }
}

/// `path`'s stage-0 index entry; `None` when it is not in the index. A
/// conflicted path has no single side to show, so it is refused.
fn index_entry(
    runner: &dyn GitRunner,
    path: &RepoPath,
    label: &str,
) -> Result<Option<Entry>, String> {
    let cmd = path_command("ls-files")
        .arg("-s")
        .arg("-z")
        .arg("--")
        .arg(path.as_os_str());
    let output = runner.run(&cmd).map_err(|e| e.to_string())?;
    if !output.success {
        return Err(failure(&output.stderr, "git ls-files"));
    }
    let mut found = None;
    // `<mode> <oid> <stage>\t<path>`
    for fields in records_of(&output.stdout, path) {
        let [mode, oid, stage] = fields[..] else {
            continue;
        };
        if stage != b"0" {
            return Err(format!("{label} cannot show a conflicted file"));
        }
        found = Some(Entry::new(mode, oid, path, "git ls-files")?);
    }
    Ok(found)
}

/// `path`'s entry in `HEAD`; `None` when it is not there or `HEAD` has no
/// commit yet.
fn head_entry(runner: &dyn GitRunner, path: &RepoPath) -> Result<Option<Entry>, String> {
    let cmd = path_command("ls-tree")
        .arg("-z")
        .arg("HEAD")
        .arg("--")
        .arg(path.as_os_str());
    let output = runner.run(&cmd).map_err(|e| e.to_string())?;
    if !output.success {
        return if head_is_unborn(runner) {
            Ok(None)
        } else {
            Err(failure(&output.stderr, "git ls-tree"))
        };
    }
    // `<mode> <type> <oid>\t<path>`
    records_of(&output.stdout, path)
        .find_map(|fields| match fields[..] {
            [mode, _, oid] => Some(Entry::new(mode, oid, path, "git ls-tree")),
            _ => None,
        })
        .transpose()
}

/// Whether `HEAD` names no commit, as on a branch with none yet.
fn head_is_unborn(runner: &dyn GitRunner) -> bool {
    let cmd = GitCommand::new("rev-parse")
        .arg("--verify")
        .arg("--quiet")
        .arg("HEAD^{commit}")
        .env("LC_ALL", "C");
    runner.run(&cmd).is_ok_and(|o| !o.success)
}

fn path_bytes(path: &RepoPath) -> &[u8] {
    path.as_os_str().as_encoded_bytes()
}

/// The blob `entry` names, or nothing when there is no entry. Only a
/// regular file's: a symlink's target or a submodule's commit is not
/// file content a diff tool can compare.
fn blob(runner: &dyn GitRunner, entry: Option<Entry>, label: &str) -> Result<Vec<u8>, String> {
    let Some(entry) = entry else {
        return Ok(Vec::new());
    };
    if !matches!(entry.mode.as_slice(), b"100644" | b"100755") {
        return Err(not_regular(label));
    }
    let size = GitCommand::new("cat-file").arg("-s").arg(&entry.oid.0);
    let output = runner.run(&size).map_err(|e| e.to_string())?;
    if !output.success {
        return Err(failure(&output.stderr, "git cat-file"));
    }
    let size: u64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|_| "git cat-file gave no size".to_string())?;
    if size > MAX_BYTES as u64 {
        return Err(too_large(label));
    }
    let read = GitCommand::new("cat-file").arg("blob").arg(entry.oid.0);
    let output = runner.run(&read).map_err(|e| e.to_string())?;
    if !output.success {
        return Err(failure(&output.stderr, "git cat-file"));
    }
    Ok(output.stdout)
}

/// The worktree file's bytes; nothing when it is gone. Never through a
/// symlink, and never outside the worktree.
fn worktree(workdir: &Path, path: &RepoPath, label: &str) -> Result<Vec<u8>, String> {
    match fs::symlink_metadata(workdir.join(path.as_os_str())) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Ok(meta) if !meta.is_file() => return Err(not_regular(label)),
        Ok(meta) if meta.len() > MAX_BYTES as u64 => return Err(too_large(label)),
        _ => {}
    }
    worktree_file::read(workdir, path, MAX_BYTES as u64)
}

fn not_regular(label: &str) -> String {
    format!("{label} shows regular files only")
}

fn too_large(label: &str) -> String {
    format!("File too large for {label}")
}

/// The file's extension, when it is 1 to 16 ASCII letters and digits, so
/// a tool can pick its language. Anything else is left off: the temp
/// names must say nothing a repo chose beyond that.
fn extension(path: &RepoPath) -> Option<&str> {
    let bytes = path_bytes(path);
    let name = bytes.rsplit(|&b| b == b'/').next()?;
    let dot = name.iter().rposition(|&b| b == b'.')?;
    let ext = &name[dot + 1..];
    let plain =
        dot > 0 && (1..=16).contains(&ext.len()) && ext.iter().all(|b| b.is_ascii_alphanumeric());
    plain.then(|| std::str::from_utf8(ext).ok()).flatten()
}

/// A fresh private directory for one run's two files, removed with
/// everything in it when dropped, on every exit path.
struct TempFiles {
    dir: PathBuf,
}

impl TempFiles {
    /// A new directory under `base`, made exclusively so an existing path
    /// or symlink of the same name is never used.
    fn create(base: &Path) -> Result<Self, String> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let fail = |e: io::Error| format!("Could not make a temp directory: {e}");
        loop {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos());
            let dir = base.join(format!(
                "kika-diff-{}-{}-{nanos}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            #[cfg_attr(not(unix), allow(unused_mut))]
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            match builder.create(&dir) {
                Ok(()) => return Ok(Self { dir }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(fail(e)),
            }
        }
    }

    /// Write `bytes` to a new file `<stem>[.ext]` in the directory.
    fn write(&self, stem: &str, ext: Option<&str>, bytes: &[u8]) -> Result<PathBuf, String> {
        let name = match ext {
            Some(ext) => format!("{stem}.{ext}"),
            None => stem.to_string(),
        };
        let path = self.dir.join(name);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options
            .open(&path)
            .and_then(|mut file| file.write_all(bytes))
            .map_err(|e| format!("Could not write a temp file: {e}"))?;
        Ok(path)
    }
}

impl Drop for TempFiles {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::time::Duration;

    use super::*;
    use crate::features::diff::renderers::external_diff::RendererKind;
    #[cfg(unix)]
    use crate::git::fake::fail;
    use crate::git::fake::{FakeRunner, TempDir, ok};

    fn tool(argv: &[&str]) -> ExternalRenderer {
        ExternalRenderer {
            label: argv[0].into(),
            argv: argv.iter().map(OsString::from).collect(),
            kind: RendererKind::Tool,
        }
    }

    fn request(path: &str, side: DiffSide) -> DiffRequest {
        DiffRequest {
            path: path.into(),
            orig_path: None,
            side,
            options: Default::default(),
            commit: None,
        }
    }

    const OID: &str = "1111111111111111111111111111111111111111";
    #[cfg(unix)]
    const HEAD_OID: &str = "2222222222222222222222222222222222222222";

    fn ls_files(path: &str) -> Vec<&str> {
        vec!["ls-files", "-s", "-z", "--", path]
    }

    fn entry(mode: &str, oid: &str, stage: u8, path: &str) -> Vec<u8> {
        format!("{mode} {oid} {stage}\t{path}\0").into_bytes()
    }

    /// A temp base whose only entries are what a run leaves behind.
    fn empty(base: &TempDir) -> bool {
        fs::read_dir(base.path()).unwrap().next().is_none()
    }

    fn run_in(
        runner: &FakeRunner,
        workdir: &Path,
        request: &DiffRequest,
        renderer: &ExternalRenderer,
        limits: Limits,
        base: &TempDir,
    ) -> Result<Vec<u8>, String> {
        render_with(runner, workdir, request, renderer, 80, limits, base.path())
    }

    #[cfg(unix)]
    #[test]
    fn a_worktree_change_compares_the_index_blob_with_the_file() {
        let work = TempDir::new("tool-work");
        let name = ":(top)x*";
        fs::write(work.path().join(name), "new\n").unwrap();
        let runner = FakeRunner::new()
            .expect(&ls_files(name), ok(&entry("100644", OID, 0, name)))
            .expect(&["cat-file", "-s", OID], ok(b"4\n"))
            .expect(&["cat-file", "blob", OID], ok(b"old\n"));
        let base = TempDir::new("tool-base");
        let out = run_in(
            &runner,
            work.path(),
            &request(name, DiffSide::Worktree),
            &tool(&["cat"]),
            LIMITS,
            &base,
        );
        assert_eq!(out, Ok(b"old\nnew\n".to_vec()));
        runner.assert_done();
        let envs = runner.envs();
        for var in [
            ("GIT_LITERAL_PATHSPECS", "1"),
            ("GIT_OPTIONAL_LOCKS", "0"),
            ("LC_ALL", "C"),
        ] {
            assert!(
                envs[0].contains(&(var.0.into(), var.1.into())),
                "{var:?} in {:?}",
                envs[0]
            );
        }
        assert!(empty(&base), "the temp directory is removed");
    }

    #[cfg(unix)]
    #[test]
    fn a_staged_change_compares_head_at_the_old_name_with_the_index() {
        let runner = FakeRunner::new()
            .expect(
                &["ls-tree", "-z", "HEAD", "--", "old.rs"],
                ok(format!("100644 blob {HEAD_OID}\told.rs\0").as_bytes()),
            )
            .expect(&["cat-file", "-s", HEAD_OID], ok(b"2\n"))
            .expect(&["cat-file", "blob", HEAD_OID], ok(b"a\n"))
            .expect(&ls_files("new.rs"), ok(&entry("100755", OID, 0, "new.rs")))
            .expect(&["cat-file", "-s", OID], ok(b"2\n"))
            .expect(&["cat-file", "blob", OID], ok(b"b\n"));
        let mut staged = request("new.rs", DiffSide::Index);
        staged.orig_path = Some("old.rs".into());
        let base = TempDir::new("tool-base");
        let out = run_in(
            &runner,
            Path::new("/"),
            &staged,
            &tool(&["cat"]),
            LIMITS,
            &base,
        );
        assert_eq!(out, Ok(b"a\nb\n".to_vec()));
        runner.assert_done();
        assert!(
            runner.envs()[0].contains(&("GIT_LITERAL_PATHSPECS".into(), "1".into())),
            "{:?}",
            runner.envs()[0]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_new_file_on_an_unborn_head_has_an_empty_old_side() {
        let runner = FakeRunner::new()
            .expect(
                &["ls-tree", "-z", "HEAD", "--", "a.rs"],
                fail("fatal: Not a valid object name HEAD"),
            )
            .expect(
                &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
                fail(""),
            )
            .expect(&ls_files("a.rs"), ok(&entry("100644", OID, 0, "a.rs")))
            .expect(&["cat-file", "-s", OID], ok(b"2\n"))
            .expect(&["cat-file", "blob", OID], ok(b"b\n"));
        let base = TempDir::new("tool-base");
        let out = run_in(
            &runner,
            Path::new("/"),
            &request("a.rs", DiffSide::Index),
            &tool(&["cat"]),
            LIMITS,
            &base,
        );
        assert_eq!(out, Ok(b"b\n".to_vec()));
        runner.assert_done();
    }

    #[cfg(unix)]
    #[test]
    fn the_tool_gets_temp_names_with_only_a_plain_extension() {
        let work = TempDir::new("tool-work");
        fs::write(work.path().join("main.rs"), "x").unwrap();
        fs::write(work.path().join("odd.r-s"), "x").unwrap();
        let base = TempDir::new("tool-base");
        let print = tool(&["sh", "-c", "printf '%s\\n' \"$@\"", "sh"]);
        let names = |name: &str| {
            let out = run_in(
                &FakeRunner::new(),
                work.path(),
                &request(name, DiffSide::Untracked),
                &print,
                LIMITS,
                &base,
            )
            .unwrap();
            let text = String::from_utf8(out).unwrap();
            text.lines()
                .map(|l| {
                    Path::new(l)
                        .file_name()
                        .map_or(l.into(), |n| n.to_string_lossy().into_owned())
                })
                .collect::<Vec<String>>()
        };
        assert_eq!(names("main.rs"), ["--", "old.rs", "new.rs"]);
        assert_eq!(names("odd.r-s"), ["--", "old", "new"]);
        assert!(empty(&base));
    }

    #[test]
    fn extensions_are_kept_only_when_plain() {
        let ext = |name: &str| extension(&name.into()).map(str::to_string);
        assert_eq!(ext("src/main.rs"), Some("rs".into()));
        assert_eq!(ext("a.b/Makefile"), None);
        assert_eq!(ext(".gitignore"), None);
        assert_eq!(ext("x."), None);
        assert_eq!(ext("x.$(id)"), None);
        assert_eq!(ext(&format!("x.{}", "a".repeat(17))), None);
        assert_eq!(ext(&format!("x.{}", "a".repeat(16))), Some("a".repeat(16)));
    }

    #[test]
    fn symlinks_submodules_and_conflicts_are_refused_before_the_tool_runs() {
        // A missing program would say "not found" if it were ever run.
        let absent = tool(&["kika-no-such-tool"]);
        let base = TempDir::new("tool-base");
        for (mode, label) in [("120000", "symlink"), ("160000", "submodule")] {
            let runner = FakeRunner::new().expect(&ls_files("a"), ok(&entry(mode, OID, 0, "a")));
            let out = run_in(
                &runner,
                Path::new("/"),
                &request("a", DiffSide::Worktree),
                &absent,
                LIMITS,
                &base,
            );
            assert_eq!(
                out,
                Err("kika-no-such-tool shows regular files only".into()),
                "{label}"
            );
            runner.assert_done();
        }
        let mut conflict = entry("100644", OID, 1, "a");
        conflict.extend(entry("100644", OID, 2, "a"));
        conflict.extend(entry("100644", OID, 3, "a"));
        let runner = FakeRunner::new().expect(&ls_files("a"), ok(&conflict));
        let out = run_in(
            &runner,
            Path::new("/"),
            &request("a", DiffSide::Worktree),
            &absent,
            LIMITS,
            &base,
        );
        assert_eq!(
            out,
            Err("kika-no-such-tool cannot show a conflicted file".into())
        );
        assert!(empty(&base));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_in_the_worktree_is_refused() {
        let work = TempDir::new("tool-work");
        std::os::unix::fs::symlink("/etc/passwd", work.path().join("link")).unwrap();
        let base = TempDir::new("tool-base");
        let out = run_in(
            &FakeRunner::new(),
            work.path(),
            &request("link", DiffSide::Untracked),
            &tool(&["cat"]),
            LIMITS,
            &base,
        );
        assert_eq!(out, Err("cat shows regular files only".into()));
    }

    #[test]
    fn a_blob_over_the_cap_is_never_read() {
        let runner = FakeRunner::new()
            .expect(&ls_files("a"), ok(&entry("100644", OID, 0, "a")))
            .expect(
                &["cat-file", "-s", OID],
                ok(format!("{}\n", MAX_BYTES + 1).as_bytes()),
            );
        let base = TempDir::new("tool-base");
        let out = run_in(
            &runner,
            Path::new("/"),
            &request("a", DiffSide::Worktree),
            &tool(&["cat"]),
            LIMITS,
            &base,
        );
        assert_eq!(out, Err("File too large for cat".into()));
        runner.assert_done();
    }

    /// Ids that are not a full hex object id: an option, non-hex, too
    /// short, too long, between the two lengths, and non-ASCII.
    fn bad_ids() -> Vec<String> {
        vec![
            "-p".into(),
            "z".repeat(40),
            "1".repeat(39),
            "1".repeat(41),
            "1".repeat(63),
            "1".repeat(65),
            format!("{}é", "1".repeat(38)),
        ]
    }

    #[test]
    fn a_bad_index_id_fails_the_read_before_any_cat_file() {
        let base = TempDir::new("tool-base");
        for bad in bad_ids() {
            // No cat-file expectation: any call would panic the fake.
            let runner =
                FakeRunner::new().expect(&ls_files("a"), ok(&entry("100644", &bad, 0, "a")));
            let out = run_in(
                &runner,
                Path::new("/"),
                &request("a", DiffSide::Worktree),
                &tool(&["cat"]),
                LIMITS,
                &base,
            );
            assert_eq!(
                out,
                Err(
                    "git ls-files gave a bad object id for a; the repository may be damaged".into()
                ),
                "{bad:?}"
            );
            runner.assert_done();
        }
    }

    #[test]
    fn a_bad_id_names_the_file_made_terminal_safe() {
        let base = TempDir::new("tool-base");
        let path = "a\x1bb";
        let runner = FakeRunner::new().expect(&ls_files(path), ok(&entry("100644", "-p", 0, path)));
        let out = run_in(
            &runner,
            Path::new("/"),
            &request(path, DiffSide::Worktree),
            &tool(&["cat"]),
            LIMITS,
            &base,
        );
        let message = out.unwrap_err();
        assert!(!message.contains('\x1b'), "{message:?}");
        assert!(!message.contains("-p"), "{message:?}");
        let shown = crate::git::path::RepoPath::from(path).display().to_string();
        assert_eq!(
            message,
            format!("git ls-files gave a bad object id for {shown}; the repository may be damaged")
        );
        runner.assert_done();
    }

    #[test]
    fn a_bad_head_id_fails_the_staged_read_before_any_cat_file() {
        let base = TempDir::new("tool-base");
        for bad in bad_ids() {
            let runner = FakeRunner::new().expect(
                &["ls-tree", "-z", "HEAD", "--", "a"],
                ok(format!("100644 blob {bad}\ta\0").as_bytes()),
            );
            let out = run_in(
                &runner,
                Path::new("/"),
                &request("a", DiffSide::Index),
                &tool(&["cat"]),
                LIMITS,
                &base,
            );
            assert_eq!(
                out,
                Err("git ls-tree gave a bad object id for a; the repository may be damaged".into()),
                "{bad:?}"
            );
            runner.assert_done();
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_sha256_id_reaches_cat_file_unchanged() {
        let oid = "ab".repeat(32);
        let runner = FakeRunner::new()
            .expect(&ls_files("a"), ok(&entry("100644", &oid, 0, "a")))
            .expect(&["cat-file", "-s", &oid], ok(b"4\n"))
            .expect(&["cat-file", "blob", &oid], ok(b"old\n"));
        let work = TempDir::new("tool-work");
        fs::write(work.path().join("a"), "new\n").unwrap();
        let base = TempDir::new("tool-base");
        let out = run_in(
            &runner,
            work.path(),
            &request("a", DiffSide::Worktree),
            &tool(&["cat"]),
            LIMITS,
            &base,
        );
        assert_eq!(out, Ok(b"old\nnew\n".to_vec()));
        runner.assert_done();
    }

    /// An untracked `a.txt` in a fresh worktree, so no git is read.
    #[cfg(unix)]
    fn untracked(work: &TempDir) -> DiffRequest {
        fs::write(work.path().join("a.txt"), "new\n").unwrap();
        request("a.txt", DiffSide::Untracked)
    }

    #[cfg(unix)]
    #[test]
    fn the_tool_runs_under_the_pager_limits_and_cleans_up_on_failure() {
        let work = TempDir::new("tool-work");
        let base = TempDir::new("tool-base");
        let short = Limits {
            timeout: Duration::from_millis(200),
            max_output: 1000,
        };
        let cases = [
            (vec!["sh", "-c", "yes"], "Output of sh is too large"),
            (vec!["sh", "-c", "sleep 30"], "sh timed out"),
            (vec!["sh", "-c", "exit 3"], "sh failed"),
        ];
        for (argv, message) in cases {
            let out = run_in(
                &FakeRunner::new(),
                work.path(),
                &untracked(&work),
                &tool(&argv),
                short,
                &base,
            );
            assert_eq!(out, Err(message.into()));
            assert!(empty(&base), "{message}: the temp directory is removed");
        }
    }
}
