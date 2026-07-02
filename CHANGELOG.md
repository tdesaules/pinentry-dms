# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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