---
title: "plan: Apple Silicon demo path and post-port execution plan"
type: plan
date: 2026-04-12
---

# Apple Silicon Demo Path and Execution Plan

## Summary

FreshOS now has two useful paths:

- **Primary demo path:** `aarch64` on QEMU `virt` with `HVF` on Apple Silicon
- **Architecture reference path:** `x86_64` on UEFI with the stricter ring 3 + `syscall/sysret` microkernel path

The Apple Silicon path should become the default workflow for interactive development because it removes the `TCG` bottleneck. The x86_64 path should remain the place where isolation, privilege boundaries, and legacy hardware assumptions are validated.

That split is healthy as long as it is named explicitly.

## Current Truth

- The aarch64/HVF path is the best platform for compositor and UX iteration.
- The aarch64/HVF path is **not yet** the full proof path for user-mode isolation.
- Under HVF, the desktop currently runs tasks at `EL1` because `tlbi` trapping blocks the intended `EL0` page-table flow.
- The x86_64 path remains the strongest evidence for the intended microkernel model.

This means FreshOS should optimise for **speed of iteration on ARM** without pretending the security/isolation story is already complete there.

## Milestones

### Milestone 1 — Make Apple Silicon the Obvious Demo Workflow

**Goal:** A contributor on Apple Silicon should reach the fast path by default.

**Repo changes:**

- Default `cargo build` target is `aarch64-unknown-uefi`
- `run-demo.sh` dispatches to `run-arm.sh` on Apple Silicon
- `run.sh` stays explicit about the x86_64 path
- Toolchain metadata declares both UEFI targets
- Docs explain that ARM/HVF is the primary demo path and x86_64 is the reference path

**Done when:**

- `cargo build` defaults to the ARM target
- `./run-demo.sh` is the shortest path to a visible desktop on Apple Silicon
- No doc implies that ARM/HVF already proves final `EL0` isolation

### Milestone 2 — Measurement Before Optimisation

**Goal:** Replace intuition with published numbers.

Add instrumentation for:

- Frame time
- Input-to-photon latency
- IPC round-trip latency
- Scheduler wake latency

**Done when:**

- The dashboard shows at least coarse real measurements
- `wiki/kernel/performance.md` is updated with measured values, not only targets

### Milestone 3 — Decide Whether `ramfb` Is Enough

**Goal:** Avoid premature GPU-driver work.

Collect data on the ARM/HVF path:

- Full-frame present time
- Dirty-region update cost
- CPU time spent compositing vs presenting

**Decision rule:**

- If `ramfb` is already interactive enough for the demo, keep it and move on
- If presentation dominates frame cost, continue the existing virtio-GPU plan

### Milestone 4 — Close the Truth Gap

**Goal:** Make the implementation match the architectural claims.

Priorities:

- Replace global channel IDs with per-task handles
- Add grant/revoke for channels and surfaces
- Validate syscall pointers against task address-space rules
- Clearly separate "implemented" from "target" in docs

**Done when:**

- "Capability" means an actual kernel-enforced handle, not a planned abstraction
- User pointers are validated before kernel dereference

### Milestone 5 — Load One Real Process

**Goal:** Prove FreshOS can host an independently loadable task.

Scope:

- Load one ELF from disk
- Map code/data/stack for that task
- Start it
- Kill it
- Restart it without rebooting the whole system

This milestone matters more than adding another built-in task.

## Non-Goals For Now

- `virgl`
- broad hardware support
- deep filesystem work
- major desktop polish passes before measurement
- stronger marketing language than the kernel can currently defend

## Working Rule

Use Apple Silicon/HVF to make the system feel alive. Use x86_64 to keep the architecture honest.
