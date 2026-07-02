# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-07-02

### Added
- `pinentry-dms` Rust binary implementing the Assuan pinentry protocol
  (GETPIN / CONFIRM / MESSAGE / GETINFO / SET* / OPTION / BYE / RESET).
- `pinentryDms` DankMaterialShell daemon plugin exposing an IPC `prompt`
  handler that shows a native themed FloatingWindow and returns the user's
  passphrase over the Unix socket the binary listens on.
- Initial support for gopass + age (`pinentry = /path/to/pinentry-dms`).