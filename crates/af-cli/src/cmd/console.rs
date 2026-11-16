//! `af console`: the debug console for the collector that is already
//! running.
//!
//! The collector (`af watch`, normally the per-user service) serves its
//! `/debug` API and the console's pages on an owner-only Unix socket in
//! the state directory, and builds what they show only while someone is
//! attached. This command is that someone: it holds one stream open for as
//! long as it runs, and relays a loopback TCP port to the socket so a
//! browser can reach it. It starts no collector and stores nothing.

use std::path::Path;

use anyhow::Result;

use super::debug_server::SOCKET_NAME;

/// `af console` arguments.
#[derive(clap::Args, Debug)]
pub struct Args {
    /// Address the console is served on. Loopback only (127.0.0.1, ::1).
    #[arg(long, default_value = super::watch::DEFAULT_DEBUG_ADDR)]
    pub addr: String,
}

/// Serves the console until interrupted.
pub fn run(state_dir: &Path, args: Args) -> Result<i32> {
    let addr = super::watch::loopback_addr("--addr", &args.addr)?;
    let socket = state_dir.join(SOCKET_NAME);
    imp::run(&socket, addr)
}

#[cfg(not(unix))]
mod imp {
    use std::net::SocketAddr;
    use std::path::Path;

    use anyhow::{bail, Result};

    pub fn run(_socket: &Path, _addr: SocketAddr) -> Result<i32> {
        bail!("`af console` needs Unix sockets; on this platform run the collector with `af watch --debug`")
    }
}

#[cfg(unix)]
mod imp {
    use std::io::{Read, Write};
    use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::{bail, Context, Result};

    /// Connections relayed at once. A page holds one stream and makes a
    /// few fetches; this is room for several tabs, and a bound on what a
    /// local process opening connections in a loop can take.
    const MAX_RELAYS: usize = 64;

    /// How long the presence stream waits before trying a collector that
    /// went away again.
    const PRESENCE_RETRY: Duration = Duration::from_secs(1);

    /// How long the presence stream may hear nothing before it is taken
    /// for dead and opened again. The collector pings every 15 s.
    const PRESENCE_SILENCE: Duration = Duration::from_secs(45);

    /// The collector notices an attachment on its next loop, at most this
    /// long after it happens.
    const ATTACH_SETTLE: Duration = Duration::from_millis(1200);

    pub fn run(socket: &Path, addr: SocketAddr) -> Result<i32> {
        if UnixStream::connect(socket).is_err() {
            bail!(
                "no collector is running (nothing answers on {}); start one with `af service install`, or `af watch` in a terminal",
                socket.display()
            );
        }
        let listener = TcpListener::bind(addr).with_context(|| {
            format!(
                "cannot serve the console on {addr} (another `af console` or `af watch --debug`?)"
            )
        })?;
        hold_presence(socket.to_path_buf(), PRESENCE_SILENCE);
        // So the first page load finds the collector already describing
        // itself, not about to.
        std::thread::sleep(ATTACH_SETTLE);
        eprintln!(
            "af console: http://{}/ (Ctrl-C to close; the collector keeps running)",
            listener.local_addr().unwrap_or(addr)
        );
        relay(listener, socket.to_path_buf());
        Ok(0)
    }

    /// Keeps one stream open on the collector for the life of this
    /// process, which is what tells it a console is attached. A collector
    /// that restarts, or goes silent for `silence`, is attached to again.
    pub(super) fn hold_presence(socket: PathBuf, silence: Duration) {
        std::thread::spawn(move || loop {
            if let Ok(mut stream) = UnixStream::connect(&socket) {
                let request = b"GET /debug/stream HTTP/1.1\r\nHost: localhost\r\n\r\n";
                let armed = stream.set_read_timeout(Some(silence)).is_ok();
                if armed && stream.write_all(request).is_ok() {
                    // Read and dropped: frames must be drained or the
                    // collector would disconnect a subscriber that lags.
                    let mut sink = [0u8; 16 * 1024];
                    while matches!(stream.read(&mut sink), Ok(n) if n > 0) {}
                }
            }
            std::thread::sleep(PRESENCE_RETRY);
        });
    }

    /// Relays every connection accepted on `listener` to the collector's
    /// socket, byte for byte. Returns when the listener fails.
    pub(super) fn relay(listener: TcpListener, socket: PathBuf) {
        let active = Arc::new(AtomicUsize::new(0));
        for client in listener.incoming() {
            let Ok(client) = client else {
                continue;
            };
            if active.fetch_add(1, Ordering::AcqRel) >= MAX_RELAYS {
                active.fetch_sub(1, Ordering::AcqRel);
                refuse(client, "too_many_connections");
                continue;
            }
            let socket = socket.clone();
            let active = Arc::clone(&active);
            std::thread::spawn(move || {
                bridge(client, &socket);
                active.fetch_sub(1, Ordering::AcqRel);
            });
        }
    }

