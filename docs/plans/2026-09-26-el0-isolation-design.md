---
title: "spec: EL0 isolation foundation (rung 1, first slice)"
type: spec
date: 2026-09-26
status: proposed
---

# EL0 isolation foundation: design

This is the first slice of rung 1 ("honest kernel") of the v1 ladder
(`docs/FreshOS-v1-Scope.md`), built on QEMU 11 with HVF. It is approach A
from the design discussion: build the minimum kernel needed to run services
properly at EL0, prove it end to end with the smallest services, then move
the remaining built-ins out one spec at a time.

Binding decisions: 0002 (daily driver; responsiveness and observability are
the twin goals), 0003 (Pi 4 base, aarch64 only), 0004 (198x projects run
natively via `no_std` cores), 0005 (MCP bridge through capabilities), 0006
(every app and service is its own binary at EL0).

## Quality bar

Build it as if it were going to production. That means **correct, bounded
and tested**, not feature-complete:

- **Correct.** Every syscall validates every pointer and handle against the
  caller's own address space and grants.
- **Bounded.** Nothing user space does can panic or corrupt the kernel. A
  misbehaving service is terminated and reported.
- **Tested.** Behaviour is proven by automated tests that run on every
  change, not by reading a boot log.

## Scope

**In scope:**

1. An automated QEMU test harness (built first).
2. `freshos-abi` and `freshos-rt` crates for userbins.
3. Per-task address spaces: a private page table and a kernel stack per task.
4. A syscall layer at EL0 that validates every pointer and handle; the
   kernel stamps each message's sender.
5. Channel handles: created by `init`, granted at spawn, enforced by the
   kernel. Each channel has exactly one receiving process.
6. Direct hand-off: tasks switch on syscall exit, so a blocking `recv`
   gives up the CPU immediately and a `send` to a waiting receiver runs it
   next.
7. `init` at EL0, with a compiled-in service table and supervision over IPC.
8. `ping`, `pong`, `pulse` and `fault` at EL0 as isolated binaries. Their
   built-in copies are deleted, as is the function-pointer ABI.

**Out of scope, each getting its own later spec:**

- moving the keyboard driver, dashboard, MCP bridge, shell and compositor out;
- shared-memory surfaces;
- mapping device memory into drivers;
- an allocator for userbins;
- handle transfer and revocation;
- threads within a process;
- CI (the harness is designed to allow it).

Until their specs land, those five built-ins stay in the kernel as trusted
kernel code, using raw channel numbers.

## Definition of done

These automated tests pass:

1. Every service in `init`'s table is running after boot, and each is named
   correctly in the MCP services view.
2. `ping` and `pong` exchange messages across two address spaces. The trace
   and flow view show their real, kernel-stamped identities.
