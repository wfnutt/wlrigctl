# wlrigctl

Wavelog Rig Control, Copyright (C) 2025-2026 William Nutt, M7CLG.

Based upon clrigctl v0.2.0, Copyright (C) 2023 Martin Brodbeck, DG2SMB.

## History

This is a fork of clrigctl v0.2.0 by Martin Brodbeck, DG2SMB.  It has been
adapted for Wavelog v2.2.1 onwards.

I first started tinkering with Martin's v0.2.0 clrigctl code when I was trying
out 2E0SQL's Cloudlog logging application. One of the features I missed from
CQRLog was the ability to capture QSOs from WSJT-X.

Then I found Wavelog, and noticed that clrigctl also worked with it.

"WaveLogGate" appeared shortly after, but the notion of a web-y rig control app
offended my sensibilities; I wanted a low-overhead,
always-running-when-I'm-logged-in daemon, not something with a UI.

Fiddling about with this little bit of glue code has given me an excuse to try
Rust and decode UDP packets from WSJT-X, which is a surprisingly powerful
feature (you can send UDP datagrams in the reverse direction too...).

I make no warranty that the Rust code is idiomatic, or indeed any good(!)
If you have an IC-703 with a narrow CW filter, you're in luck because that's
the rig wlrigctl was developed against.

The eventual goal is to migrate the logging at the club (BADARC) to Wavelog.

73 de Bill M7CLG

## Use cases
1. Sending current rig frequency and power level to the Live QSO dialog in
   Wavelog, just as clrigctl 0.2.0 did.

2. Capturing a QSO from WSJTX, and inserting it automatically into Wavelog.

3. Gatewaying clicks in the Wavelog cluster view through to flrig CAT control so
   that your rig tunes to the band and frequency required.

4. Pushing live rig state (frequency, mode, power) to the Wavelog band map via
   an encrypted WebSocket connection, so the map tracks your current VFO in
   real time without any page reload.

## Installing

Pre-built `.deb` packages for each release are attached to the
[GitHub Releases](https://github.com/wfnutt/wlrigctl/releases) page.
Download the latest `.deb` and install it:

```
$ sudo apt install ./wlrigctl_*.deb

$ mkdir -p ~/.config/wlrigctl/
$ cp /usr/share/wlrigctl/example.toml ~/.config/wlrigctl/config.toml
```

## Building from source

This software currently only works on Linux.

_Windows and MacOS will never be supported. Stop using them._

Requires Rust 1.95.0 or later.  If you use `rustup`, the correct toolchain is
selected automatically from `rust-toolchain.toml` in the repository root.

```
$ git clone -b main --single-branch --no-tags \
            https://github.com/wfnutt/wlrigctl.git
$ cd wlrigctl
$ cargo install cargo-deb
$ cargo deb
```

## Configuring

Copy the installed example and open it in your editor:

```
$ cp /usr/share/wlrigctl/example.toml ~/.config/wlrigctl/config.toml
$ $EDITOR ~/.config/wlrigctl/config.toml
```

Key settings:

**`[wavelog]`**
- `url` — your Wavelog instance URL; must end with `/index.php/api/radio`
- `key` — API key generated in the Wavelog admin panel under *Station → API Keys*
- `identifier` — short label for this station (shown in Wavelog's radio list)
- `station_profile_id` — station profile index; most users need `1`
- `cat_url` — set to `http://127.0.0.1:54321` so Wavelog auto-registers the CAT
  callback; without it the bandmap QSY button requires manual admin configuration

**`[flrig]`**
- `host` / `port` — address of your running FLRig instance (default: `http://127.0.0.1:12345`)
- `maxpower` — rig's maximum power in watts; FLRig reports power as 0–100% and
  this scales it to an absolute wattage for Wavelog

**`[CAT]`** *(optional — needed for bandmap QSY)*
- `port` — TCP port the CAT server listens on (default `54321`)
- `cw_mode` / `rtty_mode` / `digital_mode` — FLRig mode strings for your rig;
  defaults work for ICOM; see `example.toml` for Yaesu, Kenwood, Elecraft variants
- `wavelog_origin` — if Wavelog is served over HTTPS, set this to your Wavelog
  URL origin to guard against CSRF; see `example.toml` for details

**`[WSJTX]`** *(optional — needed for WSJT-X QSO capture)*
- `host` / `port` — bind address for the WSJT-X UDP listener (default `127.0.0.1:2237`);
  must match the UDP destination configured in WSJT-X settings

> **Note:** The section names `[CAT]` and `[WSJTX]` must be uppercase in your
> config file.  Lowercase `[cat]` or `[wsjtx]` will silently fail to load.

> **UK deployments:** The CAT server enforces UK Ofcom amateur band allocations
> (Foundation licence, Tables A–C) and rejects any QSY to a frequency outside
> those ranges.  Deploying outside the UK requires editing `AMATEUR_BANDS_HZ` in
> `src/cat.rs` and recompiling.

## Enabling/Starting

These are one-off operations.

```
$ systemctl --user daemon-reload
$ systemctl --user enable --now wlrigctl.service
```

## Debugging

The service produces no output in normal operation.  To enable logging, set
`RUST_LOG` before starting it:

```
$ RUST_LOG=info wlrigctl            # startup banners and mode changes only
$ RUST_LOG=wlrigctl=debug wlrigctl  # verbose, suppresses dependency library noise
$ RUST_LOG=debug wlrigctl           # very verbose including all dependencies
```

When running as a systemd service, uncomment the `Environment=` line in the
unit file:

```
$ systemctl --user edit wlrigctl.service
```

## WebSocket browser setup (one-time per Chrome restart)

wlrigctl serves live rig data over an encrypted WebSocket connection
(`wss://127.0.0.1:54323/`) so Wavelog's band map can display your
current frequency in real time.  The first time wlrigctl starts it
generates a self-signed TLS certificate.  Before Wavelog can connect
you need to tell your browser to trust it:

1. Open **https://127.0.0.1:54323/** in the same browser you use for Wavelog
2. Click **Advanced → Proceed to 127.0.0.1 (unsafe)**

The certificate is saved to `~/.config/wlrigctl/ws-cert.pem` and reused on
subsequent restarts.

**Firefox** remembers the exception permanently — you only need to do this once.

**Chrome** does not persist the exception across browser restarts.  You will
need to repeat the procedure each time Chrome is restarted.  If this is
inconvenient, use Firefox for Wavelog.

## Releasing (maintainers)

1. Bump `version` in `Cargo.toml` and commit
2. Push a matching tag:

```
$ git tag v0.5.0 && git push origin v0.5.0
```

GitHub Actions picks up the tag, runs the test suite, and attaches the
`.deb` to a new GitHub Release automatically.  The pipeline will fail
if the tag and `Cargo.toml` version don't match.
