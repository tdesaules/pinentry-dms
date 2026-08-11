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

/// Deadline (in seconds) the binary waits for the DMS plugin to *connect*
/// after firing off the IPC call. Short and distinct from the user-input
/// deadline so that a missing/dead DMS fails fast instead of blocking for the
/// full `SETTIMEOUT`. Overridable via `PINENTRY_DMS_CONNECT_TIMEOUT`.
fn connect_deadline() -> Duration {
    match std::env::var("PINENTRY_DMS_CONNECT_TIMEOUT") {
        Ok(v) => v
            .parse::<u64>()
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(5)),
        Err(_) => Duration::from_secs(5),
    }
}

/// Deadline the binary waits for the plugin to *write* its response after it
/// has connected. Covers the user typing time when `SETTIMEOUT` is set.
fn read_deadline(state: &crate::assuan::State) -> Duration {
    const READ_BUFFER: Duration = Duration::from_secs(10);
    if state.timeout > 0 {
        Duration::from_secs(state.timeout as u64).saturating_add(READ_BUFFER)
    } else {
        Duration::from_secs(60)
    }
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

    // Fire off the IPC command detached: the Go reference does not wait for it.
    let child = Command::new("dms")
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

    accept_and_read(listener, state, debug)
}

/// Accept the plugin's connection within a short connect deadline, then read
/// its JSON response within the (longer) user-input deadline. The two
/// deadlines are separated so a missing/dead DMS fails fast instead of
/// blocking for the full `SETTIMEOUT`.
fn accept_and_read(
    listener: &UnixListener,
    state: &crate::assuan::State,
    debug: bool,
) -> io::Result<Response> {
    let c_deadline = connect_deadline();
    if debug {
        eprintln!(
            "-> waiting for plugin connect (deadline {}s)",
            c_deadline.as_secs()
        );
    }
    let conn = poll_accept(listener, c_deadline)?;

    let r_deadline = read_deadline(state);
    if debug {
        eprintln!(
            "<- plugin connected, waiting for response (deadline {}s)",
            r_deadline.as_secs()
        );
    }
    // Fresh start: the read deadline covers only time spent reading, so the
    // user keeps the full window to type after the plugin has connected.
    poll_read_line(conn, r_deadline, debug)
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
    fn connect_deadline_default_is_5s() {
        // Only assert the default when the env var is not set in the test env.
        if std::env::var("PINENTRY_DMS_CONNECT_TIMEOUT").is_err() {
            assert_eq!(connect_deadline(), Duration::from_secs(5));
        }
    }
}
