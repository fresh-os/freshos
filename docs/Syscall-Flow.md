# FreshOS Syscall Flow

How user-mode tasks talk to the kernel, and what "ring 3" actually means.

---

## The privilege boundary

x86-64 CPUs have four privilege levels (rings 0–3), but modern operating systems only use two:

- **Ring 0** (kernel mode): full access to all hardware. Can execute any instruction, access any memory, change page tables, disable interrupts.
- **Ring 3** (user mode): restricted. Can't touch hardware directly. Can't disable interrupts. Can only access memory pages explicitly marked as user-accessible. Attempting a privileged instruction triggers a fault.

The CPU enforces this in hardware. It checks the current privilege level on every instruction, every memory access. Software can't bypass it.

## Before: everything in ring 0

In our first version, the pinger and ponger tasks ran in ring 0. They called `ipc::send()` and `ipc::recv()` directly — normal Rust function calls into kernel code. This was fast but meaningless as isolation: any task could overwrite any memory, disable interrupts, or corrupt the kernel.

## After: tasks in ring 3, kernel in ring 0

Now the tasks run in ring 3. They can't call kernel functions directly because the kernel's code runs at a different privilege level. Instead, they use the `syscall` instruction — a hardware-provided gate between ring 3 and ring 0.

## What happens on a syscall

When a user task executes `syscall`, the CPU does the following **in a single instruction** (no software involved):

1. **Saves the return address**: copies RIP (the instruction after `syscall`) into RCX
2. **Saves the flags**: copies RFLAGS into R11
3. **Masks the flags**: clears bits specified by the IA32_FMASK MSR — we clear the interrupt flag (IF), so interrupts are disabled on entry
4. **Loads kernel CS and SS**: from the IA32_STAR MSR, giving us ring 0 code and data segments
5. **Jumps to the kernel entry point**: loads RIP from the IA32_LSTAR MSR

After this single instruction, we're in ring 0, interrupts disabled, at our `syscall_entry_stub` in assembly.

## The syscall entry stub

The assembly stub in `kernel/src/syscall.rs` does:

```
syscall_entry_stub:
    mov  r12, rsp              ; save user stack pointer
    mov  rsp, [kernel_rsp]     ; switch to kernel stack
    push r12                   ; save user RSP for return
    push rcx                   ; save user RIP (return address)
    push r11                   ; save user RFLAGS

    ; Set up arguments for Rust dispatcher
    mov  rcx, rax              ; arg 0 = syscall number
    ; rdx = arg 1 (already there)
    ; r8  = arg 2 (already there)

    call syscall_dispatch      ; Rust function, returns result in RAX

    pop  r11                   ; restore RFLAGS
    pop  rcx                   ; restore return RIP
    pop  r12                   ; restore user RSP
    mov  rsp, r12              ; switch back to user stack

    sysretq                    ; return to ring 3
```

The `sysretq` instruction is the mirror of `syscall`: it loads RIP from RCX, RFLAGS from R11, switches to user CS/SS, and we're back in ring 3.

## The Rust dispatcher

`syscall_dispatch` is a normal Rust function that matches on the syscall number:

| Number | Name  | Arguments | What it does |
|--------|-------|-----------|-------------|
| 0 | send | channel_id, msg_ptr | Copy message from user memory into the kernel's channel buffer. Wake any blocked receiver. |
| 1 | recv | channel_id, buf_ptr | If a message is waiting, copy it to user memory. If not, block the task (sleep until a sender wakes it). |
| 2 | yield | — | Give up the current timeslice. |
| 3 | exit | — | Terminate the task. |
| 99 | debug | char | Write a byte to the serial port (temporary, for early testing). |

## A full ping-pong round trip

Here's every ring transition in one ping-pong exchange:

### Pinger sends a PING:
1. **Ring 3**: pinger builds a `Message` on its user stack
2. **Ring 3 → Ring 0**: `syscall` with RAX=0 (send), RDX=0 (channel 0), R8=pointer to message
3. **Ring 0**: kernel copies message into channel 0's ring buffer. Ponger is blocked on this channel, so kernel sets ponger to Ready.
4. **Ring 0 → Ring 3**: `sysretq` back to pinger

### Pinger waits for the PONG:
5. **Ring 3 → Ring 0**: `syscall` with RAX=1 (recv), RDX=1 (channel 1)
6. **Ring 0**: channel 1 is empty. Kernel marks pinger as Blocked, enables interrupts, halts.
7. Timer interrupt fires. Scheduler sees pinger is blocked. Switches to ponger.

