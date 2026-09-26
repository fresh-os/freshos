# AGENTS.md

Guidance for coding agents working in the FreshOS kernel repository.

## What FreshOS is, and what it is for

FreshOS is a Rust microkernel that boots from UEFI on **aarch64**. Its guiding idea is "understandable magic": one person should be able to understand each subsystem by reading its source in one sitting.

**Strategic direction (binding):** `docs/decisions/0002-daily-driver-is-the-destination.md` (proposed). In the long term, FreshOS should become an OS people can really use day to day. 0002 partly supersedes `0001-useful-means-observable.md`: 0001's rejection of the daily driver no longer holds, but its observability milestones (**★ Observable by Default**, **★ First Living Citizen**) stand until 0002's open questions are settled. The M1–M7 substrate roadmap (`docs/plans/2026-04-13-useful-os-roadmap.md`) is the route to the destination again. Treat shortcuts that block real use, such as the EL1 desktop without isolation, as temporary. The order of work is the v1 ladder in `docs/FreshOS-v1-Scope.md` (honest kernel on the Pi 4, storage, usable desktop, basic games, networking, web). Don't start a later rung while an earlier one is unfinished. **Responsiveness and observability are the twin goals:** new work must be visible in the message-flow view and measurable against the performance contracts below. FreshOS never builds a browser engine and never targets POSIX compatibility. Emu198x and the other 198x projects run natively via `no_std` + `alloc` cores, not a `std` port or a Linux VM (`docs/decisions/0004-198x-projects-run-natively.md`); changes to a 198x repo's structure are decided in that repo, not from FreshOS work. The OS is reachable by agents over MCP through a bridge service that holds capabilities like any other service, never a backdoor (`docs/decisions/0005-reachable-over-mcp.md`). Long-range ideas (editions, the physical world) live in `docs/FreshOS-Horizon.md` and are out of scope. Existing code has no protected status: rewriting a subsystem is allowed, but decide it one subsystem at a time and record the reason. Check 0002's *Drift triggers*.

**Target hardware (binding):** `docs/decisions/0003-raspberry-pi-4-base-drop-x86.md`. The **Raspberry Pi 4** is the reference hardware, booted through the `pftf/RPi4` UEFI firmware. QEMU `virt` with HVF is the development loop, and the Pi 5 comes next. **x86_64 has been dropped. Don't reintroduce it.** Its design lessons (isolation model, syscall boundary, known gaps) are in `docs/x86-lessons.md`. Board-specific addresses live in the board layer, `kernel/src/arch/aarch64/board/`, one file per board, never hardcoded elsewhere.

**Where work stands:** read the status block at the top of `docs/Where-We-Are.md` (everything below it is April history). Work is tracked as GitHub issues and milestones on `fresh-os/freshos`.

## How it runs today

Services run at **EL0, each in its own address space**, on QEMU `virt` with HVF. The kernel starts only `init`; `init` starts everything else from its table and restarts what it supervises. Five built-ins (keyboard, compositor, shell, dashboard and MCP bridge) still run at **EL1** inside the kernel until each moves out under its own spec (decision 0006).

| | QEMU `virt` + HVF (works) | Raspberry Pi 4 (not yet booted) |
|---|---|---|
| Purpose | Day-to-day development loop on Apple Silicon | Reference hardware |
| EL0 services | `init`, `ping`, `pong`, `pulse`, `fault` | Same binaries; PAN, I-cache maintenance and the GICv2 path are unverified there |
| Built-ins | `kernel/src/arm_tasks.rs` and `mcp.rs`, at EL1 | Same |
| Input | Serial UART only: type into the terminal running QEMU | USB keyboard on the VL805 xHCI controller (needs a PCIe driver, then xHCI) |
| Display | Composited to `ramfb` (virtio-GPU scanout is invisible under `-display cocoa`) | UEFI GOP framebuffer from the firmware |

