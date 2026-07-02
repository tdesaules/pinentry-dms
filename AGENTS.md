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

- Semver. Bump both `Cargo.toml` `version` **and** `plugin/plugin.json`
  `"version"` together, and add a `CHANGELOG.md` entry.
- Tagging `v*` triggers `.github/workflows/release.yml`, which builds
  `x86_64-unknown-linux-gnu` + `aarch64-unknown-linux-gnu` tarballs and
  publishes a GitHub release. Consumers install via mise:
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
trigger for the modal.

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