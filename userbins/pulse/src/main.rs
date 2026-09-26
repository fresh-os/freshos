#![no_std]
#![no_main]

use freshos_rt::{Startup, entry, exit, log, yield_now};

entry!(main);

/// Beats three times and exits cleanly, to exercise supervised restarts.
fn main(_: Startup) -> ! {
    log!("start");
    for beat in 1..=3 {
        for _ in 0..500 {
            yield_now();
        }
        log!("beat {beat}");
    }
    log!("exit");
    exit()
}
