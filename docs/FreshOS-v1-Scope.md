# FreshOS v1: A Machine You Can Use

## The test

Every decision about v1 is measured against one question:

**Can Steve use this Raspberry Pi 4 for real work and play, and see how it works while he does?**

Responsiveness and observability are the twin goals, and they run through
every rung. Each demo shows its new work in the live message-flow view, with
the latency counter on screen and within contract.

## The ladder

Each rung ends with a demo on a real Pi 4.

1. **Honest kernel.** Boots to serial on the Pi 4, then runs tasks at EL0
   with per-task page tables and validated syscall pointers.
   Demo: a service faults and its siblings survive.
   *Decision gate:* benchmark software compositing at 1080p before rung 3.
   *Decision:* whether the kernel owns EL2 (needed for rung 6's Linux VM).
2. **Remembers.** A generic service runtime, and a writable filesystem on
   the SD card.
   Demo: write a note, reboot, read it back.
3. **Usable at the desk.** USB keyboard and mouse, the HDMI desktop, a shell
   and a text editor, all as supervised services.
   Demo: edit a file, kill the editor, restart it, carry on.
4. **Plays.** Audio output, gamepad input, frame pacing, and a native game
   SDK on a shared ABI crate. Games install by copying to the SD card.
   Demo: a basic game built outside the kernel repo runs at a steady frame
   rate, with its messages visible in the flow view.
5. **Connected.** Ethernet, then a first bridge service (email).
   Demo: read and send email.
6. **Browses.** Web content through an embedded renderer (Servo) or a
   sandboxed Linux VM, per the rung 1 EL2 decision.
   Demo: read a web page.

## The compositing gate

Software compositing with damage tracking is the default. On a real Pi 4,
benchmark a full-frame redraw, a small damaged area, and several stacked
layers at 1080p. The pass mark is the manifesto's own contract: one frame at
60 Hz, with enough headroom left for a game. If software compositing passes,
a GPU (VideoCore VI) driver waits until after v1. If it fails, the GPU driver
moves up the ladder.

## v1 is not

- Hardware beyond the Pi 4 (the Pi 5 comes after v1)
- POSIX compatibility or a package manager
- A GPU driver, unless the compositing gate says software is too slow
- Anything in [`FreshOS-Horizon.md`](FreshOS-Horizon.md)

---

*FreshOS v1 Scope v2 — September 2026. Replaces the March 2026 demo-first
scope, which defined v1 as "not a daily driver" (see decisions 0002 and
0003).*
*Steve*
