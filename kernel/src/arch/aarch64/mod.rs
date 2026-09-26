/// aarch64 architecture backend. Board-specific addresses live in `board`.
pub mod addrspace;
pub mod board;
pub mod context;
pub mod exceptions;
pub mod gic;
pub mod paging;
pub mod syscall;
pub mod timer;
pub mod virtio_gpu;

// PL011 register offsets.
const UARTDR: usize = 0x00;
const UARTFR: usize = 0x18;
const UARTLCR_H: usize = 0x2C;
const UARTCR: usize = 0x30;

/// Write a byte to the PL011 UART at `base`, waiting while the TX FIFO is full.
pub fn pl011_write_byte(base: usize, byte: u8) {
    unsafe {
        // Wait for TX FIFO not full (bit 5 of FR)
        while core::ptr::read_volatile((base + UARTFR) as *const u32) & (1 << 5) != 0 {
            core::hint::spin_loop();
        }
        core::ptr::write_volatile((base + UARTDR) as *mut u32, byte as u32);
    }
}

/// Try to read a byte from the PL011 UART at `base`. Returns `Some(byte)` if
/// data is available, `None` if the RX FIFO is empty.
pub fn pl011_try_read(base: usize) -> Option<u8> {
    unsafe {
        // RXFE (bit 4) = 1 means RX FIFO empty
        if core::ptr::read_volatile((base + UARTFR) as *const u32) & (1 << 4) != 0 {
            return None;
        }
        Some((core::ptr::read_volatile((base + UARTDR) as *const u32) & 0xFF) as u8)
    }
}

/// Enable a PL011 the firmware didn't set up: 8n1 with FIFOs, TX and RX on.
///
/// Leaves the baud-rate divisors alone. QEMU ignores them; real hardware
/// will need them set from the UART clock.
pub fn pl011_enable(base: usize) {
    unsafe {
        core::ptr::write_volatile((base + UARTCR) as *mut u32, 0);
        core::ptr::write_volatile((base + UARTLCR_H) as *mut u32, 0x70); // WLEN=8, FEN
        core::ptr::write_volatile((base + UARTCR) as *mut u32, 0x301); // UARTEN, TXE, RXE
    }
}

/// Write a byte to the board's console UART.
pub fn serial_write_byte(byte: u8) {
    pl011_write_byte(board::PL011_BASE, byte);
}

/// Try to read a byte from the board's console UART.
pub fn serial_try_read() -> Option<u8> {
    pl011_try_read(board::PL011_BASE)
}

/// Let EL1 and EL0 use FP/SIMD without trapping: CPACR_EL1.FPEN = 0b11.
///
/// Set explicitly rather than inherited from the firmware. The kernel is
/// built with NEON and uses the vector registers everywhere (memcpy, memset),
/// EL0 services may use them too, and exception.s saves and restores them on
/// every exception; any trap here would be an unhandled exception. Call it
/// first thing at boot.
pub fn enable_fp() {
    unsafe {
        core::arch::asm!(
            "mrs {tmp}, CPACR_EL1",
            "orr {tmp}, {tmp}, #(0b11 << 20)",
            "msr CPACR_EL1, {tmp}",
            "isb",
            tmp = out(reg) _,
            options(nostack),
        );
    }
}

/// Disable interrupts (mask IRQs via DAIF).
#[inline(always)]
pub fn interrupt_disable() {
    unsafe { core::arch::asm!("msr DAIFSet, #0x2", options(nostack)) };
}

/// Enable interrupts (unmask IRQs via DAIF).
#[inline(always)]
pub fn interrupt_enable() {
    unsafe { core::arch::asm!("msr DAIFClr, #0x2", options(nostack)) };
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
            core::arch::asm!("msr DAIFSet, #0x2", options(nostack));
        }
        IrqGuard(daif)
    }
}

impl Drop for IrqGuard {
    #[inline(always)]
    fn drop(&mut self) {
        unsafe { core::arch::asm!("msr DAIF, {}", in(reg) self.0, options(nostack)) };
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
