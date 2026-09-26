#![no_std]
#![no_main]

use freshos_rt::abi::{MAX_LOG, USER_BASE, USER_SIZE, USER_STACK_SIZE, sys};
use freshos_rt::{Startup, entry, exit, log, time_ns, yield_now};

entry!(main);

/// Test-only. Chosen by its table argument:
///   0 = write to its own code, 1 = overflow its stack, 2 = check the syscall
///   boundary (log sanitising and capping, bad pointers, unknown syscalls,
///   TPIDR_EL0 and FP/SIMD state across switches) and exit cleanly,
///   3 = only the TPIDR_EL0 check, as a second instance writing its own value
///   at the same time, 4 = read the virtual counter (CNTVCT_EL0), which the
///   kernel denies EL0, else = read that address.
/// The forbidden accesses should never survive.
fn main(start: Startup) -> ! {
    match start.arg() {
        3 => {
            check_tpidr();
            exit()
        }
        4 => {
            log!("reading the virtual counter");
            let ticks: u64;
            unsafe { core::arch::asm!("mrs {}, CNTVCT_EL0", out(reg) ticks, options(nomem, nostack)) };
            log!("counter {ticks:#x}");
        }
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
        2 => check_abi(),
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

/// A raw syscall, to pass what freshos-rt's safe wrappers never would. This
/// test binary is the only userbin with its own `svc`.
fn raw_svc(nr: u64, a0: u64, a1: u64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") nr,
            inlateout("x0") a0 as i64 => ret,
            in("x1") a1,
            clobber_abi("C"),
            options(nostack),
        );
    }
    ret
}

fn read_tpidr() -> u64 {
    let value: u64;
    unsafe { core::arch::asm!("mrs {}, TPIDR_EL0", out(reg) value, options(nomem, nostack)) };
    value
}

/// TPIDR_EL0 is EL0-writable, so it must be per task: a new task starts with
/// 0 (not what the firmware or another task left), and a value written here
/// survives other tasks running. Two instances run this at once (probe-abi
/// and probe-tpidr), each writing its own value, so a register shared between
/// tasks shows up as "corrupt". No Rust code in a userbin uses TPIDR_EL0
/// (there is no thread-local storage), so only the kernel could change it.
fn check_tpidr() {
    log!("tpidr start={:#x}", read_tpidr());
    // Distinct per instance: the two start at different times.
    let mine = 0x7D1D_0000_0000_0000 | (time_ns() & 0xFFFF_FFFF);
    unsafe { core::arch::asm!("msr TPIDR_EL0, {}", in(reg) mine, options(nomem, nostack)) };
    // Long enough for both instances to overlap, and for many ticks.
    let deadline = time_ns() + 200_000_000;
    while time_ns() < deadline {
        yield_now();
        for _ in 0..10_000 {
            core::hint::spin_loop();
        }
    }
    let now = read_tpidr();
    log!("tpidr={}", if now == mine { "intact" } else { "corrupt" });
}

fn check_abi() -> ! {
    check_tpidr();

    // The kernel must turn these into '?' on one line: newline, ESC.
    log!("sanitise a\nb\x1b[31mc");

    // Longer than MAX_LOG: the kernel prints the first MAX_LOG bytes and "…".
    let mut long = [b'x'; MAX_LOG + 44];
    long[..4].copy_from_slice(b"cap:");
    raw_svc(sys::LOG, long.as_ptr() as u64, long.len() as u64);

    // Bad pointers are errors, not faults.
    let outside = raw_svc(sys::LOG, 0x4000_0000, 8);
    log!("log outside window = {outside}");
    let stack_bottom = USER_BASE + USER_SIZE - USER_STACK_SIZE;
    let straddle = raw_svc(sys::LOG, stack_bottom - 8, 16);
    log!("log straddling guard page = {straddle}");

    let unknown = raw_svc(999, 0, 0);
    log!("unknown syscall = {unknown}");

    log!("fp={}", if fp_survives_switches() { "intact" } else { "corrupt" });
    log!("done");
    exit()
}

