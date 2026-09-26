#![no_std]
#![no_main]

use freshos_rt::{Startup, entry, exit, log};

entry!(main);

/// Test-only. Attempts one forbidden memory access, chosen by its table
/// argument, and should never survive it:
///   0 = write to its own code, 1 = overflow its stack, else = read that address.
fn main(start: Startup) -> ! {
    match start.arg() {
        0 => {
            log!("writing to own code");
            let code = main as *const () as *mut u32;
            unsafe { code.write_volatile(0) };
        }
        1 => {
            log!("overflowing the stack");
            let depth = recurse(0);
            log!("depth {depth}");
        }
        address => {
            log!("reading {address:#x}");
            let value = unsafe { (address as *const u64).read_volatile() };
            log!("read {value:#x}");
        }
    }
    log!("SURVIVED");
    exit()
}

#[inline(never)]
#[allow(unconditional_recursion)] // the point: run into the guard page
fn recurse(depth: u64) -> u64 {
    let frame = [depth; 64];
    core::hint::black_box(&frame);
    recurse(depth + 1) + frame[0]
}
