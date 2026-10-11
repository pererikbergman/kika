//! Kika as git's sequence editor: a hidden mode for Kika's own
//! `rebase -i` child. It checks git's todo list against the plan Kika
//! built and writes the plan in its place, then exits without starting the
//! UI. A failure exits non-zero, which git takes as "abort": it restores
//! the autostash and leaves history as it was.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use crate::git::rebase_plan::{apply, decode, parse_todo};

/// The plan, as [`crate::git::rebase_plan::encode`] writes it. A repo
/// cannot set Kika's env, and `GIT_SEQUENCE_EDITOR` wins over any
/// `sequence.editor` a repo configures, so only Kika's own `rebase -i`
/// child reaches this mode.
pub const PLAN_ENV: &str = "KIKA_REBASE_PLAN";

/// Run the hidden mode if Kika was started as a sequence editor, and say
/// how to exit; `None` starts Kika as usual.
pub fn dispatch() -> Option<ExitCode> {
    let plan = std::env::var_os(PLAN_ENV)?;
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    Some(match run(&args, &plan) {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            eprintln!("kika: not rewriting the rebase todo list: {reason}");
            ExitCode::FAILURE
        }
    })
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
    let meta = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.file_type().is_file() {
        return Err(format!("not a regular file: {}", path.display()));
    }
    let plan = plan.to_str().ok_or("the plan is not UTF-8")?;
    let steps = decode(plan)?;
    let todo = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let rewritten = apply(&steps, &parse_todo(&todo)?)?;
    fs::write(path, rewritten).map_err(|e| format!("{}: {e}", path.display()))
}

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
            format!("pick {}\nreword {}\n", id('a'), id('b')),
            format!("pick {}\npick {}\n", id('b'), id('c')),
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
