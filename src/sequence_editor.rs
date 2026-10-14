//! Kika as git's sequence editor: a hidden mode for Kika's own
//! `rebase -i` child. It checks git's todo list against the plan Kika
//! built and writes the plan in its place, then exits without starting the
//! UI. A failure exits non-zero, which git takes as "abort": it restores
//! the autostash and leaves history as it was.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::ExitCode;

use crate::git::rebase_plan::{PLAN_ENV, apply, decode, parse_todo};
use crate::worktree_file::{read_file, replace_file};

/// Run the hidden mode if Kika was started as a sequence editor, and say
/// how to exit; `None` starts Kika as usual.
pub fn dispatch() -> Option<ExitCode> {
    std::env::var_os(PLAN_ENV)?;
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    Some(dispatch_with(&args))
}

/// The hidden mode on `args`, with the plan from the env.
pub fn dispatch_with(args: &[OsString]) -> ExitCode {
    let plan = std::env::var_os(PLAN_ENV).unwrap_or_default();
    match run(args, &plan) {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            eprintln!("kika: not rewriting the rebase todo list: {reason}");
            ExitCode::FAILURE
        }
    }
}

/// Rewrite the todo file in `args` with `plan`. Reads and writes that one
/// file and nothing else.
fn run(args: &[OsString], plan: &OsStr) -> Result<(), String> {
    let [path] = args else {
        return Err(format!("expected one file, got {}", args.len()));
    };
    let path = Path::new(path);
    let in_rebase_dir = path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|dir| dir == "rebase-merge");
    if path.file_name() != Some(OsStr::new("git-rebase-todo")) || !in_rebase_dir {
        return Err(format!("not a rebase todo list: {}", path.display()));
    }
    let plan = plan.to_str().ok_or("the plan is not UTF-8")?;
    let steps = decode(plan)?;
    let todo = read_file(path, MAX_TODO)?;
    // Kika's rebase passes `-c core.commentChar=#`, so git wrote this
    // list's comments with `#` whatever the user's config says.
    let rewritten = apply(&steps, &parse_todo(&todo, b"#")?)?;
    replace_file(path, &todo, &rewritten)
}

/// The largest todo list read: a plan of at most `MAX_STEPS` lines, each
/// with its subject, fits with room to spare.
pub const MAX_TODO: u64 = 1024 * 1024;

/// The `GIT_SEQUENCE_EDITOR` value that runs `exe`. Git runs it through
/// `sh`, so the path is single-quoted, each `'` in it written as `'\''`.
/// On Windows git's bundled `sh` runs it too; `/` stands in for `\`, which
/// that shell also takes as a separator.
pub fn editor_value(exe: &Path) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        OsString::from_vec(quote(exe.as_os_str().as_bytes()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let wide: Vec<u16> = exe
            .as_os_str()
            .encode_wide()
            .map(|unit| {
                if unit == u16::from(b'\\') {
                    u16::from(b'/')
                } else {
                    unit
                }
            })
            .collect();
        OsString::from_wide(&quote(&wide))
    }
}

