pub mod acpi;
/// x86_64 architecture — re-exports from top-level modules.
///
/// During migration, these modules still live at the crate root. This
/// module re-exports them so portable code can begin using `crate::arch::*`.
// The modules themselves are still at the crate root; callers access
// them directly (e.g., crate::gdt, crate::idt). This file provides
// only the architecture-portable interface functions.
pub mod pci;
pub mod virtio_gpu;

/// Write a byte to COM1 (0x3F8) — the serial debug output.
pub fn serial_write_byte(byte: u8) {
    const COM1: u16 = 0x3F8;
    unsafe {
        loop {
            let lsr: u8;
            core::arch::asm!("in al, dx", out("al") lsr, in("dx") COM1 + 5, options(nomem, nostack));
            if lsr & 0x20 != 0 {
                break;
            }
        }
        core::arch::asm!("out dx, al", in("dx") COM1, in("al") byte, options(nomem, nostack));
    }
}

/// Disable interrupts.
#[inline(always)]
pub fn interrupt_disable() {
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) };
}

/// Enable interrupts.
#[inline(always)]
pub fn interrupt_enable() {
    unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
}

/// Wait for interrupt.
#[inline(always)]
pub fn halt() {
    unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
}

/// Current time in nanoseconds.
#[inline]
pub fn time_ns() -> u64 {
    crate::tsc::now_ns()
}

/// Current task ID (from the scheduler).
#[inline]
pub fn current_task() -> usize {
    crate::scheduler::current_task()
}

/// Block the current task (for IPC recv).
pub fn block_current_task() {
    crate::scheduler::block_current();
}

/// Unblock a task by ID (for IPC send).
pub fn unblock_task(task_id: usize) {
    crate::scheduler::unblock(task_id);
}
