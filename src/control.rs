//! Control channel: a per-user Unix socket through which a new `mdpreview`
//! invocation hands its file and cursor line to an already-running server.
//!
//! One request and one reply per connection, each a single tab-separated line:
//!
//! ```text
//! request:  open\t<absolute path>\t<line or empty>\n
//! reply:    ok\t<url>\t<active SSE clients>\n
//!           err\t<reason>\n
//! ```

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

const SOCKET_NAME: &str = "mdpreview.sock";

/// How long the server waits on a connected client before giving up on it.
const SERVER_TIMEOUT: Duration = Duration::from_secs(1);

/// Upper bound on a request line: a maximal path plus the framing.
const MAX_REQUEST: u64 = 8 * 1024;

/// Show `path` and, if given, scroll to 1-based source `line`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub path: PathBuf,
    pub line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// The server's base URL and how many browser tabs are listening.
    Ok {
        url: String,
        clients: usize,
    },
    Err(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolError(pub &'static str);

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// Encode a request. Paths must be UTF-8 and free of tabs and newlines, which
/// would break the line format.
pub fn format_request(request: &Request) -> Result<String, ProtocolError> {
    let path = request
        .path
        .to_str()
        .ok_or(ProtocolError("path is not valid UTF-8"))?;
    if path.contains(['\t', '\n']) {
        return Err(ProtocolError("path contains a tab or newline"));
    }
    let line = request
        .line
        .map(|line| line.to_string())
        .unwrap_or_default();
    Ok(format!("open\t{path}\t{line}\n"))
}

pub fn parse_request(line: &str) -> Result<Request, ProtocolError> {
    let line = line
        .strip_suffix('\n')
        .ok_or(ProtocolError("missing newline"))?;
    let mut fields = line.split('\t');
    match (fields.next(), fields.next(), fields.next(), fields.next()) {
        (Some("open"), Some(path), Some(line), None) if !path.is_empty() => {
            let line = if line.is_empty() {
                None
            } else {
                Some(line.parse().map_err(|_| ProtocolError("bad line number"))?)
            };
            Ok(Request {
                path: PathBuf::from(path),
                line,
            })
        }
        _ => Err(ProtocolError("expected open<TAB>path<TAB>line")),
    }
}

/// Encode a reply. Tabs and newlines in an error reason become spaces.
pub fn format_reply(reply: &Reply) -> String {
    match reply {
        Reply::Ok { url, clients } => format!("ok\t{url}\t{clients}\n"),
        Reply::Err(reason) => format!("err\t{}\n", reason.replace(['\t', '\n'], " ")),
    }
}

pub fn parse_reply(line: &str) -> Result<Reply, ProtocolError> {
    let line = line
        .strip_suffix('\n')
        .ok_or(ProtocolError("missing newline"))?;
    match line.split_once('\t') {
        Some(("ok", rest)) => {
            let (url, clients) = rest.split_once('\t').ok_or(ProtocolError("bad reply"))?;
            let clients = clients
                .parse()
                .map_err(|_| ProtocolError("bad client count"))?;
            Ok(Reply::Ok {
                url: url.to_owned(),
                clients,
            })
        }
        Some(("err", reason)) => Ok(Reply::Err(reason.to_owned())),
        _ => Err(ProtocolError("bad reply")),
    }
}

/// Where the control socket lives: `$XDG_RUNTIME_DIR/mdpreview.sock` if that is
/// set to an absolute path, else a private per-user directory under `temp_dir`.
pub fn socket_path(xdg_runtime_dir: Option<&OsStr>, temp_dir: &Path, uid: u32) -> PathBuf {
    match xdg_runtime_dir.map(Path::new) {
        Some(dir) if dir.is_absolute() => dir.join(SOCKET_NAME),
        _ => temp_dir.join(format!("mdpreview-{uid}")).join(SOCKET_NAME),
    }
}

/// [`socket_path`] for this process's environment and user.
pub fn default_socket_path() -> PathBuf {
    let xdg = std::env::var_os("XDG_RUNTIME_DIR");
    socket_path(xdg.as_deref(), &std::env::temp_dir(), current_uid())
}

pub fn current_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// A bound control socket, plus what is needed to remove it safely on exit.
pub struct ControlSocket {
    pub listener: UnixListener,
    pub path: PathBuf,
    /// Inode of the socket file at bind time; see [`remove_socket_if_ours`].
    pub inode: u64,
}

#[derive(Debug)]
pub enum SendError {
    /// No socket file, or nothing listening on it (a stale socket).
    NoServer,
    /// A server accepted the connection but did not answer in time.
    Timeout,
    Protocol(ProtocolError),
    Io(io::Error),
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendError::NoServer => f.write_str("no preview server is running"),
            SendError::Timeout => f.write_str("the preview server did not respond"),
            SendError::Protocol(err) => write!(f, "control protocol error: {err}"),
            SendError::Io(err) => write!(f, "control socket error: {err}"),
        }
    }
}

