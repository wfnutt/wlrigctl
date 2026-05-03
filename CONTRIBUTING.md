# Contributing

Sensible contributions are welcome. This is a small club project, not a
product, so please keep that in mind before opening issues or PRs.

## Before you start

Open an issue first if you're planning anything beyond a trivial fix.
There may already be a reason something works the way it does — check
CLAUDE.md for known quirks and design decisions before assuming
something is a bug.

## What "sensible" means

- `make ci` passes before you open a PR
- New behaviour has test coverage
- Code follows the existing style — one clear way of doing things,
  no cleverness for its own sake
- Commit messages say *why*, not just *what*

## What won't be accepted

- Contributions that break the core deployment assumption: wavelog,
  wlrigctl, and flrig all run on the same machine, behind a firewall
- Frequency allowlist changes — the UK band plan is intentional;
  non-UK users should fork and recompile
- Unsolicited refactoring, abstractions, or "improvements" to things
  that aren't broken

## In short

Don't be a LID.



73 de M7CLG
