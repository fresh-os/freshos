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

This reverses 0001's explicit rejection of the daily driver. How to get there,
and in what order, is not yet decided.

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
- The anti-features in `design-philosophy.md`, especially "no browser engine",
  now conflict with the destination. Resolve them in a follow-up decision;
  do not bypass them quietly.
- **Existing code has no protected status.** Any subsystem may be rewritten
  or deleted if that serves the destination better than rescuing it. This
  overrides the global "rescue beats replace" value for FreshOS, because the
  current code was shaped by demo-first priorities (EL1 shortcuts, a
  duplicated desktop, hand-copied ABIs) that a daily driver cannot keep.
  Rewrites are still decided one subsystem at a time, with the reason
  recorded. "Start over" is not a default.

## Open questions

- Does observability remain FreshOS's distinguishing edge, and so the reason
  anyone would choose it? Or does it become one feature among many?
- What is the minimum a "daily driver" must include (persistence, networking,
  a browser, applications), and which of those does FreshOS build versus port?
- What happens to ★ First Living Citizen?

## Drift triggers

- "It only has to work in the demo"
- Treating an EL1, shared-TTBR0 or no-isolation shortcut as permanent
- Dismissing persistence, crash survival or real input as "not the point"
- Adding a large subsystem (networking, a browser, POSIX) without first
  resolving the conflict with `design-philosophy.md`
