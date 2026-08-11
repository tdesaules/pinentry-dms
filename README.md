# pinentry-dms

A pinentry implementation that displays a native **DankMaterialShell** modal for
passphrase entry. Targeted at **gopass + age** (the age store passphrase).

This repo is a Rust port of [Pacman99/DankPinentry](https://github.com/Pacman99/DankPinentry)
(Go), scoped to a gopass/age setup and following the conventions of the
[`gopass-dms`](https://github.com/tdesaules/gopass-dms) plugin.

## How it works

Two parts cooperate:

- **`pinentry-dms`** (Rust binary): speaks the Assuan pinentry protocol on
  stdin/stdout. On `GETPIN`/`CONFIRM`/`MESSAGE` it opens a Unix socket, fires off
  `dms ipc call pinentryDms prompt <json>`, and waits for the plugin to connect
  and write the answer.
- **`pinentryDms`** (DMS daemon plugin): registers an `IpcHandler` that shows a
  themed FloatingWindow and returns the user's passphrase over the socket.

`pinentry-dms` does **not** cache the passphrase. The cache is owned by the
`gopass age` agent (see "Passphrase caching" below).

## Build

```sh
cargo build --release     # → target/release/pinentry-dms
```

## Install the plugin

```sh
ln -sf "$PWD/plugin" ~/.config/DankMaterialShell/plugins/pinentryDms
dms ipc call plugins reload pinentryDms
```

Enable it in DMS settings (`~/.config/DankMaterialShell/plugin_settings.json`):
`"pinentryDms": { "enabled": true }`.

> After editing the plugin QML, prefer `dms restart` over `plugins reload`:
> reloading an `IpcHandler`-based daemon stacks stale handlers and IPC calls
> stop reaching a live modal.

## Configure gopass + age

`pinentry-dms` is resolved by gopass-age via the **GnuPG agent config**, not the
gopass `age.pinentry` key (which is vestigial and ignored). gopass-age builds its
pinentry with `twpayne/go-pinentry`'s `WithBinaryNameFromGnuPGAgentConf()`, which
reads `pinentry-program <path>` from `~/.gnupg/gpg-agent.conf`. If that line is
absent it falls back to a PATH lookup of the literal name `pinentry`.

Point `pinentry-program` at the mise `pinentry-dms` shim (absolute path —
`gpg-agent.conf` does not expand `~`):

```ini
# ~/.gnupg/gpg-agent.conf
pinentry-program /home/<you>/.local/share/mise/shims/pinentry-dms
```

gopass only needs the age agent enabled; the `age.pinentry` key is **not**
consumed by this path:

```ini
# ~/.config/gopass/config
[age]
agent-enabled = true
agent-timeout = 7200          # cache TTL in seconds (idle), 2h here
```

Then force the agent locked and trigger a prompt:

```sh
gopass age agent lock
gopass show <some/secret>      # the DMS modal must pop
```

## Passphrase caching

`pinentry-dms` is only the UI that *fills* the cache; it stores nothing.

- The cache lives in the `gopass age` agent (`gopass-age-agent.service`), which
  caches the **unlocked identity** in memory (not the passphrase itself). It
  starts **locked at boot** (`ExecStartPost lock`).
- `age.agent-timeout` is an **idle** timeout: each successful `gopass show`
  resets the timer, so "2h" means 2h of *inactivity*, not 2h after the first
  access. A `gopass show` every 30 min keeps the cache alive indefinitely.
- The cache is purged (identity dropped from memory, agent locked) by:
  - **boot** (`ExecStartPost lock`),
  - **AGE USB key removal** (the `gopass-age-usb-handler` locks the agent and
    removes the identity symlink),
  - **agent restart / host shutdown**.
- Screen lock and suspend do **not** purge the cache in this setup.

## Requirements

- DankMaterialShell with the `pinentryDms` plugin enabled
- `dms` binary on PATH (for IPC)
- gopass with an age backend

## Troubleshooting

The modal doesn't pop and gopass errors after a few seconds:

```sh
dms ipc call plugins list                 # is pinentryDms enabled & loaded?
journalctl --user -u dms.service -f        # QML load errors land here
```

If the plugin was just edited, reload cleanly (see the note in "Install the
plugin"):

```sh
dms restart
```

Verify it is enabled in `~/.config/DankMaterialShell/plugin_settings.json`:

```json
"pinentryDms": { "enabled": true }
```

Confirm gopass resolves the right pinentry:

```sh
grep pinentry-program ~/.gnupg/gpg-agent.conf
# should point at the mise shim, e.g. /home/<you>/.local/share/mise/shims/pinentry-dms
```

## Notes (advanced integration)

- On Fedora, `/usr/bin/pinentry` is a dispatcher script that execs
  `pinentry-qt`/`pinentry-gnome3`/`pinentry-tty` by environment. Setting
  `pinentry-program` short-circuits the dispatcher entirely.
- `gopass-age-agent.service` is locked at boot via `ExecStartPost lock`; the
  first gopass operation of the session is the canonical trigger for the modal.
- The modal is inlined as a `property Component` in `PinentryDaemon.qml` (not
  loaded via `Qt.createComponent` on a sibling file). On hosts where `/home` is
  a symlink to `/var/home` that pattern triggers "File name case mismatch".
