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
    pub key_info: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub repeat_error: String,
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
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub grab: bool,
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
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/tmp/dms-pinentry-{}", nix_uid()));
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

/// Zeroize a `String`'s backing buffer in place. Best-effort: overwrites the
/// heap bytes with zeros before the string is dropped, so a passphrase does
/// not linger in freed memory. Used for the GETPIN response value.
pub fn zeroize_string(s: &mut String) {
    // Safety: we have exclusive (`&mut`) access to the String, and
    // `as_bytes_mut` gives a mutable view over its heap-allocated buffer.
    // We overwrite every byte with 0 before the String is dropped/consumed.
    unsafe {
        let bytes = s.as_bytes_mut();
        for b in bytes.iter_mut() {
            *b = 0;
        }
    }
    s.clear();
}

/// Deadline the binary waits for the plugin to connect AND write its
/// response. The plugin only connects its socket AFTER the user has typed
/// (it connects in `sendResponse`, right before writing the JSON), so this
/// deadline must cover the user's typing time, not just the IPC roundtrip.
/// It must also STRICTLY exceed the modal's own timeout (`SETTIMEOUT` or the
/// 60s QML default): when the modal times out, the plugin connects to report
/// "timeout" — if both sides used the same 60s, that response would lose the
/// race and the client would get a generic "DMS plugin never connected"
/// instead of the canonical Timeout error.
fn dialog_deadline(state: &crate::assuan::State) -> Duration {
    const BUFFER: Duration = Duration::from_secs(10);
    // Must mirror the QML modal's fallback `Timer` interval (60s).
    const MODAL_DEFAULT: Duration = Duration::from_secs(60);
    let modal = match state.timeout {
        t if t > 0 => Duration::from_secs(t as u64),
        _ => MODAL_DEFAULT,
    };
    modal.saturating_add(BUFFER)
}

/// Show the modal by signaling the DMS plugin and awaiting its reply over the
/// freshly created Unix socket. `kind` is `getpin`, `confirm`, or `message`.
/// `debug` enables stderr tracing of the IPC exchange.
pub fn show_modal(kind: &str, state: &crate::assuan::State, debug: bool) -> io::Result<Response> {
    let sock_path = socket_path();

    // Remove any stale socket.
    let _ = fs::remove_file(&sock_path);

    let listener = UnixListener::bind(&sock_path)?;
    // Owner-only permissions.
    let _ = fs::set_permissions(&sock_path, fs::Permissions::from_mode(0o600));

    let result = run_dialog(kind, state, &sock_path, &listener, debug);

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
    debug: bool,
) -> io::Result<Response> {
    let req = Request {
        kind: kind.to_string(),
        socket: sock_path.to_string_lossy().into_owned(),
        title: state.title.clone(),
        desc: state.desc.clone(),
        prompt: state.prompt.clone(),
        error_text: state.error.clone(),
        key_info: state.key_info.clone(),
        repeat_error: state.repeat_error.clone(),
        ok_label: state.ok_label.clone(),
        cancel_label: state.cancel_label.clone(),
        not_ok_label: state.not_ok_label.clone(),
        timeout: state.timeout,
        repeat: state.repeat,
        grab: state.grab,
    };
    let req_json =
        serde_json::to_string(&req).map_err(|e| io::Error::other(format!("marshal: {e}")))?;

    // Fire off the IPC command. We keep the child handle so we can detect a
    // fast failure: `dms ipc call` exits non-zero quickly if DMS is down or
    // the handler is missing, which lets us fail fast instead of blocking for
    // the full user-input deadline. If the child is alive or exited 0 (IPC was
    // received, the plugin is showing a modal), we keep waiting for the user.
    let mut child = Command::new("dms")
        .args(["ipc", "call", "pinentryDms", "prompt", &req_json])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    if debug {
        eprintln!(
            "-> dms ipc call pinentryDms prompt (child pid {}, socket {})",
            child.id(),
            sock_path.display()
        );
    }

    let deadline = dialog_deadline(state);
    if debug {
        eprintln!(
            "-> waiting for plugin response (deadline {}s)",
            deadline.as_secs()
        );
    }

    accept_and_read(listener, &mut child, deadline, debug)
}

/// Accept the plugin's connection and read its JSON response within a single
/// deadline that covers the user's typing time (the plugin connects only
/// AFTER the user types, in `sendResponse`). While waiting, we poll the `dms`
/// IPC child: if it exits non-zero, DMS is unreachable / the handler is gone,
/// so we fail fast instead of blocking for the full deadline.
fn accept_and_read(
    listener: &UnixListener,
    child: &mut std::process::Child,
    deadline: Duration,
    debug: bool,
) -> io::Result<Response> {
    let conn = poll_accept(listener, child, deadline, debug)?;
    poll_read_line(conn, deadline, debug)
}

