//! Assuan protocol implementation for the pinentry side.
//!
//! Ported from Pacman99/DankPinentry `internal/assuan/assuan.go`. A pinentry
//! process reads Assuan commands on stdin and writes responses on stdout. This
//! crate only implements the subset a pinentry needs: a line reader, an OK/D/ERR
//! writer, the accumulated dialog [`State`], and the canonical error codes.
//!
//! Reference: `libgpg-error` GPG_ERR_* values and the Assuan protocol RFC

use std::io::{self, BufRead, Write};

const MAX_LINE_LEN: usize = 1000;

/// Accumulated pinentry dialog state populated from Assuan SET* / OPTION
/// commands and consumed by GETPIN / CONFIRM / MESSAGE.
#[derive(Clone, Debug, Default)]
pub struct State {
    pub title: String,
    pub desc: String,
    pub prompt: String,
    pub error: String,
    pub ok_label: String,
    pub cancel_label: String,
    pub not_ok_label: String,
    pub key_info: String,
    pub timeout: i32,
    pub repeat: bool,
    pub repeat_error: String,

    // OPTION values (mostly informational for a GUI pinentry)
    pub grab: bool,
    pub tty_name: String,
    pub tty_type: String,
    pub lc_ctype: String,
    pub display: String,
}

impl State {
    /// Apply a SET*/OPTION command to the state. Returns true when `cmd` was
    /// recognized and handled.
    pub fn apply_command(&mut self, cmd: &Command) -> bool {
        let param = percent_decode(&cmd.param);
        match cmd.name.as_str() {
            "SETTITLE" => self.title = param,
            "SETDESC" => self.desc = param,
            "SETPROMPT" => self.prompt = param,
            "SETERROR" => self.error = param,
            "SETOK" => self.ok_label = param,
            "SETCANCEL" => self.cancel_label = param,
            "SETNOTOK" => self.not_ok_label = param,
            "SETKEYINFO" => self.key_info = param,
            "SETTIMEOUT" => {
                self.timeout = param.trim().parse().unwrap_or(0);
            }
            "SETREPEAT" => {
                self.repeat = true;
                self.repeat_error = param;
            }
            "OPTION" => self.apply_option(&param),
            _ => return false,
        }
        true
    }

    fn apply_option(&mut self, param: &str) {
        let (key, val) = match param.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (param, None),
        };
        let val = val.unwrap_or("");
        match key.to_ascii_lowercase().as_str() {
            "grab" => self.grab = true,
            "no-grab" => self.grab = false,
            "ttyname" => self.tty_name = val.to_string(),
            "ttytype" => self.tty_type = val.to_string(),
            "lc-ctype" => self.lc_ctype = val.to_string(),
            "display" => self.display = val.to_string(),
            _ => {}
        }
    }

    /// Clear transient state (the error banner) after a PIN attempt.
    pub fn reset(&mut self) {
        self.error.clear();
    }
}

/// A parsed Assuan command: a name and its single space-separated parameter.
#[derive(Clone, Debug)]
pub struct Command {
    pub name: String,
    pub param: String,
}

/// Buffered reader for Assuan command lines.
pub struct Reader<R: BufRead> {
    inner: R,
    line: Vec<u8>,
}

impl<R: BufRead> Reader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            line: Vec::with_capacity(MAX_LINE_LEN),
        }
    }

    /// Read the next command. Returns `Ok(None)` on clean EOF.
    pub fn read_command(&mut self) -> io::Result<Option<Command>> {
        self.line.clear();
        let n = self.inner.read_until(b'\n', &mut self.line)?;
        if n == 0 {
            return Ok(None);
        }
        // Trim trailing newline (and optional carriage return).
        while matches!(self.line.last(), Some(b'\n') | Some(b'\r')) {
            self.line.pop();
        }
        if self.line.len() > MAX_LINE_LEN {
            self.line.truncate(MAX_LINE_LEN);
        }
        let text = String::from_utf8_lossy(&self.line);
        let (name, param) = text
            .split_once(' ')
            .map(|(n, p)| (n.to_string(), p.to_string()))
            .unwrap_or_else(|| (text.to_string(), String::new()));
        Ok(Some(Command {
            name: name.to_ascii_uppercase(),
            param,
        }))
    }
}

/// Writer for Assuan responses (`OK`, `D`, `ERR`, `#`).
pub struct Writer<W: Write> {
    inner: W,
}

impl<W: Write> Writer<W> {
    pub fn new(inner: W) -> Self {
        Self { inner }
    }

    /// `OK [msg]`
    pub fn ok(&mut self, msg: &str) -> io::Result<()> {
        if msg.is_empty() {
            writeln!(self.inner, "OK")
        } else {
            writeln!(self.inner, "OK {}", msg)
        }
    }

