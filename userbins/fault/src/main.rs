#![no_std]
#![no_main]

use freshos_rt::{Startup, entry, log, yield_now};

entry!(main);

/// Deliberately crashes after two beats, to prove that faults are contained
/// and supervised services are restarted.
fn main(_: Startup) -> ! {
    log!("start");
    for beat in 1..=2 {
        for _ in 0..300 {
            yield_now();
        }
        log!("beat {beat}");
    }
    log!("crash test");
    unsafe { core::arch::asm!("brk #0", options(noreturn)) }
}
