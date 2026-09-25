# AGENTS.md

Guidance for coding agents working in the FreshOS kernel repository.

## What FreshOS is, and what it is for

FreshOS is a Rust microkernel that boots from UEFI on **aarch64**. Its guiding idea is "understandable magic": one person should be able to understand each subsystem by reading its source in one sitting.

**Strategic direction (binding):** `docs/decisions/0002-daily-driver-is-the-destination.md` (proposed). In the long term, FreshOS should become an OS people can really use day to day. 0002 partly supersedes `0001-useful-means-observable.md`: 0001's rejection of the daily driver no longer holds, but its observability milestones (**★ Observable by Default**, **★ First Living Citizen**) stand until 0002's open questions are settled. The M1–M7 substrate roadmap (`docs/plans/2026-04-13-useful-os-roadmap.md`) is the route to the destination again. Treat shortcuts that block real use, such as the EL1 desktop without isolation, as temporary. The order of work is the v1 ladder in `docs/FreshOS-v1-Scope.md` (honest kernel on the Pi 4, storage, usable desktop, basic games, networking, web). Don't start a later rung while an earlier one is unfinished. **Responsiveness and observability are the twin goals:** new work must be visible in the message-flow view and measurable against the performance contracts below. FreshOS never builds a browser engine and never targets POSIX compatibility. Emu198x and the other 198x projects run natively via `no_std` + `alloc` cores, not a `std` port or a Linux VM (`docs/decisions/0004-198x-projects-run-natively.md`); changes to a 198x repo's structure are decided in that repo, not from FreshOS work. The OS is reachable by agents over MCP through a bridge service that holds capabilities like any other service, never a backdoor (`docs/decisions/0005-reachable-over-mcp.md`). Long-range ideas (editions, the physical world) live in `docs/FreshOS-Horizon.md` and are out of scope. Existing code has no protected status: rewriting a subsystem is allowed, but decide it one subsystem at a time and record the reason. Check 0002's *Drift triggers*.

**Target hardware (binding):** `docs/decisions/0003-raspberry-pi-4-base-drop-x86.md`. The **Raspberry Pi 4** is the reference hardware, booted through the `pftf/RPi4` UEFI firmware. QEMU `virt` with HVF is the development loop, and the Pi 5 comes next. **x86_64 has been dropped. Don't reintroduce it.** Its design lessons (isolation model, syscall boundary, known gaps) are in `docs/x86-lessons.md`. Board-specific addresses live in the board layer, `kernel/src/arch/aarch64/board/`, one file per board, never hardcoded elsewhere.

**Where work stands:** read the status block at the top of `docs/Where-We-Are.md` (everything below it is April history). Work is tracked as GitHub issues and milestones on `fresh-os/freshos`.

## How it runs today

| | QEMU `virt` + HVF (works) | Raspberry Pi 4 (not yet booted) |
|---|---|---|
| Purpose | Day-to-day development loop on Apple Silicon | Reference hardware |
| Desktop tasks | `kernel/src/arm_tasks.rs`, run at **EL1** with direct function calls | Should run at EL0 with per-task page tables |
| Input | Serial UART only: type into the terminal running QEMU | USB keyboard on the VL805 xHCI controller (needs a PCIe driver, then xHCI) |
| Display | Composited to `ramfb` (virtio-GPU scanout is invisible under `-display cocoa`) | UEFI GOP framebuffer from the firmware |

On HVF the desktop runs at EL1 because HVF traps the `tlbi` instructions that per-task user page tables need. The one exception is the `fault` service, a narrow **EL0 proof path**: real `SVC` entry and contained lower-EL faults, but a shared, patched `TTBR0`. It does not yet isolate each task. Don't describe FreshOS as isolated.

`.cargo/config.toml` makes `aarch64-unknown-uefi` the default build target and uses `build-std` for `core` and `alloc`.

## Commands

The toolchain is nightly, pinned by `rust-toolchain.toml` (channel only, no date). The run scripts call `rustup run nightly`. If a build reports "can't find crate for core", check that nothing has overridden the toolchain.

```bash
./run-arm.sh [--release]    # Builds the kernel and all userbins, stages esp-arm/, boots QEMU aarch64 + HVF (cocoa window, serial on stdio)
./run-demo.sh               # Same as run-arm.sh; exits with an error on hosts other than Apple Silicon
./run-arm.sh -display none  # Headless: extra arguments go to QEMU, and the last -display wins

# Build one piece. Always pass both --package and --target, the way run-arm.sh does.
rustup run nightly cargo build --package freshos-kernel --target aarch64-unknown-uefi
rustup run nightly cargo build --package freshos-kernel --target aarch64-unknown-uefi --no-default-features --features board-rpi4   # Pi 4 kernel
rustup run nightly cargo build --package freshos-pong   --target aarch64-unknown-none   # same for init, pulse, fault

rustup run nightly cargo clippy --package freshos-kernel
```

- **Tests:** there are none. To verify a change, boot it and read the serial log. Every boot stage prints a line, from `Boot init: N bytes from ESP` through to `Scheduler started`.
- **Warnings:** the tree is not warning-clean under `build` or `clippy`. Don't add new warnings. Clean up existing ones only when that is your task.
- **Firmware:** comes from `brew install qemu` (`/opt/homebrew/share/qemu/edk2-*`). QEMU loads it through pflash, not `-bios`. `run-arm.sh` copies a writable `edk2-arm-vars.fd` into the repo on first run.

