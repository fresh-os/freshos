#![no_std]
#![no_main]

use core::arch::asm;

const SYS_YIELD: u64 = 2;
const SYS_DEBUG: u64 = 99;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[inline(always)]
fn syscall0(nr: u64) -> i64 {
    let mut ret = 0i64;
    unsafe {
        #[cfg(target_arch = "aarch64")]
        asm!(
            "svc #0",
            in("x8") nr,
            inlateout("x0") ret,
            clobber_abi("C"),
            options(nostack),
        );
        #[cfg(target_arch = "x86_64")]
        asm!(
            "syscall",
            in("rax") nr,
            lateout("rax") ret,
            out("rcx") _,
            out("r11") _,
            options(nostack),
        );
    }
    ret
}

#[inline(always)]
fn syscall1(nr: u64, arg0: u64) -> i64 {
    let mut ret = arg0 as i64;
    unsafe {
        #[cfg(target_arch = "aarch64")]
        asm!(
            "svc #0",
            in("x8") nr,
            inlateout("x0") ret,
            clobber_abi("C"),
            options(nostack),
        );
        #[cfg(target_arch = "x86_64")]
        asm!(
            "syscall",
            in("rax") nr,
            in("rdx") arg0,
            lateout("rax") ret,
            out("rcx") _,
            out("r11") _,
            options(nostack),
        );
    }
    ret
}

fn debug_byte(byte: u8) {
    let _ = syscall1(SYS_DEBUG, byte as u64);
}

fn log(s: &str) {
    for &byte in s.as_bytes() {
        debug_byte(byte);
    }
    debug_byte(b'\n');
}

fn yield_now() {
    let _ = syscall0(SYS_YIELD);
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    log("[fault] ring 3 start");
    for msg in ["[fault] beat 1", "[fault] beat 2"] {
        for _ in 0..300 {
            yield_now();
        }
        log(msg);
    }
    log("[fault] crash test");
    #[cfg(target_arch = "aarch64")]
    unsafe { asm!("brk #0", options(noreturn)) }
    #[cfg(target_arch = "x86_64")]
    unsafe { asm!("ud2", options(noreturn)) }
}