### Ponger receives the PING:
8. **Ring 0 → Ring 3**: `iretq` resumes ponger (which was blocked on recv for channel 0). Ponger's earlier recv now completes — the message was already in the buffer.
9. **Ring 3**: ponger reads the message, builds a PONG reply.

### Ponger sends the PONG:
10. **Ring 3 → Ring 0**: `syscall` (send) on channel 1. Kernel copies pong, wakes pinger.
11. **Ring 0 → Ring 3**: `sysretq` back to ponger.

### Ponger waits for the next PING:
12. **Ring 3 → Ring 0**: `syscall` (recv) on channel 0. Empty, blocks.
13. Timer fires, scheduler switches to pinger.

### Pinger gets the PONG:
14. **Ring 0 → Ring 3**: `iretq` resumes pinger. Its blocked recv completes.
15. **Ring 3**: pinger prints "ping N -> pong N".

That's 8 ring transitions per round trip (4 syscalls + 2 timer interrupts + 2 iretq resumes).

## What ring 3 prevents

A user task **cannot**:
- Execute `in`/`out` (port I/O) → general protection fault
- Execute `cli`/`sti` (interrupt control) → general protection fault
- Execute `mov cr3, ...` (change page tables) → general protection fault
- Execute `hlt` → general protection fault
- Access memory not marked USER in the page tables → page fault

The *only* way a ring 3 task can affect the system is through `syscall`. The kernel validates every request.

## What's not yet protected

Memory isolation between tasks is incomplete. All pages are currently marked USER-accessible in a single shared page table. Task A can read task B's memory. The privilege boundary prevents tasks from touching *hardware*, but doesn't prevent them from touching *each other*.

Per-process page tables (each task gets its own view of memory, seeing only its own stack and code) are the next step.

---

## Is this fast enough for real-time?

The manifesto's performance contract:
- **IPC round-trip (small message): sub-1μs**
- **Input-to-photon: sub-5ms**

### Where we are

On real hardware, `syscall`/`sysretq` takes about **50–100 nanoseconds** for the ring transition alone (no work, just enter kernel and return). Our full send path — ring transition, copy a 40-byte message into the ring buffer, check for a blocked receiver, ring transition back — would be roughly **200–500 ns** on modern hardware.

A full ping-pong round trip (send + recv + context switch + send + recv) involves more overhead from the timer-driven scheduling. But the IPC *itself* — one send and one recv — should be well under 1μs on real silicon.

We're running on **emulated x86 via QEMU's TCG on Apple Silicon**, which is 10–50× slower than native. The ping-pong rate we see (~2 per second) is not representative of real hardware performance. On a real x86_64 machine, this would be thousands of round trips per second even at 100 Hz preemption.

### What matters for real-time

The manifesto's latency contracts aren't about raw IPC speed — they're about **bounded worst-case latency**. The key concerns are:

1. **Interrupt latency**: time from hardware signal to ISR execution. On x86_64, this is typically 1–5μs. We don't add much on top — our ISR is a thin assembly stub.

2. **Scheduling latency**: time from "task becomes Ready" to "task is Running." Currently bounded by the PIT period (10ms at 100 Hz). A higher tick rate or a priority scheduler with immediate preemption would reduce this. The APIC timer (per-CPU, higher resolution) is the path to sub-millisecond scheduling.

3. **Syscall overhead**: the `syscall`/`sysretq` path is fast by design — AMD specifically created this instruction to avoid the overhead of `int`/`iret`. Our implementation adds a stack switch and a function call. On real hardware, the full syscall path would be under 1μs.

4. **Lock-free IPC**: our channel implementation uses no locks. The ring buffer is accessed with interrupts briefly disabled during the recv check-and-block sequence (a few hundred nanoseconds). Send is fully non-blocking.

### What needs to change for real-time guarantees

- **Priority-based scheduling** instead of round-robin. The compositor and audio mixer need higher priority than background tasks.
- **APIC timer** instead of PIT. The Local APIC timer is per-CPU and programmable to sub-millisecond periods.
- **Dedicated IST stacks** for latency-critical interrupts (already partially done for double faults).
- **Measurement infrastructure**: the manifesto requires the system to *prove* its latency. We need a high-resolution timer (TSC or HPET) to measure and display actual IPC and input-to-photon latency.

The architecture is right. The syscall path is minimal. The scheduling granularity and priority model are what need work — and those are policy decisions on top of mechanisms that already exist.

---

*FreshOS Syscall Flow — April 2026*