3. A service that reads kernel memory, another service's physical frames
   (through the kernel's mappings), or an unmapped address in its own window
   is terminated with the reason `fault`, and every other service keeps
   running. This is rung 1's demo, as a test.
4. A send or receive on a channel the task wasn't granted returns an error,
   and nothing is delivered.
5. A syscall given a pointer outside the caller's own memory returns
   `BadPointer`, and the kernel is unaffected.
6. Spawning a service whose grants would give a channel a second receiver
   is refused by the kernel (`ReceiverTaken`). `init` logs the refusal, and
   every other service is unaffected.
7. `init` restarts a supervised service after it exits, and the restart
   count is visible over MCP.
8. The median ping/pong round trip over 100 exchanges is under 100 µs on
   QEMU with HVF, against a baseline recorded before hand-off lands (about
   14.5 ms today). If 100 µs proves unreachable, investigate and record why;
   don't silently relax the number.
9. There are no built-in fallbacks. Booting without `INIT.ELF` prints the
   missing-init message and halts. Booting without `PONG.ELF` starts
   everything else, and `init` reports `pong` as missing.

---

## 1. Test harness

**Why first:** every later step lands with its tests, so the first piece of
work is being able to write them.

**Language:** Python 3.14, standard library only (`unittest`, `subprocess`,
`socket`, `json`), with type hints. A Rust host-side test crate was
considered and rejected: the workspace builds everything for UEFI with
`build-std`, so a host crate would fight the configuration. A
`.python-version` file pins 3.14 for mise, and the harness fails early and
clearly on an older interpreter.

**Layout:**

- `tests/harness.py`:
  - builds and stages an ESP into a temporary directory;
  - boots QEMU headless;
  - captures the serial log;
  - provides a small MCP client over the socket;
  - offers helpers: `wait_for_log(pattern, timeout)`, `services()`,
    `metrics()`, `wait_until(condition, timeout)`.
- `tests/test_*.py`: one file per area (boot, isolation, channels,
  supervision, latency). Each file boots once and shares the boot across its
  tests.
- `./test.sh`: builds everything, then runs `python3 -m unittest discover tests`.

**One definition of how FreshOS boots.** `run-arm.sh` gains optional
environment overrides, and the harness calls it rather than keeping its own
QEMU command line:

- `ESP_DIR`: where to stage the ESP;
- `MCP_SOCK`: the MCP socket path. It must be short: Unix socket paths are
  limited to about 104 characters;
- `EXTRA_ELFS`: extra binaries to stage, for test-only services.

The harness passes `-display none` and captures stdout as the serial log.
`FRESHOS_ACCEL=tcg` selects QEMU's software emulation instead of HVF, so CI
can be added later without a redesign.

**Test-only services** are small userbins, staged only for test runs:

- `probe-bad`: attempts each forbidden memory access in turn;
- `probe-chan`: attempts ungranted channels and bad pointers.

They report results as structured log lines, such as
`[test] ungranted-send result=refused`. `init`'s table lists them as
`optional`, so they start only when their binary is present. Normal boots
never see them, and there is still only one `init`.

**Assertions** use the MCP views and the structured log lines, always with
timeouts, so a hang fails the test instead of stalling the run.

## 2. The ABI and runtime crates

Two new workspace crates, both `#![no_std]` with no external dependencies.

**`freshos-abi`** (`lib/abi/`) is the single definition of everything that
crosses the kernel/user boundary. The kernel depends on it too: its IPC
`Message` is this type. It defines:

- syscall numbers;
- `Error`, a `#[repr(i64)]` enum returned as negative values: `NoSuchSyscall`,
  `NoSuchHandle`, `NoRight`, `BadPointer`, `WouldBlock`, `Timeout`,
  `NotFound`, `NotPermitted`, `TableFull`, `ReceiverTaken`;
- `Message`: `u32` tag, `u16` sender, `u16` length, `[u64; 4]` payload,
  unchanged from today. The kernel always overwrites `sender`;
- `Handle(u32)`, and the rights `SEND` and `RECV`;
- `SpawnRequest` and `Grant`, for `init`;
- `ExitReason`: `Clean`, `Fault`, `Panic`;
- system message tags, such as `TASK_EXITED { task, reason }` and
  `RESTART_REQUEST { name }`.

**`freshos-rt`** (`lib/rt/`) is what a userbin links against:

- `freshos_rt::entry!(main)` generates `_start` and passes `main` the
  handles it was granted, in table order (`StartupHandles`);
- a panic handler that logs the message and exits with `Panic`;
- safe syscall wrappers returning `Result<_, Error>`: `send`, `recv`,
  `recv_until`, `try_recv`, `yield_now`, `exit`, `time_ns`, `log`, and, for
  `init`, `channel_create` and `spawn`;
- `log!`, which formats into a stack buffer. Userbins have no allocator in
  this spec.

All `svc` inline assembly lives in one small file in `freshos-rt`.

**Deleted:** `kernel/src/service_abi.rs`, `InitApi`'s function-pointer
table, and every hand-copied struct in `userbins/*`.

## 3. Per-task address spaces

**Concepts.** A **page table** maps virtual addresses to physical memory,
and records for each page who may use it (the kernel at EL1 only, or user
tasks at EL0 too) and whether it may be executed. **`TTBR0_EL1`** tells the
CPU which table is current. The CPU caches translations in the **TLB**, and
an **ASID** tags cached translations with the task they belong to, so
switching tasks needs no flush. Today there is one table, UEFI's identity
map (virtual = physical), shared by everyone.

**Layout.** The firmware sets `T0SZ=28`: a 64 GiB address space whose
top-level table has 64 slots of 1 GiB each.

```
Every task's address space

  0 GiB ─ 4 GiB    the kernel: RAM and devices           shared, EL1 only
  ...              unused
 16 GiB ─ 17 GiB   this task's own code, data and stack  private, EL0
```

- **Shared kernel part.** Each task's top-level table copies UEFI's
  top-level entries for slots 0–15, which point at the *same* lower-level
  tables UEFI built. The kernel looks identical in every address space and
  stays EL1-only.
- **Private window.** Slot 16 (`0x4_0000_0000`) points at tables owned by
  the task. The slot is unused on both QEMU and the Pi 4. At boot the kernel
  checks that it's empty in UEFI's map, and refuses to continue if not.

**Loading a binary.**

- Userbins are linked as static, non-PIE executables at `0x4_0000_0000`,
  set through target-specific flags in `.cargo/config.toml`.
- The loader maps each `PT_LOAD` segment at its linked address, in freshly
  allocated and zeroed frames, with permissions from the ELF's flags. Code
  is read and execute; data is read and write, never execute. A segment that
  asks for write and execute is rejected.
- A segment outside the window is rejected.
- No relocation support is needed, which removes today's fragility of
  moving images without relocating them.

**Stacks.**

- Each task gets a **64 KiB user stack** at the top of its window. The page
  below it stays unmapped as a **guard page**, so an overflow faults instead
  of overwriting data.
- Each task gets its own **16 KiB kernel stack** for syscalls and
  interrupts. On an exception from EL0 the CPU switches to it through
  `SP_EL1` automatically. The kernel sets it with `mov sp` while running on
  it, because `msr SP_EL1` is undefined at EL1.

**Switching.** The scheduler loads the next task's table and ASID into
`TTBR0_EL1`. Kernel mappings are global; user mappings are marked
not-global (`nG`), so they're tagged with the ASID. ASIDs are allocated from
1 to `MAX_TASKS`. When a task exits, `tlbi aside1` flushes its ASID before
the number is reused, and its page-table and data frames are freed. The
kernel ensures `TCR_EL1.A1 = 0`, so the ASID comes from `TTBR0`. Built-in
EL1 tasks run on the kernel-only table.

**PAN on.** PAN ("Privileged Access Never") makes the CPU fault if EL1 code
touches EL0 memory. The kernel currently turns it off; this spec turns it
back on (`PSTATE.PAN = 1`, `SCTLR_EL1.SPAN = 0`, so it's also set on every
exception entry). The only code that reads or writes user memory is the pair
of copy helpers in section 4. They copy through the kernel's map of the
physical frames the task's own table points to, never through the user
address, so PAN is never lifted.

**Memory type.** User pages use the same memory attributes the firmware
uses for RAM, read from its mapping of the kernel image at boot.

**Retired:** `grant_user_access` and its global permission patching, the
special 2 MiB region for `fault`, fixed-bias ELF loading
(`elf::load_image`), and `make_executable`.

## 4. Syscalls, handles and validation

**Mechanism.** A task executes `svc #0`: `x8` holds the syscall number,
`x0`–`x5` the arguments, and `x0` the result. Negative results are `Error`
values. The existing entry path in `exception.s` stays.

**Handles.** Each task has a 16-slot handle table, kept by the kernel. A
handle is a slot index. Each slot holds a channel and its rights (`SEND`,
`RECV` or both). Tasks never see raw channel numbers.

**One receiver per channel.** The `RECV` right on a channel belongs to
exactly one process. `spawn` refuses to grant `RECV` on a channel whose
right is already held by another process (`ReceiverTaken`). The kernel is
the only enforcement point: `init` doesn't pre-check its table, it reports
the kernel's refusal, so the check that matters is the one the tests
exercise. `SEND` can be
granted to any number of tasks. When threads exist, threads of the same
process may share its receive right.

**The syscalls:**

| # | Syscall | Behaviour | Checks |
|---|---|---|---|
| 0 | `send(h, *msg)` | Queue a message. If the receiver is waiting, it runs next (direct hand-off). | `h` has `SEND`; `msg` readable. `sender` is overwritten with the caller's task id. |
| 1 | `recv(h, *msg, deadline_ns)` | Wait for a message, until `deadline_ns` if non-zero. Returns `Timeout` if the deadline passes. | `h` has `RECV`; `msg` writable |
| 2 | `try_recv(h, *msg)` | As `recv`, but returns `WouldBlock` instead of waiting | As `recv` |
| 3 | `yield()` | Give up the CPU | – |
| 4 | `exit()` | Exit with reason `Clean` | – |
| 5 | `time_ns()` | Nanoseconds since boot | – |
| 6 | `log(ptr, len)` | Write a log line | Text readable; capped at 256 bytes. The kernel prefixes the task's registered name. |
| 7 | `channel_create()` | New channel; returns a handle with `SEND` and `RECV` | Caller is `init` |
| 8 | `spawn(*request)` | Start a service by binary name, with the listed grants as its handles, in order: `SEND` is copied, `RECV` moves (see section 5). Returns the task id. | Caller is `init`; each grant names a handle `init` holds, with rights no wider than `init`'s; `RECV` still available |

The old EL0 syscalls for the framebuffer, surfaces, trace and raw debug
output are removed. The in-kernel built-ins read those directly.

**Pointer validation.** Every user pointer goes through two helpers,
`copy_from_user` and `copy_to_user`. They check that:

- the whole range, computed with overflow checks, lies inside the caller's
  16 GiB window;
- it's aligned for the type;
- it's mapped in the caller's own page table, with read permission (in) or
  write permission (out).

Only then does the copy run, through the physical frames, so PAN stays on.
The kernel runs on one CPU and a task's mappings don't change during its own syscall,
so a passed check can't turn into a faulting copy.

**Errors are returned, not fatal.** A bad pointer, missing handle, missing
right or unknown syscall returns an `Error`. What terminates a task is a
hardware fault from its own code touching memory it doesn't own. That is
recorded with reason `Fault`.

**Direct hand-off.** Tasks can now switch on the way out of a syscall, not
only on timer interrupts. A syscall entry already saves the task's full
state, as an interrupt does, so the scheduler's switch applies to both:

- a `recv` with nothing waiting marks the task blocked and switches away
  immediately;
- a `send` that wakes a waiting receiver switches straight to it. The
  sender stays ready. If the receiver then blocks in `recv` before the next
  tick and the sender is still ready, the sender runs next (sender-return);
  otherwise it runs in its round-robin turn. Without this, a request/response
  pair would wait up to a tick behind every task idling in `wfi`. The timer
  tick still preempts and rotates, so fairness holds at tick granularity;
- a `recv` deadline is checked on each timer tick.

**Delivery-latency metric.** The kernel records the time from each send to
the moment the receiver has the message, for every message, in `metrics`
as `ipc_delivery` (latest and max). This replaces the ping-reported
`ipc_rtt`, which an EL0 ping can no longer write. `ping` logs each batch of
100 round trips as `[ping] rtt_median_ns=… samples=… failures=…` for
tests; the median counts only successful round trips.

## 5. `init`, supervision and boot

**Boot sequence.**

1. While UEFI's file access is still available, the kernel loads every
   `.ELF` in `\EFI\FreshOS\` into a table of named boot images. The
   hardcoded list of four files goes.
2. The kernel creates three channels: the keyboard's events and the shell's
   keys (for the in-kernel built-ins, by number), and **`init`'s inbox**,
   channel 2. A task waits on one channel at a time, so `init` has one
   inbox for everything it hears: task-exit notices from the kernel
   (sender 0) and restart requests (from the in-kernel shell, by number,
   during the transition), told apart by tag.
3. The kernel starts exactly one task, `init` from `INIT.ELF`, with `RECV`
   on its inbox as handle 0. If `INIT.ELF` is missing, the kernel prints
   `INIT.ELF missing from \EFI\FreshOS — nothing to run` and halts (0006).

**`init`'s service table** is compiled into `init`, and is the one place
system policy lives:

```rust
Service { name: "pong",  binary: "PONG.ELF",
          grants: &[recv(PING), send(PONG)],
          start: Autostart, supervise: None, optional: false },
Service { name: "pulse", binary: "PULSE.ELF",
          grants: &[],
          start: Autostart, supervise: Restart { after_ms: 250 }, optional: false },
Service { name: "probe-bad", binary: "PROBEBAD.ELF",
          grants: &[], start: Autostart, supervise: None, optional: true },
```

At startup `init`:

1. creates the channels its table names;
2. spawns each service with its grants.

If the kernel refuses a spawn (for example `ReceiverTaken` or `NotFound`),
`init` logs the service name and the error, skips that service, and carries
on with the rest. An `optional` service whose binary isn't present is
skipped without an error. Test-only entries such as `probe-dup-recv` (which
asks for a second receiver on `PING`) are `optional`, so they only exist
when a test stages them.

**Supervision.** On every task exit or fault, the kernel sends
`TASK_EXITED { task, reason }` on `init`'s inbox, as sender 0; `init`
ignores a `TASK_EXITED` from anyone else. A notice is never lost to a full
inbox: it waits in a small kernel backlog and moves into the inbox, in
order, as `init` makes room. `init` waits on its inbox using `recv` with a
deadline set to its next pending restart, then restarts supervised services
on schedule. It never polls. If `init` itself exits or faults, the kernel
prints why and halts: nothing is left to supervise.

**The kernel keeps the facts; `init` keeps the policy.**

- The kernel keeps a **task registry**, because it witnesses every spawn and
  exit first-hand. For each task: name (from the spawn request), state,
  exit count, last exit reason, and how many times a service of that name
  has started. The MCP views, the flow view and the dashboard read the
  registry, so none of them trusts `init`'s account. It absorbs
  `task_names.rs`. `spawn` records the new task's name and service, and
  moves its receive rights, before the task can first run, so even a task
  that exits on its first tick is charged to the right service. The ELF
  load itself runs with interrupts on.
- `init` owns the rules: what to start, with which grants, and what to
  restart when.

**The in-kernel shell** reads the registry for `services`, and sends
`RESTART_REQUEST { name }` on `init`'s inbox for `restart`,
instead of calling `init`'s internals.

**Deleted:** `init_abi.rs`, `task_names.rs` (absorbed into the registry),
and the hardcoded ELF loading in `main.rs`.

---

## Risks and how they surface

- **Linker flags for the 16 GiB link address.** The exact `rust-lld` option
  must be verified at implementation. The loader's window check turns a
  wrong link address into a clear load error, not a wild jump.
- **Firmware table assumptions.** Copying UEFI's top-level entries assumes
  the kernel never adds top-level slots after boot, which is true today. The
  boot-time check on slot 16 catches a firmware that uses it.
- **HVF behaviour.** The QEMU 11 checks (tlbi, block faults, stack
  registers) were one-off experiments. The harness makes the relevant
  behaviour a permanent test: `probe-bad` exercises faults at EL0 on every
  run.
- **The 100 µs target.** Chosen before measuring. The baseline is recorded
  first; an unreachable target is investigated, not relaxed.

## What changes elsewhere

- **Receive rights move on grant and come home on exit.** `spawn` moves a
  granted `RECV` out of `init`'s handle into the child; the channel records
  where it came from. When the child exits, the right returns to that
  handle, and any messages queued meanwhile wait there for the next
  receiver: the manifesto's "channels buffer during the restart". Asking to
  grant a `RECV` that has already moved gives `ReceiverTaken`.
- **Built-ins during the transition.** `init`'s table starts the in-kernel
  built-ins by the binary name `builtin:<name>`, so the kernel still starts
  only `init` (decision 0006) and `init` owns all startup policy. They run
  at EL1 and hold no handles. Each later spec that moves a built-in out
  deletes its `builtin:` entry.
- `AGENTS.md`: the service model, syscall table, how to add a userbin, and
  how to run tests.
- `docs/Where-We-Are.md`: status once this lands.
- Decision 0006 records the separate-binaries rule this spec implements.
