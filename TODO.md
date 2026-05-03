# wlrigctl TODO

Items are grouped by area and roughly prioritised within each group.
See CLAUDE.md for design decisions and known quirks.

---

## Dependency maintenance

- **Upgrade reqwest 0.11 → 0.12** (`reqwest` is pinned at 0.11.22 because
  `dxr_client 0.7.1` pins it; watch for a `dxr_client` release that supports
  0.12).  Pulling in 0.12 also resolves the trailing TLS-stack lag:
  `rustls 0.21 → 0.23`, `tokio-rustls 0.24 → 0.26`, `rustls-pemfile 1 → 2`.

- **Add `cargo audit` to CI** — detect known CVEs in the dependency tree on
  every push.  One additional `run: cargo audit` step in the `test` job is
  sufficient.  Needs `cargo install cargo-audit` (or a cache step) first.

---

## Configuration

- **Validate `interval` lower bound** — `interval = 0` in `[wavelog]` causes
  an unthrottled spin-loop; a minimum of ~50 ms should be enforced at startup
  with a clear error message.

- **Make `qso_url` optional** — users who only want rig control (no WSJT-X QSO
  upload) are currently forced to supply a placeholder URL.  `Option<String>`
  with `skip_serializing_if` and a guard in `upload_wsjtx_qso_data` would fix
  this cleanly.

- **Make `[WSJTX]` section optional** — if `qso_url` is also optional (above),
  the `[WSJTX]` block should become optional too so a minimal config for
  rig-control-only use is valid without dummy values.

---

## Robustness

- **Exponential back-off on FLRig poll errors** — currently the poll loop logs
  every error at `info` level and then retries immediately at the configured
  interval.  If FLRig is not running at startup, logs are noisy and the retry
  rate is high.  A capped exponential back-off (e.g. 200 ms → 30 s) with a
  single "FLRig unreachable, retrying…" message would be cleaner.

- **FLRig error log level** — `wavelog.rs` logs FLRig poll failures at `info`
  but Wavelog upload failures at `debug`.  The asymmetry makes `RUST_LOG=info`
  very noisy during startup when FLRig is not yet running.  Both should be
  `debug` (transient operational errors) with a single `warn` on the first
  failure.

---

## Code quality

- **`cw_narrow_index` only matches `Mode::CW`** — the narrow filter bodge is
  IC-703–specific, so matching only `Mode::CW` is correct today.  If a future
  rig has the same FLRig bandwidth-reset bug but uses `CW-U` (Yaesu), the
  bodge will silently not fire.  Document this assumption in the `cw_narrow_index`
  doc comment, and/or extend the match to include `Mode::CW_U` / `Mode::CW_L`
  if a real case arises.

- **`FLRig::get_identifier()` clones the String** — the method could return
  `&str` (borrowing `self.identifier`) at every call site without any semantic
  change.  Low priority; only matters if called in a hot path.

- **CORS wildcard vs. Origin check** (`cat.rs:314`) — `Access-Control-Allow-Origin: *`
  and the optional `wavelog_origin` Origin check are logically consistent (the
  browser rejects the request before a response is sent; the CORS header only
  controls whether the *response* is readable).  The combination could confuse
  a future reader.  A short comment explaining why both coexist would prevent
  a future "simplification" that breaks one of them.

---

## Upstream / packaging

- **Consider adding `cargo deny`** — `cargo deny check` enforces licence
  compatibility and duplicate-crate detection.  Useful given the number of
  transitive dependencies (`quick-xml` is pulled in twice at different versions
  by `config` and `dxr_client`).

- **Pi cross-compilation in CI** — `aarch64-unknown-linux-gnu` is present in
  `.cargo/config.toml` but not built in CI.  If BADARC ever deploys on a Pi,
  add a cross-build step (e.g. using `cross`) so breakage is caught early.

- **CHANGELOG** — there is no changelog.  GitHub's auto-generated release notes
  are adequate for now, but a hand-curated `CHANGELOG.md` becomes valuable once
  external users are tracking upgrades.
