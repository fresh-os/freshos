---
title: "0003 — Raspberry Pi 4 is the reference hardware; x86_64 is dropped"
type: decision
status: accepted
date: 2026-09-24
deciders: Steve
---

# 0003 — Raspberry Pi 4 is the reference hardware; x86_64 is dropped

- **Status:** Accepted
- **Date:** 2026-09-24
- **Deciders:** Steve

## Decision

- FreshOS targets the **Raspberry Pi 4** (BCM2711) as its reference hardware,
  booted through the community `pftf/RPi4` UEFI firmware.
- **QEMU `virt` with HVF** remains the day-to-day development loop.
- The **Pi 5** is the next board, reached through the same board layer.
- **x86_64 support is dropped.** Its code is removed, not kept on life
  support.

## Why

0002 makes a daily driver the destination, which needs real hardware that
people can buy.

- The Pi 4 has mature UEFI firmware, so the existing UEFI kernel needs the
  least change to boot.
- Its peripherals sit on the main chip and are covered by Broadcom's BCM2711
  peripherals document.
- Bare metal removes the HVF limitation that forces the desktop to run at EL1
  without isolation.
- x86_64 no longer builds on current nightly: `x86_64` 0.15.4 does not
  implement the new methods on the unstable `Step` trait.
- x86_64 is the reason the desktop is written twice. Maintaining two
  architectures slows the route to a daily driver, and aarch64 is where the
  working code already is.

## Alternatives considered

- **Pi 5 first.** Rejected for now: its UEFI support is uncertain, most I/O
  sits behind the partly documented RP1 chip, and QEMU cannot emulate it.
- **Keep x86_64 as the reference path.** Rejected. This costs something real:
  x86 is the only path with working ring-3 isolation, per-task page tables and
  a syscall boundary. That design has to be rebuilt at EL0 on the Pi.

## Consequences

- Add a board layer under `arch/aarch64/` so that QEMU `virt` and the Pi 4
  differ only in addresses and drivers: UART, GIC, timer, framebuffer and
  memory map.
- The first milestone is serial output on a real Pi 4.
- Remove the x86_64 code:
  - `gdt`, `idt`, `pic`, `paging`, `scheduler`, `syscall`, `keyboard`,
    `mouse`, `speaker` and `tsc` from `kernel/src/`
  - `arch/x86_64/`
  - the `x86_tasks` module in `main.rs`
  - `run.sh`
  - the `x86_64` crate dependency and target
- `docs/Boot-Walkthrough.md` and `docs/Syscall-Flow.md` become historical.
- Before deleting, carry forward the design lessons that aren't specific to
  x86: race-free blocking IPC, per-task address spaces and the syscall
  boundary.

## Drift triggers

- "Add x86 back for testing / for real PCs"
- Hardcoding QEMU `virt` addresses outside the board layer
- Relying on Pi 5-only features before the Pi 4 path works
