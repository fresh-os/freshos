/// Time Stamp Counter — nanosecond-resolution timing.
///
/// The TSC is a per-CPU counter that increments at a fixed rate on modern
/// x86 CPUs. Reading it is a single `rdtsc` instruction (~20 ns). We
/// calibrate the TSC frequency against the PIT at boot so we can convert
/// tick counts to wall-clock time.
///
/// This is the clock everything real-time depends on: IPC latency
/// measurement, the visible latency contract, and frame timing.
use core::sync::atomic::{AtomicU64, Ordering};

use crate::serial::serial_println;

/// TSC ticks per second, determined by calibration.
static TSC_FREQ: AtomicU64 = AtomicU64::new(0);

/// Read the raw TSC value.
#[inline(always)]
pub fn now() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "rdtsc",
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack),
        );
    }
    (hi as u64) << 32 | lo as u64
}

/// Current time in nanoseconds since boot (approximate).
#[inline]
pub fn now_ns() -> u64 {
    let freq = TSC_FREQ.load(Ordering::Relaxed);
    if freq == 0 {
        return 0;
    }
    // Use 128-bit math to avoid overflow: ticks * 1_000_000_000 / freq
    let ticks = now() as u128;
    (ticks * 1_000_000_000 / freq as u128) as u64
}

/// Convert a TSC tick count to nanoseconds.
#[inline]
pub fn ticks_to_ns(ticks: u64) -> u64 {
    let freq = TSC_FREQ.load(Ordering::Relaxed);
    if freq == 0 {
        return 0;
    }
    (ticks as u128 * 1_000_000_000 / freq as u128) as u64
}

/// Convert a TSC tick count to microseconds.
#[inline]
pub fn ticks_to_us(ticks: u64) -> u64 {
    let freq = TSC_FREQ.load(Ordering::Relaxed);
    if freq == 0 {
        return 0;
    }
    (ticks as u128 * 1_000_000 / freq as u128) as u64
}

// ---------------------------------------------------------------------------
// Calibration using PIT channel 2
// ---------------------------------------------------------------------------

fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack));
    }
}

fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe {
        core::arch::asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack));
    }
    val
}

/// Calibrate the TSC against the PIT. Call once during boot.
///
/// Uses PIT channel 2 (the speaker timer) in one-shot mode. Measures
/// how many TSC ticks elapse during a known PIT interval (~10 ms).
///
/// # Safety
/// Must be called with interrupts disabled.
pub unsafe fn calibrate() {
    const PIT_FREQ: u64 = 1_193_182;
    const DIVISOR: u16 = 11932; // ~10 ms at PIT_FREQ

    // Disable speaker, set gate low (to prepare for rising edge trigger)
    let port_b = inb(0x61);
    outb(0x61, (port_b & 0xFC) | 0x01);

    // Program PIT channel 2: mode 0 (one-shot), access lo/hi
    outb(0x43, 0xB0);
    outb(0x42, (DIVISOR & 0xFF) as u8);
    outb(0x42, (DIVISOR >> 8) as u8);

    // Trigger: gate low then high (rising edge starts countdown)
    outb(0x61, inb(0x61) & 0xFE); // gate low
    outb(0x61, inb(0x61) | 0x01); // gate high — countdown starts

    let start = now();

    // Wait for PIT output to go high (bit 5 of port 0x61)
    while inb(0x61) & 0x20 == 0 {
        core::hint::spin_loop();
    }

    let end = now();
    let ticks = end - start;

    // freq = ticks / (DIVISOR / PIT_FREQ) = ticks * PIT_FREQ / DIVISOR
    let freq = ticks * PIT_FREQ / DIVISOR as u64;
    TSC_FREQ.store(freq, Ordering::SeqCst);

    serial_println!(
        "  TSC calibrated: {} MHz ({} ticks in ~10 ms)",
        freq / 1_000_000,
        ticks
    );
}