    /// One connection: both directions until either side ends, then both
    /// sockets are closed, so the collector sees a closed tab at once and
    /// a browser sees a collector that went away.
    fn bridge(client: TcpStream, socket: &Path) {
        let Ok(collector) = UnixStream::connect(socket) else {
            refuse(client, "collector_not_running");
            return;
        };
        let (Ok(mut client_read), Ok(mut collector_write)) =
            (client.try_clone(), collector.try_clone())
        else {
            return;
        };
        let (mut client_write, mut collector_read) = (client, collector);
        let upstream = std::thread::spawn(move || {
            let _ = std::io::copy(&mut client_read, &mut collector_write);
            let _ = collector_write.shutdown(Shutdown::Both);
            let _ = client_read.shutdown(Shutdown::Both);
        });
        let _ = std::io::copy(&mut collector_read, &mut client_write);
        let _ = client_write.shutdown(Shutdown::Both);
        let _ = collector_read.shutdown(Shutdown::Both);
        let _ = upstream.join();
    }

    /// Answers `503` with a reason the console's client can read.
    fn refuse(mut client: TcpStream, reason: &str) {
        let body = format!(r#"{{"error":"{reason}"}}"#);
        let response = format!(
            "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = client.set_write_timeout(Some(Duration::from_secs(2)));
        let _ = client.set_read_timeout(Some(Duration::from_millis(200)));
        // The request is read before answering: closing a socket with
        // unread bytes resets it, and the answer is lost with them.
        let mut request = [0u8; 8192];
        let _ = client.read(&mut request);
        let _ = client.write_all(response.as_bytes());
        let _ = client.shutdown(Shutdown::Write);
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::cmd::debug_server::{serve_unix, DebugState};
        use serde_json::json;
        use std::sync::Mutex;

        fn get(addr: SocketAddr, path: &str) -> String {
            let mut stream = TcpStream::connect(addr).expect("connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("timeout");
            write!(
                stream,
                "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
            )
            .expect("write");
            let mut response = Vec::new();
            let _ = stream.read_to_end(&mut response);
            String::from_utf8_lossy(&response).into_owned()
        }

        fn relay_to(socket: &Path) -> SocketAddr {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            let socket = socket.to_path_buf();
            std::thread::spawn(move || relay(listener, socket));
            addr
        }

        #[test]
        fn a_request_on_the_port_is_answered_by_the_collector_behind_the_socket() {
            let dir = tempfile::tempdir().expect("tempdir");
            let socket = dir.path().join("console.sock");
            let state = Arc::new(Mutex::new(DebugState::default()));
            state.lock().unwrap().set_status(json!({"boot_id": "b7"}));
            let _server = serve_unix(&socket, Arc::clone(&state)).expect("serve");

            let response = get(relay_to(&socket), "/debug/status");
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
            assert!(response.contains(r#""boot_id":"b7""#), "{response}");
        }

        #[test]
        fn with_no_collector_the_port_says_so() {
            let dir = tempfile::tempdir().expect("tempdir");
            let response = get(relay_to(&dir.path().join("console.sock")), "/debug/status");
            assert!(response.starts_with("HTTP/1.1 503"), "{response}");
            assert!(response.contains("collector_not_running"), "{response}");
        }

        #[test]
        fn presence_attaches_and_reattaches_to_a_collector_that_restarted() {
            let dir = tempfile::tempdir().expect("tempdir");
            let socket = dir.path().join("console.sock");
            let attached = |state: &Arc<Mutex<DebugState>>| {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while state.lock().unwrap().attached() == 0 && std::time::Instant::now() < deadline
                {
                    std::thread::sleep(Duration::from_millis(20));
                }
                state.lock().unwrap().attached()
            };

            let first = Arc::new(Mutex::new(DebugState::default()));
            let server = serve_unix(&socket, Arc::clone(&first)).expect("serve");
            // In one process a dropped server's connection lingers open and
            // silent, where a collector that exited would close it: the
            // silence limit is what notices either.
            hold_presence(socket.clone(), Duration::from_millis(300));
            assert!(attached(&first) >= 1);

            drop(server);
            let second = Arc::new(Mutex::new(DebugState::default()));
            let _server = serve_unix(&socket, Arc::clone(&second)).expect("serve again");
            assert!(attached(&second) >= 1, "the new collector is attached to");
        }

        #[test]
        fn run_refuses_when_no_collector_is_running() {
            let dir = tempfile::tempdir().expect("tempdir");
            let error = run(
                &dir.path().join("console.sock"),
                "127.0.0.1:0".parse().unwrap(),
            )
            .expect_err("nothing to attach to");
            assert!(
                format!("{error:#}").contains("af service install"),
                "{error:#}"
            );
        }
    }
}
