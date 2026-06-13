---
title: "plan: roadmap from FreshOS demo to useful OS"
type: plan
date: 2026-04-13
---

# Roadmap From FreshOS Demo to Useful OS

## What "Useful" Means

For FreshOS, "useful" should mean:

- it boots into `init`
- it mounts a writable root filesystem
- it launches the shell, compositor, and dashboard as external binaries
- the shell can create, edit, list, read, and delete files
- a crashed service can be inspected and restarted without rebooting
- data survives reboot

That is enough to make FreshOS a real operating system for simple local work.

It does **not** need to mean:

- web browser
- package manager
- broad hardware support
- daily-driver application set
- full networking stack

Those can come later. The first target is a system that can do a small amount of real work, persist it, and survive faults.

## Two Checkpoints

There are really two targets here.

### Checkpoint A — Minimally Useful

FreshOS can boot, launch services from disk, store files, edit them, and recover from service failure.

### Checkpoint B — Architecturally Honest

FreshOS does all of that while enforcing handles/capabilities and running core services in real isolated userspace on the truth path.

Checkpoint A should come first. Checkpoint B must follow quickly so the project does not drift into "convenient demo OS" territory.

## Ground Rules

- Keep `aarch64 + HVF` as the fast iteration and demo path.
- Keep `x86_64` as the reference path for strict isolation until ARM can prove the same thing.
- Prefer boring formats and protocols over invention: `ELF`, a simple block device, and a simple filesystem.
- Every milestone must end with a concrete demo, not just internal refactoring.
- Avoid major new UI work until process, storage, and service lifecycle are solid.

## Milestone 1 — Generic Service Runtime

**Goal:** replace one-off boot logic with a real service model.

**Tasks:**

- Generalise the external ELF loader so `init` can launch arbitrary services from descriptors rather than hardcoded service cases.
- Define a process/service record: name, pid/task id, binary path, args, restart policy, exit reason, restart count.
- Add a small service manifest format or compiled-in launch table owned by `init`.
- Add per-service logs or at least a ring buffer with service attribution.
- Add service status inspection: running, exited, faulted, restarting.
- Externalise one consequential service beyond probes, ideally the shell or dashboard.

**Done when:**

- `init` launches at least four services through one generic path.
- Service crashes and clean exits both produce visible exit reasons.
- A service can be restarted without kernel special-casing.

**Demo:**

- Boot, show service list, kill one service, watch `init` restart it.

## Milestone 2 — Writable Root Filesystem

**Goal:** make the system remember things.

**Tasks:**

- Pick one block-device path for the primary route.
- On `aarch64 + HVF`, prefer a simple virtio block device over inventing more boot-time hacks.
- Keep the filesystem choice boring and swappable.
- For speed, start with a simple filesystem the host can inspect easily.
- Add a narrow VFS surface:
  - `open`
  - `read`
  - `write`
  - `create`
  - `mkdir`
  - `unlink`
  - `readdir`
- Mount a root filesystem during boot.
- Move service binaries off the ESP-only path and into a root-owned service directory.
- Add persistence smoke tests.

**Done when:**

- The shell can create a file.
- The file survives reboot.
- At least one service binary is loaded from the root filesystem rather than only from the ESP.

**Demo:**

- Create `notes.txt`, reboot, read it back.

## Milestone 3 — Tiny Useful Userland

**Goal:** let the machine do small real tasks.

**Tasks:**

- Make the shell a normal external binary.
- Add a minimal command set:
  - `ls`
  - `cat`
  - `mkdir`
  - `rm`
  - `echo`
  - `write` or a tiny line editor
  - `ps`
  - `kill`
  - `log`
  - `restart`
- Add simple path handling.
- Add clear stderr/stdout style reporting, even if it is only conceptual at first.
- Add service-manager commands backed by `init` state.

**Done when:**

- A user can navigate files, edit a text file, inspect running services, and restart a dead one.

**Demo:**

- Create a note, read it, kill the shell or dashboard, restart it, continue working.

## Milestone 4 — Kernel Shrinks to Policy-Free Core

**Goal:** move the visible OS out of the kernel.

**Tasks:**

