# wlrigctl — notes for Claude Code

Wavelog Rig Control daemon, by William Nutt M7CLG.
Forked from clrigctl v0.2.0 by Martin Brodbeck DG2SMB.
Club project: target deployment is BADARC (a local radio club).

## What it does

Three concurrent async tasks glued together with `Arc<FLRig>`:

| Task | Direction | Protocol |
|---|---|---|
| `wavelog_thread` | FLRig → Wavelog | XMLRPC poll + HTTP POST |
| `wsjtx_thread` | WSJT-X → Wavelog | UDP receive + HTTP POST |
| `CAT_thread` | Wavelog → FLRig | HTTP GET receive + XMLRPC |

Config lives at `~/.config/wlrigctl/config.toml` (XDG-aware).
Runs as a systemd user service (`systemctl --user`).

## Building and packaging

```sh
cargo build --release          # development
cargo deb                      # produces target/debian/wlrigctl_*.deb
```

`cargo deb` requires `cargo install cargo-deb` first.
The .deb installs the binary, a systemd user service, and an example config.
Targets x86_64 and aarch64 (cross-compilation works).

## Known quirks and non-obvious design decisions

### Wavelog drops the TCP connection before the response arrives (cat.rs)
Wavelog (or the browser) sends a CAT request and then closes the write side of
the TCP connection before reading the full HTTP response. `half_close(true)` on
the hyper server is required so the server keeps the connection open long enough
to finish writing the reply. Without it, serving the connection errors out.

### IC-703 CW narrow filter bodge (flrig.rs `set_mode`)
When changing to CW mode on an IC-703, FLRig briefly applies a wide default
bandwidth before the mode change fully settles. The `cwbandwidth` config option
triggers a follow-up `rig.set_bw` call to restore the narrow filter. This causes
a brief audio glitch. The hysteresis in `set_mode` (no-op if already in the
target mode) minimises how often this happens. The `cwbandwidth` follow-up call
is confirmed necessary: without it, switching away from CW and back via the
Wavelog bandlist does not restore the narrow filter.

### Per-rig mode naming (`flrig.rs` `Mode` enum, `cat.rs` `CatSettings`)
FLRig mirrors whatever mode names the physical radio displays rather than
providing a brand-agnostic interface.  The optional `cw_mode`, `rtty_mode` and
`digital_mode` fields in `[CAT]` specify the exact FLRig mode strings to use for
each concept.  All three default to ICOM/generic names (`CW`, `RTTY`, `D-USB`)
when absent.  Examples: Yaesu needs `CW-U`/`RTTY-U`/`DATA-U`; newer ICOM rigs
(IC-7300) need `USB-D`; Kenwood may need `FSK`; Elecraft uses `DATA`.  The
`Mode` enum in `flrig.rs` covers all known variants; add new ones there if a
future rig introduces an unfamiliar string.

### FT8 frequency detection is heuristic (cat.rs `is_ft8`)
When Wavelog sends a CAT QSY request, the mode hint from the bandmap is
unreliable. `is_ft8()` checks whether the target frequency falls within ±2–3 kHz
of the ten known FT8 dial frequencies and forces the correct digital mode
regardless of what Wavelog says. The FT8 frequency list is **hardcoded** in
`cat.rs:74`; update it if the band plan changes.

### Power scaling (flrig.rs `rig_power_watts`)
FLRig reports transmit power as a 0–100 percentage of the rig's maximum. The
`maxpower` config field (in watts) is used to scale this to an absolute wattage
for Wavelog. If FLRig returns 0 for `get_maxpwr()` the function returns "0"
rather than dividing by zero.

### WSJT-X protocol (wsjtx.rs)
Only schema version 2 is handled. Magic number: `0xadbccbda`. Only
`LoggedADIF` messages trigger a Wavelog upload; everything else is debug-logged
and discarded. If WSJT-X changes its schema number, `decode_hdr` will return
`UnsupportedSchema` for every packet.

### Config section names must match exactly (`[CAT]` and `[WSJTX]`)
The `config` crate v0.13 does **not** lowercase keys. The `Settings` struct uses
`#[serde(rename = "CAT")]` and `#[serde(rename = "WSJTX")]` so that the TOML
section names `[CAT]` and `[WSJTX]` map to the snake_case Rust fields `cat` and
`wsjtx`. Config files must use the uppercase names; lowercase `[cat]`/`[wsjtx]`
will not deserialise.

### CORS headers on CAT responses
Wavelog's bandmap makes HTTP requests from browser JavaScript, which requires
CORS headers (`Access-Control-Allow-*`). Without them the browser blocks the
response.

## Remaining TODO items


## Dependency notes

| Crate | Why it's here |
|---|---|
| `dxr_client` | FLRig XMLRPC client (reqwest + multicall backend) |
| `dxr` | XMLRPC types (`TryFromValue`) used directly for multicall result extraction |
| `bincode2` | WSJT-X UDP packet deserialisation |
| `hyper` / `hyper-util` | CAT HTTP server (Wavelog → us) |
| `reqwest` | Wavelog HTTP client (us → Wavelog), rustls-tls backend |
| `config` | TOML config file loading (toml feature; yaml feature not needed) |
| `home` | XDG-aware home directory (replaces deprecated std::env::home_dir) |
| `quick-xml` | Pulled in transitively; not used directly |

## Logging

The service has no output in normal operation. Enable logging via the systemd
unit's commented-out `Environment=` lines, or when running manually:

```sh
RUST_LOG=debug wlrigctl          # verbose
RUST_LOG=wlrigctl=debug wlrigctl # verbose, suppress dependency noise
RUST_LOG=info wlrigctl           # startup banners and mode changes only
```
