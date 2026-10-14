//! The command line the Command log shows for a git command. It is drawn
//! for the user's eyes only, so anything that could be a secret is left
//! out: env and stdin never, config values that carry credentials as
//! `<redacted>`, URL userinfo stripped, and message bodies cut to a subject.

use super::runner::GitCommand;
use super::safe_text::{redact_userinfo, sanitize};

/// The longest line drawn, in chars, `…` included.
pub const MAX_LINE: usize = 1000;
/// The longest message subject drawn, in chars, before `…`.
pub const MAX_SUBJECT: usize = 50;
/// What a config value that may be a secret is drawn as.
pub const REDACTED: &str = "<redacted>";

/// Subcommands whose `-m` and `--message` carry a message, not a name.
const MESSAGE_SUBCOMMANDS: [&str; 6] = ["commit", "commit-tree", "tag", "merge", "stash", "notes"];

/// Global options before the subcommand that take the next arg as a path,
/// name or tree.
const GLOBAL_WITH_VALUE: [&str; 5] = [
    "-C",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--attr-source",
];

/// Where a message's subject ends: every line break a terminal or a font
/// may draw as one.
const LINE_BREAKS: [char; 7] = [
    '\n', '\r', '\u{0B}', '\u{0C}', '\u{85}', '\u{2028}', '\u{2029}',
];

/// `git` and the args of `cmd`, one space apart, with secrets left out.
/// Env and stdin are never read. Forms Kika never builds today (joined
/// `-ckey=value`, bundled `-am`, abbreviated `--mess=`) are still read the
/// way git reads them, so a future caller cannot leak through them.
pub fn display_line(cmd: &GitCommand) -> String {
    let args: Vec<String> = cmd
        .args()
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let mut words = vec!["git".to_string()];
    let mut iter = args.iter();
    let mut subcommand = None;
    while let Some(arg) = iter.next() {
        if arg == "-c" || arg == "--config-env" {
            words.push(arg.clone());
            words.extend(iter.next().map(|pair| config_pair(pair)));
        } else if let Some(pair) = arg.strip_prefix("--config-env=") {
            words.push(format!("--config-env={}", config_pair(pair)));
        } else if let Some(pair) = arg.strip_prefix("-c") {
            words.push(format!("-c{}", config_pair(pair)));
        } else if GLOBAL_WITH_VALUE.contains(&arg.as_str()) {
            words.push(arg.clone());
            words.extend(iter.next().map(|value| plain(value)));
        } else if arg.starts_with('-') {
            words.push(plain(arg));
        } else {
            words.push(plain(arg));
            subcommand = Some(arg.as_str());
            break;
        }
    }
    let messages = subcommand.is_some_and(|sub| MESSAGE_SUBCOMMANDS.contains(&sub));
    let config = subcommand == Some("config");
    let mut options = true;
    let mut secret_value = false;
    while let Some(arg) = iter.next() {
        if secret_value {
            words.push(REDACTED.to_string());
        } else if !options || arg == "--" {
            // `git config -- <key> <value>`: after `--` an arg is a key
            // even if it starts with `-`, so it is still checked.
            secret_value = config && !options && secret_key(arg);
            options = false;
            words.push(plain(arg));
        } else if messages && arg.starts_with('-') {
            message_option(arg, &mut iter, &mut words);
        } else {
            // `git config <key> <value>`: what follows a secret key goes.
            secret_value = config && !arg.starts_with('-') && secret_key(arg);
            words.push(plain(arg));
        }
    }
    cut_line(words.join(" "))
}

