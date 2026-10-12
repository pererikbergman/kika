//! Where every local branch points, so a history rewrite can tell which
//! branches sit on the commits it would replace.

use super::commit_files::CommitId;
use super::diff::terminal_safe;
use super::log::CommitRow;
use super::rebase_plan::branch_name;
use super::runner::{GitCommand, GitRunner};

/// One branch tip: the commit and the full ref name, raw bytes, as
/// `refs/heads/<name>`.
pub type Tip = (CommitId, Vec<u8>);

/// List every local branch with its commit, one `<oid>NUL<ref>` line each.
/// A read: no optional locks, and the C locale.
pub fn tips_command() -> GitCommand {
    GitCommand::new("for-each-ref")
        .arg("--format=%(objectname)%00%(refname)")
        .arg("refs/heads/")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
}

/// [`tips_command`]'s output as tips. A line that is not an id and a ref
/// is skipped: it cannot be a branch Kika would have to move.
pub fn parse_tips(stdout: &[u8]) -> Vec<Tip> {
    stdout
        .split(|&b| b == b'\n')
        .filter_map(|line| {
            let at = line.iter().position(|&b| b == 0)?;
            let oid = std::str::from_utf8(&line[..at]).ok()?;
            let name = &line[at + 1..];
            if name.is_empty() {
                return None;
            }
            Some((CommitId::parse(oid)?, name.to_vec()))
        })
        .collect()
}

/// Read the branch tips.
pub fn read_tips(runner: &dyn GitRunner) -> Result<Vec<Tip>, String> {
    let output = runner
        .run(&tips_command())
        .map_err(|e| terminal_safe(e.to_string().as_bytes()))?;
    if !output.success {
        let message = terminal_safe(&output.stderr);
        return Err(if message.is_empty() {
            "git for-each-ref failed".into()
        } else {
            message
        });
    }
    Ok(parse_tips(&output.stdout))
}

/// The refs other than `head_ref`, the branch being rewritten, that point
/// at any of `rows[..range_len]`: branches stacked on the commits a
/// rewrite would replace.
pub fn stacked(
    rows: &[CommitRow],
    range_len: usize,
    tips: &[Tip],
    head_ref: Option<&[u8]>,
) -> Vec<Vec<u8>> {
    let range = &rows[..range_len.min(rows.len())];
    tips.iter()
        .filter(|(oid, name)| {
            Some(name.as_slice()) != head_ref && range.iter().any(|row| row.oid == oid.as_str())
        })
        .map(|(_, name)| name.clone())
        .collect()
}

/// Why a rewrite is refused on a git without `--update-refs`.
pub fn left_behind(branch: &[u8]) -> String {
    format!(
        "Branch {} sits on these commits; Git 2.38 or newer moves it along",
        branch_name(branch)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(digit: char) -> CommitRow {
        CommitRow {
            oid: digit.to_string().repeat(40),
            short_oid: digit.to_string().repeat(7),
            parents: vec![],
            author: "Ann".into(),
            time: 0,
            subject: "s".into(),
            subject_altered: false,
        }
    }

    fn tip(digit: char, name: &str) -> Tip {
        (
            CommitId::parse(&digit.to_string().repeat(40)).unwrap(),
            name.as_bytes().to_vec(),
        )
    }

    #[test]
    fn the_command_is_a_read_in_the_c_locale() {
        let cmd = tips_command();
        assert_eq!(
            cmd.args(),
            [
                "for-each-ref",
                "--format=%(objectname)%00%(refname)",
                "refs/heads/"
            ]
        );
        assert_eq!(
            cmd.envs(),
            [
                ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
                ("LC_ALL".into(), "C".into())
            ]
        );
    }

    #[test]
    fn tips_keep_raw_ref_bytes_and_skip_odd_lines() {
        let out = b"aaaa\0refs/heads/main\nbbbb\0refs/heads/caf\xe9\n\nzz\0refs/heads/x\ncccc\0\n";
        assert_eq!(
            parse_tips(out),
            [
                (
                    CommitId::parse("aaaa").unwrap(),
                    b"refs/heads/main".to_vec()
                ),
                (
                    CommitId::parse("bbbb").unwrap(),
                    b"refs/heads/caf\xe9".to_vec()
                ),
            ]
        );
    }

    #[test]
    fn stacked_lists_other_branches_inside_the_range() {
        let rows = [row('a'), row('b'), row('c')];
        let tips = [
            tip('a', "refs/heads/main"),
            tip('b', "refs/heads/feature"),
            tip('c', "refs/heads/below"),
            tip('a', "refs/heads/also-head"),
        ];
        assert_eq!(
            stacked(&rows, 2, &tips, Some(b"refs/heads/main")),
            [
                b"refs/heads/feature".to_vec(),
                b"refs/heads/also-head".to_vec()
            ]
        );
        assert_eq!(stacked(&rows, 1, &tips, None).len(), 2);
        assert_eq!(stacked(&rows, 9, &tips, Some(b"refs/heads/main")).len(), 3);
    }

    #[test]
    fn the_reason_names_the_branch_safely() {
        assert_eq!(
            left_behind(b"refs/heads/feature"),
            "Branch feature sits on these commits; Git 2.38 or newer moves it along"
        );
        assert!(!left_behind(b"refs/heads/x\x1b]0;t\x07").contains('\x1b'));
    }
}