fn poll_accept(
    listener: &UnixListener,
    child: &mut std::process::Child,
    deadline: Duration,
    debug: bool,
) -> io::Result<UnixStream> {
    listener.set_nonblocking(true)?;
    let start = Instant::now();
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                return Ok(stream);
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                // Fast-fail: if the `dms ipc call` child has already exited
                // with a non-zero status, DMS is unreachable or the handler is
                // missing — no modal will ever appear. Abort now rather than
                // blocking for the full user-input deadline.
                if let Ok(Some(status)) = child.try_wait() {
                    if !status.success() {
                        if debug {
                            eprintln!("<- dms ipc call child exited {status} (DMS unreachable)");
                        }
                        return Err(io::Error::new(
                            io::ErrorKind::ConnectionRefused,
                            "accept: dms ipc call failed (DMS unreachable or handler missing)",
                        ));
                    }
                    // Child exited 0: the IPC was received and the plugin is
                    // showing a modal. Keep waiting for the user to type and
                    // the plugin to connect back with the response.
                }
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

fn poll_read_line(mut conn: UnixStream, deadline: Duration, debug: bool) -> io::Result<Response> {
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

    if debug {
        eprintln!(
            "<- plugin response in {}ms ({} bytes)",
            start.elapsed().as_millis(),
            buf.len()
        );
    }

    // Trim trailing newline/whitespace.
    while matches!(buf.last(), Some(b'\n') | Some(b'\r') | Some(b' ')) {
        buf.pop();
    }

    serde_json::from_slice::<Response>(&buf).map_err(|e| io::Error::other(format!("decode: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_omits_empty_optional_fields() {
        let req = Request {
            kind: "getpin".into(),
            socket: "/tmp/x.sock".into(),
            title: String::new(),
            desc: String::new(),
            prompt: String::new(),
            error_text: String::new(),
            key_info: String::new(),
            repeat_error: String::new(),
            ok_label: String::new(),
            cancel_label: String::new(),
            not_ok_label: String::new(),
            timeout: 0,
            repeat: false,
            grab: false,
        };
        let json = serde_json::to_string(&req).unwrap();
        // Required fields always present.
        assert!(json.contains("\"type\":\"getpin\""));
        assert!(json.contains("\"socket\":\"/tmp/x.sock\""));
        // Empty/zero/false optional fields are skipped.
        assert!(!json.contains("title"));
        assert!(!json.contains("keyInfo"));
        assert!(!json.contains("repeatError"));
        assert!(!json.contains("grab"));
        assert!(!json.contains("timeout"));
    }

    #[test]
    fn request_serializes_camel_case_with_keyinfo_and_repeaterror() {
        let req = Request {
            kind: "getpin".into(),
            socket: "/tmp/y.sock".into(),
            title: "T".into(),
            desc: String::new(),
            prompt: String::new(),
            error_text: "err".into(),
            key_info: "gopass/age-identities".into(),
            repeat_error: "Passphrases do not match".into(),
            ok_label: String::new(),
            cancel_label: "Cancel".into(),
            not_ok_label: String::new(),
            timeout: 30,
            repeat: true,
            grab: true,
        };
        let json = serde_json::to_string(&req).unwrap();
        // camelCase + rename rules.
        assert!(json.contains("\"error\":\"err\""));
        assert!(json.contains("\"keyInfo\":\"gopass/age-identities\""));
        assert!(json.contains("\"repeatError\":\"Passphrases do not match\""));
        assert!(json.contains("\"cancelLabel\":\"Cancel\""));
        assert!(json.contains("\"timeout\":30"));
        assert!(json.contains("\"repeat\":true"));
        assert!(json.contains("\"grab\":true"));
    }

    #[test]
    fn response_decodes_pin_with_value() {
        let json = b"{\"type\":\"pin\",\"value\":\"hunter2\"}\n";
        let resp: Response = serde_json::from_slice(json).unwrap();
        assert_eq!(resp.kind, "pin");
        assert_eq!(resp.value, "hunter2");
    }

    #[test]
    fn response_decodes_cancel_without_value() {
        let json = b"{\"type\":\"cancel\"}";
        let resp: Response = serde_json::from_slice(json).unwrap();
        assert_eq!(resp.kind, "cancel");
        assert_eq!(resp.value, ""); // default
    }

    #[test]
    fn zeroize_string_clears_buffer() {
        let mut s = String::from("s3cr3t");
        zeroize_string(&mut s);
        assert!(s.is_empty());
    }

    #[test]
    fn dialog_deadline_exceeds_modal_default_when_no_timeout() {
        // No SETTIMEOUT: modal defaults to 60s, IPC deadline must be 60s+buffer
        // so the plugin's "timeout" response wins the race.
        let state = crate::assuan::State::default();
        assert_eq!(dialog_deadline(&state), Duration::from_secs(70));
    }

    #[test]
    fn dialog_deadline_uses_settimeout_plus_buffer() {
        let state = crate::assuan::State {
            timeout: 30,
            ..Default::default()
        };
        assert_eq!(dialog_deadline(&state), Duration::from_secs(40));
    }
}
