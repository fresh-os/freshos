# FreshOS Boot Walkthrough

A step-by-step explanation of what happens from power-on to ping-pong, written for someone with no bare-metal programming experience.

---

## 1. The firmware wakes up

The CPU starts in a very primitive state. It can't do much. The first code that runs isn't ours — it's **firmware** burned into the motherboard (or in QEMU's case, the OVMF file we give it). This firmware is called **UEFI**.

UEFI's job is to get the machine into a usable state: initialise RAM, find storage devices, set up the display. Think of it as the hotel concierge who turns on the lights and unlocks the doors before the guests arrive.

## 2. UEFI finds and loads our binary

UEFI looks at the disk (our `esp/` directory in QEMU) for a file at a specific path: `EFI/BOOT/BOOTX64.EFI`. That's our kernel — the `freshos-kernel.efi` file that `cargo build` produces.

UEFI loads this file into RAM and jumps to its entry point — the `main()` function in our `kernel/src/main.rs`. At this point, UEFI is still running. We're a guest in UEFI's house, using its services.

## 3. We ask UEFI for what we need

While UEFI is still active, we ask it two critical questions:

**"Where's the screen?"** — We ask for the **Graphics Output Protocol** (GOP). UEFI tells us: "The framebuffer is a block of memory starting at address X, it's 1280×800 pixels, and each pixel is 4 bytes (blue, green, red, padding)." From this point on, writing a pixel is just writing 4 bytes to the right memory address. No GPU driver, no API — just raw memory writes.

**"What does memory look like?"** — We ask for the **memory map**. UEFI tells us: "Addresses 0x100000–0x0CFFFFFF are free RAM. Addresses 0xFD000000–0xFDFFFFFF are the framebuffer. Addresses 0xFF000000–0xFFFFFFFF are my firmware. Don't touch those." This map is a list of regions, each tagged as "usable," "reserved," "firmware," etc.

We copy all of this information into local variables on the stack, because we're about to lose access to UEFI.

## 4. We kick UEFI out

`exit_boot_services()` is the point of no return. We tell UEFI: "Thanks, I'll take it from here." UEFI shuts down most of its services. We can no longer ask it for anything. We own the machine — all the RAM, the CPU, the display. But we also have *no safety net*. If we crash, there's nothing to catch us. The screen goes black and the CPU resets.

From here on, every single thing the machine does is code we wrote.

## 5. We set up the CPU's ground rules (GDT)

The CPU has a concept of **segments** — regions of memory with specific permissions. Even though modern 64-bit CPUs mostly ignore segmentation, they still *require* a table describing at least two segments: one for code (instructions the CPU executes) and one for data (memory the CPU reads/writes).

The **Global Descriptor Table** is this list. We write it to memory and tell the CPU "here are your rules" by loading a special register (`LGDT`). We also set up a **Task State Segment** (TSS) which tells the CPU "if something goes catastrophically wrong (a double fault), use this emergency stack instead of whatever stack was corrupted."

Without a GDT, the CPU doesn't know what memory it's allowed to execute.

**Source:** `kernel/src/gdt.rs`

## 6. We set up the crash handlers (IDT)

When something goes wrong — dividing by zero, accessing invalid memory, executing garbage bytes — the CPU needs to know what to do. The **Interrupt Descriptor Table** is a list of 256 entries, one for each possible "interrupt" (a CPU event that demands immediate attention).

We fill in entries for the important faults: divide error, page fault, invalid opcode, double fault, etc. Each entry points to a Rust function that logs the error to the serial port and halts. Without an IDT, any fault causes a **triple fault** — the CPU gives up entirely and resets.

We also reserve entry 32 for the timer interrupt (more on that later).

**Source:** `kernel/src/idt.rs`

## 7. We build a memory bookkeeper (frame allocator)

Physical RAM is divided into **frames** — 4,096-byte (4 KiB) chunks. This is the smallest unit of memory the hardware can manage. Our 256 MB of RAM contains about 52,000 usable frames.