/// Make sure the socket's directory exists and only `uid` can use it, so no
/// other user can plant or intercept the socket. A missing directory is
/// created with mode 0700; an existing one must be a real directory (not a
/// symlink) owned by `uid` with no group or other permissions.
pub fn ensure_socket_dir(socket: &Path, uid: u32) -> io::Result<()> {
    let dir = socket
        .parent()
        .ok_or_else(|| io::Error::other("socket path has no parent directory"))?;
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => return Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err),
    }
    let meta = fs::symlink_metadata(dir)?;
    let problem = if !meta.is_dir() {
        "is not a directory"
    } else if meta.uid() != uid {
        "is owned by another user"
    } else if meta.mode() & 0o077 != 0 {
        "is accessible to other users"
    } else {
        return Ok(());
    };
    Err(io::Error::other(format!("{} {problem}", dir.display())))
}

/// Bind the control socket at `path`, replacing any stale socket file a dead
/// server left behind. Call this only after [`send_open`] found no live server.
pub fn bind(path: &Path) -> io::Result<ControlSocket> {
    ensure_socket_dir(path, current_uid())?;
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let listener = UnixListener::bind(path)?;
    let inode = fs::symlink_metadata(path)?.ino();
    Ok(ControlSocket {
        listener,
        path: path.to_owned(),
        inode,
    })
}

/// Send one `open` request to the server listening on `socket` and wait up to
/// `timeout` (per read or write) for its reply.
pub fn send_open(socket: &Path, request: &Request, timeout: Duration) -> Result<Reply, SendError> {
    let message = format_request(request).map_err(SendError::Protocol)?;
    let stream = UnixStream::connect(socket).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => SendError::NoServer,
        _ => SendError::Io(err),
    })?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(SendError::Io)?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(SendError::Io)?;
    (&stream)
        .write_all(message.as_bytes())
        .map_err(timeout_or_io)?;
    let mut reply = String::new();
    BufReader::new(&stream)
        .read_line(&mut reply)
        .map_err(timeout_or_io)?;
    parse_reply(&reply).map_err(SendError::Protocol)
}

fn timeout_or_io(err: io::Error) -> SendError {
    match err.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => SendError::Timeout,
        _ => SendError::Io(err),
    }
}

/// Answer requests on `listener` with `handle`, one connection at a time on a
/// background thread. Each request is only a mutex update and a channel send,
/// so there is nothing to gain from concurrency; the per-connection timeout
/// keeps one silent client from blocking the next.
pub fn spawn_listener<F>(listener: UnixListener, handle: F)
where
    F: Fn(Request) -> Reply + Send + 'static,
{
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = serve_connection(&stream, &handle);
        }
    });
}

fn serve_connection(stream: &UnixStream, handle: &impl Fn(Request) -> Reply) -> io::Result<()> {
    stream.set_read_timeout(Some(SERVER_TIMEOUT))?;
    stream.set_write_timeout(Some(SERVER_TIMEOUT))?;
    let mut line = String::new();
    BufReader::new(stream.take(MAX_REQUEST)).read_line(&mut line)?;
    let reply = match parse_request(&line) {
        Ok(request) => handle(request),
        Err(err) => Reply::Err(format!("bad request: {err}")),
    };
    let mut writer = stream;
    writer.write_all(format_reply(&reply).as_bytes())
}

