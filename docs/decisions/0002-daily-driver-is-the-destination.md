---
title: "0002 — A daily driver is the long-term destination"
type: decision
status: proposed
date: 2026-09-24
deciders: Steve
supersedes: 0001 (in part)
---

# 0002 — A daily driver is the long-term destination

- **Status:** Proposed
- **Date:** 2026-09-24
- **Deciders:** Steve

## Decision

FreshOS should be able to become an operating system that people really use:
one you could reboot into and live in. That is the long-term destination.

This reverses 0001's explicit rejection of the daily driver. The route is the
v1 ladder in `docs/FreshOS-v1-Scope.md`: an honest kernel on the Pi 4, then
storage, a usable desktop, basic games, networking (email first), and the web.

**Responsiveness and observability are FreshOS's twin goals**, and the reason
to choose it over any other daily driver:

- **Responsiveness:** the manifesto's performance contracts are guarantees,
  shown on screen. Input reaches the display within one frame, a small IPC
  round trip takes under 1 µs, and the compositor never misses a frame.
- **Observability:** you can see the machine think. Every message,
  capability and latency is visible in the running system.

A daily driver that gives up either one is not FreshOS.

★ First Living Citizen is answered by 0004: Emu198x and the other 198x
projects run natively on FreshOS.

## Why

An OS that nobody can ever really use is pointless, however observable it is.
0001 treated "daily driver" as a trap to avoid. In practice, ruling it out
capped what FreshOS could ever be. Omarchy shows there is appetite for an
opinionated, single-author desktop. FreshOS would have to build its own
substrate rather than configure Linux's, so the path is longer, but the
destination is legitimate.

## Consequences

- The substrate roadmap (M1–M7, `docs/plans/2026-04-13-useful-os-roadmap.md`)
  becomes the route to the destination again, not merely plumbing.
- M3 (Tiny Useful Userland) is no longer deprioritised.
- Shortcuts that permanently prevent real use must be named as temporary.
  The main one is aarch64 running the desktop at EL1 without per-task
  isolation.
- The web comes through an embedded renderer (Servo) or a sandboxed Linux VM,
  as the manifesto's *Web Browsing* section already says. FreshOS still never
  builds a browser engine, and POSIX compatibility is still not a goal.
- **Existing code has no protected status.** Any subsystem may be rewritten
  or deleted if that serves the destination better than rescuing it. This
  overrides the global "rescue beats replace" value for FreshOS, because the
  current code was shaped by demo-first priorities (EL1 shortcuts, a
  duplicated desktop, hand-copied ABIs) that a daily driver cannot keep.
  Rewrites are still decided one subsystem at a time, with the reason
  recorded. "Start over" is not a default.

## Drift triggers

- "It only has to work in the demo"
- Treating an EL1, shared-TTBR0 or no-isolation shortcut as permanent
- Dismissing persistence, crash survival or real input as "not the point"
- Building a browser engine, or pursuing POSIX compatibility (both remain
  anti-features)
- Working on a later rung of the v1 ladder while an earlier one is unfinished
- Adding a feature that cannot be seen in the flow view or measured against
  the performance contracts, or deferring that measurement to "later"