- Externalise the shell.
- Externalise the dashboard.
- Externalise the compositor, if needed in stages.
- Externalise the input path as far as practical on the primary route.
- Reduce the kernel's job to:
  - boot
  - memory
  - scheduling
  - IPC
  - storage primitives
  - process loading
  - syscall enforcement
- Replace ad hoc boot-policy code with `init`-owned startup ordering.

**Done when:**

- The kernel boots `init`, and `init` is the component that makes the system become a desktop.
- The visible UX is no longer secretly hardcoded in `main.rs`.

**Demo:**

- Boot to desktop through external services only.

## Checkpoint A — Minimally Useful OS

FreshOS reaches "actually useful" when Milestones 1 through 4 are done.

At that point it should be able to:

- boot
- load services from disk
- persist files
- let the user edit simple text
- show service status
- survive service crashes without full reboot

That is enough to justify calling it a small but real operating system.

## Milestone 5 — Handles, Capabilities, and Syscall Hardening

**Goal:** make the implementation match the architecture claims.

**Tasks:**

- Replace global numeric channel IDs with per-process handle tables.
- Make surfaces, files, logs, and channels kernel-owned objects referenced by handles.
- Add explicit grant, inherit, and revoke rules at spawn time.
- Add `copyin`/`copyout` helpers for all syscall buffer traffic.
- Validate user pointers before dereference.
- Kill the offending process on invalid user pointers or illegal handle use.
- Audit existing direct kernel dereferences of userspace memory.

**Done when:**

- A service cannot touch a channel, surface, or file it was not granted.
- Bad user pointers kill the caller instead of risking kernel corruption.

**Demo:**

- Deliberately pass an invalid pointer and show process-only failure.

## Milestone 6 — Real Isolation on the Truth Path

**Goal:** stop relying on prototype-mode compromises.

**Tasks:**

- Choose the truth path for strict isolation.
- Near-term, that is probably still `x86_64`.
- Keep ARM/HVF as accelerated mode until per-task `TTBR0` switching is proven.
- Run at least shell plus one sibling service in real isolated userspace on the truth path.
- Unify exit reasons and supervision across EL0/ring 3 faults.
- Add tests for:
  - page fault in one process
  - illegal syscall in one process
  - sibling survival
  - restart after fault

**Done when:**

- Two external services run isolated.
- One can fault without taking the other down.
- The same service lifecycle model works on the strict path, not only the accelerated one.

**Demo:**

- Crash a real isolated shell process and restart it while the rest of the system stays up.

## Milestone 7 — Distribution and Repeatability

**Goal:** make the system easy to build, boot, and evaluate.

**Tasks:**

- Produce one bootable disk image with kernel, init, services, and root filesystem.
- Add a single build command for the primary demo path.
- Add a second command for the strict reference path.
- Seed the image with sample files and service config.
- Add smoke tests for boot, service launch, file persistence, and restart behavior.
- Document the expected boot flows.

**Done when:**

- A fresh clone can produce a bootable image and reach a usable shell or desktop with one command.

**Demo:**

- Clean checkout to bootable image without manual file copying.

## After That

Only after the above should FreshOS spend real effort on:

- networking
- richer applications
- package/update flows
- broader device support
- deeper graphics work

Those are valuable, but they are not the blockers between "promising demo" and "useful OS."

## Recommended Execution Order

1. Milestone 1 — generic service runtime
2. Milestone 2 — writable root filesystem
3. Milestone 3 — tiny useful userland
4. Milestone 4 — externalise the visible OS
5. Checkpoint A — call it useful
6. Milestone 5 — handle/capability hardening
7. Milestone 6 — real isolation on the truth path
8. Milestone 7 — distribution and repeatability

## What Not To Do Next

- Don't build a package manager first.
- Don't build networking first.
- Don't build a custom filesystem first if a simple one will get persistence landed.
- Don't spend the next month on desktop polish.
- Don't let ARM/HVF convenience erase the need for a strict isolation proof path.

## Short Version

FreshOS becomes useful when it can load services from disk, persist files, let the user manipulate them, and recover from faults without rebooting. After that, make the security and isolation story honest.