/// Remove the socket at `path` if it is still the one bound with `inode`. If a
/// racing server has since replaced it, leave that server's socket alone.
pub fn remove_socket_if_ours(path: &Path, inode: u64) {
    if fs::symlink_metadata(path).is_ok_and(|meta| meta.ino() == inode) {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    use crate::testutil::TestDir;

    const WAIT: Duration = Duration::from_secs(3);

    fn request(path: &str, line: Option<u32>) -> Request {
        Request {
            path: PathBuf::from(path),
            line,
        }
    }

    #[test]
    fn request_with_line_round_trips() {
        let original = request("/home/me/notes/a b.md", Some(42));
        let wire = format_request(&original).unwrap();
        assert_eq!(wire, "open\t/home/me/notes/a b.md\t42\n");
        assert_eq!(parse_request(&wire), Ok(original));
    }

    #[test]
    fn request_without_line_round_trips() {
        let original = request("/tmp/x.md", None);
        let wire = format_request(&original).unwrap();
        assert_eq!(wire, "open\t/tmp/x.md\t\n");
        assert_eq!(parse_request(&wire), Ok(original));
    }

    #[test]
    fn paths_with_tabs_or_newlines_are_rejected() {
        assert!(format_request(&request("/tmp/a\tb.md", None)).is_err());
        assert!(format_request(&request("/tmp/a\nb.md", None)).is_err());
    }

    #[test]
    fn malformed_requests_are_rejected() {
        for wire in [
            "",
            "open\t/tmp/x.md\t1",     // no trailing newline
            "close\t/tmp/x.md\t1\n",  // unknown verb
            "open\t/tmp/x.md\n",      // missing line field
            "open\t\t1\n",            // empty path
            "open\t/tmp/x.md\tabc\n", // non-numeric line
            "open\t/tmp/x.md\t1\textra\n",
        ] {
            assert!(parse_request(wire).is_err(), "accepted {wire:?}");
        }
    }

    #[test]
    fn replies_round_trip() {
        let ok = Reply::Ok {
            url: "http://127.0.0.1:4242/".into(),
            clients: 2,
        };
        assert_eq!(format_reply(&ok), "ok\thttp://127.0.0.1:4242/\t2\n");
        assert_eq!(parse_reply(&format_reply(&ok)), Ok(ok));

        let err = Reply::Err("not a file: /x".into());
        assert_eq!(format_reply(&err), "err\tnot a file: /x\n");
        assert_eq!(parse_reply(&format_reply(&err)), Ok(err));
    }

    #[test]
    fn error_reasons_are_kept_on_one_line() {
        let wire = format_reply(&Reply::Err("a\tb\nc".into()));
        assert_eq!(wire, "err\ta b c\n");
    }

    #[test]
    fn malformed_replies_are_rejected() {
        for wire in [
            "",
            "ok\thttp://x/\t2",
            "ok\thttp://x/\n",
            "ok\thttp://x/\tmany\n",
            "yes\n",
        ] {
            assert!(parse_reply(wire).is_err(), "accepted {wire:?}");
        }
    }

    #[test]
    fn socket_path_prefers_xdg_runtime_dir() {
        let path = socket_path(Some(OsStr::new("/run/user/1000")), Path::new("/tmp"), 1000);
        assert_eq!(path, PathBuf::from("/run/user/1000/mdpreview.sock"));
    }

    #[test]
    fn socket_path_falls_back_to_private_temp_dir() {
        let expected = PathBuf::from("/tmp/mdpreview-1000/mdpreview.sock");
        assert_eq!(socket_path(None, Path::new("/tmp"), 1000), expected);
        assert_eq!(
            socket_path(Some(OsStr::new("")), Path::new("/tmp"), 1000),
            expected
        );
        assert_eq!(
            socket_path(Some(OsStr::new("run/user")), Path::new("/tmp"), 1000),
            expected
        );
    }

    #[test]
    fn ensure_socket_dir_creates_a_private_dir() {
        let dir = TestDir::new("ensure-create");
        let socket = dir.join("sub").join(SOCKET_NAME);
        ensure_socket_dir(&socket, current_uid()).unwrap();
        let mode = fs::metadata(dir.join("sub")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn ensure_socket_dir_accepts_an_existing_private_dir() {
        let dir = TestDir::new("ensure-ok");
        ensure_socket_dir(&dir.join(SOCKET_NAME), current_uid()).unwrap();
    }

    #[test]
    fn ensure_socket_dir_rejects_shared_or_foreign_dirs() {
        let dir = TestDir::new("ensure-reject");

        let shared = dir.join("shared");
        fs::DirBuilder::new().mode(0o755).create(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(ensure_socket_dir(&shared.join(SOCKET_NAME), current_uid()).is_err());

        let link = dir.join("link");
        std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        assert!(ensure_socket_dir(&link.join(SOCKET_NAME), current_uid()).is_err());

        // Owned by someone else, as far as the check can tell.
        assert!(ensure_socket_dir(&dir.join(SOCKET_NAME), current_uid() + 1).is_err());
    }

    #[test]
    fn send_open_round_trips_through_the_listener() {
        let dir = TestDir::new("round-trip");
        let socket = dir.join(SOCKET_NAME);
        let control = bind(&socket).unwrap();
        spawn_listener(control.listener, |request| Reply::Ok {
            url: request.path.display().to_string(),
            clients: request.line.unwrap_or(0) as usize,
        });

        let reply = send_open(&socket, &request("/x/a b.md", Some(4)), WAIT).unwrap();
        assert_eq!(
            reply,
            Reply::Ok {
                url: "/x/a b.md".into(),
                clients: 4
            }
        );
    }

    #[test]
    fn send_open_reports_no_server_without_a_socket() {
        let dir = TestDir::new("no-socket");
        let result = send_open(&dir.join(SOCKET_NAME), &request("/x.md", None), WAIT);
        assert!(matches!(result, Err(SendError::NoServer)), "{result:?}");
    }

    #[test]
    fn stale_sockets_report_no_server_and_are_replaced_by_bind() {
        let dir = TestDir::new("stale");
        let socket = dir.join(SOCKET_NAME);
        drop(bind(&socket).unwrap()); // The file stays behind, like after a crash.
        assert!(socket.exists());

        let result = send_open(&socket, &request("/x.md", None), WAIT);
        assert!(matches!(result, Err(SendError::NoServer)), "{result:?}");
        bind(&socket).expect("bind replaces the stale socket");
    }

    #[test]
    fn send_open_times_out_on_a_silent_server() {
        let dir = TestDir::new("silent-server");
        let socket = dir.join(SOCKET_NAME);
        // Never accepted: the connection sits in the backlog, unanswered.
        let _listener = UnixListener::bind(&socket).unwrap();

        let started = Instant::now();
        let result = send_open(&socket, &request("/x.md", None), Duration::from_millis(200));
        assert!(matches!(result, Err(SendError::Timeout)), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_silent_client_does_not_wedge_the_listener() {
        let dir = TestDir::new("silent-client");
        let socket = dir.join(SOCKET_NAME);
        let control = bind(&socket).unwrap();
        spawn_listener(control.listener, |_| Reply::Ok {
            url: "u".into(),
            clients: 0,
        });

        // Connects and never sends anything.
        let _idle = UnixStream::connect(&socket).unwrap();
        thread::sleep(Duration::from_millis(50));

        let started = Instant::now();
        let reply = send_open(&socket, &request("/x.md", None), WAIT).unwrap();
        assert_eq!(
            reply,
            Reply::Ok {
                url: "u".into(),
                clients: 0
            }
        );
        // The listener gives up on the idle client after its 1 s timeout.
        assert!(started.elapsed() < Duration::from_millis(2500));
    }

    #[test]
    fn malformed_requests_get_an_error_reply() {
        let dir = TestDir::new("malformed");
        let socket = dir.join(SOCKET_NAME);
        let control = bind(&socket).unwrap();
        spawn_listener(control.listener, |_| unreachable!("handler must not run"));

        let mut stream = UnixStream::connect(&socket).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        stream.write_all(b"hello\n").unwrap();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).unwrap();
        assert!(reply.starts_with("err\tbad request"), "{reply:?}");
    }

    #[test]
    fn remove_socket_if_ours_spares_a_replacement() {
        let dir = TestDir::new("cleanup");
        let socket = dir.join(SOCKET_NAME);
        let ours = bind(&socket).unwrap();

        // Another server took over the path: a different file (and inode) now
        // sits there. Creating it while ours still exists guarantees a new inode.
        let theirs = dir.join("theirs");
        fs::write(&theirs, "").unwrap();
        let their_inode = fs::metadata(&theirs).unwrap().ino();
        fs::rename(&theirs, &socket).unwrap();

        remove_socket_if_ours(&socket, ours.inode);
        assert!(socket.exists(), "removed another server's socket");

        remove_socket_if_ours(&socket, their_inode);
        assert!(!socket.exists());
    }
}
