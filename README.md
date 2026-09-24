# FreshOS

A microkernel operating system written in Rust, targeting aarch64 / UEFI. The reference hardware is the Raspberry Pi 4.

The guiding thesis is **understandable magic**: the system should be perceptible to the person using it — small enough to hold in your head, with the interesting parts on the surface rather than buried under abstraction. The full vision is in [`docs/FreshOS-Manifesto.md`](docs/FreshOS-Manifesto.md).

## What works today

FreshOS boots in QEMU (`virt` machine, HVF acceleration on Apple Silicon), and:

- loads its services (`init`, `pong`, `pulse`, `fault`) as **separate ELF files** from the boot disk. `init` supervises them and restarts them if they crash;
- passes messages through **typed, bounded IPC channels**, recording every message in a trace buffer;
- runs a **composited desktop**: a shell, and a dashboard that draws the live message flow between tasks as it happens;
- preempts tasks on a **1000 Hz timer**, with an assembly context switch.

The desktop still runs at EL1, because QEMU's HVF acceleration traps the TLB-maintenance instructions that per-task page tables need. Real per-task isolation at EL0 is the next major piece of work, on real hardware. Input currently arrives over the serial console.

## Build and run

Requires an Apple Silicon Mac, Rust nightly (pinned by `rust-toolchain.toml`) and QEMU (`brew install qemu`).

```bash
# Build the kernel and services, then boot in QEMU (serial on stdio)
./run-arm.sh

# Release build
./run-arm.sh --release
```

QEMU loads the UEFI firmware (`edk2-aarch64-code.fd`) via pflash, not `-bios`.

## Architecture

A fuller tour lives in [`docs/`](docs/): the manifesto, the v1 scope, the current status, and the design decisions. In brief:

- **The kernel handles scheduling, memory, IPC and interrupt routing.** Everything else is meant to live in userspace services.
- **Services are ELF files** that the kernel loads from the EFI system partition and spawns under a supervisor.
- **Every message is observable.** The kernel attributes each message to a named sender and receiver, and the dashboard draws the traffic live.
- **x86_64 was dropped** in favour of aarch64 (decision 0003). The design lessons from its ring-3 isolation work are in [`docs/x86-lessons.md`](docs/x86-lessons.md).

## Performance contracts

The manifesto sets hard targets, not aspirations — sub-5 ms input-to-photon, sub-1 µs IPC round-trip, zero compositor frame misses. None of these has been measured on real hardware yet. The Raspberry Pi 4 is where they will be.

## Status

Early, and active. The kernel boots and runs everything above; much of the manifesto is still ahead. Built carefully, mostly for the love of it.

## Licence

[MIT](LICENSE) © 2026 Steve Hill