/// Fill every vector register and FPCR with a pattern, yield and spin across
/// several timer ticks, then check that all of it is still there. Done in one
/// asm block so that no compiled code touches the registers in between.
fn fp_survives_switches() -> bool {
    let pattern: u64 = 0x5eed_f00d_0000_0000;
    let rounding_toward_zero: u64 = 0b11 << 22;
    let mismatch: u64;
    let fpcr: u64;
    unsafe {
        core::arch::asm!(
        "add x11, x9, #0", "fmov d0, x11", "mvn x11, x11", "mov v0.d[1], x11",
        "add x11, x9, #1", "fmov d1, x11", "mvn x11, x11", "mov v1.d[1], x11",
        "add x11, x9, #2", "fmov d2, x11", "mvn x11, x11", "mov v2.d[1], x11",
        "add x11, x9, #3", "fmov d3, x11", "mvn x11, x11", "mov v3.d[1], x11",
        "add x11, x9, #4", "fmov d4, x11", "mvn x11, x11", "mov v4.d[1], x11",
        "add x11, x9, #5", "fmov d5, x11", "mvn x11, x11", "mov v5.d[1], x11",
        "add x11, x9, #6", "fmov d6, x11", "mvn x11, x11", "mov v6.d[1], x11",
        "add x11, x9, #7", "fmov d7, x11", "mvn x11, x11", "mov v7.d[1], x11",
        "add x11, x9, #8", "fmov d8, x11", "mvn x11, x11", "mov v8.d[1], x11",
        "add x11, x9, #9", "fmov d9, x11", "mvn x11, x11", "mov v9.d[1], x11",
        "add x11, x9, #10", "fmov d10, x11", "mvn x11, x11", "mov v10.d[1], x11",
        "add x11, x9, #11", "fmov d11, x11", "mvn x11, x11", "mov v11.d[1], x11",
        "add x11, x9, #12", "fmov d12, x11", "mvn x11, x11", "mov v12.d[1], x11",
        "add x11, x9, #13", "fmov d13, x11", "mvn x11, x11", "mov v13.d[1], x11",
        "add x11, x9, #14", "fmov d14, x11", "mvn x11, x11", "mov v14.d[1], x11",
        "add x11, x9, #15", "fmov d15, x11", "mvn x11, x11", "mov v15.d[1], x11",
        "add x11, x9, #16", "fmov d16, x11", "mvn x11, x11", "mov v16.d[1], x11",
        "add x11, x9, #17", "fmov d17, x11", "mvn x11, x11", "mov v17.d[1], x11",
        "add x11, x9, #18", "fmov d18, x11", "mvn x11, x11", "mov v18.d[1], x11",
        "add x11, x9, #19", "fmov d19, x11", "mvn x11, x11", "mov v19.d[1], x11",
        "add x11, x9, #20", "fmov d20, x11", "mvn x11, x11", "mov v20.d[1], x11",
        "add x11, x9, #21", "fmov d21, x11", "mvn x11, x11", "mov v21.d[1], x11",
        "add x11, x9, #22", "fmov d22, x11", "mvn x11, x11", "mov v22.d[1], x11",
        "add x11, x9, #23", "fmov d23, x11", "mvn x11, x11", "mov v23.d[1], x11",
        "add x11, x9, #24", "fmov d24, x11", "mvn x11, x11", "mov v24.d[1], x11",
        "add x11, x9, #25", "fmov d25, x11", "mvn x11, x11", "mov v25.d[1], x11",
        "add x11, x9, #26", "fmov d26, x11", "mvn x11, x11", "mov v26.d[1], x11",
        "add x11, x9, #27", "fmov d27, x11", "mvn x11, x11", "mov v27.d[1], x11",
        "add x11, x9, #28", "fmov d28, x11", "mvn x11, x11", "mov v28.d[1], x11",
        "add x11, x9, #29", "fmov d29, x11", "mvn x11, x11", "mov v29.d[1], x11",
        "add x11, x9, #30", "fmov d30, x11", "mvn x11, x11", "mov v30.d[1], x11",
        "add x11, x9, #31", "fmov d31, x11", "mvn x11, x11", "mov v31.d[1], x11",
        "msr fpcr, x14",
        "mov x10, #8",
        "2:",
        "mov x8, #{yield_nr}",
        "svc #0",
        "movz x11, #0x40, lsl #16",
        "3:",
        "subs x11, x11, #1",
        "b.ne 3b",
        "subs x10, x10, #1",
        "b.ne 2b",
        "mov x12, #0",
        "fmov x11, d0", "add x13, x9, #0", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v0.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d1", "add x13, x9, #1", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v1.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d2", "add x13, x9, #2", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v2.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d3", "add x13, x9, #3", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v3.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d4", "add x13, x9, #4", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v4.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d5", "add x13, x9, #5", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v5.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d6", "add x13, x9, #6", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v6.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d7", "add x13, x9, #7", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v7.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d8", "add x13, x9, #8", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v8.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d9", "add x13, x9, #9", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v9.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d10", "add x13, x9, #10", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v10.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d11", "add x13, x9, #11", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v11.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d12", "add x13, x9, #12", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v12.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d13", "add x13, x9, #13", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v13.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d14", "add x13, x9, #14", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v14.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d15", "add x13, x9, #15", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v15.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d16", "add x13, x9, #16", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v16.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d17", "add x13, x9, #17", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v17.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d18", "add x13, x9, #18", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v18.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d19", "add x13, x9, #19", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v19.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d20", "add x13, x9, #20", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v20.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d21", "add x13, x9, #21", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v21.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d22", "add x13, x9, #22", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v22.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d23", "add x13, x9, #23", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v23.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d24", "add x13, x9, #24", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v24.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d25", "add x13, x9, #25", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v25.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d26", "add x13, x9, #26", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v26.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d27", "add x13, x9, #27", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v27.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d28", "add x13, x9, #28", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v28.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d29", "add x13, x9, #29", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v29.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d30", "add x13, x9, #30", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v30.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "fmov x11, d31", "add x13, x9, #31", "eor x11, x11, x13", "orr x12, x12, x11",
        "mov x11, v31.d[1]", "mvn x13, x13", "eor x11, x11, x13", "orr x12, x12, x11",
        "mrs x15, fpcr",
        "msr fpcr, xzr",
            yield_nr = const sys::YIELD,
            in("x9") pattern,
            in("x14") rounding_toward_zero,
            out("x12") mismatch,
            out("x15") fpcr,
            clobber_abi("C"),
            options(nostack),
        );
    }
    mismatch == 0 && fpcr & (0b11 << 22) == rounding_toward_zero
}
