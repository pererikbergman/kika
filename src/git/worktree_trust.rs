//! Whether a worktree record read without `-z` really belongs to this
//! repository. On git older than 2.36 a path holding a newline can forge a
//! whole record, so a record is trusted only when its `.git` leads back
//! into the git directory all worktrees share. Blocking; called from tasks.

use std::path::{Path, PathBuf};

use super::path::os_string;
use super::worktrees::{Worktree, canonical};
use crate::worktree_file::read_file;

/// A gitfile is one short line; anything larger is not one.
const GITFILE_MAX: u64 = 4 * 1024;

/// Mark `unsure` every record whose `.git` does not lead back to
/// `common_dir`. Never clears `unsure`: other signs of a split path stand.
pub fn mark_untrusted(list: &mut [Worktree], common_dir: &Path) {
    let common = canonical(common_dir);
    for worktree in list {
        if !points_here(worktree, &common) {
            worktree.unsure = true;
        }
    }
}

/// Whether `worktree` is the bare repository itself, or its `.git` is the
/// shared git directory or a gitfile naming it or one of its worktrees.
/// Symlinked files are never read; resolved paths are only compared. Git
/// lists absolute paths; a relative one would be checked against Kika's
/// own working directory but acted on from the runner's, so it is refused.
fn points_here(worktree: &Worktree, common: &Path) -> bool {
    let path = worktree.path_buf();
    if !path.is_absolute() {
        return false;
    }
    if worktree.bare && canonical(&path) == common {
        return true;
    }
    let dot_git = path.join(".git");
    match std::fs::symlink_metadata(&dot_git) {
        Ok(meta) if meta.file_type().is_symlink() => false,
        Ok(meta) if meta.is_dir() => canonical(&dot_git) == common,
        Ok(_) => gitfile_target(&path, &dot_git).is_some_and(|target| {
            let target = canonical(&target);
            target == common || target.parent() == Some(&common.join("worktrees"))
        }),
        Err(_) => false,
    }
}

