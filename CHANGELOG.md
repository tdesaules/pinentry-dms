# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.2.1] - 2026-08-11

### Fixed
- A1 fast-fail was broken: the connect deadline (5s) expired before the
  user could type, because the DMS plugin only connects its response
  socket AFTER the user submits the passphrase (in `sendResponse`), not
  when the modal appears. Replaced the split connect/read deadlines with
  a single deadline covering the full user-input window. Fast-fail is
  now achieved by polling the `dms ipc call` child: if it exits non-zero
  (DMS unreachable / handler missing) we abort immediately; if it exits 0
  or stays alive (IPC received, modal showing) we keep waiting for the
  user. Restores normal `gopass show` which was failing with "DMS plugin
  never connected" after 5s.

## [1.2.0] - 2026-08-11

### Added
- Deduplication of concurrent `GETPIN` prompts: when N gopass processes
  prompt for the same passphrase at once (e.g. boot-time `git fetch` +
  `chezmoi apply` + IDE git fetch hitting a locked agent), the plugin now
  shows ONE modal and broadcasts the passphrase to every waiting socket.
  Previously each concurrent prompt was queued and shown sequentially
  ("popup storm"); before that, v1.0.0 clobbered them (lost responses).
- Best-effort pre-unlock of the gopass age agent at DMS session start: the
  plugin fires `gopass age agent unlock` once on load so the agent has
  identities before background callers hit a locked agent. This prevents
  the half-unlocked zombie state (locked=false, identities=nil) where every
  gopass client re-prompts indefinitely. Failure is non-fatal.

## [1.1.0] - 2026-08-11

### Added
- Forward `SETKEYINFO` and `SETREPEAT` error text to the DMS modal: the
  plugin now displays which key is being unlocked (discrete label under the
  prompt) and uses the caller-provided repeat-error message instead of a
  hardcoded string.
- `--version` / `--help` CLI flags (exit before the Assuan greeting).
- Verbose `--debug` tracing of the IPC exchange: child pid, socket path,
  connect/read deadlines, response timing.
- 21 unit tests covering Assuan percent encode/decode (including `+`
  literal), `Error::wire` packing, `Command` parsing, `State::apply_command`,
  `Request`/`Response` serde, and passphrase zeroization.

### Changed
- Fast-fail when DMS is unreachable: the connect deadline is now a short,
  dedicated window (5s, overridable via `PINENTRY_DMS_CONNECT_TIMEOUT`),
  distinct from the user-input read deadline. A missing/dead DMS now errors
  in ~5s instead of blocking for the full `SETTIMEOUT` (60s by default).
- Concurrent prompts are now queued instead of clobbering the in-flight
  modal (the previous behavior left the first pinentry process waiting for
  a response that never came).
- Passphrase is zeroized from Rust memory after being sent on the Assuan
  `D` line, and the QML modal clears its input bindings immediately after
  emitting `submitted`.

### Fixed
- README now documents the correct gopass+age integration (`pinentry-program`
  in `~/.gnupg/gpg-agent.conf`), not the vestigial `age.pinentry` config key.
  Added a Troubleshooting section and notes on the passphrase caching
  lifecycle.

## [1.0.0] - 2026-07-02

### Changed
- First stable release. The Assuan pinentry protocol surface, the
  binary ↔ plugin JSON IPC contract, and the GnuPG/mise integration
  model (`pinentry-program` in `~/.gnupg/gpg-agent.conf` pointing at the
  mise `pinentry-dms` shim) are now considered stable.

## [0.3.0] - 2026-07-02

### Changed
- Release artifacts split: raw Rust binaries (`pinentry-dms-<version>-<target>`,
  no tar.gz) per arch + a single `pinentry-dms-plugin-<version>.tar.gz`
  containing only the DMS plugin files, for simpler deployment.

## [0.2.0] - 2026-07-02

### Changed
- CI consolidated into a single workflow (`.github/workflows/release.yml`)
  that auto-publishes the GitHub release + `v<version>` tag on push to `main`
  when the `Cargo.toml` version changes; no manual tagging required.
- Release archive now bundles `bin/pinentry-dms` + `plugin/` (DMS plugin) +
  docs + `sha256`.

## [0.1.0] - 2026-07-02

### Added
- `pinentry-dms` Rust binary implementing the Assuan pinentry protocol
  (GETPIN / CONFIRM / MESSAGE / GETINFO / SET* / OPTION / BYE / RESET).
- `pinentryDms` DankMaterialShell daemon plugin exposing an IPC `prompt`
  handler that shows a native themed FloatingWindow and returns the user's
  passphrase over the Unix socket the binary listens on.
- Initial support for gopass + age (`pinentry = /path/to/pinentry-dms`).