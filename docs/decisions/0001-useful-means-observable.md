---
title: "0001 — \"Useful\" means observable"
type: decision
status: accepted
date: 2026-06-13
deciders: Steve
---

# 0001 — "Useful" means observable

- **Status:** Accepted
- **Date:** 2026-06-13
- **Deciders:** Steve

## Decision

FreshOS is useful as **the most observable computing environment there is** — a
system where you can *see the machine think* — with the 198x vintage-computing
convergence as the content that proves it.

We optimise for two kinds of useful, and we lean to the second:

1. **Useful as an experience.** It teaches and inspires. The value is the demo
   moment and comprehensibility. This is the wrapper.
2. **Useful as a scalpel.** The one thing FreshOS is genuinely best in the world
   at: live, inspectable, scriptable observability of a running system — vintage
   machines included as first-class, living citizens. This is the edge.

We are explicitly **not** building the third kind: a daily driver. FreshOS is not
trying to replace the operating system you reboot into.

## Why

The roadmap quietly defined "useful" as *boots, edits a text file, survives a
crash, persists data* (`docs/plans/2026-04-13-useful-os-roadmap.md`). That is not
useful — it is **complete**. Nobody picks FreshOS over `nano` on Linux to edit a
file. Proving the architecture is real matters, but it gives no one a reason to
reach for FreshOS.

"Useful" and "architecturally finished" are different roads. The substrate
roadmap paves the second one. The thing that makes FreshOS worth existing is the
thing nothing else does: it lets you watch a SID chip and a message bus in the
same introspection view and actually understand how a computer works. That is the
manifesto's own thesis — *understandable magic* — and it is the same thesis
running through all of 198x (Emu198x, Code198x).

`docs/FreshOS-v1-Scope.md` already says the quiet part: v1 is "not a daily
driver... a proof of concept." This decision makes that explicit and turns it from
an apology into a strategy.

## Consequences

- The substrate roadmap (**M1–M7**) is plumbing **in service of** the scalpel, not
  the destination. It still has to happen — you cannot observe a system that does
  not run — but it is not the point.
- Two north-star milestones now lead the project:
  - **★ Observable by Default** — the live message-flow view, the OS as its own
    debugger, the visible capability graph, the always-on latency counter. Today
    this is a 64-entry trace on the dashboard. This is the scalpel's edge.
  - **★ First Living Citizen** — one Emu198x core (a SID, a 6502) running *inside*
    FreshOS as an inspectable, scriptable workspace. The convergence proof.
- **M3 (Tiny Useful Userland) is deprioritised.** "Edit a text file in a shell" is
  daily-driver work we are leaning away from. Keep it minimal or shelve it.
- **M5 (Capabilities) is reframed as content, not just hardening.** The manifesto
  pitch is security you can *see* in the introspection view. The capability graph
  is scalpel material.
- **A thin observability slice is pulled forward** and started before the
  substrate is "done", to avoid the hobby-OS death spiral of infinite plumbing
  that never reaches the demo moment.

## Drift triggers

Stop and re-consult this decision if you catch any of these — in a prompt, a plan,
or your own reasoning:

- "Let's make it a daily driver" / "people should be able to live in it"
- "Add a package manager / networking / a browser" as a near-term priority
- "POSIX compatibility"
- Treating "edit a text file in a shell" as the bar for useful
- Prioritising M1–M7 plumbing while **★ Observable** and **★ First Living Citizen**
  stay untouched
- "Let's just finish the OS first, then do the visualisation"

## References

- `docs/FreshOS-Manifesto.md` — §3 *The System Is Comprehensible*, *The OS is the
  Debugger*, *The Convergence*
- `docs/FreshOS-v1-Scope.md`
- `docs/plans/2026-04-13-useful-os-roadmap.md` — the substrate roadmap this
  decision reframes
- GitHub milestones ★ Observable by Default, ★ First Living Citizen
