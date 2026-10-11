//! Tags for the Branches panel's Tags tab, and the checks a tag name the
//! user typed must pass before it reaches git. Reads only.

use std::ffi::OsString;

use super::commit_files::CommitId;
use super::diff::terminal_safe;
use super::runner::{GitCommand, GitRunner};

/// A tag's short name as the raw bytes of its ref, so a name that is not
/// UTF-8 reaches argv unchanged. Equality is on the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagName(pub Vec<u8>);

impl TagName {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }

    /// For drawing only: lossy, with control characters replaced.
    pub fn display(&self) -> String {
        terminal_safe(&self.0)
    }

    /// `refs/tags/<name>`, for argv: a full ref is never read as an
    /// option, even for a hand-made tag whose name starts with `-`.
    pub fn full_ref(&self) -> OsString {
        super::path::os_string(&self.ref_bytes())
    }

    /// `refs/tags/<name>` as bytes, to name the ref a log belongs to.
    pub fn ref_bytes(&self) -> Vec<u8> {
        let mut bytes = TAGS_PREFIX.to_vec();
        bytes.extend(&self.0);
        bytes
    }
}

impl From<&str> for TagName {
    fn from(name: &str) -> Self {
        Self::from_bytes(name.as_bytes())
    }
}

/// One tag. Not called `Tag`: the effects already have one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagRef {
    pub name: TagName,
    /// The commit the tag points at, through its tag object if annotated.
    pub target: CommitId,
    pub target_short: String,
    /// The tag ref's own object: the tag object of an annotated tag, else
    /// the commit. Deleting checks the ref still holds it.
    pub ref_oid: String,
    pub annotated: bool,
    /// The tagger's date, or the commit's for a lightweight tag.
    /// Terminal-safe.
    pub date: String,
    /// The tag message's subject, or the commit's. Terminal-safe.
    pub subject: String,
}

const TAGS_PREFIX: &[u8] = b"refs/tags/";

/// Columns of [`TagRef::target_short`].
const SHORT: usize = 7;

/// The listing command: newest first, one record of NUL-separated fields
/// per tag. `%(*objecttype)` says whether an annotated tag points at a
/// commit; a tag of a tree or a blob is not listed. `%(symref)` names
/// what a symbolic ref under `refs/tags` points at; such a ref is not a
/// tag Kika can act on, so it is not listed either.
pub fn list_command() -> GitCommand {
    GitCommand::new("for-each-ref")
        .arg("--sort=-creatordate")
        .arg(
            "--format=%(refname)%00%(objecttype)%00%(objectname)%00%(*objectname)%00\
             %(*objecttype)%00%(symref)%00%(creatordate:short)%00%(contents:subject)",
        )
        .arg("refs/tags")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
}

/// Every tag on a commit, newest first.
pub fn list_tags(runner: &dyn GitRunner) -> Result<Vec<TagRef>, String> {
    let out = runner
        .run(&list_command())
        .map_err(|e| terminal_safe(e.to_string().as_bytes()))?;
    if !out.success {
        let message = terminal_safe(&out.stderr);
        return Err(if message.is_empty() {
            "git for-each-ref failed".into()
        } else {
            message
        });
    }
    Ok(parse_tags(&out.stdout))
}

/// Read [`list_command`]'s output. Git ends each record with a newline,
/// which lands in the subject field before the next record's ref name; a
/// ref name holds no newline, so the last one splits them.
pub fn parse_tags(out: &[u8]) -> Vec<TagRef> {
    let mut fields = out.split(|&b| b == 0);
    let mut refname = fields.next().filter(|f| !f.is_empty());
    let mut tags = Vec::new();
    while let Some(name) = refname {
        let mut take = || fields.next();
        let (
            Some(kind),
            Some(oid),
            Some(peeled),
            Some(peeled_kind),
            Some(symref),
            Some(date),
            Some(last),
        ) = (take(), take(), take(), take(), take(), take(), take())
        else {
            break;
        };
        let (subject, next) = match last.iter().rposition(|&b| b == b'\n') {
            Some(at) => (&last[..at], &last[at + 1..]),
            None => (last, &b""[..]),
        };
        refname = Some(next).filter(|n| !n.is_empty());
        let record = Record {
            name,
            kind,
            oid,
            peeled,
            peeled_kind,
            symref,
            date,
            subject,
        };
        tags.extend(record.parse());
    }
    tags
}

/// One record's raw fields.
struct Record<'a> {
    name: &'a [u8],
    kind: &'a [u8],
    oid: &'a [u8],
    peeled: &'a [u8],
    peeled_kind: &'a [u8],
    symref: &'a [u8],
    date: &'a [u8],
    subject: &'a [u8],
}

