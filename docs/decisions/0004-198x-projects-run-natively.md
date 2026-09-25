---
title: "0004 — 198x projects run natively on FreshOS"
type: decision
status: proposed
date: 2026-09-25
deciders: Steve
---

# 0004 — 198x projects run natively on FreshOS

- **Status:** Proposed
- **Date:** 2026-09-25
- **Deciders:** Steve

## Decision

FreshOS should run Emu198x and the other 198x projects as native
applications (the manifesto's Tier 2), not inside a Linux VM.

- **Emu198x is the games demo on rung 4 (Plays)** of the v1 ladder, with
  Play198x alongside it as the media player.
- **The default route is `no_std` + `alloc`:** the 198x cores drop their
  `std` dependency, and FreshOS supplies a small native host API. A Rust
  `std` port for FreshOS is not planned. Reopen that question when Asm198x
  or Build198x is next in line, after rung 2 (storage).
- **Asm198x and Build198x** follow once FreshOS has storage.
- **Cat198x is Horizon.** It needs SQLite, `tokio`, networking and threads.
- **Code198x** is reached through its samples running in Emu198x, and its
  website through rung 6 (the web). Forge198x has no code yet.

This answers 0002's open question about ★ First Living Citizen.

## Why

- Emu198x's cores already talk to the host through one narrow interface in
  `emu198x-shell`: `MachineCore::run_until` with a `HostIo` that carries
  input events, a `FrameSink`, an `AudioSink` and a `TraceSink`. Those map
  onto FreshOS's compositor, audio service, input services and message-flow
  view. An emulated machine shows up in the same flow view as the rest of
  the system, which serves both twin goals (0002).
- The cores already build for WebAssembly without threads, a real
  filesystem or audio devices, which is the same kind of constraint FreshOS
  imposes.
- A `std` port means maintaining a fork of Rust's standard-library platform
  layer, which cuts against keeping each subsystem understandable by one
  person. Emu198x and Play198x don't need one.

## Consequences

- FreshOS needs, from Emu198x: the host-interface types (`MachineCore`,
  `HostIo`, the sinks, `InputEvent`, the frame and audio packets,
  `MachineTime`, `MachineError`) in a `no_std` + `alloc` crate, and `no_std`
  CPU, chip, machine and runtime crates.
- **How Emu198x splits is Emu198x's decision**, recorded in its own
  `knowledge/decisions/`. The manifesto keeps the 198x projects independent:
  FreshOS states what it needs, not how they build it. Emu198x's save-state
  decision already chose postcard partly to keep `no_std` open.
- Format198x, Isa198x and Debug198x become `no_std` + `alloc` as
  dependencies of the ports above.
- Until FreshOS has storage, ROMs and media are built into the binary as
  byte slices.

## Drift triggers

- Running a 198x project in a Linux VM instead of natively
- Forking Rust `std` for FreshOS without reopening this decision
- Editing a 198x repo's structure from FreshOS work without a decision in
  that repo