`.cargo/config.toml` makes `aarch64-unknown-uefi` the default build target, uses `build-std` for `core` and `alloc`, and links `aarch64-unknown-none` userbins at `0x4_0000_0000`.

## Commands

The toolchain is nightly, pinned by `rust-toolchain.toml` (channel only, no date). The run scripts call `rustup run nightly`. If a build reports "can't find crate for core", check that nothing has overridden the toolchain.

```bash
./test.sh                   # Build everything and run every test (about two minutes)
./test.sh -v -k supervision # Arguments go to unittest: -v for names, -k to filter

./run-arm.sh [--release]    # Builds the kernel and all userbins, stages esp-arm/, boots QEMU aarch64 + HVF (cocoa window, serial on stdio)
./run-demo.sh               # Same as run-arm.sh; exits with an error on hosts other than Apple Silicon
./run-arm.sh -display none  # Headless: extra arguments go to QEMU, and the last -display wins

# While QEMU runs, the read-only MCP bridge is on mcp.sock in the repo root.
# Any MCP client can use `nc -U` as its server command, for example:
claude mcp add freshos -- nc -U "$PWD/mcp.sock"

# Build one piece. Always pass both --package and --target, the way run-arm.sh does.
rustup run nightly cargo build --package freshos-kernel --target aarch64-unknown-uefi
rustup run nightly cargo build --package freshos-kernel --target aarch64-unknown-uefi --no-default-features --features board-rpi4   # Pi 4 kernel
rustup run nightly cargo build --package freshos-pong   --target aarch64-unknown-none   # any userbin

rustup run nightly cargo clippy --package freshos-kernel
```

`run-arm.sh` reads these environment overrides. The test harness uses them to boot each test class in private directories:

| Variable | Effect | Default |
|---|---|---|
| `ESP_DIR` | Where to stage the ESP | `./esp-arm` |
| `MCP_SOCK` | The MCP bridge's Unix socket; keep it short (macOS allows about 104 bytes) | `./mcp.sock` |
| `EXTRA_ELFS` | Extra userbin packages, without `freshos-`, to build and stage | none |
| `OMIT_ELFS` | Userbins to build but leave off the ESP, such as `init` | none |
| `EXTRA_FILES_DIR` | Files copied as-is into `\EFI\FreshOS\`, such as a malformed ELF | none |
| `OVMF_VARS` | The writable UEFI variable store | `./edk2-arm-vars.fd` |
| `SKIP_BUILD=1` | Stage what is already built | build first |
| `BUILD_ONLY=1` | Build, then exit without staging or booting | boot |
| `FRESHOS_ACCEL` | `hvf`, or `tcg` for software emulation (`-cpu max`) | `hvf` |

- **Tests:** `tests/` holds Python 3.14 `unittest` suites, standard library only. `tests/harness.py` boots QEMU through `run-arm.sh` once per test class, captures the serial log, and talks to the MCP bridge. Assert on MCP views and log lines, always with a timeout. `tests/test_abi.py` runs `freshos-abi`'s Rust unit tests on the host. Every change lands with its tests, and a test must fail when its claim is false: never let one pass without checking anything. Every test also fails if its boot's log shows `KERNEL PANIC`, `*** TASK FAULT ***` or `*** EXCEPTION ***` (`FreshOSTestCase.tearDown`); the expected EL0 faults print `*** EL0 TASK FAULT ***` and don't count.
- **Warnings:** the tree is not warning-clean under `build` or `clippy`. Don't add new warnings. Clean up existing ones only when that is your task.
- **Firmware:** comes from `brew install qemu` (`/opt/homebrew/share/qemu/edk2-*`). QEMU loads it through pflash, not `-bios`. `run-arm.sh` copies a writable `edk2-arm-vars.fd` into the repo on first run.

## Architecture

The design is in `docs/plans/2026-09-26-el0-isolation-design.md`. This section is the map.

### Kernel layout

- **Portable modules in `kernel/src/`:**
  - `syscalls` (every EL0 syscall), `handles`, `ipc`, `registry`, `boot_images`, `elf`
  - `frame_alloc`, `heap`, `framebuffer`, `font`/`font_aa`, `metrics`, `serial`, `scripting` (Rhai, `no_std`)
  - `arm_tasks` and `mcp`, the EL1 built-ins. The compositor draws the chrome (menu bar, taskbar and stats overlay); `../docs/decisions/chrome-as-services.md` plans to move each piece into its own service.
- **`kernel/src/arch/aarch64/board/`:** the board layer. One file per board (`qemu_virt.rs`, `rpi4.rs`) holding its addresses: PL011 UARTs, GIC, and the virtio-mmio window if any. Exactly one `board-*` cargo feature selects it (`board-qemu-virt` is the default), and `compile_error!` rejects zero or two.
- **`kernel/src/arch/aarch64/`:** exceptions, GIC, timer, the scheduler (`context`), address spaces (`addrspace`), system-register setup (`paging`), the syscall entry (`syscall`) and a virtio-GPU driver. It is re-exported as `arch::*`, and portable code calls through `arch::`.
- **`main.rs`:** the UEFI `#[entry]`. It picks the largest graphics mode up to 1920×1200, reads every `*.ELF` in `\EFI\FreshOS\` into memory (`boot_images`), exits boot services, brings up the kernel, and starts `init`. **The kernel requires only `INIT.ELF`** (decision 0006). Without it, the kernel prints `INIT.ELF missing from \EFI\FreshOS — nothing to run` and halts.

