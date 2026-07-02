# pinentry-dms

A pinentry implementation that displays a native **DankMaterialShell** modal for
passphrase entry. Targeted at **gopass + age** (the age store passphrase).

This repo is a Rust port of [Pacman99/DankPinentry](https://github.com/Pacman99/DankPinentry)
(Go), scoped to a gopass/age setup and following the conventions of the
[`gopass-dms`](https://github.com/tdesaules/gopass-dms) plugin.

## How it works

Two parts cooperate:

- **`pinentry-dms`** (Rust binary): speaks the Assuan pinentry protocol on
  stdin/stdout. On `GETPIN` it opens a Unix socket, fires off
  `dms ipc call pinentryDms prompt <json>`, and waits for the plugin to
  connect and write the answer.
- **`pinentryDms`** (DMS daemon plugin): registers an `IpcHandler` that shows a
  themed FloatingWindow and returns the user's passphrase over the socket.

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

## Configure gopass + age

Point the gopass age config at the binary (absolute path recommended when used
from a systemd unit without mise's PATH):

```ini
# ~/.config/gopass/config
[age]
agent-enabled = true
pinentry = /home/<you>/.local/share/mise/shims/pinentry-dms
```

Then force the agent locked and trigger a prompt:

```sh
gopass age agent lock
gopass show <some/secret>      # the DMS modal must pop
```

## Requirements

- DankMaterialShell with the `pinentryDms` plugin enabled
- `dms` binary on PATH (for IPC)
- gopass with an age backend