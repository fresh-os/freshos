---
title: "0006 — Every app and service ships as its own binary"
type: decision
status: accepted
date: 2026-09-26
deciders: Steve
---

# 0006 — Every app and service ships as its own binary

- **Status:** Accepted
- **Date:** 2026-09-26
- **Deciders:** Steve

## Decision

The kernel binary contains only the kernel: boot, memory and address
spaces, scheduling, IPC, interrupt routing, and the syscall boundary.
Every app, driver and service, including the compositor, shell, dashboard,
input drivers and the MCP bridge, is a separate ELF binary. `init` loads
and supervises it, and it runs at EL0 in its own address space.

**The kernel requires only `init`.** If `INIT.ELF` is missing, the kernel
prints a clear message on serial and halts. Every other binary is `init`'s
policy: a missing one is logged by `init` and visible over MCP, and the rest
of the system keeps running. There are no built-in fallbacks.

## Why

Last time, the desktop's apps were compiled into the kernel (`arm_tasks.rs`,
and `x86_tasks` before it). That made isolation and capabilities fictional:
every "service" could call kernel functions and touch kernel memory
directly. Any app change rebuilt the kernel, and the desktop existed twice.
It contradicts the manifesto's "drivers as ring 3 services" and the
chrome-as-services decision.

Even the services that were already separate binaries (`init`, `pong`,
`pulse`) ran at EL1 and called kernel functions through a table of function
pointers. Separate binaries are not enough on their own: they must also run
isolated, reaching the kernel only through syscalls.

A fallback service built into the kernel duplicates the real one and hides
packaging mistakes. A clear failure is better.

## Consequences

- Services reach the kernel only through syscalls: no kernel function calls
  and no shared kernel statics.
- A shared ABI crate (`freshos-abi`) replaces every hand-copied `#[repr(C)]`
  struct. The kernel uses it too. The rung 4 game SDK builds on it.
- The built-ins in `arm_tasks.rs` and `mcp.rs` move out during rung 1, and
  their built-in fallbacks are deleted, not kept as a safety net.
- Packaging is load-bearing: every boot depends on the right binaries being
  on the ESP. Staging scripts must be reliable, and failures must name the
  missing file.

## Drift triggers

- "Just add it as a kernel task for now"
- A built-in fallback for when an ELF is missing
- A service calling a kernel function or reading kernel memory directly
- A userbin redeclaring ABI types by hand
- A separate binary that runs at EL1 "temporarily"
