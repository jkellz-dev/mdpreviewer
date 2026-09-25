//! mdpreview: a live-reloading browser preview for Markdown, with mermaid.
//!
//! Open mode, `mdpreview [--line N] [--no-open] <file>`: if a preview server is
//! already running for this user, tell it (over the control socket, see
//! `control.rs`) to show the file and scroll to the line; otherwise start one.
//! A new server binds a local port, the browser opens at that URL, and the
//! page reloads on save. On Unix the server detaches from the launching
//! process (for example Helix's `:sh`) so the caller returns immediately.
//!
//! Sync mode, `mdpreview --sync [--line N] <file>`: only tell a running server
//! to show the file and scroll. It never starts a server, opens a tab, or
//! prints anything, so it is cheap enough to run on every save.

#[cfg(unix)]
mod control;
mod render;
mod server;
#[cfg(all(test, unix))]
mod testutil;
mod watch;

use std::io::IsTerminal;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process;
use std::time::Duration;

const USAGE: &str = "usage: mdpreview [--line N] [--no-open] <file.md>\n       \
                     mdpreview --sync [--line N] <file.md>";

/// Exit the server this long after the last browser tab closes.
const IDLE_GRACE: Duration = Duration::from_secs(15);

/// How long open mode waits for a running server before giving up.
#[cfg(unix)]
const OPEN_TIMEOUT: Duration = Duration::from_secs(1);

/// How long sync mode waits: it runs on every save, so it must stay snappy.
#[cfg(unix)]
const SYNC_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, PartialEq, Eq)]
struct Args {
    file: String,
    line: Option<u32>,
    sync: bool,
    no_open: bool,
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(raw.iter().cloned()) {
        Ok(args) => args,
        // Sync mode runs on every save; it never complains.
        Err(_) if raw.iter().any(|arg| arg == "--sync") => process::exit(0),
        Err(msg) => {
            eprintln!("mdpreview: {msg}\n{USAGE}");
            process::exit(2);
        }
    };
    if args.sync {
        run_sync(&args);
        process::exit(0);
    }
    run_open(&args);
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut file = None;
    let mut line = None;
    let mut sync = false;
    let mut no_open = false;
    while let Some(arg) = args.next() {
        if arg == "--sync" {
            sync = true;
        } else if arg == "--no-open" {
            no_open = true;
        } else if arg == "--line" {
            let value = args.next().ok_or("--line needs a value")?;
            line = Some(
                value
                    .parse()
                    .map_err(|_| format!("invalid line number: {value}"))?,
            );
        } else if arg.starts_with("--") {
            return Err(format!("unknown option: {arg}"));
        } else if file.is_some() {
            return Err(format!("unexpected argument: {arg}"));
        } else {
            file = Some(arg);
        }
    }
    let file = file.ok_or("missing file")?;
    Ok(Args {
        file,
        line,
        sync,
        no_open,
    })
}

/// Sync mode only acts on Markdown files; `C-s` runs it for every buffer.
fn is_markdown(file: &str) -> bool {
    Path::new(file)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
}

/// The URL to open in a browser. `#line=N` tells a freshly opened tab where to
/// scroll, since it was not connected yet when the editor asked.
fn page_url(url: &str, line: Option<u32>) -> String {
    match line {
        Some(line) => format!("{url}#line={line}"),
        None => url.to_owned(),
    }
}

#[cfg(unix)]
fn run_sync(args: &Args) {
    if !is_markdown(&args.file) {
        return;
    }
    let Ok(path) = Path::new(&args.file).canonicalize() else {
        return;
    };
    let socket = control::default_socket_path();
    // Never talk to a socket in a directory another user could control.
    if control::ensure_socket_dir(&socket, control::current_uid()).is_err() {
        return;
    }
    let request = control::Request {
        path,
        line: args.line,
    };
    let _ = control::send_open(&socket, &request, SYNC_TIMEOUT);
}

#[cfg(not(unix))]
fn run_sync(_args: &Args) {}

fn run_open(args: &Args) {
    let file = Path::new(&args.file).canonicalize().unwrap_or_else(|err| {
        eprintln!("mdpreview: cannot open {}: {err}", args.file);
        process::exit(1);
    });

    // Hand the file to a running server if there is one (exits), otherwise
    // claim the control socket for the server we are about to start.
    #[cfg(unix)]
    let control = reuse_or_bind(&file, args);

    // Bind before any fork so the browser can connect via the kernel's accept
    // backlog even before the server thread starts accepting.
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("mdpreview: cannot bind local port: {err}");
            process::exit(1);
        }
    };
    let port = listener.local_addr().map(|addr| addr.port()).unwrap_or(0);
    let url = format!("http://127.0.0.1:{port}/");

    let config = server::Config {
        url: url.clone(),
        idle_grace: Some(IDLE_GRACE),
        #[cfg(unix)]
        control,
    };

    if detach(&url, args) {
        // Only the process that should serve reaches here.
        run_server(listener, file, config);
    }
}

