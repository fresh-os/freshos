/// aarch64 architecture backend — QEMU virt with HVF.
pub mod context;
pub mod exceptions;
pub mod gic;
pub mod paging;
pub mod syscall;
pub mod timer;
pub mod virtio_gpu;

/// Write a byte to PL011 UART at 0x0900_0000 (QEMU virt).
pub fn serial_write_byte(byte: u8) {
    const PL011_BASE: usize = 0x0900_0000;
    const UARTDR: *mut u32 = PL011_BASE as *mut u32;
    const UARTFR: *const u32 = (PL011_BASE + 0x18) as *const u32;
    unsafe {
        // Wait for TX FIFO not full (bit 5 of FR)
        while core::ptr::read_volatile(UARTFR) & (1 << 5) != 0 {
            core::hint::spin_loop();
        }
        core::ptr::write_volatile(UARTDR, byte as u32);
    }
}

/// Try to read a byte from PL011 UART RX. Returns `Some(byte)` if data
/// is available, `None` if the RX FIFO is empty.
pub fn serial_try_read() -> Option<u8> {
    const PL011_BASE: usize = 0x0900_0000;
    const UARTDR: *const u32 = PL011_BASE as *const u32;
    const UARTFR: *const u32 = (PL011_BASE + 0x18) as *const u32;
    unsafe {
        // RXFE (bit 4) = 1 means RX FIFO empty
        if core::ptr::read_volatile(UARTFR) & (1 << 4) != 0 {
            return None;
        }
        Some((core::ptr::read_volatile(UARTDR) & 0xFF) as u8)
    }
}

/// Disable interrupts (mask IRQs via DAIF).
#[inline(always)]
pub fn interrupt_disable() {
    unsafe { core::arch::asm!("msr DAIFSet, #0x2", options(nomem, nostack)) };
}

/// Enable interrupts (unmask IRQs via DAIF).
#[inline(always)]
pub fn interrupt_enable() {
    unsafe { core::arch::asm!("msr DAIFClr, #0x2", options(nomem, nostack)) };
}

/// Wait for interrupt.
#[inline(always)]
pub fn halt() {
    unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
}

/// Current time in nanoseconds.
#[inline]
pub fn time_ns() -> u64 {
    timer::time_ns()
}

/// Current task ID (from the scheduler).
#[inline]
pub fn current_task() -> usize {
    context::current_task()
}

/// Block the current task (for IPC recv).
pub fn block_current_task() {
    context::block_current();
}

/// Unblock a task by ID (for IPC send).
pub fn unblock_task(task_id: usize) {
    context::unblock(task_id);
}
