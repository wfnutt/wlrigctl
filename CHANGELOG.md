# Changelog

All notable changes to wlrigctl are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

---

## [Unreleased]

### Changed
- Licence corrected from MIT to X11 (the upstream clrigctl licence was always X11)
- GITHUB_TOKEN restricted to read-only by default in CI; `package` job retains write access for releases
- Version bumped in preparation for next release (v0.4.3)

### Added
- Makefile with `ci`, `fix`, `build`, `deb`, and `clean` targets for local development
- `cargo deny` added to the CI audit job (licence compatibility and duplicate-crate detection)

### Fixed
- Dependency footprint reduced; `ring` eliminated from the dependency tree

### Security
- openssl bump from 0.10.78 to 0.10.79
- openssl-sys bump from 0.9.114 to 0.9.115
- other crates per cargo update

---

## [0.4.2] — 2026-05-03

### Security
- CAT server now binds exclusively to loopback (`127.0.0.1`); the previously
  configurable `host` field has been removed to prevent accidental LAN exposure
- QSY requests outside the UK amateur band allocations (Ofcom Tables A–C) are
  rejected with HTTP 400; the allowlist is hardcoded by design so non-UK
  deployments must recompile
- HTTP Origin header check replaces the earlier per-startup token approach,
  which was ineffective against CSRF from browser JavaScript

### Fixed
- IC-703 CW narrow filter is now correctly restored after every mode change
  (FLRig resets bandwidth on mode apply; a follow-up `set_bw` call is required)
- WebSocket server updated for rustls 0.23 API changes

### Added
- Unit tests for WSJT-X header decoding error paths, `parse_qsy_path`, and
  `rig_power_watts` boundary conditions

### Changed
- CI actions bumped to Node 24 runtimes
- Rust toolchain pinned to 1.95.0 in CI; third-party CI actions removed

---

## [0.4.1] — 2026-04-26

### Changed
- Rig power reported to Wavelog as integer watts (was fractional)
- CI migrated from GitLab CI to GitHub Actions

---

## [0.4.0] — 2026-04-16

### Added
- WebSocket server (WSS on `127.0.0.1:54323`) for Wavelog live-radio bandmap
  integration; matches the WaveLogGate wire format
- Self-signed TLS certificate generated on first run and persisted so the
  browser's one-time security exception remains valid across restarts
- `cat_url` field in `[wavelog]`: Wavelog auto-registers the CAT callback URL
  on each radio POST, removing the need for manual admin-panel configuration
- Per-rig mode naming via `cw_mode`, `rtty_mode`, and `digital_mode` config
  fields; covers Yaesu (`CW-U`/`DATA-U`), Kenwood (`FSK`), Elecraft (`DATA`),
  and IC-7300 (`USB-D`) variants
- Graceful shutdown via `CancellationToken`

### Fixed
- `[CAT]` and `[WSJTX]` config section names were broken by an earlier rename;
  `serde(rename)` attributes now enforce the correct uppercase names
- Blocking file I/O inside async Tokio task in `wsjtx.rs`

### Changed
- FLRig polling reduced from 4 TCP connections per cycle to 1–2

---

## [0.3.0] — 2025-12-21

First wlrigctl release, forked from clrigctl v0.2.0 by Martin Brodbeck, DG2SMB.

### Added
- CAT control module (`cat.rs`): accepts QSY requests from Wavelog's bandmap
  and forwards them to FLRig over XMLRPC
- WSJT-X QSO upload: receives ADIF log frames over UDP and posts them to Wavelog
- Systemd user service packaging (`.deb` via `cargo-deb`)
- Unit tests for CAT conversion logic (FT8 frequency detection, mode mapping)

---

*Earlier history is in the commit log; clrigctl v0.2.0 is the upstream origin.*