/// Ask a running server to show `file`. If it does, point the browser at it
/// when no tab is listening (or print the URL with `--no-open`) and exit.
/// If no server is running, bind the control socket for a new one; if the
/// socket cannot be used, warn and return `None` for a standalone server.
#[cfg(unix)]
fn reuse_or_bind(file: &Path, args: &Args) -> Option<control::ControlSocket> {
    let socket = control::default_socket_path();
    // Check the directory before connecting: a socket planted by another user
    // could learn our paths and hand back a URL for us to open.
    if let Err(err) = control::ensure_socket_dir(&socket, control::current_uid()) {
        eprintln!(
            "mdpreview: cannot use {} ({err}); running a standalone preview",
            socket.display()
        );
        return None;
    }
    let request = control::Request {
        path: file.to_owned(),
        line: args.line,
    };
    match control::send_open(&socket, &request, OPEN_TIMEOUT) {
        Ok(control::Reply::Ok { url, clients }) => {
            if args.no_open {
                println!("{url}");
            } else if clients == 0 {
                let _ = open::that(page_url(&url, args.line));
            }
            process::exit(0);
        }
        Ok(control::Reply::Err(reason)) => {
            eprintln!("mdpreview: {reason}");
            process::exit(1);
        }
        Err(control::SendError::NoServer) => {}
        Err(err @ control::SendError::Unusable(_)) => {
            eprintln!("mdpreview: {err}; running a standalone preview");
            return None;
        }
        Err(err) => {
            eprintln!("mdpreview: {err}");
            process::exit(1);
        }
    }
    match control::bind(&socket) {
        Ok(control) => Some(control),
        Err(err) => {
            eprintln!(
                "mdpreview: cannot use {} ({err}); running a standalone preview",
                socket.display()
            );
            None
        }
    }
}

/// Detach the server from the launching process.
///
/// On Unix: fork; the parent announces the URL (see [`announce`]) and exits,
/// while the child starts a new session and redirects its standard streams to
/// `/dev/null` (so the launcher's pipe sees EOF). Returns `true` in the process
/// that should run the server.
///
/// On other platforms: announce in-process and run in the foreground.
#[cfg(unix)]
fn detach(url: &str, args: &Args) -> bool {
    match unsafe { libc::fork() } {
        -1 => {
            eprintln!("mdpreview: fork failed");
            process::exit(1);
        }
        0 => {
            detach_child();
            true
        }
        _ => {
            announce(url, args);
            process::exit(0);
        }
    }
}

#[cfg(not(unix))]
fn detach(url: &str, args: &Args) -> bool {
    announce(url, args);
    true
}

/// Open the browser at the preview (scrolled to `--line`) and report the URL.
/// With `--no-open`, only print the URL, even when stdout is not a terminal.
fn announce(url: &str, args: &Args) {
    if args.no_open {
        println!("{url}");
        return;
    }
    let _ = open::that(page_url(url, args.line));
    report_url(url);
}

/// Print the URL only when stdout is a real terminal. When launched from an
/// editor command (for example Helix's `:sh`), stdout is a pipe, so staying
/// silent avoids a captured-output popup; the browser opens regardless.
fn report_url(url: &str) {
    if std::io::stdout().is_terminal() {
        println!("{url}");
    }
}

/// Put the child in its own session and detach its standard streams.
#[cfg(unix)]
fn detach_child() {
    unsafe {
        libc::setsid();
        let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if devnull >= 0 {
            libc::dup2(devnull, libc::STDIN_FILENO);
            libc::dup2(devnull, libc::STDOUT_FILENO);
            libc::dup2(devnull, libc::STDERR_FILENO);
            if devnull > libc::STDERR_FILENO {
                libc::close(devnull);
            }
        }
    }
}

/// Start the HTTP server on `listener` and serve until the process exits.
fn run_server(listener: TcpListener, file: PathBuf, config: server::Config) {
    let server = match tiny_http::Server::from_listener(listener, None) {
        Ok(server) => server,
        Err(err) => {
            eprintln!("mdpreview: cannot start server: {err}");
            process::exit(1);
        }
    };
    server::serve(server, file, config);
}

#[cfg(test)]
mod tests {
    use super::{Args, is_markdown, page_url, parse_args};

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn open_mode_takes_a_file_and_optional_line() {
        assert_eq!(
            parse(&["notes.md"]),
            Ok(Args {
                file: "notes.md".into(),
                line: None,
                sync: false,
                no_open: false
            })
        );
        assert_eq!(
            parse(&["--line", "42", "--no-open", "my notes.md"]),
            Ok(Args {
                file: "my notes.md".into(),
                line: Some(42),
                sync: false,
                no_open: true
            })
        );
    }

    #[test]
    fn sync_mode_is_a_flag() {
        assert_eq!(
            parse(&["--sync", "--line", "7", "a.md"]),
            Ok(Args {
                file: "a.md".into(),
                line: Some(7),
                sync: true,
                no_open: false
            })
        );
    }

    #[test]
    fn bad_arguments_are_rejected() {
        assert!(parse(&[]).is_err());
        assert!(parse(&["--line"]).is_err());
        assert!(parse(&["--line", "x", "a.md"]).is_err());
        assert!(parse(&["--line", "-1", "a.md"]).is_err());
        assert!(parse(&["--bogus", "a.md"]).is_err());
        assert!(parse(&["a.md", "b.md"]).is_err());
    }

    #[test]
    fn only_markdown_files_are_synced() {
        for yes in ["a.md", "docs/README.MD", "notes.markdown", "/abs/x.Md"] {
            assert!(is_markdown(yes), "{yes}");
        }
        for no in ["main.rs", "[scratch]", "foo.md.bak", ".md", "Makefile", ""] {
            assert!(!is_markdown(no), "{no}");
        }
    }

    #[test]
    fn page_url_carries_the_line_as_a_fragment() {
        assert_eq!(
            page_url("http://127.0.0.1:1/", Some(12)),
            "http://127.0.0.1:1/#line=12"
        );
        assert_eq!(page_url("http://127.0.0.1:1/", None), "http://127.0.0.1:1/");
    }
}
