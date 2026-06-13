// aarch64 exception vector table
//
// 16 entries × 128 bytes each = 2048 bytes, installed at VBAR_EL1.
// Each vector entry has room for 32 instructions.
//
// We care about three entries:
//   - current_el_spx_irq  (0x280): timer/device IRQs while in EL1
//   - lower_el_a64_sync   (0x400): SVC syscalls + faults from EL0
//   - lower_el_a64_irq    (0x480): timer preempting user tasks

.section .text
.balign 2048

.global exception_vectors
exception_vectors:

// -------------------------------------------------------
// Current EL with SP_EL0 (not used — kernel uses SP_EL1)
// -------------------------------------------------------
// 0x000: Synchronous
    b       unhandled_exception
.balign 128
// 0x080: IRQ
    b       unhandled_exception
.balign 128
// 0x100: FIQ
    b       unhandled_exception
.balign 128
// 0x180: SError
    b       unhandled_exception
.balign 128

// -------------------------------------------------------
// Current EL with SP_ELx (kernel running, IRQ fires)
// -------------------------------------------------------
// 0x200: Synchronous
    b       current_sync_entry
.balign 128
// 0x280: IRQ — timer fires while kernel is running
    b       kernel_irq_entry
.balign 128
// 0x300: FIQ
    b       unhandled_exception
.balign 128
// 0x380: SError
    b       unhandled_exception
.balign 128

// -------------------------------------------------------
// Lower EL using AArch64 (user tasks in EL0)
// -------------------------------------------------------
// 0x400: Synchronous — SVC syscalls, page faults
    b       lower_sync_entry
.balign 128
// 0x480: IRQ — timer preempts user task
    b       lower_irq_entry
.balign 128
// 0x500: FIQ
    b       unhandled_exception
.balign 128
// 0x580: SError
    b       unhandled_exception
.balign 128

// -------------------------------------------------------
// Lower EL using AArch32 (not supported)
// -------------------------------------------------------
// 0x600: Synchronous
    b       unhandled_exception
.balign 128
// 0x680: IRQ
    b       unhandled_exception
.balign 128
// 0x700: FIQ
    b       unhandled_exception
.balign 128
// 0x780: SError
    b       unhandled_exception
.balign 128


// ===================================================================
// Heavyweight save: all 31 GPRs + SP_EL0 + ELR_EL1 + SPSR_EL1
// Used for IRQ preemption where any register could be live.
// Total: 34 × 8 = 272 bytes on stack.
// ===================================================================

