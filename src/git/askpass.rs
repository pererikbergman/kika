//! The bridge that lets git and ssh ask the user something while Kika owns
//! the screen. Both run an "askpass" program for a username, password,
//! passphrase or host-key answer; Kika names its own binary, which (with
//! [`SOCKET_ENV`] set) connects back over a private unix socket and prints
//! whatever the user typed into a popup. Credential helpers and ssh-agent are
//! asked first by git and ssh, so the bridge only runs when they had nothing.

use std::fmt;

/// Set on the askpass process's environment: the socket to ask over. When
/// Kika starts with it set, it is the askpass program, not the app.
pub const SOCKET_ENV: &str = "KIKA_ASKPASS_SOCKET";

/// Something the user typed for git or ssh. Its `Debug` hides the text, so
/// no intent, effect or state printed for a log or a failed test shows it.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// The text itself, only for handing it to the process that asked.
    // Only the unix bridge hands answers over.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// One question from git or ssh, its text made safe to draw.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Question {
    /// Wants an answer. `confirm` is ssh's yes/no kind, answered by the
    /// exit status rather than by text.
    Ask { prompt: String, confirm: bool },
    /// Only says something, such as "touch your key"; nothing is typed.
    Notice(String),
}

#[cfg(unix)]
pub use bridge::{AskpassServer, client};

