# FreshOS: Where We Are

*April 2026 — last updated 26 September 2026*

## Current status — 26 September 2026

**Direction.** A daily driver is the long-term destination (decision 0002), with
**responsiveness and observability as twin goals**. The **Raspberry Pi 4** is the
reference hardware and x86_64 is gone (0003; its lessons are in
`docs/x86-lessons.md`). The 198x projects, starting with Emu198x, run natively
via `no_std` + `alloc` cores (0004). The order of work is the v1 ladder in
`docs/FreshOS-v1-Scope.md`.

**Done: EL0 isolation on QEMU — 26 September 2026**
(`docs/plans/2026-09-26-el0-isolation-design.md`). Services run at EL0, each in
its own address space, with W^X pages, a kernel that never dereferences a user
pointer, and PAN where the CPU has it. They talk only through channel handles
that `init` grants, one receiving process per channel. The kernel starts only
`init`, which starts and supervises everything else; the kernel's registry
records every spawn and exit, and the MCP bridge and flow view read it. Direct
hand-off took the steady-state ping/pong median round trip from about 10.8 ms
to 10–28 µs (four runs), under the spec's 100 µs target. 27 automated tests cover it
(`./test.sh`); `AGENTS.md` describes the model.

**▶ Next:** move the in-kernel built-ins out to EL0, one spec each: the keyboard
driver, dashboard, MCP bridge, shell and compositor. Each spec deletes its
`builtin:` entry in `init`'s table.

**Known gaps to verify or close:**

- Verify on a real Pi 4: the no-PAN path, I-cache maintenance and the GICv2 board path.
- On a real PL011 at 115200 baud, a 256-byte LOG busy-writes for about 23 ms with IRQs masked; it needs a log ring.
- Kernel stacks have no guard pages; the canary only detects an overrun.
- The keyboard channel holds 16 events, so a burst of keys loses some.
- Serial lines from different tasks can interleave.
- `probe-bad-kernel` hardcodes QEMU's RAM base, `0x4000_0000`.

**Shipped:** the live message-flow diagram (OBS.1) on QEMU. The dashboard renders
tasks as nodes and recent messages as arcs with a pulse that travels sender →
receiver. Nodes are labelled from the kernel's task registry
(`kernel/src/registry.rs`), and destinations are attributed via channel consumers
(`kernel/src/ipc.rs`) even when delivery was buffered. On QEMU the desktop is
composited to **ramfb**, because the virtio-GPU scanout is invisible under
`-display cocoa`.

**Working QEMU-first.** There is no Pi 4 on hand yet (the boards available are a
Pi 5, a Pi 2B and a Pi 1), so work that doesn't need the board comes first.

**Done:** the board layer (`kernel/src/arch/aarch64/board/`). QEMU `virt` and the
Pi 4 now differ only in one file of addresses each, selected by a cargo feature.
The Pi 4 addresses (PL011 at `0xFE20_1000`, GIC-400 at `0xFF84_1000` and
`0xFF84_2000`) come from the BCM2711 peripherals document and are not yet
verified on hardware.

**Done:** the read-only MCP bridge (decision 0005, `kernel/src/mcp.rs`). While
QEMU runs, any MCP client can connect with `nc -U mcp.sock` and read the system
summary, services, tasks, message trace and metrics.

**Fixed:** Homebrew's QEMU 11.1.1 gives HVF a GICv3 and refuses GICv2, so the
kernel's GICv2-only driver received no timer interrupts and no service ever ran
after "Scheduler started". `gic.rs` now drives GICv3 on QEMU and GICv2 on the
Pi 4.

**Found:** `tlbi` works under QEMU 11 with HVF (on older QEMU it hung the guest,
which is why tasks used to run at EL1 on the Mac). A remap test showed stale
translations persist until `tlbi`, which is why an exiting task's ASID is
flushed before its slot is reused. The two other old HVF limits don't block it
either. A permission fault on a 2 MiB block now reaches the guest as a normal
data abort (it used to crash QEMU with an `isv` assertion). `msr SP_EL1` at EL1
is undefined by the architecture on any hardware, so the kernel sets its own
stack with `mov sp` and user stacks with `msr SP_EL0`, which works. That is what made
EL0 isolation possible on QEMU without waiting for a Pi 4.