/// `units` in single quotes for `sh`, built from raw code units so a name
/// that is not UTF-8 stays as it is.
fn quote<T: Copy + PartialEq + From<u8>>(units: &[T]) -> Vec<T> {
    let q = T::from(b'\'');
    let mut out = vec![q];
    for &unit in units {
        if unit == q {
            out.extend([q, T::from(b'\\'), q, q]);
        } else {
            out.push(unit);
        }
    }
    out.push(q);
    out
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::git::fake::TempDir;

    fn id(digit: char) -> String {
        digit.to_string().repeat(40)
    }

    /// A temp `rebase-merge/git-rebase-todo` holding `todo`.
    fn todo_file(tmp: &TempDir, todo: &[u8]) -> std::path::PathBuf {
        let dir = tmp.path().join("rebase-merge");
        fs::create_dir(&dir).unwrap();
        let path = dir.join("git-rebase-todo");
        fs::write(&path, todo).unwrap();
        path
    }

    fn todo() -> Vec<u8> {
        format!(
            "pick {} second\npick {} first\n\n# Commands:\n# p, pick <commit>\n",
            &id('b')[..7],
            &id('a')[..7]
        )
        .into_bytes()
    }

    fn plan() -> OsString {
        format!("reword {}\npick {}\n", id('b'), id('a')).into()
    }

    #[test]
    fn a_valid_todo_is_rewritten_with_the_plan() {
        let tmp = TempDir::new("seq-editor");
        let path = todo_file(&tmp, &todo());
        assert_eq!(run(&[path.clone().into()], &plan()), Ok(()));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("reword {} second\npick {} first\n", id('b'), id('a'))
        );
    }

    /// Run with `args` and `plan`, which must fail and leave `path` as it was.
    fn refused(args: &[OsString], plan: &OsStr, path: &Path) {
        let before = fs::read(path).unwrap();
        assert!(run(args, plan).is_err(), "{args:?}");
        assert_eq!(fs::read(path).unwrap(), before, "{args:?}");
    }

    #[test]
    fn a_file_that_is_not_the_rebase_todo_is_left_alone() {
        let tmp = TempDir::new("seq-editor");
        let other = tmp.path().join("other");
        fs::create_dir(&other).unwrap();
        for name in ["git-rebase-todo", "COMMIT_EDITMSG"] {
            let path = other.join(name);
            fs::write(&path, todo()).unwrap();
            refused(&[path.clone().into()], &plan(), &path);
        }
        let wrong_name = tmp.path().join("rebase-merge");
        fs::create_dir(&wrong_name).unwrap();
        let path = wrong_name.join("done");
        fs::write(&path, todo()).unwrap();
        refused(&[path.clone().into()], &plan(), &path);
    }

    #[test]
    fn anything_but_one_argument_is_refused() {
        let tmp = TempDir::new("seq-editor");
        let path = todo_file(&tmp, &todo());
        refused(&[path.clone().into(), path.clone().into()], &plan(), &path);
        assert!(run(&[], &plan()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_refused_and_its_target_left_alone() {
        let tmp = TempDir::new("seq-editor");
        let target = tmp.path().join("target");
        fs::write(&target, todo()).unwrap();
        let dir = tmp.path().join("rebase-merge");
        fs::create_dir(&dir).unwrap();
        let link = dir.join("git-rebase-todo");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        refused(&[link.into()], &plan(), &target);
    }

    #[test]
    fn a_plan_that_does_not_match_the_todo_is_refused() {
        let tmp = TempDir::new("seq-editor");
        let path = todo_file(&tmp, &todo());
        for bad in [
            format!("reword {}\n", id('b')),
            format!("pick {}\nreword {}\n", id('b'), id('b')),
            format!("pick {}\nbreak\nbreak\npick {}\n", id('b'), id('a')),
            format!("pick {}\npick {}\n", id('b'), id('c')),
            // Short ids, though they are the todo's own abbreviations.
            format!("pick {}\npick {}\n", &id('b')[..7], &id('a')[..7]),
            format!("pick {}\npick {}\n", id('b'), id('A')),
            format!("pick {}\npick {}\n", id('b'), "a".repeat(64)),
            "nonsense".into(),
            String::new(),
        ] {
            refused(&[path.clone().into()], OsStr::new(&bad), &path);
        }
        let exec = tmp.path().join("exec");
        fs::create_dir(&exec).unwrap();
        let dir = exec.join("rebase-merge");
        fs::create_dir(&dir).unwrap();
        let path = dir.join("git-rebase-todo");
        let mut todo = todo();
        todo.extend_from_slice(b"exec rm -rf /\n");
        fs::write(&path, todo).unwrap();
        refused(&[path.clone().into()], &plan(), &path);
        // Git reads no further than a NUL; the list is refused.
        let mut nul = self::todo();
        nul.extend_from_slice(b"pick 1234567 x\0\n");
        fs::write(&path, nul).unwrap();
        refused(&[path.clone().into()], &plan(), &path);
        // Git runs an indented command too, so it is no comment.
        let mut indented = self::todo();
        indented.extend_from_slice(b"\t  exec rm -rf /\n");
        fs::write(&path, indented).unwrap();
        refused(&[path.clone().into()], &plan(), &path);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_rebase_dir_is_refused_and_its_todo_left_alone() {
        let tmp = TempDir::new("seq-editor");
        let real = tmp.path().join("real");
        fs::create_dir(&real).unwrap();
        let target = real.join("git-rebase-todo");
        fs::write(&target, todo()).unwrap();
        let dir = tmp.path().join("rebase-merge");
        std::os::unix::fs::symlink(&real, &dir).unwrap();
        refused(&[dir.join("git-rebase-todo").into()], &plan(), &target);
    }

    #[test]
    fn a_rewrite_leaves_no_temp_file_beside_the_todo() {
        let tmp = TempDir::new("seq-editor");
        let path = todo_file(&tmp, &todo());
        assert_eq!(run(&[path.clone().into()], &plan()), Ok(()));
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, [OsString::from("git-rebase-todo")]);
    }

    #[test]
    fn edit_break_and_moves_are_written_and_update_refs_stay_after_their_commits() {
        let tmp = TempDir::new("seq-editor");
        let todo = format!(
            "pick {} third\nupdate-ref refs/heads/x\n\npick {} second\npick {} first\n",
            &id('c')[..7],
            &id('b')[..7],
            &id('a')[..7]
        );
        let path = todo_file(&tmp, todo.as_bytes());
        let plan = format!(
            "edit {}\nbreak\npick {}\npick {}\n",
            id('c'),
            id('a'),
            id('b')
        );
        assert_eq!(run(&[path.clone().into()], OsStr::new(&plan)), Ok(()));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!(
                "edit {} third\nbreak\nupdate-ref refs/heads/x\npick {} first\npick {} second\n",
                id('c'),
                id('a'),
                id('b')
            )
        );
        // Moving a commit across the branch is refused.
        let tmp = TempDir::new("seq-editor");
        let path = todo_file(&tmp, todo.as_bytes());
        let across = format!("pick {}\npick {}\npick {}\n", id('b'), id('c'), id('a'));
        refused(&[path.clone().into()], OsStr::new(&across), &path);
    }

    #[cfg(unix)]
    #[test]
    fn editor_value_quotes_the_path_for_sh() {
        assert_eq!(
            editor_value(Path::new("/opt/my apps/it's/kika")),
            OsStr::new(r"'/opt/my apps/it'\''s/kika'")
        );
        assert_eq!(
            editor_value(Path::new("/bin/kika")),
            OsStr::new("'/bin/kika'")
        );
    }

    #[cfg(unix)]
    #[test]
    fn editor_value_keeps_raw_bytes() {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(
            editor_value(Path::new(OsStr::from_bytes(b"/x\xff/k"))).as_bytes(),
            b"'/x\xff/k'"
        );
    }

    #[cfg(unix)]
    #[test]
    fn sh_runs_the_quoted_path_with_the_file_argument() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new("seq-editor");
        let dir = tmp.path().join("my dir's");
        fs::create_dir(&dir).unwrap();
        let exe = dir.join("ed itor");
        fs::write(&exe, "#!/bin/sh\nprintf '%s' \"$1\" > \"$1\"\n").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        let out = tmp.path().join("out");
        // As git runs an editor: the value, then the file, through sh.
        let mut script = editor_value(&exe);
        script.push(" \"$@\"");
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(&script)
            .arg("sh")
            .arg(&out)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(fs::read(&out).unwrap(), out.as_os_str().as_encoded_bytes());
    }

    #[cfg(windows)]
    #[test]
    fn editor_value_uses_forward_slashes_on_windows() {
        assert_eq!(
            editor_value(Path::new(r"C:\Program Files\it's\kika.exe")),
            OsStr::new(r"'C:/Program Files/it'\''s/kika.exe'")
        );
    }
}