impl Record<'_> {
    fn parse(&self) -> Option<TagRef> {
        if !self.symref.is_empty() {
            return None;
        }
        let name = self.name.strip_prefix(TAGS_PREFIX)?;
        let (annotated, target) = match (self.kind, self.peeled_kind) {
            (b"commit", _) => (false, self.oid),
            (b"tag", b"commit") => (true, self.peeled),
            _ => return None,
        };
        let hex = |bytes: &[u8]| CommitId::parse(std::str::from_utf8(bytes).ok()?);
        let target = hex(target)?;
        let ref_oid = hex(self.oid)?.as_str().to_string();
        let target_short = target.as_str().chars().take(SHORT).collect();
        Some(TagRef {
            name: TagName::from_bytes(name),
            target,
            target_short,
            ref_oid,
            annotated,
            date: terminal_safe(self.date),
            subject: terminal_safe(self.subject).replace('\n', " "),
        })
    }
}

/// Why `name` cannot be a new tag's name, if it cannot. Git's rules for
/// `refs/tags/<name>`, plus a name git would read as an option, and `@`
/// and `HEAD`, which read as something else wherever a revision goes.
pub fn validate_tag_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("A tag needs a name");
    }
    if name.starts_with('-') {
        return Err("A tag name cannot start with -");
    }
    if name == "@" || name == "HEAD" {
        return Err("A tag cannot be called @ or HEAD");
    }
    if name
        .chars()
        .any(|c| c.is_ascii_control() || " ~^:?*[\\".contains(c))
    {
        return Err("A tag name cannot contain spaces, control characters or ~^:?*[\\");
    }
    if name.contains("..") || name.contains("@{") {
        return Err("A tag name cannot contain .. or @{");
    }
    if name.starts_with('/') || name.ends_with('/') || name.contains("//") {
        return Err("A tag name cannot start or end with / or contain //");
    }
    if name.ends_with('.') {
        return Err("A tag name cannot end with .");
    }
    let bad_part = |part: &str| part.starts_with('.') || part.ends_with(".lock");
    if name.split('/').any(bad_part) {
        return Err("No part of a tag name can start with . or end with .lock");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::CliRunner;
    use crate::git::fake::{FakeRunner, TempDir, git, ok};

    const C1: &str = "1111111111111111111111111111111111111111";
    const C2: &str = "2222222222222222222222222222222222222222";
    const T1: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    /// A record with no symref: name, type, oid, peeled oid, peeled
    /// type, date and subject.
    fn record(fields: [&str; 7]) -> Vec<u8> {
        let [name, kind, oid, peeled, peeled_kind, date, subject] = fields;
        symbolic([name, kind, oid, peeled, peeled_kind, "", date, subject])
    }

    /// A record with every field, the symref sixth.
    fn symbolic(fields: [&str; 8]) -> Vec<u8> {
        let mut out = fields.join("\0").into_bytes();
        out.push(b'\n');
        out
    }

    #[test]
    fn a_symbolic_ref_under_refs_tags_is_not_listed() {
        let mut out = symbolic([
            "refs/tags/evil",
            "commit",
            C1,
            "",
            "",
            "refs/heads/keep",
            "d",
            "s",
        ]);
        out.extend(record(["refs/tags/ok", "commit", C1, "", "", "d", "s"]));
        let names: Vec<_> = parse_tags(&out).iter().map(|t| t.name.display()).collect();
        assert_eq!(names, ["ok"]);
    }

    #[test]
    fn lightweight_and_annotated_tags_give_their_commit_and_fields() {
        let mut out = record([
            "refs/tags/v2",
            "tag",
            T1,
            C2,
            "commit",
            "2026-10-02",
            "Release two",
        ]);
        out.extend(record([
            "refs/tags/v1",
            "commit",
            C1,
            "",
            "",
            "2026-10-01",
            "init",
        ]));

        let tags = parse_tags(&out);

        assert_eq!(
            tags,
            [
                TagRef {
                    name: "v2".into(),
                    target: CommitId::parse(C2).unwrap(),
                    target_short: "2222222".into(),
                    ref_oid: T1.into(),
                    annotated: true,
                    date: "2026-10-02".into(),
                    subject: "Release two".into(),
                },
                TagRef {
                    name: "v1".into(),
                    target: CommitId::parse(C1).unwrap(),
                    target_short: "1111111".into(),
                    ref_oid: C1.into(),
                    annotated: false,
                    date: "2026-10-01".into(),
                    subject: "init".into(),
                },
            ]
        );
        assert_eq!(tags[0].name.full_ref(), OsString::from("refs/tags/v2"));
        assert_eq!(tags[0].name.ref_bytes(), b"refs/tags/v2");
    }

    #[test]
    fn a_tag_of_a_tree_a_blob_or_a_bad_id_is_skipped() {
        let mut out = record(["refs/tags/tree", "tag", T1, C1, "tree", "d", "s"]);
        out.extend(record(["refs/tags/blob", "blob", C1, "", "", "d", "s"]));
        out.extend(record(["refs/tags/bad", "commit", "-x", "", "", "d", "s"]));
        out.extend(record(["refs/tags/ok", "commit", C1, "", "", "d", "s"]));
        out.extend(record(["refs/heads/x", "commit", C1, "", "", "d", "s"]));
        let names: Vec<_> = parse_tags(&out).iter().map(|t| t.name.display()).collect();
        assert_eq!(names, ["ok"]);
        assert_eq!(parse_tags(b""), []);
    }

    #[test]
    fn control_bytes_in_a_name_or_subject_come_out_replaced() {
        let mut out = b"refs/tags/v\x1b[31m\0commit\0".to_vec();
        out.extend(C1.as_bytes());
        out.extend(b"\0\0\0\0d\x07\0sub\x1b[2Jject\nwith a line\n");
        out.extend(record(["refs/tags/next", "commit", C1, "", "", "d", "s"]));
        let tags = parse_tags(&out);
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0].name.0, b"v\x1b[31m", "the bytes stay for argv");
        assert_eq!(tags[0].name.display(), "v\u{FFFD}[31m");
        assert_eq!(tags[0].subject, "sub\u{FFFD}[2Jject with a line");
        assert_eq!(tags[0].date, "d\u{FFFD}");
        assert_eq!(tags[1].name.display(), "next");
    }

    #[test]
    fn the_list_runs_without_locks_in_the_c_locale() {
        let fake = FakeRunner::new().expect(
            &[
                "for-each-ref",
                "--sort=-creatordate",
                "--format=%(refname)%00%(objecttype)%00%(objectname)%00%(*objectname)%00\
                 %(*objecttype)%00%(symref)%00%(creatordate:short)%00%(contents:subject)",
                "refs/tags",
            ],
            ok(&record(["refs/tags/v1", "commit", C1, "", "", "d", "s"])),
        );
        let tags = list_tags(&fake).unwrap();
        fake.assert_done();
        assert_eq!(tags.len(), 1);
        assert_eq!(
            fake.envs()[0],
            [
                ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
                ("LC_ALL".into(), "C".into()),
            ]
        );
    }

    #[test]
    fn tags_list_in_real_git() {
        let tmp = TempDir::new("tags");
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git(dir, &["tag", "light"]);
        git(dir, &["tag", "-a", "-m", "Annotated\n\nbody", "heavy"]);
        git(dir, &["tag", "-a", "-m", "tree", "of-tree", "HEAD^{tree}"]);
        git(dir, &["branch", "keep"]);
        git(dir, &["symbolic-ref", "refs/tags/sym", "refs/heads/keep"]);
        let head = String::from_utf8(git(dir, &["rev-parse", "HEAD"])).unwrap();

        let tags = list_tags(&CliRunner::isolated(dir)).unwrap();

        let mut names: Vec<_> = tags.iter().map(|t| t.name.display()).collect();
        names.sort();
        assert_eq!(names, ["heavy", "light"]);
        for tag in &tags {
            assert_eq!(tag.target.as_str(), head.trim());
        }
        let heavy = tags.iter().find(|t| t.name.display() == "heavy").unwrap();
        assert!(heavy.annotated);
        assert_eq!(heavy.subject, "Annotated");
        assert_ne!(heavy.ref_oid, head.trim());
        let light = tags.iter().find(|t| t.name.display() == "light").unwrap();
        assert!(!light.annotated);
        assert_eq!(light.subject, "init");
        assert_eq!(light.ref_oid, head.trim());
    }

    #[test]
    fn names_like_these_are_good_tags() {
        for name in ["v1.0", "release/2026-10", "é", "日本", "a@b", "x.lockx"] {
            assert_eq!(validate_tag_name(name), Ok(()), "{name:?}");
        }
        for name in ["", "-x", "a..b", "a.lock", "x/", "@", "HEAD", "a b", "/a"] {
            assert!(validate_tag_name(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn a_name_is_refused_exactly_when_git_refuses_it_plus_a_few_more() {
        // Kika also refuses these, which git accepts in this position.
        const KIKA_ONLY: &[&str] = &["-x", "-", "@", "HEAD"];
        // Control characters do not survive every platform's argv.
        #[cfg(unix)]
        const CONTROL: &[&str] = &["a\x01", "a\x7f", "a\tb", "v\x1b[31m"];
        #[cfg(not(unix))]
        const CONTROL: &[&str] = &[];
        let names = [
            "v1.0",
            "release/2026-10",
            "é",
            "a@b",
            "@a",
            "lock",
            "a.lockx",
            "-x",
            "-",
            "@",
            "HEAD",
            "",
            "a.lock",
            "a.lock/b",
            ".a",
            "a/.b",
            "a..b",
            "a@{b",
            "a b",
            "a~",
            "a^",
            "a:b",
            "a?",
            "a*",
            "a[",
            "a\\b",
            "a//b",
            "/a",
            "x/",
            "foo.",
            ".lock",
        ];
        for name in names.into_iter().chain(CONTROL.iter().copied()) {
            let ours = validate_tag_name(name).is_ok();
            let theirs = !name.is_empty()
                && crate::git::fake::try_git(
                    &std::env::temp_dir(),
                    &["check-ref-format", &format!("refs/tags/{name}")],
                );
            if KIKA_ONLY.contains(&name) {
                assert!(theirs && !ours, "{name:?}");
            } else {
                assert_eq!(ours, theirs, "{name:?}");
            }
        }
    }
}
