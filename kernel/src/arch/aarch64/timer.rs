/// ARM generic timer — virtual timer (CNTV_*) for guests under HVF.
///
/// Under a hypervisor, the physical timer (CNTP_*) is owned by the host.
/// The guest uses the virtual timer (CNTV_*) instead. The physical
/// counter (CNTPCT_EL0) is still readable for time_ns().
///
/// Virtual timer PPI: INTID 27.
use crate::serial::serial_println;
use core::sync::atomic::{AtomicU64, Ordering};

/// Virtual timer PPI interrupt ID.
pub const TIMER_INTID: u32 = 27;

/// Timer frequency from CNTFRQ_EL0 (cached at init).
static FREQ: AtomicU64 = AtomicU64::new(0);

/// Ticks per timer interval (cached at init).
static INTERVAL: AtomicU64 = AtomicU64::new(0);

/// Read the physical counter (CNTPCT_EL0) — readable by guests.
#[inline]
fn counter() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("mrs {}, CNTPCT_EL0", out(reg) v, options(nomem, nostack)) };
    v
}

/// Read the timer frequency (CNTFRQ_EL0).
#[inline]
fn frequency() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("mrs {}, CNTFRQ_EL0", out(reg) v, options(nomem, nostack)) };
    v
}

/// Current time in nanoseconds since boot.
#[inline]
pub fn time_ns() -> u64 {
    let freq = FREQ.load(Ordering::Relaxed);
    if freq == 0 {
        return 0;
    }
    let cnt = counter();
    // Avoid overflow: split into seconds + remainder
    let secs = cnt / freq;
    let rem = cnt % freq;
    secs * 1_000_000_000 + rem * 1_000_000_000 / freq
}

/// Configure the virtual timer to fire at `hz` interrupts per second.
///
/// # Safety
/// Must be called after GIC init, with interrupts masked.
pub unsafe fn init(hz: u32) {
    let freq = frequency();
    FREQ.store(freq, Ordering::Relaxed);

    let ticks_per_interval = freq / hz as u64;
    INTERVAL.store(ticks_per_interval, Ordering::Relaxed);

    // Set the virtual timer countdown value
    unsafe {
        core::arch::asm!(
            "msr CNTV_TVAL_EL0, {}",
            in(reg) ticks_per_interval,
            options(nomem, nostack),
        );
    }

    // Enable the virtual timer, unmask interrupt (CNTV_CTL_EL0: ENABLE=1, IMASK=0)
    unsafe {
        core::arch::asm!(
            "msr CNTV_CTL_EL0, {}",
            in(reg) 1u64,
            options(nomem, nostack),
        );
        core::arch::asm!("isb", options(nomem, nostack));
    }

    serial_println!(
        "  Timer: {} Hz (freq={}, interval={})",
        hz,
        freq,
        ticks_per_interval,
    );
}

/// Handle a timer IRQ: rearm the virtual timer for the next interval.
#[inline]
pub fn handle_irq() {
    let interval = INTERVAL.load(Ordering::Relaxed);
    unsafe {
        core::arch::asm!(
            "msr CNTV_TVAL_EL0, {}",
            in(reg) interval,
            options(nomem, nostack),
        );
    }
}
