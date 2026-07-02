//! IPC bridge between the pinentry binary and the DankMaterialShell plugin.
//!
//! On each `GETPIN`/`CONFIRM`/`MESSAGE` the binary creates a Unix-domain socket
//! at a random path under `XDG_RUNTIME_DIR`, fires off `dms ipc call
//! pinentryDms prompt <json>` (detached, non-blocking) and waits for the plugin
//! to connect and write a JSON [`Response`]. Mirrors Pacman99/DankPinentry's
//! `cmd/pinentry-dms/main.go::showModal`.

use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Request sent to the DMS plugin to trigger the modal.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    #[serde(rename = "type")]
    pub kind: String,
    pub socket: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub desc: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub prompt: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    #[serde(rename = "error")]
    pub error_text: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub ok_label: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cancel_label: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub not_ok_label: String,
    #[serde(skip_serializing_if = "eq_int_zero")]
    pub timeout: i32,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub repeat: bool,
}

/// Response the plugin writes back over the socket.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub value: String,
}

fn eq_int_zero(v: &i32) -> bool {
    *v == 0
}

/// Pick a unique socket path under `XDG_RUNTIME_DIR` (falling back to a user
/// tmp dir), with an 8-byte random suffix like the Go reference.
pub fn socket_path() -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| {
        format!("/tmp/dms-pinentry-{}", nix_uid())
    });
    let id = random_hex8();
    PathBuf::from(dir).join(format!("dms-pinentry-{}.sock", id))
}

fn nix_uid() -> u32 {
    // Safety: getuid is always safe and returns the real UID.
    unsafe { getuid() }
}

extern "C" {
    fn getuid() -> u32;
}

fn random_hex8() -> String {
    let mut buf = [0u8; 8];
    if let Ok(mut f) = fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    } else {
        // Fallback: time + pid mixing (non-cryptographic but unique enough).
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(0);
        let pid = std::process::id() as u64;
        let mixed = nanos ^ (pid << 8);
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (mixed >> ((i * 8) % 32)) as u8;
        }
    }
    hex_encode(&buf)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

/// Show the modal by signaling the DMS plugin and awaiting its reply over the
/// freshly created Unix socket. `kind` is `getpin`, `confirm`, or `message`.
pub fn show_modal(kind: &str, state: &crate::assuan::State) -> io::Result<Response> {
    let sock_path = socket_path();

    // Remove any stale socket.
    let _ = fs::remove_file(&sock_path);

    let listener = UnixListener::bind(&sock_path)?;
    // Owner-only permissions.
    let _ = fs::set_permissions(&sock_path, fs::Permissions::from_mode(0o600));

    let result = run_dialog(kind, state, &sock_path, &listener);

    // Always clean up.
    drop(listener);
    let _ = fs::remove_file(&sock_path);

    result
}

fn run_dialog(
    kind: &str,
    state: &crate::assuan::State,
    sock_path: &Path,
    listener: &UnixListener,
) -> io::Result<Response> {
    let req = Request {
        kind: kind.to_string(),
        socket: sock_path.to_string_lossy().into_owned(),
        title: state.title.clone(),
        desc: state.desc.clone(),
        prompt: state.prompt.clone(),
        error_text: state.error.clone(),
        ok_label: state.ok_label.clone(),
        cancel_label: state.cancel_label.clone(),
        not_ok_label: state.not_ok_label.clone(),
        timeout: state.timeout,
        repeat: state.repeat,
    };
    let req_json = serde_json::to_string(&req)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("marshal: {e}")))?;

    // Fire off the IPC command detached: the Go reference does not wait for it.
    let _child = Command::new("dms")
        .args(["ipc", "call", "pinentryDms", "prompt", &req_json])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    accept_and_read(listener, state)
}

/// Accept the plugin's connection (within a buffered timeout) and decode the
/// JSON response. Mirrors the Go accept-deadline + read-deadline logic without
/// pulling in extra crates: we poll the non-blocking listener and stream.
fn accept_and_read(listener: &UnixListener, state: &crate::assuan::State) -> io::Result<Response> {
    const ACCEPT_BUFFER: Duration = Duration::from_secs(10);
    let mut accept_deadline = Duration::from_secs(60);
    if state.timeout > 0 {
        accept_deadline =
            Duration::from_secs(state.timeout as u64).saturating_add(ACCEPT_BUFFER);
    }

    let conn = poll_accept(listener, accept_deadline)?;

    // Read until a newline or the deadline (the plugin writes `<json>\n`).
    poll_read_line(conn, accept_deadline)
}

fn poll_accept(listener: &UnixListener, deadline: Duration) -> io::Result<UnixStream> {
    listener.set_nonblocking(true)?;
    let start = Instant::now();
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                return Ok(stream);
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                if start.elapsed() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "accept: DMS plugin never connected",
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e),
        }
    }
}

fn poll_read_line(mut conn: UnixStream, deadline: Duration) -> io::Result<Response> {
    conn.set_nonblocking(true)?;
    let start = Instant::now();
    let mut buf = Vec::<u8>::with_capacity(256);
    let mut chunk = [0u8; 256];
    loop {
        match conn.read(&mut chunk) {
            Ok(0) => {
                // EOF: parse what we have.
                break;
            }
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.contains(&b'\n') {
                    break;
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                if start.elapsed() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "read: plugin response timed out",
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e),
        }
    }

    // Trim trailing newline/whitespace.
    while matches!(buf.last(), Some(b'\n') | Some(b'\r') | Some(b' ')) {
        buf.pop();
    }

    serde_json::from_slice::<Response>(&buf)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("decode: {e}")))
}