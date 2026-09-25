/// aarch64 architecture backend. Board-specific addresses live in `board`.
pub mod board;
pub mod context;
pub mod exceptions;
pub mod gic;
pub mod paging;
pub mod syscall;
pub mod timer;
pub mod virtio_gpu;

/// Write a byte to the board's PL011 UART.
pub fn serial_write_byte(byte: u8) {
    const PL011_BASE: usize = board::PL011_BASE;
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
    const PL011_BASE: usize = board::PL011_BASE;
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

/// Masks IRQs until dropped, then restores the previous mask.
///
/// FreshOS runs on one CPU, so this is a sufficient lock for state shared
/// between preemptible tasks. Nesting is safe: an inner guard restores the
/// mask the outer guard set.
pub struct IrqGuard(u64);

impl IrqGuard {
    #[inline(always)]
    pub fn mask() -> Self {
        let daif: u64;
        unsafe {
            core::arch::asm!("mrs {}, DAIF", out(reg) daif, options(nomem, nostack));
            core::arch::asm!("msr DAIFSet, #0x2", options(nomem, nostack));
        }
        IrqGuard(daif)
    }
}

impl Drop for IrqGuard {
    #[inline(always)]
    fn drop(&mut self) {
        unsafe { core::arch::asm!("msr DAIF, {}", in(reg) self.0, options(nomem, nostack)) };
    }
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
