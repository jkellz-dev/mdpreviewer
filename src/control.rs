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
use std::path::{Path, PathBuf};

const SOCKET_NAME: &str = "mdpreview.sock";

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