**Then, on a real Pi 4 (rung 1):**

1. Build the kernel for the Pi:
   `rustup run nightly cargo build --package freshos-kernel --target aarch64-unknown-uefi --no-default-features --features board-rpi4`.
   There is no Pi staging script yet: copy the kernel to `EFI/BOOT/BOOTAA64.EFI`
   and the service ELFs to `EFI/FreshOS/`, as `run-arm.sh` does for `esp-arm/`.
2. Put the community `pftf/RPi4` UEFI firmware on an SD card with those files,
   and add `dtoverlay=disable-bt` to `config.txt` so the PL011 is on GPIO 14/15.
3. Wire the serial adapter: a Raspberry Pi Pico flashed with `debugprobe`,
   GP4 (TX) to Pi header pin 10, GP5 (RX) to pin 8, GND to GND. The firmware
   prints to serial itself, which proves the wiring before FreshOS runs.
4. Boot FreshOS and note where it stops. The first log lines should read
   `Board: Raspberry Pi 4`.

**Parked:** #63 (a `virtio-input` keyboard driver). It only helps QEMU, and rung 1
is the Pi. On the Pi, input will come from USB. #65 (the stats overlay flickers
over the dashboard: compositor z-order) is also open.

> The sections below predate this status block and describe the April state,
> including the now-removed x86_64 path. Treat them as historical.

---

## What exists

A Rust microkernel OS that boots on x86_64/UEFI, runs 7 concurrent ring 3 tasks with per-process page tables, and presents a desktop with overlapping windows, a menu bar, taskbar, desktop icons, and a chiptune melody.

## Primary Development Path

FreshOS now has two distinct "working" paths:

- **Primary demo path:** aarch64 on QEMU `virt` with `HVF` on Apple Silicon. This is the fast path for compositor iteration, interaction tuning, and demo preparation.
- **Architecture reference path:** x86_64 on UEFI with ring 3 tasks, `syscall/sysret`, per-task page tables, PS/2 input, and ATA PIO.

Important caveat: the current aarch64/HVF build still runs the desktop path at **EL1**, not **EL0**, because HVF traps the `tlbi` instructions needed for the intended per-task user-space page-table flow. There is now a narrow EL0 proof path for one external service running from a dedicated pre-granted region, which demonstrates real `SVC` entry and contained lower-EL faults on Apple Silicon. That is useful progress, but it is still **not** the final proof of isolation: all tasks still share the same patched `TTBR0`, and FreshOS does not yet switch per-task user page tables on this path.

The practical rule is:

- Use `./run-demo.sh` on Apple Silicon for the fastest interactive path.
- Use `./run.sh` when validating the stricter x86_64 microkernel path.

### Kernel (ring 0)
- UEFI boot → exit boot services → full hardware control
- GDT with kernel + user segments, TSS with double-fault IST
- IDT with 15 CPU exception handlers + keyboard IRQ + mouse IRQ
- Bitmap frame allocator (128 KiB, 4 GiB address space)
- Per-process page tables (2 MiB granularity, shared kernel PDs)
- 8259 PIC remapped to vectors 32–47
- PIT timer at 1000 Hz driving preemptive round-robin scheduler
- Assembly context switch: save/restore 15 GPRs, switch stacks + CR3
- syscall/sysret interface (IA32_STAR/LSTAR/FMASK MSRs)
- 14 syscalls: send, recv, yield, exit, fbinfo, time, trace, surface_info, beep, port_in8, port_out8, port_ins16, port_outs16, debug
- TSC calibrated against PIT for nanosecond timing
- 1 MiB kernel heap (linked-list allocator with coalescing)
- IPC trace buffer (64 entries, recorded on every send)
- PC speaker driver (PIT channel 2)
- Capability-checked port I/O (ATA ports whitelisted)

### Userspace tasks (ring 3)
1. **Keyboard driver** — IRQ 1 → IPC → scancode decode → key events
2. **Mouse driver** — IRQ 12 → IPC → packet assembly → cursor position
3. **Compositor** — owns framebuffer, composites surfaces, draws menu bar/taskbar/stats/cursor
4. **Shell** — draws to surface 0, receives key events, renders typed text
5. **Dashboard** — draws to surface 1, shows uptime/tasks/message trace
6. **Chiptune** — background melody via SYS_BEEP
7. **Idle** — halts between interrupts

