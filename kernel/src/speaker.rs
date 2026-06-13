/// PC speaker — the machine's voice.
///
/// Uses PIT channel 2 and the speaker gate (port 0x61) to generate
/// square-wave tones. Not hi-fi, but it gives the system an acoustic
/// identity: a startup chime, keystroke clicks, workspace switch tones.
///
/// "You know your OS is working without looking at the screen, the same
/// way you know a car engine is healthy by its sound."
///                                              — FreshOS Manifesto
use crate::tsc;

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

/// Play a tone at the given frequency for the given duration.
/// Blocks the caller (busy-waits on TSC). Keep durations short (< 200ms).
pub fn beep(freq_hz: u32, duration_ms: u32) {
    if freq_hz == 0 || duration_ms == 0 {
        return;
    }

    let divisor = 1_193_182 / freq_hz;

    // Program PIT channel 2: mode 3 (square wave), lo/hi access
    outb(0x43, 0xB6);
    outb(0x42, (divisor & 0xFF) as u8);
    outb(0x42, ((divisor >> 8) & 0xFF) as u8);

    // Enable speaker gate (bits 0 and 1 of port 0x61)
    let prev = inb(0x61);
    outb(0x61, prev | 0x03);

    // Wait for the duration
    let start = tsc::now_ns();
    let target = duration_ms as u64 * 1_000_000;
    while tsc::now_ns().wrapping_sub(start) < target {
        core::hint::spin_loop();
    }

    // Disable speaker
    outb(0x61, inb(0x61) & 0xFC);
}

/// Short silence between notes.
pub fn pause(ms: u32) {
    let start = tsc::now_ns();
    let target = ms as u64 * 1_000_000;
    while tsc::now_ns().wrapping_sub(start) < target {
        core::hint::spin_loop();
    }
}

/// The FreshOS startup chime — a clean ascending arpeggio.
pub fn boot_chime() {
    beep(523, 60); // C5
    pause(20);
    beep(659, 60); // E5
    pause(20);
    beep(784, 60); // G5
    pause(20);
    beep(1047, 120); // C6 (sustained)
}
