# What the x86_64 path taught us

x86_64 support was removed under decision 0003. It was the only path with real
user-mode isolation, so this note keeps the design lessons the Raspberry Pi 4
rebuild at EL0 needs. The code itself is in git history before the removal
commit (`kernel/src/{paging,scheduler,syscall}.rs`, `x86_tasks` in `main.rs`).

## Isolation model

- Every task has its own page-table root. All roots share the kernel's
  mappings, marked supervisor-only, so interrupt handlers work whichever task
  was running.
- A task gets user access only to its own code and stack, plus regions the
  kernel grants explicitly.
- **A resource grant is a mapping.** Only the compositor had the framebuffer
  mapped. The shell and dashboard each had only their own off-screen surface,
  and the input drivers had neither. The kernel decided who could see what.
- Known shortcut: isolation was at 2 MiB granularity. Finer 4 KiB isolation
  was always planned.

## Kernel stacks

On every context switch, x86 updated two values: `TSS.RSP0`, the stack the CPU
uses for interrupts from ring 3, and `kernel_rsp`, used by the syscall entry
stub. Each task therefore needs its own kernel stack, and the switch code must
install it. On aarch64 the equivalent is `SP_EL1`.

## The syscall boundary

- One assembly entry stub swaps to the kernel stack and calls
  `dispatch(nr, args…)`. Rust never runs on the user stack.
- **Known gap, do not copy it:** user pointers were never validated. `send`,
  `recv`, `trace` and `fbinfo` read and wrote user-supplied addresses directly
  (see the `TODO`s in the old `syscall.rs`). The rebuild must check every
  pointer against the calling task's granted regions.
- **Known gap:** `exit` halted forever and never reclaimed the task.

## Device access as a capability

x86 exposed port I/O through syscalls, gated by `is_port_allowed`, which
allowed only the ATA controller ports. The list was global, not per task. The
Pi has no port I/O. The equivalent is mapping a device's MMIO region into the
one task that drives it, decided per task.

## Already portable

Race-free blocking receive (mask interrupts, check for empty, then sleep and
unmask atomically) lives in the shared `kernel/src/ipc.rs` behind
`arch::block_current_task`, and needs no rescuing.
