//! mdpreview: a live-reloading browser preview for Markdown, with mermaid.
//!
//! Invoked as `mdpreview <file.md>`. It binds a local port, opens the default
//! browser at that URL, and serves a preview that reloads on save. On Unix it
//! detaches from the launching process (for example Helix's `:sh`) so the
//! caller returns immediately while the server keeps running.

#[cfg(unix)]
mod control;
mod render;
mod server;
#[cfg(all(test, unix))]
mod testutil;
mod watch;

use std::io::IsTerminal;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process;
use std::sync::mpsc::channel;

fn main() {
    let file = match parse_target() {
        Ok(file) => file,
        Err(code) => process::exit(code),
    };

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

    if detach(&url) {
        // Parent process has handled the browser and exited; only the detached
        // child reaches here.
        run_server(listener, file);
    }
}

/// Resolve the target file from the first CLI argument. Returns an exit code on
/// error.
fn parse_target() -> Result<PathBuf, i32> {
    let arg = std::env::args().nth(1).ok_or_else(|| {
        eprintln!("usage: mdpreview <file.md>");
        2
    })?;
    PathBuf::from(&arg).canonicalize().map_err(|err| {
        eprintln!("mdpreview: cannot open {arg}: {err}");
        1
    })
}

/// Detach the server from the launching process.
///
/// On Unix: fork; the parent opens the browser, prints the URL, and exits, while
/// the child starts a new session and redirects its standard streams to
/// `/dev/null` (so the launcher's pipe sees EOF). Returns `true` in the process
/// that should run the server.
///
/// On other platforms: open the browser in-process and run in the foreground.
#[cfg(unix)]
fn detach(url: &str) -> bool {
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
            let _ = open::that(url);
            report_url(url);
            process::exit(0);
        }
    }
}

#[cfg(not(unix))]
fn detach(url: &str) -> bool {
    let _ = open::that(url);
    report_url(url);
    true
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

/// Wire the watcher to the server and serve until the process exits.
fn run_server(listener: TcpListener, file: PathBuf) {
    let (reload_tx, reload_rx) = channel::<server::Event>();

    // Keep the watcher alive for the lifetime of the process by holding it until
    // `serve` returns (which it does only at shutdown).
    let _watcher = match watch::watch_file(&file, reload_tx) {
        Ok(watcher) => Some(watcher),
        Err(err) => {
            eprintln!("mdpreview: file watch failed, live reload disabled: {err}");
            None
        }
    };

    let server = match tiny_http::Server::from_listener(listener, None) {
        Ok(server) => server,
        Err(err) => {
            eprintln!("mdpreview: cannot start server: {err}");
            process::exit(1);
        }
    };

    server::serve(server, file, reload_rx);
}
