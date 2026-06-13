# FreshOS

A microkernel operating system written in Rust, targeting x86_64 / UEFI.

The guiding thesis is **understandable magic**: the system should be perceptible to the person using it — small enough to hold in your head, with the interesting parts on the surface rather than buried under abstraction. The full vision is in [`docs/FreshOS-Manifesto.md`](docs/FreshOS-Manifesto.md).

## What works today

FreshOS boots on QEMU and on real UEFI hardware, and:

- runs user-mode tasks in **ring 3** with **per-process page tables** — each task sees only its own code, stack, and explicitly granted resources;
- passes messages through **typed, bounded IPC channels** delivered as syscalls — no direct kernel calls from user mode;
- preempts via a **PIT timer at 100 Hz**, with an assembly context switch;
- runs a **userspace keyboard driver** and a **graphical shell** that renders straight to the framebuffer.

The kernel handles scheduling, memory, IPC, and interrupt routing. Everything else — drivers included — lives in userspace. The kernel reads port `0x60` and forwards a raw scancode as an IPC message; it doesn't know what a keyboard is.

## Build and run

Requires Rust nightly (pinned by `rust-toolchain.toml`) and QEMU with OVMF.

```bash
# Build the kernel
RUSTUP_TOOLCHAIN=nightly cargo build --package freshos-kernel

# Build and boot in QEMU (VNC on localhost:5900)
./run.sh

# Release build
./run.sh --release
```

UEFI firmware comes from `brew install qemu` (`edk2-x86_64-code.fd`); QEMU loads it via pflash, not `-bios`.

## Architecture

A fuller tour lives in [`docs/`](docs/) — the manifesto, the v1 scope, a beginner's boot walkthrough, and a syscall-flow analysis. In brief:

- **Capability-style resource grants** through page-table mappings — the framebuffer is mapped into the shell's address space but not the keyboard driver's. The kernel decides who sees what.
- **syscall/sysret** for the user–kernel boundary, configured through the `IA32_STAR` / `LSTAR` / `FMASK` MSRs.
- **2 MiB huge-page** identity mapping, with per-task page directories overriding the shared kernel ones for regions that need user access.
- **Blocking IPC** that disables interrupts before the empty-check to avoid a race, then sleeps with `sti; hlt`; the keyboard IRQ wakes a blocked task immediately, without waiting for the next timer tick.

## Performance contracts

The manifesto sets hard targets, not aspirations — sub-5 ms input-to-photon, sub-1 µs IPC round-trip, zero compositor frame misses. The syscall path (~200–500 ns on real hardware) already meets the IPC target; scheduling latency at 100 Hz does not yet meet input-to-photon. The analysis is in [`docs/Syscall-Flow.md`](docs/Syscall-Flow.md).

## Status

Early, and active. The kernel boots and runs everything above; much of the manifesto is still ahead. Built carefully, mostly for the love of it.

## Licence

[MIT](LICENSE) © 2026 Steve Hill
