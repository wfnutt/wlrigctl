# wlrigctl — notes for Claude Code

Wavelog Rig Control daemon, by William Nutt M7CLG.
Forked from clrigctl v0.2.0 by Martin Brodbeck DG2SMB.
Club project: target deployment is BADARC (a local radio club).

## Development workflow

Before suggesting a push, run `make ci` and confirm it passes.  The Makefile
comment says the same: "This is the thing to run before pushing."  Do not
suggest pushing until CI is green locally.

`make fix` auto-corrects formatting and clippy lint suggestions, then runs
the full suite — use it when `make ci` fails on fmt or clippy.

## What it does

Four concurrent async tasks glued together with `Arc<FLRig>`:

| Task | Direction | Protocol |
|---|---|---|
| `wavelog_thread` | FLRig → Wavelog | XMLRPC poll + HTTP POST |
| `wsjtx_thread` | WSJT-X → Wavelog | UDP receive + HTTP POST |
| `CAT_thread` | Wavelog → FLRig | HTTP GET receive + XMLRPC |
| `ws_thread` *(optional)* | FLRig → browser | WebSocket push |

`wavelog_thread` and `ws_thread` share a `tokio::sync::watch` channel;
`wavelog_thread` publishes on every rig-state change, `ws_thread` fans the
latest value out to all connected WebSocket clients and pushes it immediately
to each new client on connect.

Config lives at `~/.config/wlrigctl/config.toml` (XDG-aware).
Runs as a systemd user service (`systemctl --user`).

## Known quirks and non-obvious design decisions

### Wavelog drops the TCP connection before the response arrives (cat.rs)
Wavelog (or the browser) sends a CAT request and then closes the write side of
the TCP connection before reading the full HTTP response. `half_close(true)` on
the hyper server is required so the server keeps the connection open long enough
to finish writing the reply. Without it, serving the connection errors out.

### IC-703 CW narrow filter bodge (flrig.rs `set_mode`)
FLRig presents three CW bandwidth options for the IC-703: NARR / MED / WIDE
(indices 0 / 1 / 2).  Due to a bug in FLRig's IC-703 driver, selecting NARR
(index 0) does **not** activate the hardware narrow filter (N indicator goes
out); selecting MED (index 1) **does** (N indicator comes on).  The correct
`cw_bw_index` value is therefore 1, not 0.

`cw_bw_index` is an index into FLRig's bandwidth table, **not a value in Hz**.
When set, `set_mode` calls `rig.set_bw` with that index after every CW mode
change.  This is necessary because FLRig resets bandwidth to a wide default
when applying a mode change; without the follow-up call the narrow filter is
not restored.

The `cw_narrow_index` helper in `flrig.rs` encapsulates the two-condition check
(target mode is CW AND `cw_bw_index` is configured) and returns the index
directly, so the call site can `if let Some(idx) = cw_narrow_index(...)` with
no unwrap.  The helper is unit-tested independently of the async XMLRPC path.

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

### CAT frequency allowlist is UK-only and has no config override (`cat.rs`)
`AMATEUR_BANDS_HZ` enforces UK Ofcom amateur allocations (Foundation licence
baseline, Tables A–C).  Any QSY request outside those ranges is rejected with
400.  This is intentional: it prevents Wavelog from accidentally QSYing a shared
club radio to a non-amateur frequency.

There is no runtime config to override the band plan.  Non-UK deployments must
edit `AMATEUR_BANDS_HZ` in `cat.rs` directly and recompile.  This is a
deliberate design choice — requiring a recompile ensures the operator has read
and understood the change rather than accidentally disabling the check.

### CORS headers on CAT responses
Wavelog's bandmap makes HTTP requests from browser JavaScript, which requires
CORS headers (`Access-Control-Allow-*`). Without them the browser blocks the
response.

### WebSocket server architecture (`ws.rs`)
Wavelog's WebSocket support is designed around WaveLogGate (the Electron
desktop companion), which acts as the WS *server* on port 54323 (WSS); the
browser connects outbound to it.  wlrigctl provides the same server role: when
`[websocket]` is present in the config it binds a **WSS** server (default
`127.0.0.1:54323`) and pushes `radio_status` JSON frames matching the
WaveLogGate wire format.

TLS is mandatory — Wavelog's `cat.js` hardcodes `wss://127.0.0.1:54323/`
(WSS), and browsers refuse mixed-content `ws://` connections from HTTPS pages.
If no `tls_cert`/`tls_key` are configured, a self-signed certificate is
generated using `rcgen` and **saved to `~/.config/wlrigctl/ws-cert.pem`** on
first run; subsequent starts reload the same cert so the browser's one-time
security exception remains valid.  Users who want no browser warning should use
`mkcert` — see `example.toml`.

The `[websocket]` config section is **optional**; the server always starts on
`127.0.0.1:54323` with auto-generated TLS even when the section is absent.

The `cat_url` field in `[wavelog]` is included in every live-radio POST to
Wavelog's `/index.php/api/radio` endpoint.  When set, Wavelog auto-registers the
CAT callback URL so the bandmap QSY button works without manual configuration in
the Wavelog admin panel.

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
| `tokio-tungstenite` | WebSocket server (wraps tungstenite over tokio TLS TCP) |
| `tokio-rustls` | TLS acceptor wrapping each TCP stream before WebSocket upgrade |
| `rustls` / `rustls-pemfile` | TLS server config; PEM cert/key file loading |
| `rcgen` | Self-signed certificate generation when no cert files are configured |
| `futures-util` | `SinkExt`/`StreamExt` traits needed by tungstenite async API |
| `quick-xml` | Pulled in transitively; not used directly |