## Architecture

### Kernel layout

- **Portable modules in `kernel/src/`:**
  - `ipc`, `frame_alloc`, `heap`, `framebuffer`, `font`/`font_aa`, `metrics`, `task_names`, `serial`
  - `scripting` (Rhai, `no_std`)
  - `elf`, `init_abi` and `service_abi`, used by the service loader
  - `arm_tasks`, the desktop
- **`kernel/src/arch/aarch64/board/`:** the board layer. One file per board (`qemu_virt.rs`, `rpi4.rs`) holding its addresses: PL011 UART, GIC, and the virtio-mmio window if any. Exactly one `board-*` cargo feature selects it (`board-qemu-virt` is the default), and `compile_error!` rejects zero or two.
- **`kernel/src/arch/aarch64/`:** exceptions, GIC, timer, context switch and scheduler, paging, syscalls and a virtio-GPU driver. It is re-exported as `arch::*`, and portable code calls through `arch::`.
- **`main.rs`:** the UEFI `#[entry]`. It picks the largest graphics mode up to 1920×1200, loads the service ELFs from the ESP, exits boot services, then brings up the kernel and starts the scheduler.

The chrome (menu bar, taskbar and stats overlay) is drawn inside the compositor task in `arm_tasks.rs`. `../docs/decisions/chrome-as-services.md` plans to move each piece into its own service.

### External services (`userbins/`)

`init`, `pong`, `pulse` and `fault` are `#![no_std]`, `#![no_main]` ELFs built for `aarch64-unknown-none`. The flow is:

1. `run-arm.sh` copies each one to `esp-arm/EFI/FreshOS/<NAME>.ELF`.
2. `main.rs` loads each file through UEFI (`load_esp_file`) *before* `exit_boot_services`. If a file is missing, the kernel uses a built-in stand-in.
3. `init` is the supervisor. It enumerates services, autostarts them, and restarts supervised ones after they exit.

A service's entry point is `_start(api: *const InitApi | *const ServiceApi)`. That argument is a **table of `extern "C"` function pointers**, not a syscall interface. **The ABI structs are copied by hand.** Each `#[repr(C)]` type in `kernel/src/init_abi.rs` and `kernel/src/service_abi.rs` is redeclared in every userbin. No shared crate exists. When you change a layout, change every copy in the same commit.

To add a service, touch all of these:

- the workspace `members` list
- the build and copy steps in `run-arm.sh`
- a `load_esp_file` call in `main`
- a `SERVICE_*` id and record in `init_abi.rs`

The spawn dispatch in `init_abi.rs` calls `task_names::register`. That call gives the task the name the flow view shows.

### IPC and observability

- `ipc.rs`: bounded channels carry a type tag and 32 bytes of inline payload. `send` never blocks. `recv` blocks and wakes when a message arrives. Interrupts are masked between the empty check and the sleep (`arch::block_current_task`), so a wakeup cannot be lost.
- Every send is recorded in a 64-entry trace ring. The destination is attributed through the channel's consumer, even when delivery was buffered.
- The dashboard's live message-flow diagram (OBS.1, in `arm_tasks.rs`) draws that trace as nodes and arcs, labelled from the kernel-owned `task_names` registry. Keep the identities it shows honest: a name must come from the kernel, never be guessed by the viewer.
- `metrics.rs` feeds the stats overlay: latency histograms and damage rectangles.

### Memory

- `frame_alloc`: a bitmap over the UEFI memory map.
- `heap`: a 1 MiB linked-list allocator. It exists and is used; Rhai needs `alloc`. Older docs that say "no post-boot heap" are stale.

## Gotchas

- **Both boards use GICv2** (QEMU under HVF gives v2, not v3; the Pi 4 has a GIC-400). The timer is the **virtual** timer, PPI INTID 27, and its frequency is read from `CNTFRQ_EL0`, so neither is board-specific.
- **The Pi 4 addresses in `board/rpi4.rs` are unverified on hardware.** The kernel also relies on the firmware having initialised the PL011; it never sets the baud rate itself.
- **Edition 2024:**
  - An `unsafe fn` body needs explicit `unsafe {}` blocks.
  - Access `static mut` through `addr_of_mut!`, because `static_mut_refs` denies by default.
  - Write `#[unsafe(no_mangle)]`.
- **Firmware and device tables can be misaligned.** On x86, ACPI XSDT entries and virtio PCI capabilities were. Read such structures with `read_unaligned`.

## Performance contracts

The manifesto sets hard targets, not aspirations:

- sub-5 ms input-to-photon, never more than one frame
- sub-3 ms audio round-trip
- sub-1 µs small-message IPC round-trip
- zero missed compositor frames

Measure before you optimise; `metrics.rs` and the stats overlay exist for this.

## Documentation map

This repository and the sibling `../docs` repository are both **public** (`fresh-os/freshos` and `fresh-os/docs`).

- `docs/` in this repo holds:
  - the manifesto, the v1 scope and the demo script
  - `Where-We-Are.md`, `x86-lessons.md`, dated `plans/` and numbered `decisions/`
  - two historical x86 documents: `Boot-Walkthrough.md` and `Syscall-Flow.md`
- `../docs` holds development notes: architecture decisions (`aarch64-port`, `chrome-as-services`, `design-philosophy`), kernel notes and a running `log.md`.

Treat decisions in both places as binding.