/// An option of a message subcommand, and its value if it takes the next
/// arg. Any unique prefix of `--message` or `--file` counts, as git
/// accepts one, and so does `m` or `F` anywhere in a bundle of short
/// flags such as `-am`: what follows it in the bundle, or else the next
/// arg, is its value.
fn message_option<'a>(
    arg: &str,
    rest: &mut impl Iterator<Item = &'a String>,
    words: &mut Vec<String>,
) {
    if let Some(long) = arg.strip_prefix("--") {
        let (name, value) = match long.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (long, None),
        };
        let message = !name.is_empty() && "message".starts_with(name);
        let file = !name.is_empty() && "file".starts_with(name);
        match (value, message, file) {
            (Some(value), true, _) => words.push(safe(&format!("--{name}={}", subject(value)))),
            (None, true, _) => {
                words.push(plain(arg));
                words.extend(rest.next().map(|value| safe(&subject(value))));
            }
            (None, _, true) => {
                words.push(plain(arg));
                words.extend(rest.next().map(|value| plain(value)));
            }
            _ => words.push(plain(arg)),
        }
        return;
    }
    let Some(at) = arg
        .char_indices()
        .skip(1)
        .find(|&(_, c)| c == 'm' || c == 'F')
        .map(|(i, _)| i)
    else {
        words.push(plain(arg));
        return;
    };
    let (flags, value) = arg.split_at(at + 1);
    let message = flags.ends_with('m');
    match (value.is_empty(), message) {
        (false, true) => words.push(safe(&format!("{flags}{}", subject(value)))),
        (false, false) => words.push(safe(&format!("{flags}{}", path(value)))),
        (true, true) => {
            words.push(plain(flags));
            words.extend(rest.next().map(|value| safe(&subject(value))));
        }
        (true, false) => {
            words.push(plain(flags));
            words.extend(rest.next().map(|value| plain(value)));
        }
    }
}

/// Terminal-safe, with the userinfo taken out of every URL in it.
fn safe(text: &str) -> String {
    redact_userinfo(&sanitize(text.as_bytes()))
}

/// An arg with no special meaning: safe, and short if it is a full path,
/// or a `--name=` option whose value is one.
fn plain(arg: &str) -> String {
    if arg.starts_with("--")
        && let Some((name, value)) = arg.split_once('=')
    {
        return safe(&format!("{name}={}", path(value)));
    }
    safe(&path(arg))
}

/// `key=value` from a leading `-c`, with the value redacted where the key
/// is one that holds credentials or runs programs.
fn config_pair(pair: &str) -> String {
    let Some((key, value)) = pair.split_once('=') else {
        return safe(pair);
    };
    let value = if secret_key(key) {
        REDACTED.to_string()
    } else {
        safe(value)
    };
    format!("{}={value}", safe(key))
}

/// Whether a config key's value may be a secret or a command line. A last
/// segment that merely contains `pass` (`smtpPass`, `passphrase`, but also
/// `bypass`) counts, as a false alarm only hides a harmless value.
fn secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    let last = key.rsplit('.').next().unwrap_or_default();
    key.starts_with("credential.")
        || key.starts_with("url.")
        || key.starts_with("alias.")
        || key == "core.askpass"
        || key == "core.sshcommand"
        || key.starts_with("http.") && key.ends_with(".extraheader")
        || ["pass", "token", "secret", "cookie"]
            .iter()
            .any(|word| last.contains(word))
}

/// A message's first line, cut to [`MAX_SUBJECT`] chars, with `…` when
/// anything was left out, the body included.
fn subject(message: &str) -> String {
    let (first, body) = message.split_once(LINE_BREAKS).unwrap_or((message, ""));
    let kept: String = first.chars().take(MAX_SUBJECT).collect();
    let dropped = kept.len() < first.len() || !body.trim().is_empty();
    if dropped { format!("{kept}…") } else { kept }
}

/// `…/<last component>` for an absolute path, which may name a user's
/// home or a temp file; anything else as it is.
fn path(arg: &str) -> String {
    if !absolute(arg) {
        return arg.to_string();
    }
    let last = arg
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or_default();
    format!("…/{last}")
}

/// Unix, home-relative, UNC and drive-letter paths.
fn absolute(arg: &str) -> bool {
    let bytes = arg.as_bytes();
    let drive = matches!(bytes, [letter, b':', b'\\' | b'/', ..] if letter.is_ascii_alphabetic());
    arg.starts_with('/') || arg.starts_with("~/") || arg.starts_with("\\\\") || drive
}