### Visual layer
- Anti-aliased SF Mono font (16px, 95 glyphs, grayscale alpha blending)
- Old 8x8 bitmap font still available
- Desktop: solid background, stacked overlapping windows with title bars
- Menu bar (top): "FreshOS | microkernel", task count, clock
- Taskbar (bottom): centred workspace pills
- System stats panel (top-right): uptime, tasks, scheduler, latency
- Desktop icons: System and Disk 0 with 2x sprites
- Mouse cursor (12×16 arrow with outline)
- Boot chime (C-E-G-C ascending arpeggio)
- Workspace switch audio cues

### Other
- Rhai scripting engine (no_std) with IPC bindings
- ATA PIO storage driver (read/write sectors, verified)
- 3 documentation files (Boot Walkthrough, Syscall Flow, this file)

## What's blocking progress

**QEMU TCG performance.** The emulated x86_64 CPU is 10–50× slower than real hardware. Per-pixel operations that would take microseconds on real silicon take milliseconds under emulation. The compositor can render a complete frame, but it takes several seconds — far too slow for interactive use or visual polish.

This is not a FreshOS problem. It's an emulation problem. The same code on real x86_64 hardware would composite at 30+ fps.

For that reason, x86_64 under TCG is no longer the right default development loop. It remains valuable as the correctness path for ring 3 isolation, but Apple Silicon/HVF is the path that can move the visual and interaction work forward.

## Three paths forward

### Path 1: Run on real hardware
Boot FreshOS on a real x86_64/UEFI machine. The kernel already targets real hardware — UEFI boot, PCI bus, PS/2 keyboard, ATA PIO. A USB drive with the ESP partition would work. This eliminates the emulation bottleneck entirely and lets us see true performance.

### Path 2: QEMU with KVM (Linux host)
On a Linux x86_64 host, QEMU with KVM runs at near-native speed. The kernel code is unchanged — KVM just removes the emulation layer. This is the easiest path if a Linux machine is available.

### Path 3: Virtio-GPU driver
Build a PCI bus enumerator and virtio transport layer, then a virtio-GPU driver. This gives us:
- Dirty region updates (only transfer changed pixels to the host)
- Page flipping (double buffering)
- Proper vsync

The virtio-GPU 2D protocol doesn't do compositing — that stays in software. But dirty region tracking would dramatically reduce how many pixels need to cross the emulation boundary each frame.

This is the correct long-term investment but requires: PCI enumeration, virtio transport (virtqueues, descriptor rings), and the GPU command protocol.

### Path 4: virgl (3D GPU)
Extend the virtio-GPU driver with 3D context support (virgl protocol). This enables submitting textured quads with alpha as GPU commands — true hardware-accelerated compositing. The host GPU does the blending. This is the "real" GPU path but requires a substantial graphics driver stack.

## Recommended next session priorities

1. **Stabilise Apple Silicon/HVF as the primary demo loop** — the repo should default to the fast path for local iteration, while keeping x86_64 as the reference path.

2. **Add measurement before optimisation** — publish frame time, input-to-photon, IPC round-trip, and scheduler wake latency instead of reasoning from feel.

3. **Decide whether `ramfb` is sufficient** — only continue down the virtio-GPU path if measurements show framebuffer presentation is still the dominant bottleneck on HVF.

4. **Tighten the truth gap** — handle-based channel/surface capabilities, syscall pointer validation, and clearer boundaries between "implemented now" and "target architecture."

5. **Load one real process** — replace one statically linked task with an ELF loaded from disk, then crash and restart it independently.

## The vision gap

The architecture is complete. Every v1 scope item has been built. The demo moment — spatial workspaces, live message flow, latency counter, keyboard/mouse input, scripting, storage — all exist and work.

The gap is visual fidelity and interactive speed. The desktop vision (translucent windows, gradient background, animated elements, chiptune) is implemented but can't be experienced at interactive frame rates on QEMU TCG. This is a tooling constraint, not an architecture constraint.

The next session should focus on removing that constraint.