    /// `D <percent-encoded-data>`
    pub fn data(&mut self, data: &str) -> io::Result<()> {
        writeln!(self.inner, "D {}", percent_encode(data))
    }

    /// Comment line `# msg`.
    #[allow(dead_code)]
    pub fn comment(&mut self, msg: &str) -> io::Result<()> {
        writeln!(self.inner, "# {}", msg)
    }

    /// `ERR <num> <msg> <Source>`, matching libassuan's canonical pinentry.
    pub fn err(&mut self, e: &Error) -> io::Result<()> {
        match e.source.name() {
            Some(src) => writeln!(self.inner, "ERR {} {} <{}>", e.wire(), e.message, src),
            None => writeln!(self.inner, "ERR {} {}", e.wire(), e.message),
        }
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// libgpg-error error source (only the ones we emit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Source {
    Unspecified,
    Pinentry,
}

impl Source {
    fn name(self) -> Option<&'static str> {
        match self {
            Source::Pinentry => Some("Pinentry"),
            Source::Unspecified => None,
        }
    }
}

/// A GPG error: code/source (libgpg-error fields) plus human message.
#[derive(Clone, Debug)]
pub struct Error {
    pub code: u16,
    pub source: Source,
    pub message: String,
}

impl Error {
    /// Build a dynamic error.
    pub fn new(code: u16, source: Source, message: impl Into<String>) -> Self {
        Self {
            code,
            source,
            message: message.into(),
        }
    }

    /// Returns a copy with `message` replaced.
    pub fn with_message(&self, msg: impl Into<String>) -> Self {
        Self {
            code: self.code,
            source: self.source,
            message: msg.into(),
        }
    }

    /// Packed integer libgpg-error puts on the wire: `(source << 24) | code`.
    fn wire(&self) -> u32 {
        let src = match self.source {
            Source::Unspecified => 0,
            Source::Pinentry => 5,
        };
        ((src as u32) << 24) | u32::from(self.code)
    }

    // Canonical pinentry errors.
    pub fn timeout() -> Self {
        Self::new(62, Source::Pinentry, "Timeout")
    }
    pub fn canceled() -> Self {
        Self::new(99, Source::Pinentry, "Operation cancelled")
    }
    pub fn not_confirmed() -> Self {
        Self::new(114, Source::Pinentry, "Operation not confirmed")
    }
    pub fn general() -> Self {
        Self::new(49, Source::Pinentry, "General error")
    }
}

