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