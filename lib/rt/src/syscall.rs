//! The only `svc` in any userbin. The kernel preserves every register except
//! x0 across a syscall (exception.s saves and restores the full frame).
use freshos_abi::Error;

#[inline(always)]
pub(crate) fn svc(nr: u64, a0: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") nr,
            inlateout("x0") a0 as i64 => ret,
            in("x1") a1,
            in("x2") a2,
            options(nostack),
        );
    }
    ret
}

pub(crate) fn check(ret: i64) -> Result<u64, Error> {
    if ret < 0 {
        Err(Error::from_code(ret))
    } else {
        Ok(ret as u64)
    }
}
