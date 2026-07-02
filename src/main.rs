//! pinentry-dms — DankMaterialShell-styled pinentry for gopass + age.
//!
//! Speaks the Assuan pinentry protocol on stdin/stdout. On GETPIN/CONFIRM/
//! MESSAGE it asks the `pinentryDms` DMS plugin to show a native modal and
//! relays the user's answer back to the pinentry client (gopass age agent).

mod assuan;
mod ipc;

use std::io::{self, BufReader, Write};

use assuan::{Command, Error, Reader, State, Writer};

fn main() {
    let args = parse_args();
    let default_timeout = args.timeout;

    let stdin = io::stdin();
    let stdout = io::stdout();

    let mut writer = Writer::new(stdout.lock());
    // Initial greeting: `OK Pleased to meet you`
    let _ = writer.ok("Pleased to meet you");
    let _ = writer.flush();

    let mut reader = Reader::new(BufReader::new(stdin.lock()));
    let mut state = State {
        timeout: default_timeout,
        ..State::default()
    };

    loop {
        let cmd = match reader.read_command() {
            Ok(Some(c)) => c,
            Ok(None) => return,
            Err(_) => return,
        };

        if args.debug {
            eprintln!("<- {} {}", cmd.name, cmd.param);
        }

        match cmd.name.as_str() {
            "GETPIN" => {
                handle_get_pin(&mut state, &mut writer);
                state.reset();
            }
            "CONFIRM" => {
                let one_button = cmd.param == "--one-button";
                handle_confirm(&mut state, &mut writer, one_button);
            }
            "MESSAGE" => {
                handle_message(&mut state, &mut writer);
            }
            "GETINFO" => handle_get_info(&cmd, &mut writer),
            "BYE" => {
                let _ = writer.ok("closing connection");
                let _ = writer.flush();
                return;
            }
            "RESET" => {
                state = State {
                    timeout: default_timeout,
                    ..State::default()
                };
                let _ = writer.ok("");
            }
            "NOP" => {
                let _ = writer.ok("");
            }
            other => {
                let _ = state.apply_command(&cmd);
                let _ = writer.ok("");
                let _ = other; // accepted (SET*/OPTION) or unknown-but-OK
            }
        }
        let _ = writer.flush();
    }
}

fn handle_get_pin(state: &mut State, writer: &mut Writer<impl Write>) {
    match ipc::show_modal("getpin", state) {
        Ok(resp) => match resp.kind.as_str() {
            "pin" => {
                let _ = writer.data(&resp.value);
                let _ = writer.ok("");
            }
            "cancel" => {
                let _ = writer.err(&Error::canceled());
            }
            "timeout" => {
                let _ = writer.err(&Error::timeout());
            }
            _ => {
                let _ = writer.err(&Error::general().with_message("unexpected response"));
            }
        },
        Err(e) => {
            let _ = writer.err(&Error::general().with_message(e.to_string()));
        }
    }
}

fn handle_confirm(state: &mut State, writer: &mut Writer<impl Write>, one_button: bool) {
    let kind = if one_button { "message" } else { "confirm" };
    match ipc::show_modal(kind, state) {
        Ok(resp) => match resp.kind.as_str() {
            "ok" => {
                let _ = writer.ok("");
            }
            "cancel" => {
                let _ = writer.err(&Error::canceled());
            }
            "timeout" => {
                let _ = writer.err(&Error::timeout());
            }
            "notok" => {
                let _ = writer.err(&Error::not_confirmed());
            }
            _ => {
                let _ = writer.err(&Error::general().with_message("unexpected response"));
            }
        },
        Err(e) => {
            let _ = writer.err(&Error::general().with_message(e.to_string()));
        }
    }
}

fn handle_message(state: &mut State, writer: &mut Writer<impl Write>) {
    match ipc::show_modal("message", state) {
        Ok(resp) => match resp.kind.as_str() {
            "ok" => {
                let _ = writer.ok("");
            }
            "timeout" => {
                let _ = writer.err(&Error::timeout());
            }
            _ => {
                let _ = writer.err(&Error::general().with_message("unexpected response"));
            }
        },
        Err(e) => {
            let _ = writer.err(&Error::general().with_message(e.to_string()));
        }
    }
}

fn handle_get_info(cmd: &Command, writer: &mut Writer<impl Write>) {
    match cmd.param.as_str() {
        "pid" => {
            let _ = writer.data(&std::process::id().to_string());
            let _ = writer.ok("");
        }
        "version" => {
            let _ = writer.data(env!("CARGO_PKG_VERSION"));
            let _ = writer.ok("");
        }
        "flavor" => {
            let _ = writer.data("dms");
            let _ = writer.ok("");
        }
        "ttyinfo" => {
            let _ = writer.data("");
            let _ = writer.ok("");
        }
        _ => {
            let _ = writer.ok("");
        }
    }
}

struct Args {
    debug: bool,
    timeout: i32,
}

fn parse_args() -> Args {
    let mut debug = false;
    let mut timeout = 0;
    let mut i = 1;
    let argv: Vec<String> = std::env::args().collect();
    while i < argv.len() {
        let arg = &argv[i];
        match arg.as_str() {
            "--debug" | "-d" => debug = true,
            "--timeout" | "-o" => {
                i += 1;
                if i < argv.len() {
                    timeout = argv[i].parse().unwrap_or(0);
                }
            }
            // Standard pinentry compatibility flags (accepted, ignored).
            "--display" | "-D" | "--ttyname" | "-T" | "--ttytype" | "-N" | "--lc-ctype" | "-C"
            | "--lc-messages" | "-M" | "--colors" | "--xauthority" => {
                i += 1; // swallow the value
            }
            "--parent-wid" | "-W" => {
                i += 1;
            }
            "--no-global-grab" | "-g" => {}
            // Allow `--timeout=30` style.
            other if other.starts_with("--timeout=") => {
                timeout = other["--timeout=".len()..].parse().unwrap_or(0);
            }
            other if other.starts_with("-o") && other.len() > 2 => {
                timeout = other[2..].parse().unwrap_or(0);
            }
            _ => {}
        }
        i += 1;
    }
    Args { debug, timeout }
}