#[cfg(unix)]
mod bridge {
    use std::ffi::OsStr;
    use std::fs::{self, DirBuilder};
    use std::io::{self, Read, Write};
    use std::net::Shutdown;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard};
    use std::time::{Duration, Instant};

    use super::{Question, Secret};
    use crate::git::diff::terminal_safe;

    /// A question longer than this is cut; no real prompt comes near it.
    const MAX_REQUEST: usize = 64 * 1024;

    /// How often the accept loop looks for a connection or a stop.
    const POLL: Duration = Duration::from_millis(20);

    /// How long a client may take to send its question. The askpass
    /// program sends it at once; a connection that never finishes must
    /// not hold up every later question.
    const READ_TIMEOUT: Duration = Duration::from_secs(5);

    /// How long a client may take to send its whole question, however
    /// often it sends a byte. Without it a connection that trickles in
    /// just under [`READ_TIMEOUT`] would hold every later question forever.
    const TOTAL_READ_DEADLINE: Duration = Duration::from_secs(10);

    /// The read limits for one connection; tests shorten them.
    #[derive(Debug, Clone, Copy)]
    struct Limits {
        read_timeout: Duration,
        deadline: Duration,
    }

    /// The listening end. Questions are answered one at a time, in the
    /// order they arrive, by the `ask` given to [`AskpassServer::start`].
    /// Dropping it removes the socket and its directory and hangs up on a
    /// question still waiting, whose client then exits 1.
    pub struct AskpassServer {
        dir: PathBuf,
        socket: PathBuf,
        stop: Arc<AtomicBool>,
        /// The connection being answered, so dropping can hang up on it.
        current: Arc<Mutex<Option<UnixStream>>>,
    }

    impl AskpassServer {
        /// Listen in a new directory only this user can enter. `ask` runs
        /// on the server's thread and may block until the user answers;
        /// `None` cancels. A notice is answered before `ask` sees it, and
        /// what `ask` returns for it is ignored.
        pub fn start(
            ask: impl Fn(Question) -> Option<Secret> + Send + 'static,
        ) -> io::Result<Self> {
            let limits = Limits {
                read_timeout: READ_TIMEOUT,
                deadline: TOTAL_READ_DEADLINE,
            };
            Self::start_with_limits(ask, limits)
        }

        fn start_with_limits(
            ask: impl Fn(Question) -> Option<Secret> + Send + 'static,
            limits: Limits,
        ) -> io::Result<Self> {
            let (dir, listener) = bind()?;
            let socket = dir.join(SOCKET_NAME);
            let stop = Arc::new(AtomicBool::new(false));
            let current = Arc::new(Mutex::new(None));
            let server = Self {
                dir,
                socket,
                stop: Arc::clone(&stop),
                current: Arc::clone(&current),
            };
            // Not joined: it may be waiting on the user when the server is
            // dropped, and ends once `ask` returns.
            std::thread::Builder::new()
                .name("kika-askpass".into())
                .spawn(move || serve(&listener, &ask, limits, &stop, &current))?;
            Ok(server)
        }

        pub fn socket(&self) -> &Path {
            &self.socket
        }
    }

    impl Drop for AskpassServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            // The client has shut its own write side already, and some
            // systems refuse `Both` on a half-closed socket.
            if let Some(stream) = lock(&self.current).take() {
                let _ = stream.shutdown(Shutdown::Write);
            }
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    const SOCKET_NAME: &str = "s";

    /// A fresh private directory with the socket bound in it: under the
    /// temp dir, or `/tmp` when that path is too long for a socket.
    fn bind() -> io::Result<(PathBuf, UnixListener)> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut last = io::Error::other("no directory for the askpass socket");
        for base in [std::env::temp_dir(), PathBuf::from("/tmp")] {
            let dir = base.join(format!(
                "kika-askpass-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            // `create` fails on an existing path, so nobody else's directory
            // is ever used; the umask can only narrow 0700.
            if let Err(e) = DirBuilder::new().mode(0o700).create(&dir) {
                last = e;
                continue;
            }
            let bound = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
                .and_then(|()| UnixListener::bind(dir.join(SOCKET_NAME)))
                .and_then(|listener| listener.set_nonblocking(true).map(|()| listener));
            match bound {
                Ok(listener) => return Ok((dir, listener)),
                Err(e) => {
                    let _ = fs::remove_dir_all(&dir);
                    last = e;
                }
            }
        }
        Err(last)
    }

    fn serve(
        listener: &UnixListener,
        ask: &dyn Fn(Question) -> Option<Secret>,
        limits: Limits,
        stop: &AtomicBool,
        current: &Mutex<Option<UnixStream>>,
    ) {
        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => answer(stream, ask, limits, stop, current),
                Err(_) => std::thread::sleep(POLL),
            }
        }
    }

    /// Read one question, ask it, and write the reply: `1` and the answer,
    /// or `0` for cancelled.
    fn answer(
        stream: UnixStream,
        ask: &dyn Fn(Question) -> Option<Secret>,
        limits: Limits,
        stop: &AtomicBool,
        current: &Mutex<Option<UnixStream>>,
    ) {
        if stream.set_nonblocking(false).is_err() {
            return;
        }
        *lock(current) = stream.try_clone().ok();
        let read = read_request(&stream, limits);
        // Stopped while this one connected: nobody is left to answer.
        let question = match read {
            Ok(request) if !stop.load(Ordering::SeqCst) => Some(parse(&request)),
            _ => None,
        };
        let (reply, notice) = match question {
            // Answered before it is shown, so ssh goes on at once.
            Some(notice @ Question::Notice(_)) => (Some(Secret::default()), Some(notice)),
            Some(question) => (ask(question), None),
            None => (None, None),
        };
        let bytes = match reply {
            Some(secret) => [b"1", secret.expose().as_bytes()].concat(),
            None => b"0".to_vec(),
        };
        // A client that went away has nothing to read it.
        let _ = (&stream).write_all(&bytes);
        let _ = stream.shutdown(Shutdown::Write);
        if let Some(notice) = notice {
            ask(notice);
        }
        *lock(current) = None;
    }

    /// The whole question, up to [`MAX_REQUEST`]: an error once one read
    /// waits longer than `read_timeout` or all of them take longer than
    /// `deadline`.
    fn read_request(stream: &UnixStream, limits: Limits) -> io::Result<Vec<u8>> {
        let deadline = Instant::now() + limits.deadline;
        let mut request = Vec::new();
        let mut buf = [0; 4096];
        while request.len() < MAX_REQUEST {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            stream.set_read_timeout(Some(limits.read_timeout.min(left)))?;
            let room = buf.len().min(MAX_REQUEST - request.len());
            match (&*stream).read(&mut buf[..room]) {
                Ok(0) => break,
                Ok(n) => request.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(request)
    }

    /// A request is ssh's `SSH_ASKPASS_PROMPT` value, a newline, then the
    /// prompt.
    fn parse(request: &[u8]) -> Question {
        let (kind, prompt) = match request.iter().position(|&b| b == b'\n') {
            Some(i) => (&request[..i], &request[i + 1..]),
            None => (&b""[..], request),
        };
        let prompt = terminal_safe(prompt);
        match kind {
            b"none" => Question::Notice(prompt),
            b"confirm" => Question::Ask {
                prompt,
                confirm: true,
            },
            _ => Question::Ask {
                prompt,
                confirm: false,
            },
        }
    }

    /// The askpass program: send `prompt` (and ssh's `kind`, its
    /// `SSH_ASKPASS_PROMPT`) to Kika at `socket`, and print the answer and a
    /// newline to `out`, which git and ssh read. Exit 0 when answered, 1
    /// when the user cancelled or Kika could not be reached.
    pub fn client(
        socket: &Path,
        prompt: &OsStr,
        kind: Option<&OsStr>,
        out: &mut dyn Write,
    ) -> ExitCode {
        let printed = ask_server(socket, prompt, kind).is_some_and(|answer| {
            out.write_all(&answer)
                .and_then(|()| out.write_all(b"\n"))
                .and_then(|()| out.flush())
                .is_ok()
        });
        if printed {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    }

    fn ask_server(socket: &Path, prompt: &OsStr, kind: Option<&OsStr>) -> Option<Vec<u8>> {
        let mut stream = UnixStream::connect(socket).ok()?;
        let kind = kind.map_or(&b""[..], OsStr::as_bytes);
        let request = [kind, b"\n", prompt.as_bytes()].concat();
        stream.write_all(&request).ok()?;
        stream.shutdown(Shutdown::Write).ok()?;
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).ok()?;
        match reply.split_first() {
            Some((b'1', answer)) => Some(answer.to_vec()),
            _ => None,
        }
    }

    /// A poisoned lock only means another thread panicked; the slot is fine.
    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(test)]
    mod tests {
        use std::sync::mpsc;
        use std::thread::JoinHandle;

        use super::*;

        /// A server whose questions arrive on the returned channel, each
        /// with a sender to answer it.
        type Asked = mpsc::Receiver<(Question, mpsc::Sender<Option<Secret>>)>;

        fn server() -> (AskpassServer, Asked) {
            server_with(None)
        }

        /// As [`server`], with the production limits unless `limits` is given.
        fn server_with(limits: Option<Limits>) -> (AskpassServer, Asked) {
            let (asked_tx, asked) = mpsc::channel();
            let asked_tx = Mutex::new(asked_tx);
            let ask = move |question| {
                let (answer_tx, answer_rx) = mpsc::channel();
                lock(&asked_tx).send((question, answer_tx)).ok()?;
                answer_rx.recv().ok().flatten()
            };
            let server = match limits {
                Some(limits) => AskpassServer::start_with_limits(ask, limits),
                None => AskpassServer::start(ask),
            }
            .unwrap();
            (server, asked)
        }

        /// Run the client in a thread, as git would, collecting its stdout.
        fn run_client(
            socket: &Path,
            prompt: &str,
            kind: Option<&str>,
        ) -> JoinHandle<(ExitCode, Vec<u8>)> {
            let socket = socket.to_path_buf();
            let prompt = prompt.to_string();
            let kind = kind.map(str::to_string);
            std::thread::spawn(move || {
                let mut out = Vec::new();
                let code = client(
                    &socket,
                    OsStr::new(&prompt),
                    kind.as_deref().map(OsStr::new),
                    &mut out,
                );
                (code, out)
            })
        }

        #[test]
        fn an_answer_reaches_the_client_which_prints_it_and_exits_0() {
            let (server, asked) = server();
            let waiting = run_client(server.socket(), "Password for 'https://h': ", None);

            let (question, answer) = asked.recv().unwrap();
            assert_eq!(
                question,
                Question::Ask {
                    prompt: "Password for 'https://h':".into(),
                    confirm: false
                }
            );
            answer.send(Some(Secret::new("hunter2"))).unwrap();

            let (code, out) = waiting.join().unwrap();
            assert_eq!(code, ExitCode::SUCCESS);
            assert_eq!(out, b"hunter2\n");
        }

        #[test]
        fn a_cancelled_question_exits_1_and_prints_nothing() {
            let (server, asked) = server();
            let waiting = run_client(server.socket(), "Username for 'https://h': ", None);
            let (_, answer) = asked.recv().unwrap();
            answer.send(None).unwrap();

            let (code, out) = waiting.join().unwrap();
            assert_eq!(code, ExitCode::FAILURE);
            assert!(out.is_empty());
        }

        #[test]
        fn ssh_confirm_and_notice_kinds_are_told_apart() {
            let (server, asked) = server();
            let waiting = run_client(server.socket(), "Allow use of key?", Some("confirm"));
            let (question, answer) = asked.recv().unwrap();
            assert_eq!(
                question,
                Question::Ask {
                    prompt: "Allow use of key?".into(),
                    confirm: true
                }
            );
            answer.send(Some(Secret::new("yes"))).unwrap();
            assert_eq!(waiting.join().unwrap().0, ExitCode::SUCCESS);

            // A notice is answered at once, whatever `ask` does with it.
            let waiting = run_client(server.socket(), "Touch your key\x1b[2J", Some("none"));
            assert_eq!(waiting.join().unwrap().0, ExitCode::SUCCESS);
            let (question, _ignored) = asked.recv().unwrap();
            assert_eq!(
                question,
                Question::Notice("Touch your key\u{FFFD}[2J".into())
            );
        }

        #[test]
        fn dropping_the_server_hangs_up_on_a_waiting_client_and_removes_the_dir() {
            let (server, asked) = server();
            let dir = server.socket().parent().unwrap().to_path_buf();
            let waiting = run_client(server.socket(), "Passphrase: ", None);
            // Asked, and never answered.
            let (_question, _answer) = asked.recv().unwrap();

            drop(server);

            let (code, out) = waiting.join().unwrap();
            assert_eq!(code, ExitCode::FAILURE);
            assert!(out.is_empty());
            assert!(!dir.exists(), "{dir:?} is left behind");
        }

        #[test]
        fn the_socket_lives_in_a_directory_only_this_user_can_enter() {
            let (server, _asked) = server();
            let dir = server.socket().parent().unwrap();
            let mode = fs::metadata(dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
            let (other, _asked) = self::server();
            assert_ne!(other.socket(), server.socket());
        }

        #[test]
        fn a_client_that_never_finishes_its_question_does_not_block_the_next() {
            let (server, asked) = server();
            // Connected, and never shut down its write side.
            let _stuck = UnixStream::connect(server.socket()).unwrap();
            let waiting = run_client(server.socket(), "Password: ", None);

            let (question, answer) = asked
                .recv_timeout(READ_TIMEOUT * 3)
                .expect("the stuck connection gave up");
            assert!(matches!(question, Question::Ask { .. }), "{question:?}");
            answer.send(Some(Secret::new("x"))).unwrap();
            assert_eq!(waiting.join().unwrap().0, ExitCode::SUCCESS);
        }

        #[test]
        fn a_client_that_trickles_its_question_is_cut_off_at_the_deadline() {
            let limits = Limits {
                read_timeout: Duration::from_millis(200),
                deadline: Duration::from_millis(600),
            };
            let (server, asked) = server_with(Some(limits));
            let started = Instant::now();
            // One byte every 100 ms, so no single read ever times out.
            let mut trickle = UnixStream::connect(server.socket()).unwrap();
            let done = Arc::new(AtomicBool::new(false));
            let trickling = {
                let done = Arc::clone(&done);
                std::thread::spawn(move || {
                    while !done.load(Ordering::SeqCst) && trickle.write_all(b"x").is_ok() {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                })
            };
            let waiting = run_client(server.socket(), "Password: ", None);

            let (question, answer) = asked
                .recv_timeout(limits.deadline * 4)
                .expect("the trickling connection was cut off");
            // The trickled text never became a question.
            assert_eq!(
                question,
                Question::Ask {
                    prompt: "Password:".into(),
                    confirm: false
                }
            );
            answer.send(Some(Secret::new("x"))).unwrap();
            assert_eq!(waiting.join().unwrap().0, ExitCode::SUCCESS);
            assert!(started.elapsed() < limits.deadline * 4);
            done.store(true, Ordering::SeqCst);
            trickling.join().unwrap();
        }

        #[test]
        fn a_client_with_no_server_exits_1() {
            let mut out = Vec::new();
            let missing = std::env::temp_dir().join("kika-askpass-no-such-socket");
            let code = client(&missing, OsStr::new("Password: "), None, &mut out);
            assert_eq!(code, ExitCode::FAILURE);
            assert!(out.is_empty());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_never_prints_its_text() {
        let secret = Secret::new("hunter2");
        assert_eq!(format!("{secret:?}"), "Secret(..)");
        assert_eq!(format!("{:?}", Some(secret.clone())), "Some(Secret(..))");
        assert_eq!(secret.expose(), "hunter2");
    }
}