The frame allocator is a giant bitmap — one bit per frame. Bit is 0? Frame is free. Bit is 1? Frame is in use. We walk the UEFI memory map and set bits to 0 for every region marked "conventional" (free RAM). Everything else stays 1 (in use — don't touch).

When the kernel needs memory later (for page tables, task stacks, etc.), it asks the frame allocator: "give me a free frame." The allocator scans the bitmap, finds a 0 bit, flips it to 1, and returns the address.

**Source:** `kernel/src/frame_alloc.rs`

## 8. We take control of virtual memory (page tables)

This is the most conceptually tricky part.

The CPU doesn't access physical RAM directly. Every memory address your code uses goes through a **translation layer** called **paging**. The address you write to (a "virtual address") is translated by hardware into a "physical address" that actually hits RAM. The translation rules are stored in **page tables** — tree structures in memory that the CPU walks on every single memory access.

UEFI set up its own page tables (mapping virtual address X to physical address X for everything — an "identity map"). We build our own page tables that do the same thing: virtual address = physical address for the first 4 GiB. We use **2 MiB huge pages** to keep the tables small (6 frames = 24 KiB covers all 4 GiB).

Then we write the address of our top-level page table into the CPU's **CR3 register**. The instant we do this, the CPU switches to our translation rules. If we got anything wrong — if any address the CPU is currently using doesn't map correctly — the machine crashes immediately. We got it right, so execution continues seamlessly.

**Source:** `kernel/src/paging.rs`

## 9. We set up the interrupt controller (PIC)

Hardware devices (keyboard, timer, disk) signal the CPU through **interrupts** — electrical signals on specific wires. These arrive at a chip called the **Programmable Interrupt Controller** (PIC), which translates them into interrupt numbers and delivers them to the CPU.

The PIC defaults to mapping hardware interrupts to numbers 0–15, which collide with CPU exception numbers. We reprogram it to use numbers 32–47 instead, then mask (silence) all interrupt lines. Nothing can interrupt us yet.

**Source:** `kernel/src/pic.rs`

## 10. We create tasks and the scheduler

A **task** is just a saved CPU state and a stack. We create three:

- **Task 0** (boot/idle): the code currently running in `main()`. Its state will be saved naturally the first time the timer interrupts it.
- **Task 1** (pinger): we allocate 16 KiB of RAM for its stack, then write a *fake* set of saved registers at the top — as if this task had been running and was interrupted. The key value is the "return address" which points at the `task_pinger` function. When the scheduler "resumes" this task for the first time, it pops these fake values and jumps to `task_pinger` — the task starts running.
- **Task 2** (ponger): same setup, pointing at `task_ponger`.

**Source:** `kernel/src/scheduler.rs`

## 11. We create IPC channels

Two channels are created — simple ring buffers in memory, each holding up to 16 messages. Channel 0 carries pings (pinger to ponger), channel 1 carries pongs (ponger to pinger). A channel is just an array with a head pointer, a tail pointer, and a count.

**Source:** `kernel/src/ipc.rs`

## 12. We start the timer and let go

This is the moment the system comes alive.

We program the **PIT** (Programmable Interval Timer) — a hardware clock chip — to fire 100 times per second. We unmask IRQ 0 on the PIC so the timer signal reaches the CPU. Then we execute `STI` (Set Interrupt Flag), which tells the CPU: "start accepting interrupts."

From this instant, every 10 milliseconds, the hardware yanks control away from whatever task is running.

## 13. The timer fires — context switch

Here's what happens 100 times per second:

1. The CPU is in the middle of executing task code. The timer chip fires. The CPU **immediately stops** whatever it was doing.
2. The CPU pushes five values onto the current stack: the instruction pointer (where we were), the code segment, the flags register, the stack pointer, and the stack segment. This is the minimum the CPU needs to resume later.
3. The CPU looks up entry 32 in the IDT and jumps to our timer ISR (written in assembly).
4. Our ISR pushes 15 more values — all the general-purpose registers (RAX through R15). Now the *entire* CPU state is saved on this task's stack.
5. The ISR calls `scheduler_tick()` with the current stack pointer.
6. `scheduler_tick` stores that stack pointer in the current task's record, marks it "Ready," and picks the next task. It returns the *other* task's saved stack pointer.
7. The ISR sets the stack pointer to the new value. We're now on a different task's stack.
8. The ISR sends "end of interrupt" to the PIC (acknowledging the timer).
9. The ISR pops 15 registers from the *new* stack — loading the new task's saved register values into the CPU.
10. `IRETQ` pops the final 5 values — restoring the instruction pointer, flags, and stack pointer. The CPU resumes the new task exactly where it left off.

The task that was interrupted has no idea anything happened. It just sees time pass slightly faster than expected.

**Source:** `kernel/src/scheduler.rs` (the `global_asm!` block and `scheduler_tick`)

## 14. A keystroke becomes a pixel

With the scheduler running and the keyboard unmasked, here's the full path for a single keystroke — say, pressing 'h':

### Hardware → Kernel

1. You press 'h'. The PS/2 controller asserts IRQ 1.
2. The PIC translates IRQ 1 into interrupt vector 33 and signals the CPU.
3. The CPU saves the current task's state, switches to ring 0 (using TSS.RSP0), and jumps to our keyboard ISR.
4. The ISR reads scancode `0x23` from port 0x60.
5. The ISR wraps it in a `MSG_IRQ` message and calls `ipc::send(channel_0, msg)`.
6. The channel had a blocked receiver (the keyboard driver). `send` copies the message and sets the driver to Ready.
7. The ISR sends EOI to the PIC and returns. The interrupted task resumes.

**Source:** `kernel/src/keyboard.rs`

### Kernel → Keyboard driver (ring 3)

8. The keyboard driver was blocked on `recv(channel_0)` via the `SYS_RECV` syscall. It was sleeping in `sti; hlt` waiting for a message.
9. The keyboard IRQ woke it from `hlt`. The recv loop checks the channel — the message is there. It dequeues scancode `0x23`.
10. The driver looks up `0x23` in the PS/2 scancode table → ASCII `'h'`.
11. The driver builds a `MSG_KEY_DOWN` message with payload `'h'`.
12. The driver calls `SYS_SEND` on channel 1. This is a syscall: ring 3 → ring 0 → copy message → wake the shell → ring 0 → ring 3.
13. The driver loops back to `recv(channel_0)` and blocks again.

**Source:** `user_kbd_driver()` in `kernel/src/main.rs`

### Keyboard driver → Shell (ring 3)

14. The shell was blocked on `recv(channel_1)`. It's woken by the driver's send.
15. The shell reads the `MSG_KEY_DOWN` message. `payload[0]` is `'h'`.
16. The shell writes the pixel data for `'h'` directly to the framebuffer at its cursor position. This is a plain memory write — the framebuffer pages are mapped USER in the shell's page table.
17. The shell advances the cursor and loops back to `recv`.

**Source:** `user_shell()` in `kernel/src/main.rs`

### What each component can and cannot see

| Component | Can access | Cannot access |
|-----------|-----------|---------------|
| Kernel ISR | All memory, all ports | — |
| Keyboard driver | Its own stack, code, IPC channels | Framebuffer, kernel data, shell's stack |
| Shell | Its own stack, code, IPC channels, framebuffer | Kernel data, driver's stack, port I/O |

The driver can't touch the screen. The shell can't touch the hardware. The kernel mediates everything through IPC and page table grants.

---

## What's actually running

After boot, FreshOS has:

| Component | Ring | Function |
|-----------|------|----------|
| Idle task | 0 | Halts between interrupts (saves power) |
| Keyboard driver | 3 | Receives raw scancodes via IPC, decodes PS/2, sends key events |
| Shell | 3 | Receives key events via IPC, renders characters on the framebuffer |
| Timer ISR | 0 | Fires 100×/sec, saves registers, switches tasks, switches CR3 |
| Keyboard ISR | 0 | Reads port 0x60, sends IPC message, returns |
| Syscall handler | 0 | Entered via `syscall` instruction, dispatches send/recv/fbinfo/etc. |

Three tasks, two IPC channels, two interrupt handlers, one syscall entry point. The microkernel pattern in its simplest useful form.

For a detailed explanation of the syscall mechanism and ring transitions, see `docs/Syscall-Flow.md`.

---

*FreshOS Boot Walkthrough — April 2026*
