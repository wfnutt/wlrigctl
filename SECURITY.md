# Security Policy

## Supported Versions

v0.4.y branch is currently being supported with security updates.

| Version | Supported          |
| ------- | ------------------ |
| 0.4.y   | :white_check_mark: |
| < 4.0   | :x:                |

## Reporting a Vulnerability

Vulnerability reports are gratefully received via: https://github.com/wfnutt/wlrigctl/security/advisories/new

This is a solo, part-time hobby-project for amateur radio. However, the current author/maintainer is motivated to keep wlrigctl as safe as is reasonable, given inevitable constraints.

The current intent is to deal with reported vulnerabilities within one week, in the majority of cases. A report will be considered in light of the stated use case: solo machine, running both wavelog and flrig locally, behind a firewall.
If it is not possible to address the vulnerability directly in the short term, affected functionality may be disabled as a short-term mitigating measure.

If you have decided to run wlrigctl on a public or unsecured network such as the internet and someone has used CAT control to manipulate your transceiver, or sent dummy data to wlrigctl to pollute your log with spurious FT8 contacts, I am afraid you are on your own.
