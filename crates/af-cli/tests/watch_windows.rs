//! Windows counterpart of the SIGTERM assertions in `tests/watch.rs`:
//! `af watch` must stop gracefully (exit 0) when its console process
//! group receives CTRL_BREAK — the closest Windows analogue of the
//! operator's SIGTERM, and one of the events the watch shutdown handler
//! registers for.
#![cfg(windows)]

use std::net::{TcpListener, TcpStream};
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use windows_sys::Win32::System::Console::{GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT};

/// Gives the child its own process group (group id = its pid), so the
/// CTRL_BREAK below reaches exactly it and not this test runner.
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

#[test]
fn watch_exits_gracefully_on_ctrl_break() {
    let dir = tempfile::tempdir().unwrap();
    // An ephemeral debug port doubles as the readiness signal: the watch
    // loop installs its console control handler before it binds any
    // server, so once this port accepts, the handler is in place and the
    // CTRL_BREAK below exercises the graceful path rather than the
    // default hard kill.
    let debug_addr = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().to_string()
    };
    let mut child = Command::new(env!("CARGO_BIN_EXE_af"))
        .args([
            "watch",
            "--no-sidecars",
            "--no-otlp",
            "--debug",
            "--debug-addr",
        ])
        .arg(&debug_addr)
        .env("AF_STATE_DIR", dir.path())
        .creation_flags(CREATE_NEW_PROCESS_GROUP)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn af watch");

    let ready_deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(&debug_addr).is_err() {
        if Instant::now() >= ready_deadline {
            let _ = child.kill();
            panic!("af watch never opened its debug port at {debug_addr}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // SAFETY: plain win32 call; the child was created as its own process
    // group leader above.
    let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()) };
    if sent == 0 {
        // Console control events need a shared console; some CI shells run
        // tests without one, where delivery is impossible rather than the
        // behavior being wrong. Clean up and skip instead of failing.
        let _ = child.kill();
        eprintln!("skipping: no console available for GenerateConsoleCtrlEvent");
        return;
    }

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => {
                assert!(
                    status.success(),
                    "af watch exited {status:?} after CTRL_BREAK instead of shutting down gracefully"
                );
                return;
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("af watch did not exit within 15s of CTRL_BREAK");
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}
