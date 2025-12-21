# wlrigctl

Wavelog Rig Control, Copyright (C) 2025 William Nutt, M7CLG.

Based upon clrigctl v0.2.0, Copyright (C) 2023 Martin Brodbeck, DG2SMB.

## History
This is a fork of clrigctl v0.2.0, Copyright (C) 2023 Martin Brodbeck, DG2SMB.

It has been adapted for Wavelog v2.2.1.

I first started tinkering with Martin's v0.2.0 clrigctl code when I was trying
out 2E0SQL's Cloudlog logging application. One of the features I missed from
CQRLog was the ability to capture QSOs from WSJT-X.

Then I found Wavelog, and noticed that clrigctl also worked with it.

"WavelogGate" appeared shortly after, but the notion of a web-y rig control app
offended my sensibilities; I wanted a low-overhead,
always-running-when-I'm-logged-in daemon, not something with a UI.

I also don't need to mess about with certs; I just want something running
locally, on the same machine as wavelog, flrig, wsjtx. The machine is behind a
firewall, and I only need to listen on 127.0.0.1.

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

2. Capturing a QSO from WSJTX, and inserting it automatically into Wavelog

3. Gatewaying clicks in the Wavelog cluster view through to flrig CAT control so
   that your rig tunes to the band and frequency required.

## Building

This software only works on Linux. It might one day be ported to *BSD.

_Windows* and MacOS* will never be supported. Stop using them._

It's Rust, so build from source:

```
$ git clone -b main --single-branch --no-tags \
            https://github.com/wfnutt/wlrigctl.git
$ cd wlrigctl
$ cargo install cargo-deb
$ cargo deb
```

_OPTION: take target/debian/wlrigctl.deb to your radio club..._

## Installing

The build process created a .deb package, which should be usable on plenty of
flavours of Linux. The daemon will run as you rather than root, so copy the
example config file for the daemon.

```
$ sudo apt install target/debian/wlrigctl*.deb

$ mkdir -p ~/.config/wlrigctl/
$ cp /usr/share/wlrigctl/example.toml ~/.config/wlrigctl/config.toml
```

## Configuring

_Edit ~/.config/wlrigctl/config.toml as you wish_

(you'll need to insert some Wavelog API keys that you must generate
for yourself)

## Enabling/Starting

These are one-off operations.

```
$ systemctl --user daemon-reload
$ systemctl --user enable --now wlrigctl.service
```