### Address spaces and protection

- **One table per EL0 task.** Each task's top-level table copies the kernel's top-level entries, so the kernel (RAM and devices, EL1-only) is identical in every space. Slot 16, the 1 GiB window at `0x4_0000_0000` (`USER_BASE`), is private: the task's code, data and a 64 KiB stack at the top, with an unmapped guard page below it.
- **ASID = task slot.** User pages are not-global (`nG`), so switching tasks needs no TLB flush. A space's drop flushes its ASID before the slot can be reused.
- **`TaskRef` = slot + generation.** Each slot's generation goes up on every spawn and never repeats, so a `TaskRef` never names two tasks. Exit notices and the registry use it.
- **W^X per page.** The ELF loader maps code read+execute and data read+write, never both. It refuses a segment that asks for both, or one outside the window.
- **The kernel never dereferences a user virtual address.** `copy_from_user`/`copy_to_user` (`addrspace.rs`) check the whole range against the task's own table, then copy through the kernel's map of the physical frame.
- **PAN** is detected at boot and turned on where the CPU has it (it does under QEMU with HVF). **The Pi 4's Cortex-A72 (ARMv8.0) has no PAN**; there the copy discipline is the only enforcement.
- **I-cache maintenance** (`paging::sync_icache`) runs after the loader writes code. Apple cores don't need it; the Pi 4 does.
- **EL0-facing controls are set at boot, not inherited** (`paging::init`): `CNTKCTL_EL1 = 0` (no counter or timer access at EL0; `time_ns` is a syscall), and `SCTLR_EL1` UMA, DZE, UCT and UCI cleared (no DAIF, `dc zva`, `CTR_EL0` or cache maintenance at EL0; nothing uses them). The boot log prints what the firmware left. `TPIDRRO_EL0` is zeroed, and the TLB is flushed (`tlbi vmalle1is`) before the first user space exists. A userbin that needs any of these must change `paging::init` first.
- A fault at EL0 terminates only that task, with reason `fault`, under the banner `*** EL0 TASK FAULT ***`. A fault at EL1 while an EL0 task is current happened in the kernel (a syscall or an IRQ on that task's behalf), so it panics with ESR, FAR, ELR and the task's id and name, and says when it is a PAN violation. An EL1 built-in's own fault still terminates only that built-in (`*** TASK FAULT ***`).

### Tasks and scheduling (`arch/aarch64/context.rs`)

- `MAX_TASKS` is 16 slots. Slot 0 is the boot/idle task.
- **Exception frame:** `FRAME_SIZE` is 816 bytes. Every exception saves the general registers, eagerly the FP/SIMD state (q0–q31, FPCR, FPSR), and `TPIDR_EL0`, which EL0 can write, so each task has its own. A new task's frame is zeroed, so it starts with all of them 0. `TPIDRRO_EL0` is zeroed once at boot and never saved: EL0 can't write it and nothing uses it. `enable_fp` sets `CPACR_EL1.FPEN` at boot.
- **Kernel stacks:** 32 KiB per task. A canary at the bottom is checked on every switch and when a task exits or faults, and an overrun panics naming the task. The MCP `tasks` view shows each task's `stack_peak_bytes`. There are no guard pages yet.
- **Direct hand-off:** a send to a waiting receiver runs that receiver next. A blocking `recv` switches away at once.
- **Sender-return:** if the hand-off target then blocks while its donor is still Ready, the donor runs next. Otherwise the choice is round-robin. Every timer tick clears the hand-off and rotates, so fairness holds at tick granularity (1 ms).
- The ping/pong median round trip was 10–42 µs across seven runs on QEMU with HVF (`tests/test_latency.py`), down from about 10.8 ms before hand-off.

### The ABI, the runtime and the syscalls

- **`freshos-abi` (`lib/abi/`)** defines everything that crosses the boundary, once: syscall numbers, `Error`, `Message`, `Handle`, `Rights`, `SpawnRequest`, `Grant`, `TaskRef`, `ExitReason`, message tags and the window constants. The kernel uses it too. Never redeclare these types by hand.
- **`freshos-rt` (`lib/rt/`)** is all a userbin links: `entry!(main)`, where `main(Startup) -> !` receives its granted handles and its table argument; a panic handler that logs and exits with `panic`; safe syscall wrappers returning `Result`; and `log!`. Userbins have no allocator. All `svc` assembly is in `lib/rt/src/syscall.rs`.
- **Syscalls** (`svc #0`, number in `x8`, arguments in `x0`–`x5`, result in `x0`; negative results are `Error`):

| # | Syscall | Notes |
|---|---|---|
| 0 | `send(h, *msg)` | Needs `SEND`. The kernel stamps `sender`. Hands off to a waiting receiver. `Full` when 16 messages are queued. |
| 1 | `recv(h, *msg, deadline_ns)` | Needs `RECV`. Blocks; `Timeout` after a non-zero deadline (checked each tick). |
| 2 | `try_recv(h, *msg)` | As `recv`, but `WouldBlock` instead of waiting. |
| 3 | `yield()` | |
| 4 | `exit(reason)` | `clean` or `panic`; `fault` is the kernel's to give. |
| 5 | `time_ns()` | Nanoseconds since boot. |
| 6 | `log(ptr, len)` | One line, at most 256 bytes, prefixed `[name]` by the kernel. Control characters become `?`, so no task can forge another's line. |
| 7 | `channel_create()` | `init` only. Returns a handle with `SEND` and `RECV`. |
| 8 | `spawn(*request)` | `init` only. Starts a binary by name with the listed grants as handles 0.., and returns the packed `TaskRef`. |

- **Handles** (`handles.rs`): each task has a 16-slot table; a handle is a slot index naming a channel and its rights. Tasks never see channel numbers. `SEND` grants are copied. **Each channel has one receiving process:** a `RECV` grant moves out of `init`'s handle into the child and returns home when the child exits, and messages queued meanwhile wait for the next receiver. Granting a `RECV` twice, or after it has moved, fails with `ReceiverTaken`.

### `init`, the registry and supervision

- **The registry keeps the facts; `init` keeps the policy.** `registry.rs` records every spawn and exit first-hand: names, generations, start and exit counts, and the last exit reason. The MCP views, the flow view and the shell read it, so none of them trusts `init`'s account.
- **`init`'s table** (`userbins/init/src/main.rs`) lists each service: name, binary, grants, argument, restart delay, and whether it is optional. An optional service whose binary is absent is skipped silently; any other refused spawn is logged with the service and its binary, as `[init] cannot start pong (PONG.ELF): NotFound`, and `init` carries on. A supervised service (one with a restart delay) that can't start is retried, whatever the error (`TableFull`, `OutOfMemory`, a bad image): first after its restart delay, then after double the last wait, up to 5 s, logged as `[init] retrying NAME in Nms: ERROR`. A successful start resets the backoff.
- **`init` has one inbox, channel 2.** The kernel sends `TASK_EXITED` there as sender 0, carrying the task id, reason and generation, and `init` matches it against the exact `TaskRef` it spawned. Restart requests (the shell's `restart <name>`) arrive on the same inbox. When the inbox is full, kernel notices wait in a bounded backlog and move in, in order.
- If `init` exits or faults, the kernel prints why and halts.
- **Built-ins:** table entries with the binary `builtin:<name>` start the in-kernel EL1 built-ins, so `init` owns all startup policy. They hold no handles and use raw channel numbers (0: keyboard events, 1: shell keys). Each leaves in its own spec, which deletes its entry.

### Adding a userbin

1. Create a package under `userbins/<name>/` named `freshos-<name>`, depending on `freshos-rt` only, and add it to the workspace `members`.
2. Add it to `USERBINS` in `run-arm.sh`. A test-only userbin goes in `TEST_ELFS` in `tests/harness.py` instead.
3. Add an entry to `init`'s table with its grants. Its ESP name is the package name without `freshos-` or hyphens, upper-cased and cut to eight characters, plus `.ELF` (`probe-chan` is `PROBECHA.ELF`). Mark test-only entries `optional`.

### MCP bridge (`kernel/src/mcp.rs`)

A built-in service, `mcp`, that speaks MCP's stdio transport (newline-delimited JSON-RPC) over the board's second PL011 UART (`board::MCP_UART_BASE`; QEMU only for now). `run-arm.sh` exposes that UART as `mcp.sock`. It offers five read-only views, each as a tool and as a resource at `freshos://<name>`: `system`, `services`, `tasks`, `message_trace` and `metrics`. Decision 0005 governs it: no write tools until capabilities exist, and never a path around them. It runs at EL1 and reads kernel state directly until it moves out.

### IPC and observability

- `ipc.rs`: bounded channels of 16 messages, each a type tag and 32 bytes of inline payload.
- Every send is recorded in a 64-entry trace ring. The destination is attributed through the channel's consumer, even when delivery was buffered.
- The dashboard's live message-flow diagram (OBS.1, in `arm_tasks.rs`) draws that trace as nodes and arcs, labelled from the registry. Keep the identities it shows honest: a name must come from the kernel, never be guessed by the viewer.
- `metrics.rs` feeds the stats overlay and MCP: latency histograms, damage rectangles, and `ipc_delivery` (send to receipt, for every message).

### Memory

- `frame_alloc`: a bitmap over the UEFI memory map.
- `heap`: a 1 MiB linked-list allocator for the kernel. Rhai needs `alloc`.

## Gotchas

- **The boards have different GICs.** QEMU 11 under HVF offers only a GICv3 (it refuses `gic-version=2` with "HVF does not support GICv2 emulation"; older QEMU gave v2). The Pi 4 has a GIC-400, which is v2. `gic.rs` drives both, chosen by `board::GIC`. **The tell** that the GIC and the machine disagree: the boot log stops at "Scheduler started", and the QEMU monitor shows the CPU idling in `wfi` with IRQs unmasked.
- The timer is the **virtual** timer, PPI INTID 27, and its frequency is read from `CNTFRQ_EL0`, so neither is board-specific.
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