/// At most [`MAX_LINE`] chars, the last one `…` if any were cut.
fn cut_line(line: String) -> String {
    if line.chars().count() <= MAX_LINE {
        return line;
    }
    let mut cut: String = line.chars().take(MAX_LINE - 1).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::askpass::SOCKET_ENV;
    use crate::rebase_plan::PLAN_ENV;

    fn cmd(args: &[&str]) -> GitCommand {
        let (first, rest) = args.split_first().unwrap();
        rest.iter()
            .fold(GitCommand::new(*first), |cmd, arg| cmd.arg(*arg))
    }

    #[test]
    fn env_names_and_values_are_never_drawn() {
        let command = cmd(&["rebase", "-i", "main"])
            .env(SOCKET_ENV, "/tmp/kika-sock/s")
            .env("GIT_SEQUENCE_EDITOR", "/usr/bin/kika-editor")
            .env(PLAN_ENV, "pick abc secret-plan\n")
            .env("GIT_REFLOG_ACTION", "kika-action");
        let line = display_line(&command);
        assert_eq!(line, "git rebase -i main");
        for hidden in [
            SOCKET_ENV,
            "kika-sock",
            "GIT_SEQUENCE_EDITOR",
            "kika-editor",
            PLAN_ENV,
            "secret-plan",
            "GIT_REFLOG_ACTION",
            "kika-action",
        ] {
            assert!(!line.contains(hidden), "{hidden} in {line}");
        }
    }

    #[test]
    fn stdin_bytes_are_never_drawn() {
        let command = cmd(&["commit", "-F", "-"]).stdin(b"subject\n\nsecret body\n".to_vec());
        let line = display_line(&command);
        assert_eq!(line, "git commit -F -");
        assert!(!line.contains("secret") && !line.contains("subject"));
    }

    #[test]
    fn userinfo_is_stripped_from_any_url() {
        let cases: &[(&[&str], &str)] = &[
            (
                &["remote", "add", "--", "o", "https://u:tok@h/r"],
                "git remote add -- o https://h/r",
            ),
            (
                &["fetch", "ssh://git@h:22/r", "https://tok@h"],
                "git fetch ssh://h:22/r https://h",
            ),
            (
                &["clone", "--template=https://u:p@h/t"],
                "git clone --template=https://h/t",
            ),
        ];
        for (args, want) in cases {
            assert_eq!(display_line(&cmd(args)), *want, "{args:?}");
        }
    }

    #[test]
    fn leading_config_values_that_may_be_secret_are_redacted() {
        let cases = [
            (
                "http.extraHeader=Authorization: Bearer x",
                "http.extraHeader=<redacted>",
            ),
            (
                "http.https://h/.extraheader=Authorization: Bearer x",
                "http.https://h/.extraheader=<redacted>",
            ),
            (
                "HTTP.https://u:p@h/.ExtraHeader=x",
                "HTTP.https://h/.ExtraHeader=<redacted>",
            ),
            (
                "credential.helper=store --file x",
                "credential.helper=<redacted>",
            ),
            (
                "credential.https://h.username=bob",
                "credential.https://h.username=<redacted>",
            ),
            ("remote.o.password=hunter2", "remote.o.password=<redacted>"),
            (
                "url.https://u:p@h/.insteadOf=https://other/",
                "url.https://h/.insteadOf=<redacted>",
            ),
            ("url.x.pushInsteadOf=y", "url.x.pushInsteadOf=<redacted>"),
            ("alias.x=!sh -c 'tok'", "alias.x=<redacted>"),
            ("core.sshCommand=ssh -i key", "core.sshCommand=<redacted>"),
            ("core.askPass=/bin/pass", "core.askPass=<redacted>"),
            ("foo.apiToken=t0k", "foo.apiToken=<redacted>"),
            ("foo.SECRET_key=s", "foo.SECRET_key=<redacted>"),
            ("x.y.sessionCookie=c", "x.y.sessionCookie=<redacted>"),
            ("sendemail.smtpPass=x", "sendemail.smtpPass=<redacted>"),
            ("imap.pass=x", "imap.pass=<redacted>"),
            ("IMAP.PASS=x", "IMAP.PASS=<redacted>"),
            ("foo.passphrase=x", "foo.passphrase=<redacted>"),
        ];
        for (pair, want) in cases {
            let line = display_line(&cmd(&["-c", pair, "status"]));
            assert_eq!(line, format!("git -c {want} status"), "{pair}");
        }
    }

    #[test]
    fn other_config_values_are_drawn_safely() {
        let cases = [
            ("rebase.updateRefs=false", "rebase.updateRefs=false"),
            ("user.name=t", "user.name=t"),
            ("commit.gpgsign", "commit.gpgsign"),
            // A token-looking word in a non-final segment is not a secret key.
            ("token.enabled=yes", "token.enabled=yes"),
            ("pass.enabled=yes", "pass.enabled=yes"),
            ("remote.o.url=https://u:p@h/r", "remote.o.url=https://h/r"),
            ("core.editor=\x1b[2Jvi", "core.editor=\u{FFFD}[2Jvi"),
        ];
        for (pair, want) in cases {
            let line = display_line(&cmd(&["-c", pair, "-c", "a.b=c", "status"]));
            assert_eq!(line, format!("git -c {want} -c a.b=c status"), "{pair}");
        }
    }

    #[test]
    fn every_global_option_form_is_read_before_the_subcommand() {
        let cases: &[(&[&str], &str)] = &[
            // Joined, as `-c` and `--config-env` may be written.
            (
                &["-ccredential.helper=store", "fetch"],
                "git -ccredential.helper=<redacted> fetch",
            ),
            (
                &["-crebase.updateRefs=false", "status"],
                "git -crebase.updateRefs=false status",
            ),
            (
                &["--config-env=http.extraHeader=AUTH_ENV", "fetch"],
                "git --config-env=http.extraHeader=<redacted> fetch",
            ),
            (
                &["--config-env", "remote.o.password=PW_ENV", "fetch"],
                "git --config-env remote.o.password=<redacted> fetch",
            ),
            // `-C <dir>` first does not hide the `-c` pairs after it.
            (
                &[
                    "-C",
                    "/home/me/repo",
                    "-c",
                    "core.sshCommand=ssh -i k",
                    "fetch",
                ],
                "git -C …/repo -c core.sshCommand=<redacted> fetch",
            ),
            (
                &[
                    "--git-dir",
                    "/home/me/r/.git",
                    "--work-tree=/home/me/r",
                    "status",
                ],
                "git --git-dir …/.git --work-tree=…/r status",
            ),
            // And the subcommand is still found for the message rule.
            (
                &[
                    "-C",
                    "sub",
                    "--no-pager",
                    "-c",
                    "a.b=c",
                    "commit",
                    "-m",
                    "s\nbody secret",
                ],
                "git -C sub --no-pager -c a.b=c commit -m s…",
            ),
            // `--attr-source <tree>` takes the next arg, so the subcommand
            // after it is still found.
            (
                &["--attr-source", "HEAD", "config", "credential.helper", "x"],
                "git --attr-source HEAD config credential.helper <redacted>",
            ),
            (
                &["--attr-source", "HEAD", "commit", "-m", "s\nbody secret"],
                "git --attr-source HEAD commit -m s…",
            ),
            (
                &["--attr-source=HEAD", "config", "credential.helper", "x"],
                "git --attr-source=HEAD config credential.helper <redacted>",
            ),
            (
                &["--attr-source=HEAD", "commit", "-m", "s\nbody secret"],
                "git --attr-source=HEAD commit -m s…",
            ),
        ];
        for (args, want) in cases {
            let line = display_line(&cmd(args));
            assert_eq!(line, *want, "{args:?}");
            assert!(!line.contains("secret"), "{line}");
        }
    }

    #[test]
    fn bundled_and_abbreviated_message_options_are_cut_too() {
        let cases: &[(&[&str], &str)] = &[
            (&["commit", "-am", "s\nbody secret"], "git commit -am s…"),
            (&["commit", "-qm", "s\nbody secret"], "git commit -qm s…"),
            (&["commit", "-qams\nbody secret"], "git commit -qams…"),
            (&["commit", "-aF", "/tmp/k/msg"], "git commit -aF …/msg"),
            (&["commit", "-aF/tmp/k/msg"], "git commit -aF…/msg"),
            (&["commit", "--mess=s\nbody secret"], "git commit --mess=s…"),
            (&["commit", "--me", "s\nbody secret"], "git commit --me s…"),
            (&["commit", "--fi", "/tmp/k/msg"], "git commit --fi …/msg"),
            (&["commit", "--fil=/tmp/k/msg"], "git commit --fil=…/msg"),
            (
                &["commit-tree", "-p", "HEAD", "-m", "s\nbody secret", "abc"],
                "git commit-tree -p HEAD -m s… abc",
            ),
            // Not message options.
            (
                &["commit", "--amend", "--no-edit"],
                "git commit --amend --no-edit",
            ),
            (&["commit", "-a", "-q"], "git commit -a -q"),
            (&["commit", "-F", "-"], "git commit -F -"),
        ];
        for (args, want) in cases {
            let line = display_line(&cmd(args));
            assert_eq!(line, *want, "{args:?}");
            assert!(!line.contains("secret"), "{line}");
        }
    }

    #[test]
    fn a_subject_ends_at_any_line_break() {
        for brk in [
            "\n", "\r", "\r\n", "\u{2028}", "\u{2029}", "\u{85}", "\u{0B}", "\u{0C}",
        ] {
            let message = format!("subj{brk}body secret");
            let line = display_line(&cmd(&["tag", "-a", "-m", &message, "v1"]));
            assert_eq!(line, "git tag -a -m subj… v1", "{brk:?}");
        }
        // A break with nothing after it drops nothing.
        let line = display_line(&cmd(&["commit", "-m", "subj\r\n"]));
        assert_eq!(line, "git commit -m subj");
    }

    #[test]
    fn a_config_command_hides_the_value_of_a_secret_key() {
        let cases: &[(&[&str], &str)] = &[
            (
                &["config", "credential.helper", "store --file x"],
                "git config credential.helper <redacted>",
            ),
            (
                &["config", "set", "--local", "remote.o.password", "hunter2"],
                "git config set --local remote.o.password <redacted>",
            ),
            (
                &["config", "--get", "core.askPass"],
                "git config --get core.askPass",
            ),
            (&["config", "user.name", "Ann"], "git config user.name Ann"),
            (
                &["config", "sendemail.smtpPass", "hunter2"],
                "git config sendemail.smtpPass <redacted>",
            ),
        ];
        for (args, want) in cases {
            assert_eq!(display_line(&cmd(args)), *want, "{args:?}");
        }
    }

    #[test]
    fn a_config_key_after_dashes_is_still_checked() {
        let cases: &[(&[&str], &str)] = &[
            (
                &["config", "--", "credential.helper", "x"],
                "git config -- credential.helper <redacted>",
            ),
            (
                &["config", "set", "--local", "--", "imap.pass", "p"],
                "git config set --local -- imap.pass <redacted>",
            ),
            (
                &["config", "--", "user.name", "Ann"],
                "git config -- user.name Ann",
            ),
            // After `--` a key is a key even if it starts with `-`.
            (
                &["config", "--", "-x.password", "p"],
                "git config -- -x.password <redacted>",
            ),
            // `--` outside `config` still means everything after it is plain.
            (&["add", "--", "imap.pass", "x"], "git add -- imap.pass x"),
        ];
        for (args, want) in cases {
            assert_eq!(display_line(&cmd(args)), *want, "{args:?}");
        }
    }

    #[test]
    fn whitespace_in_a_url_password_is_stripped_with_it() {
        let cases: &[(&[&str], &str)] = &[
            (
                &["remote", "add", "--", "o", "https://u:my pass@h/r"],
                "git remote add -- o https://h/r",
            ),
            (
                &["remote", "add", "--", "o", "https://u:a\tb@h/r"],
                "git remote add -- o https://h/r",
            ),
        ];
        for (args, want) in cases {
            assert_eq!(display_line(&cmd(args)), *want, "{args:?}");
        }
    }

    #[test]
    fn a_config_pair_after_the_subcommand_is_not_treated_as_config() {
        // `commit -c <commit>` reuses a message; it is not a config pair.
        let line = display_line(&cmd(&["commit", "-c", "credential.helper=x"]));
        assert_eq!(line, "git commit -c credential.helper=x");
    }

    #[test]
    fn messages_show_only_their_subject() {
        let sixty = "s".repeat(60);
        let fifty = "s".repeat(50);
        let cases: Vec<(Vec<&str>, String)> = vec![
            (
                vec!["commit", "-m", "line1\nbody secret"],
                "git commit -m line1…".into(),
            ),
            (vec!["commit", "-m", "line1"], "git commit -m line1".into()),
            (
                vec!["commit", "-m", "line1\n"],
                "git commit -m line1".into(),
            ),
            (
                vec!["commit", "-m", "line1\r\n\r\nbody"],
                "git commit -m line1…".into(),
            ),
            (
                vec!["commit", "-m", &sixty],
                format!("git commit -m {fifty}…"),
            ),
            (
                vec!["commit", "-m", &fifty],
                format!("git commit -m {fifty}"),
            ),
            (
                vec!["tag", "-a", "-m", "v1\n\nbody secret", "v1"],
                "git tag -a -m v1… v1".into(),
            ),
            (
                vec!["commit", "--message=line1\nbody secret"],
                "git commit --message=line1…".into(),
            ),
            (
                vec!["commit", "--message", "line1\nbody secret"],
                "git commit --message line1…".into(),
            ),
            (
                vec!["commit", "-mline1\nbody secret"],
                "git commit -mline1…".into(),
            ),
            (
                vec!["merge", "--no-ff", "-m", "Merge x\n\nsecret", "x"],
                "git merge --no-ff -m Merge x… x".into(),
            ),
            (
                vec!["stash", "push", "-m", "wip\nsecret"],
                "git stash push -m wip…".into(),
            ),
            (
                vec!["notes", "add", "-m", "note\nsecret", "HEAD"],
                "git notes add -m note… HEAD".into(),
            ),
            (
                vec!["-c", "user.name=t", "commit", "-q", "-m", "a\nsecret"],
                "git -c user.name=t commit -q -m a…".into(),
            ),
        ];
        for (args, want) in cases {
            let line = display_line(&cmd(&args));
            assert_eq!(line, want, "{args:?}");
            assert!(!line.contains("secret"), "{line}");
        }
    }

    #[test]
    fn a_message_flag_outside_a_message_subcommand_or_after_dashes_is_left_alone() {
        let cases: &[(&[&str], &str)] = &[
            (&["branch", "-m", "--", "a", "b"], "git branch -m -- a b"),
            (&["branch", "-m", "old", "new"], "git branch -m old new"),
            (&["commit", "--", "-m"], "git commit -- -m"),
        ];
        for (args, want) in cases {
            assert_eq!(display_line(&cmd(args)), *want, "{args:?}");
        }
    }

    #[test]
    fn message_files_and_absolute_paths_show_only_their_last_component() {
        let cases: &[(&[&str], &str)] = &[
            (
                &["commit", "-F", "/tmp/kika-xyz/msg"],
                "git commit -F …/msg",
            ),
            (&["commit", "-F", "-"], "git commit -F -"),
            (
                &["commit", "--file", "/tmp/kika-xyz/msg"],
                "git commit --file …/msg",
            ),
            (
                &["commit", "--file=/tmp/kika-xyz/msg"],
                "git commit --file=…/msg",
            ),
            (&["commit", "--file=-"], "git commit --file=-"),
            (
                &["worktree", "add", "~/work/x/", "b"],
                "git worktree add …/x b",
            ),
            (
                &["worktree", "add", "C:\\Users\\me\\wt", "b"],
                "git worktree add …/wt b",
            ),
            (&["worktree", "add", "d:/me/wt"], "git worktree add …/wt"),
            (
                &["worktree", "add", "\\\\srv\\share\\wt"],
                "git worktree add …/wt",
            ),
            (&["add", "--", "src/a.rs", "C:x"], "git add -- src/a.rs C:x"),
            (&["init", "/"], "git init …/"),
        ];
        for (args, want) in cases {
            assert_eq!(display_line(&cmd(args)), *want, "{args:?}");
        }
    }

    #[test]
    fn control_bytes_are_never_drawn_raw() {
        let line = display_line(&cmd(&["checkout", "x\x1b[2Jy", "a\nb"]));
        assert!(!line.contains('\x1b'), "{line:?}");
        assert!(!line.contains('\n'), "{line:?}");
        assert_eq!(line, "git checkout x\u{FFFD}[2Jy a\u{FFFD}b");
    }

    #[test]
    fn a_long_line_is_cut_to_the_limit() {
        let long = "x".repeat(5000);
        let line = display_line(&cmd(&["add", "--", &long]));
        assert_eq!(line.chars().count(), MAX_LINE);
        assert!(line.starts_with("git add -- xxx"));
        assert!(line.ends_with("x…"));

        // Multi-byte chars are cut on a char boundary.
        let wide = "é".repeat(5000);
        let line = display_line(&cmd(&["add", "--", &wide]));
        assert_eq!(line.chars().count(), MAX_LINE);

        let fits = "x".repeat(MAX_LINE - "git add -- ".len());
        let line = display_line(&cmd(&["add", "--", &fits]));
        assert_eq!(line.chars().count(), MAX_LINE);
        assert!(!line.ends_with('…'));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_args_are_drawn_lossily() {
        use std::os::unix::ffi::OsStringExt;
        let raw = std::ffi::OsString::from_vec(b"a\xffb".to_vec());
        let line = display_line(&GitCommand::new("add").arg("--").arg(raw));
        assert_eq!(line, "git add -- a\u{FFFD}b");
    }
}