/// Decodes Assuan percent-encoded strings, including `+` as a literal `+`
/// (not a space, unlike url query encoding). Mirrors the Go reference.
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' {
            if let Some(v) = decode_hex(bytes, i + 1) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn decode_hex(bytes: &[u8], pos: usize) -> Option<u8> {
    if pos + 1 >= bytes.len() {
        return None;
    }
    let hi = hex_digit(bytes[pos])?;
    let lo = hex_digit(bytes[pos + 1])?;
    Some(hi * 16 + lo)
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Percent-encodes only `%`, `\n` and `\r` for Assuan `D` payloads, matching
/// the reference Go implementation.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        match c {
            '%' => out.push_str("%25"),
            '\n' => out.push_str("%0A"),
            '\r' => out.push_str("%0D"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encode_escapes_only_percent_and_newlines() {
        assert_eq!(percent_encode("hello"), "hello");
        assert_eq!(percent_encode("100%"), "100%25");
        assert_eq!(percent_encode("a\nb"), "a%0Ab");
        assert_eq!(percent_encode("a\rb"), "a%0Db");
        // `+` and space are literal (NOT url-query encoding).
        assert_eq!(percent_encode("a+b c"), "a+b c");
    }

    #[test]
    fn percent_decode_handles_hex_and_plus_literal() {
        assert_eq!(percent_decode("hello"), "hello");
        assert_eq!(percent_decode("100%25"), "100%");
        assert_eq!(percent_decode("a%0Ab"), "a\nb");
        assert_eq!(percent_decode("a%0Db"), "a\rb");
        // `+` is literal, NOT a space.
        assert_eq!(percent_decode("a+b"), "a+b");
        // Lower and upper hex both accepted.
        assert_eq!(percent_decode("%0a%0A"), "\n\n");
        assert_eq!(percent_decode("%0D%0d"), "\r\r");
    }

    #[test]
    fn percent_decode_roundtrips_with_encode() {
        for s in ["hello", "100%", "a\nb\rc", "espèce café %+"] {
            assert_eq!(percent_decode(&percent_encode(s)), s);
        }
    }

    #[test]
    fn percent_decode_truncated_hex_is_literal() {
        // A `%` not followed by two hex digits is left as a literal `%`.
        assert_eq!(percent_decode("%2"), "%2");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%"), "%");
    }

    #[test]
    fn error_wire_packs_source_and_code() {
        // (source << 24) | code. Pinentry source = 5.
        let e = Error::canceled();
        assert_eq!(e.code, 99);
        assert_eq!(e.wire(), (5u32 << 24) | 99);

        let e = Error::timeout();
        assert_eq!(e.code, 62);
        assert_eq!(e.wire(), (5u32 << 24) | 62);

        let e = Error::not_confirmed();
        assert_eq!(e.code, 114);
        assert_eq!(e.wire(), (5u32 << 24) | 114);

        let e = Error::general();
        assert_eq!(e.code, 49);
        assert_eq!(e.wire(), (5u32 << 24) | 49);
    }

    #[test]
    fn error_with_message_replaces_only_message() {
        let e = Error::general().with_message("boom");
        assert_eq!(e.code, 49);
        assert_eq!(e.message, "boom");
    }

    #[test]
    fn source_name_pinentry_is_some_unspecified_none() {
        assert_eq!(Source::Pinentry.name(), Some("Pinentry"));
        assert_eq!(Source::Unspecified.name(), None);
    }

    #[test]
    fn apply_command_recognizes_set_commands() {
        let mut s = State::default();
        assert!(s.apply_command(&Command {
            name: "SETTITLE".into(),
            param: "My Title".into(),
        }));
        assert_eq!(s.title, "My Title");

        assert!(s.apply_command(&Command {
            name: "SETKEYINFO".into(),
            param: "gopass/age-identities".into(),
        }));
        assert_eq!(s.key_info, "gopass/age-identities");

        assert!(s.apply_command(&Command {
            name: "SETREPEAT".into(),
            param: "Passphrases do not match".into(),
        }));
        assert!(s.repeat);
        assert_eq!(s.repeat_error, "Passphrases do not match");

        // Unknown command is not handled.
        assert!(!s.apply_command(&Command {
            name: "NOPE".into(),
            param: "".into(),
        }));
    }

    #[test]
    fn apply_command_percent_decodes_params() {
        let mut s = State::default();
        assert!(s.apply_command(&Command {
            name: "SETDESC".into(),
            param: "Enter%20passphrase%0Afor key".into(),
        }));
        assert_eq!(s.desc, "Enter passphrase\nfor key");
    }

    #[test]
    fn apply_option_parses_grab_and_tty() {
        let mut s = State::default();
        s.apply_command(&Command {
            name: "OPTION".into(),
            param: "grab".into(),
        });
        assert!(s.grab);

        s.apply_command(&Command {
            name: "OPTION".into(),
            param: "no-grab".into(),
        });
        assert!(!s.grab);

        s.apply_command(&Command {
            name: "OPTION".into(),
            param: "ttyname=/dev/tty1".into(),
        });
        assert_eq!(s.tty_name, "/dev/tty1");
    }

    #[test]
    fn settimeout_parses_value() {
        let mut s = State::default();
        s.apply_command(&Command {
            name: "SETTIMEOUT".into(),
            param: "30".into(),
        });
        assert_eq!(s.timeout, 30);
        // Non-numeric falls back to 0.
        s.apply_command(&Command {
            name: "SETTIMEOUT".into(),
            param: "abc".into(),
        });
        assert_eq!(s.timeout, 0);
    }

    #[test]
    fn reset_clears_error_only() {
        let mut s = State {
            title: "T".into(),
            error: "bad pin".into(),
            timeout: 30,
            ..State::default()
        };
        s.reset();
        assert_eq!(s.error, "");
        assert_eq!(s.title, "T");
        assert_eq!(s.timeout, 30);
    }

    #[test]
    fn reader_parses_name_and_param() {
        let input = b"SETTITLE My Title\nGETPIN\n";
        let mut r = Reader::new(io::Cursor::new(input));
        let c1 = r.read_command().unwrap().unwrap();
        assert_eq!(c1.name, "SETTITLE");
        assert_eq!(c1.param, "My Title");
        let c2 = r.read_command().unwrap().unwrap();
        assert_eq!(c2.name, "GETPIN");
        assert_eq!(c2.param, "");
        assert!(r.read_command().unwrap().is_none());
    }

    #[test]
    fn reader_uppercases_command_name() {
        let mut r = Reader::new(io::Cursor::new(b"getpin\n"));
        let c = r.read_command().unwrap().unwrap();
        assert_eq!(c.name, "GETPIN");
    }

    #[test]
    fn reader_strips_trailing_cr() {
        let mut r = Reader::new(io::Cursor::new(b"GETPIN\r\n"));
        let c = r.read_command().unwrap().unwrap();
        assert_eq!(c.name, "GETPIN");
        assert_eq!(c.param, "");
    }
}