/// The directory a gitfile names, relative to `dir` when not absolute;
/// `None` unless the file is exactly one `gitdir: <path>` line.
fn gitfile_target(dir: &Path, file: &Path) -> Option<PathBuf> {
    let bytes = read_file(file, GITFILE_MAX).ok()?;
    let line = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    #[cfg(windows)]
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let target = line.strip_prefix(b"gitdir: ")?;
    if target.is_empty() || target.contains(&b'\n') {
        return None;
    }
    Some(dir.join(PathBuf::from(os_string(target))))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::git::fake::TempDir;
    use crate::git::worktrees::parse_list;

    const OID: &str = "0123456789abcdef0123456789abcdef01234567";

    /// `<tmp>/main` with a `.git` directory holding `worktrees/l`, and
    /// `<tmp>/l` whose gitfile points there. Returns the temp dir, the
    /// common dir, the main path and the linked path.
    fn layout() -> (TempDir, PathBuf, PathBuf, PathBuf) {
        let tmp = TempDir::new("trust");
        let main = tmp.path().join("main");
        let common = main.join(".git");
        std::fs::create_dir_all(common.join("worktrees").join("l")).unwrap();
        let linked = tmp.path().join("l");
        std::fs::create_dir(&linked).unwrap();
        gitfile(&linked, &common.join("worktrees").join("l"));
        (tmp, common, main, linked)
    }

    fn gitfile(dir: &Path, target: &Path) {
        let text = format!("gitdir: {}\n", target.display());
        std::fs::write(dir.join(".git"), text).unwrap();
    }

    /// The newline porcelain record for `path`, with a HEAD and a branch.
    fn record(path: &Path) -> String {
        format!(
            "worktree {}\nHEAD {OID}\nbranch refs/heads/b\n\n",
            path.display()
        )
    }

    /// Whether the single linked record at `path` is still trusted.
    fn trusted(path: &Path, common: &Path) -> bool {
        let mut list = parse_list(record(path).as_bytes(), b'\n');
        assert!(!list[0].unsure);
        mark_untrusted(&mut list, common);
        !list[0].unsure
    }

    #[test]
    fn a_forged_record_is_marked_unsure() {
        let (tmp, common, main, linked) = layout();
        let other = tmp.path().join("other");
        std::fs::create_dir(&other).unwrap();
        let forged = format!(
            "{}/a\nHEAD {OID}\n\nworktree {}",
            tmp.path().display(),
            other.display()
        );
        let out = [record(&main), record(Path::new(&forged)), record(&linked)].concat();
        let mut list = parse_list(out.as_bytes(), b'\n');
        assert_eq!(list.len(), 4, "{list:#?}");
        assert!(list.iter().all(|w| !w.unsure), "{list:#?}");
        mark_untrusted(&mut list, &common);
        let unsure: Vec<_> = list.iter().map(|w| w.unsure).collect();
        assert_eq!(unsure, [false, true, true, false], "{list:#?}");
        assert_eq!(list[2].path_buf(), other);
    }

    #[test]
    fn unsure_is_never_cleared() {
        let (_tmp, common, main, _) = layout();
        let mut list = parse_list(record(&main).as_bytes(), b'\n');
        list[0].unsure = true;
        mark_untrusted(&mut list, &common);
        assert!(list[0].unsure);
    }

    #[test]
    fn the_main_and_a_linked_worktree_are_trusted() {
        let (_tmp, common, main, linked) = layout();
        assert!(trusted(&main, &common));
        assert!(trusted(&linked, &common));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_dot_git_or_parent_is_not_trusted() {
        let (tmp, common, _, linked) = layout();
        let via_link = tmp.path().join("via-link");
        std::fs::create_dir(&via_link).unwrap();
        std::os::unix::fs::symlink(linked.join(".git"), via_link.join(".git")).unwrap();
        assert!(!trusted(&via_link, &common));

        let parent_link = tmp.path().join("parent-link");
        std::os::unix::fs::symlink(&linked, &parent_link).unwrap();
        assert!(!trusted(&parent_link, &common));

        let dir_link = tmp.path().join("dir-link");
        std::fs::create_dir(&dir_link).unwrap();
        std::os::unix::fs::symlink(&common, dir_link.join(".git")).unwrap();
        assert!(!trusted(&dir_link, &common));
    }

    #[test]
    fn a_gitfile_pointing_elsewhere_is_not_trusted() {
        let (tmp, common, _, _) = layout();
        let elsewhere = tmp.path().join("elsewhere").join(".git");
        std::fs::create_dir_all(elsewhere.join("worktrees").join("x")).unwrap();
        let cases = [
            elsewhere.join("worktrees").join("x"),
            common.join("worktrees").join("x").join("sub"),
            common.join("worktrees"),
            common.join("worktrees").join("..").join("worktrees"),
        ];
        for (i, target) in cases.iter().enumerate() {
            let dir = tmp.path().join(format!("case{i}"));
            std::fs::create_dir(&dir).unwrap();
            gitfile(&dir, target);
            assert!(!trusted(&dir, &common), "{}", target.display());
        }
    }

    #[test]
    fn a_gitfile_with_other_content_is_not_trusted() {
        let (tmp, common, _, _) = layout();
        let target = common.join("worktrees").join("l");
        let contents: [&[u8]; 6] = [
            &[],
            b"gitdir: \n",
            b"not a gitfile\n",
            b"\xffgitdir: x\n",
            b"gitdir:",
            b"gitdir: /\n",
        ];
        let extra = format!("gitdir: {}\nmore\n", target.display()).into_bytes();
        let twice = format!("gitdir: {0}\n\n", target.display()).into_bytes();
        let all = contents
            .iter()
            .copied()
            .chain([extra.as_slice(), twice.as_slice()]);
        for (i, content) in all.enumerate() {
            let dir = tmp.path().join(format!("case{i}"));
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join(".git"), content).unwrap();
            assert!(
                !trusted(&dir, &common),
                "{}",
                String::from_utf8_lossy(content)
            );
        }
    }

    #[test]
    fn a_gitfile_without_a_trailing_newline_is_trusted() {
        let (tmp, common, _, _) = layout();
        let dir = tmp.path().join("bare-line");
        std::fs::create_dir(&dir).unwrap();
        let target = common.join("worktrees").join("l");
        std::fs::write(dir.join(".git"), format!("gitdir: {}", target.display())).unwrap();
        assert!(trusted(&dir, &common));
    }

    #[test]
    fn a_missing_path_or_another_repositorys_git_directory_is_not_trusted() {
        let (tmp, common, _, _) = layout();
        assert!(!trusted(&tmp.path().join("gone"), &common));

        let other = tmp.path().join("other");
        std::fs::create_dir_all(other.join(".git")).unwrap();
        assert!(!trusted(&other, &common));
    }

    /// The process's working directory resolves relative paths, not the
    /// directory git commands run in, so a relative record is never
    /// trusted, even one that names a worktree of this repository.
    #[test]
    fn a_relative_path_is_not_trusted_even_from_inside_the_repository() {
        let cwd = std::env::current_dir().unwrap();
        let out = std::process::Command::new("git")
            .args(["rev-parse", "--git-common-dir"])
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        if !out.status.success() {
            return; // Built outside a checkout: there is nothing to forge.
        }
        let common = cwd.join(PathBuf::from(os_string(out.stdout.trim_ascii_end())));
        assert!(trusted(&cwd, &common), "the setup must trust the cwd");
        for relative in [".", "src/.."] {
            assert!(!trusted(Path::new(relative), &common), "{relative}");
        }
    }

    #[test]
    fn a_bare_main_record_at_the_common_dir_is_trusted() {
        let (tmp, common, _, _) = layout();
        let bare = |path: &Path| {
            let out = format!("worktree {}\nbare\n\n", path.display());
            let mut list = parse_list(out.as_bytes(), b'\n');
            mark_untrusted(&mut list, &common);
            !list[0].unsure
        };
        assert!(bare(&common));
        assert!(!bare(tmp.path()));
    }

    #[test]
    fn a_relative_gitdir_into_the_common_worktrees_is_trusted() {
        let (tmp, common, _, _) = layout();
        let dir = tmp.path().join("rel");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join(".git"), "gitdir: ../main/.git/worktrees/l\n").unwrap();
        assert!(trusted(&dir, &common));

        std::fs::write(dir.join(".git"), "gitdir: ../main/.git\n").unwrap();
        assert!(trusted(&dir, &common));
    }
}
