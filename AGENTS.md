# AGENTS.md

`pinentry-dms` — DankMaterialShell-styled pinentry for **gopass + age**.

## Architecture

Two parts cooperate at runtime:

1. **`pinentry-dms`** (Rust binary, `src/`): speaks the Assuan pinentry
   protocol on stdin/stdout. On `GETPIN`/`CONFIRM`/`MESSAGE` it creates a
   Unix socket under `XDG_RUNTIME_DIR`, spawns `dms ipc call pinentryDms
   prompt <json>` **detached** (does not wait), then `accept`s the plugin's
   connection and decodes a JSON `{type,value}` reply.
2. **`pinentryDms`** (DMS daemon plugin, `plugin/PinentryDaemon.qml`):
   registers an `IpcHandler` with `target:"pinentryDms"`; its `prompt(json)`
   method opens a `FloatingWindow` modal and writes the answer back over the
   socket via `Quickshell.Io` `Socket`.

Wire contract between binary and plugin: JSON `Request` (camelCase, omitempty)
and `Response` (`{type: "pin|ok|cancel|notok|timeout", value?: string}`). Field
names must stay in sync across `src/ipc.rs` and `plugin/PinentryDaemon.qml`.

## Build & dev

```sh
cargo build --release                    # binary → target/release/pinentry-dms
cargo run --                              # quick debug run
```

Rust toolchain comes from mise (`rust = "latest"`) on the target host; no
rust-toolchain file is committed.

### Plugin live dev

```sh
# One-time: expose the plugin dir to DMS (host uses /var/home, /home is a symlink)
ln -sf "$PWD/plugin" ~/.config/DankMaterialShell/plugins/pinentryDms

# Reload after edits. NOTE: `plugins reload` for an IpcHandler-based daemon
# stacks stale handlers (the new one is shadowed by the dead old one and IPC
# calls stop reaching a live modal). After editing PinentryDaemon.qml prefer a
# full `dms restart` to get a single working handler.
dms ipc call plugins reload pinentryDms     # OK for trivial checks only
dms restart                                  # use this after QML edits
dms ipc call plugins list              # check status
journalctl --user -u dms.service -f    # live QML load errors
```

Settings live at `~/.config/DankMaterialShell/plugin_settings.json`; enable
`"pinentryDms": { "enabled": true }`.

## Repo-specific conventions (do not guess)

- **Plugin id is `pinentryDms`.** It is referenced in `dms ipc call
  pinentryDms prompt …` from `src/ipc.rs` and as `IpcHandler.target` in
  `plugin/PinentryDaemon.qml`. Renaming requires updating both.
- **Modal is inlined as a `property Component`** in `PinentryDaemon.qml` —
  do **not** split it into a sibling `PinentryModal.qml` and load with
  `Qt.createComponent(Qt.resolvedUrl(...))`. On this host `/home` is a
  symlink to `/var/home` and that pattern triggers "File name case mismatch".
  Same convention proven in the `gopass-dms` plugin.
- **Focus by `Qt.callLater(() => field.forceActiveFocus())`**, not direct
  `forceActiveFocus()` (race on show).
- `PluginComponent` is the correct root for a **daemon** plugin. `QtObject`
  (used by launcher plugins) is wrong here.
- **Assuan percent-encoding encodes only `%`, `\n`, `\r`** (see
  `src/assuan.rs::percent_encode`) — not full URL encoding. `+` is literal.

## Versioning & release

- **Strict semver** for all releases: tags must match `vX.Y.Z` (no pre/build
  suffixes); the `validate-tag` job in `release.yml` enforces this.
- **Conventional Commits** are enforced on pull requests by `ci.yml`:
  `<type>(<scope>)?!?: <description>` with types
  `feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert`.
- Bump **both** `Cargo.toml` `version` **and** `plugin/plugin.json`
  `"version"` together; add a `## [x.y.z]` section to `CHANGELOG.md` describing
  the changes.
- **No manual tagging.** Commit + push the version bump to `main` — the CI
  detects the new version (tag `v<version>` absent) and auto-creates the GitHub
  release + that tag (via `softprops/action-gh-release` `tag_name`). If the
  tag already exists, the release job is skipped (the build/test still runs).
- Strict semver must be respected (`vX.Y.Z`, no pre/build suffixes); the CI's
  `detect-release` job enforces this.

## CI

Single workflow `.github/workflows/release.yml`, triggered on push to `main` and on
PRs when files under `src/`, `plugin/`, `Cargo.toml`, `Cargo.lock`, or the
workflow itself change. Jobs:
- `lint-commits` (PR only): Conventional Commits format.
- `check`: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo build`,
  `cargo test`.
- `build`: matrix x86_64 + aarch64 release build; uploads the **raw binary**
  `pinentry-dms-<version>-<target>` (+ `.sha256`) — no tar.gz around the binary.
- `package-plugin`: creates a single `pinentry-dms-plugin-<version>.tar.gz`
  containing only the DMS plugin files (`plugin/`), + `.sha256`.
- `detect-release` (push to `main` only, after `check`+`build`): reads
  `Cargo.toml` version, asserts strict semver and parity with `plugin.json`,
  and checks whether tag `v<version>` already exists on the remote.
- `release` (push to `main` only, gated on `should_release == true`):
  downloads the two arch artifacts, verifies all assets present, extracts the
  matching `CHANGELOG.md` section as the release body, and creates the GitHub
  release (which auto-creates the `v<version>` tag).
- Consumers install via mise:
  `"github:tdesaules/pinentry-dms" = "latest"` in `dot_config/mise/config.toml.tmpl`.

## End-to-end test (on target host)

```sh
gopass age agent lock                       # force the age store locked
gopass show <some/secret>                   # must pop the DMS modal
# type passphrase → D <pin> returned over Assuan → gopass decrypts
gopass age agent status
```

The `gopass-age-agent.service` user systemd unit is **locked at boot**
(`ExecStartPost lock`); the first gopass op of the session is the canonical
trigger for the modal. The unit's PATH only contains mise's shims, so the
integration must expose the binary as `pinentry` within mise's shims (see
"Integration" below) — the service itself needs no change.

## Integration (chez-moi repo, separate)

- **gopass age ignores the `age.pinentry` config value** and resolves
  `pinentry` by PATH lookup only (verified: pointing the config at a
  nonexistent path leaves behavior unchanged). The binary must therefore be
  reachable on PATH **as `pinentry`** — not `pinentry-dms`.
- The `gopass-age-agent.service` systemd unit only has mise's shims on PATH.
  The integration must ship a shim that mise exposes as `pinentry`
  (e.g. `~/.local/share/mise/shims/pinentry` → `pinentry-dms`), or add
  `~/.local/bin` to the unit's `Environment=PATH=…`. A bare `pinentry-dms`
  shin on PATH is **not** enough — gopass looks up the literal name `pinentry`.
- The existing `gopass-dms` launcher plugin **bypasses pinentry** via
  `GOPASS_AGE_PASSWORD` and is unaffected by this pinentry; both coexist.

## Known DMS-side bug

`PluginService._flushStateToDisk` references `FileView.loaded` as a signal
when it's a bool → non-fatal `Property 'connect' of object false is not a
function`. This pinentry daemon persists no state, so it's irrelevant here;
don't add `savePluginState` calls expecting on-disk persistence.