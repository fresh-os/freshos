# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

FreshOS is a microkernel operating system written in Rust, targeting x86_64/UEFI. The guiding thesis is "understandable magic" — the architecture should be perceptible to the user. See `docs/FreshOS-Manifesto.md` for the full vision.

The kernel is functional: it boots on QEMU (and real UEFI hardware), runs user-mode tasks in ring 3 with per-process page tables, provides typed message-passing IPC through syscalls, and has a userspace keyboard driver and a graphical shell rendering to the framebuffer.

## Build and Run

Requires: Rust nightly (managed by `rust-toolchain.toml`), QEMU with OVMF.

```bash
# Build
RUSTUP_TOOLCHAIN=nightly cargo build --package freshos-kernel

# Build and launch in QEMU (VNC on localhost:5900)
./run.sh

# Release build
./run.sh --release
```

**Note:** The `RUSTUP_TOOLCHAIN` env var may override `rust-toolchain.toml`. If builds fail with "can't find crate for core", set `RUSTUP_TOOLCHAIN=nightly` explicitly.

UEFI firmware: `/opt/homebrew/share/qemu/edk2-x86_64-code.fd` (from `brew install qemu`). QEMU uses pflash, not `-bios`.

## Project Structure

```
kernel/src/
  main.rs           Entry point, boot sequence, userspace task code
  gdt.rs            GDT: kernel + user segments (ring 0/3), TSS
  idt.rs            IDT: CPU exception handlers + keyboard IRQ
  frame_alloc.rs    Bitmap physical frame allocator (4 GiB, single + contiguous)
  paging.rs         Per-process page tables (2 MiB isolation, shared kernel PDs)
  pic.rs            8259 PIC initialisation (remapped to vectors 32-47)
  scheduler.rs      Preemptive round-robin (PIT @ 100 Hz, assembly context switch)
  ipc.rs            Typed message-passing channels (ring buffer, blocking recv)
  syscall.rs        syscall/sysret interface (send, recv, yield, exit, fbinfo, debug)
  keyboard.rs       IRQ 1 handler — reads port 0x60, sends scancode via IPC
  framebuffer.rs    Direct pixel rendering (put_pixel, draw_rect, draw_string)
  font.rs           8x8 bitmap font (full printable ASCII)
  serial.rs         COM1 serial output for debugging
docs/
  FreshOS-Manifesto.md     Full vision and design principles
  FreshOS-v1-Scope.md      What v1 must deliver
  FreshOS-Demo-Script.md   The two-minute demo
  Boot-Walkthrough.md      Step-by-step boot explanation for beginners
  Syscall-Flow.md          Ring transitions, syscall mechanics, real-time analysis
```

## Architecture

The kernel handles scheduling, memory management, IPC, and interrupt routing. Everything else is userspace:

- **Ring 3 user tasks** with per-process page tables. Each task sees only its own code, stack, and explicitly granted resources (like the framebuffer). Kernel memory and other tasks' stacks are invisible.
- **Typed IPC** through bounded channels. Messages carry a type tag and 32 bytes of inline payload. Non-blocking send, blocking recv with automatic wake-on-send. All IPC goes through syscalls — no direct kernel function calls from user mode.
- **Syscall/sysret** for the user-kernel boundary. Configured via IA32_STAR/LSTAR/FMASK MSRs. Assembly entry stub swaps stacks and dispatches to Rust.
- **Preemptive scheduling** via PIT timer at 100 Hz. Assembly ISR saves/restores all 15 GPRs, switches stacks and CR3.
- **Capability-based resource grants** through page table mappings. The framebuffer is mapped USER in the shell's page table but not the keyboard driver's. The kernel decides who sees what.
- **Microkernel driver model**: the keyboard driver runs in ring 3. The kernel reads port 0x60 and sends a raw scancode as an IPC message. The driver decodes it and sends a typed key event to the shell. The kernel doesn't know what a keyboard is.

## Key Design Decisions

- **GDT segment order** matters for `sysret`: kernel code, kernel data, user data, user code, TSS. The CPU hardcodes the relationship.
- **Page tables use 2 MiB huge pages** for the identity map. Per-task isolation is at 2 MiB granularity — task-specific PDs override shared kernel PDs for regions needing USER access.
- **Context switch writes CR3** when switching between tasks with different page tables. The kernel is identity-mapped supervisor-only in all page tables, so interrupt handlers work regardless of which task was running.
- **TSS.RSP0** and the `kernel_rsp` global are updated per-task on every context switch. RSP0 is the stack the CPU switches to on ring 3 → ring 0 transitions (interrupts). `kernel_rsp` is used by the syscall entry stub.
- **Blocking IPC** uses `cli` before the empty-check to prevent races, then `sti; hlt` to atomically enable interrupts and sleep. The keyboard IRQ wakes blocked tasks immediately (no timer tick wait for the first hop).

## Performance Contracts (from the manifesto)

Hard targets, not aspirations:
- Input-to-photon: sub-5ms target, hard ceiling at one frame
- Audio latency: sub-3ms round-trip
- IPC round-trip (small message): sub-1μs
- Compositor frame miss: zero tolerance

Current status: the syscall path (~200-500 ns on real hardware) meets the IPC target. Scheduling latency (up to 10 ms at 100 Hz) does not yet meet input-to-photon. Needs: APIC timer, priority scheduling, immediate wakeup on send. See `docs/Syscall-Flow.md` for detailed analysis.

## Tooling Notes

- **Rust edition 2024** — requires `#![feature(abi_x86_interrupt)]` for IDT handlers
- **Edition 2024 unsafe rules**: `unsafe fn` bodies need explicit `unsafe {}` blocks. Use `core::ptr::addr_of_mut!` for static mut access (the `static_mut_refs` lint is deny-by-default). `#[no_mangle]` requires `#[unsafe(no_mangle)]`.
- **Cross-compiling from aarch64-apple-darwin** — QEMU emulates x86_64 via TCG. Performance in QEMU is 10-50× slower than native.
- **MS x64 ABI**: the `x86_64-unknown-uefi` target uses the Microsoft calling convention for `extern "C"` (first arg in RCX, not RDI). Assembly stubs must follow this.
- **No heap allocator** after exit_boot_services. Use `StackBuf` for `core::fmt::Write`, `UnsafeCell` wrappers for global state, and `AtomicUsize` for counters.