.macro save_all_regs
    sub     sp, sp, #272
    stp     x0,  x1,  [sp, #0]
    stp     x2,  x3,  [sp, #16]
    stp     x4,  x5,  [sp, #32]
    stp     x6,  x7,  [sp, #48]
    stp     x8,  x9,  [sp, #64]
    stp     x10, x11, [sp, #80]
    stp     x12, x13, [sp, #96]
    stp     x14, x15, [sp, #112]
    stp     x16, x17, [sp, #128]
    stp     x18, x19, [sp, #144]
    stp     x20, x21, [sp, #160]
    stp     x22, x23, [sp, #176]
    stp     x24, x25, [sp, #192]
    stp     x26, x27, [sp, #208]
    stp     x28, x29, [sp, #224]
    str     x30,      [sp, #240]    // LR

    mrs     x10, SP_EL0
    mrs     x11, ELR_EL1
    mrs     x12, SPSR_EL1
    stp     x10, x11, [sp, #248]    // SP_EL0, ELR_EL1
    str     x12,      [sp, #264]    // SPSR_EL1
.endm

.macro restore_all_regs
    ldp     x10, x11, [sp, #248]
    ldr     x12,      [sp, #264]
    msr     SP_EL0, x10
    msr     ELR_EL1, x11
    msr     SPSR_EL1, x12

    ldp     x0,  x1,  [sp, #0]
    ldp     x2,  x3,  [sp, #16]
    ldp     x4,  x5,  [sp, #32]
    ldp     x6,  x7,  [sp, #48]
    ldp     x8,  x9,  [sp, #64]
    ldp     x10, x11, [sp, #80]
    ldp     x12, x13, [sp, #96]
    ldp     x14, x15, [sp, #112]
    ldp     x16, x17, [sp, #128]
    ldp     x18, x19, [sp, #144]
    ldp     x20, x21, [sp, #160]
    ldp     x22, x23, [sp, #176]
    ldp     x24, x25, [sp, #192]
    ldp     x26, x27, [sp, #208]
    ldp     x28, x29, [sp, #224]
    ldr     x30,      [sp, #240]
    add     sp, sp, #272
.endm


// ===================================================================
// kernel_irq_entry: IRQ while in EL1 (kernel code running)
//
// Save all regs, call scheduler_tick_arm(sp) → new sp, restore, eret.
// ===================================================================

kernel_irq_entry:
    save_all_regs

    mov     x0, sp              // arg0: current stack pointer
    bl      scheduler_tick_arm  // returns new SP in x0

    mov     sp, x0              // switch to (possibly different) task's stack

    restore_all_regs
    eret


// ===================================================================
// current_sync_entry: synchronous exception while running at EL1
//
// On the ARM/HVF path, scheduled services currently run at EL1. If one of
// those tasks faults, route to a containment handler instead of panicking the
// whole kernel immediately.
// ===================================================================

current_sync_entry:
    save_all_regs

    mrs     x0, ESR_EL1
    mrs     x1, ELR_EL1
    mrs     x2, FAR_EL1
    bl      exception_current_sync
    b       .


// ===================================================================
// lower_irq_entry: IRQ while in EL0 (user task running)
//
// Same as kernel IRQ — the CPU has already switched to SP_EL1.
// We save SP_EL0 (user stack) as part of the context.
// ===================================================================

lower_irq_entry:
    save_all_regs

    mov     x0, sp
    bl      scheduler_tick_arm

    mov     sp, x0

    restore_all_regs
    eret


// ===================================================================
// lower_sync_entry: synchronous exception from EL0
//
// Read ESR_EL1 to determine the exception class:
//   EC=0x15 (SVC from AArch64): dispatch syscall
//   EC=0x20 (instruction abort from lower EL): page fault
//   EC=0x24 (data abort from lower EL): page fault
//   Anything else: unhandled exception panic
//
// SVC ABI:
//   x8  = syscall number
//   x0-x5 = arguments
//   x0  = return value (written back before eret)
// ===================================================================

lower_sync_entry:
    // Save caller-clobbered regs + system regs.
    // We use the full save because the scheduler might preempt us
    // (e.g., during blocking recv).
    save_all_regs

    // Check exception class
    mrs     x9, ESR_EL1
    lsr     x10, x9, #26       // EC = bits[31:26]
    and     x10, x10, #0x3F

    cmp     x10, #0x15          // SVC from AArch64?
    b.eq    svc_dispatch

    // Not an SVC — route EL0 faults to the lower-EL containment path.
    mov     x0, x9               // ESR_EL1
    mrs     x1, ELR_EL1
    mrs     x2, FAR_EL1
    bl      exception_lower_sync
    b       .

svc_dispatch:
    // Reload the saved x8 (syscall number) and x0-x3 (args) from the stack.
    // save_all_regs layout: x0 at [sp+0], x8 at [sp+64]
    ldr     x0, [sp, #64]       // arg0 = saved x8 (syscall number)
    ldr     x1, [sp, #0]        // arg1 = saved x0 (first arg)
    ldr     x2, [sp, #8]        // arg2 = saved x1
    ldr     x3, [sp, #16]       // arg3 = saved x2
    ldr     x4, [sp, #24]       // arg4 = saved x3
    ldr     x5, [sp, #32]       // arg5 = saved x4

    bl      syscall_dispatch_arm // returns result in x0

    // Write return value into the saved x0 slot so restore_all_regs
    // puts it back in x0 for the user task.
    str     x0, [sp, #0]

    restore_all_regs
    eret


// ===================================================================
// unhandled_exception: catch-all panic
// ===================================================================

unhandled_exception:
    mrs     x0, ESR_EL1         // arg0: exception syndrome
    mrs     x1, ELR_EL1         // arg1: exception link register
    mrs     x2, FAR_EL1         // arg2: fault address register
    bl      exception_panic     // Rust handler — does not return
    b       .                   // safety loop
