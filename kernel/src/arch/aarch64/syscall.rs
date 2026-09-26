/// aarch64 syscall entry: SVC #0 from EL0.
///
/// Syscall ABI: x8 = syscall number, x0-x5 = arguments, x0 = return value.
/// exception.s saves the whole frame and passes it here; the syscalls
/// themselves live in `crate::syscalls`, which validates every argument.
///
/// This module also holds the framebuffer and surface descriptions that the
/// in-kernel built-ins read.
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

// ---------------------------------------------------------------------------
// Framebuffer info — set by the kernel at boot, read by the built-ins
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FbInfo {
    pub address: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub is_bgr: u32,
}

pub struct FbInfoCell(pub UnsafeCell<FbInfo>);
unsafe impl Sync for FbInfoCell {}

pub static FB_INFO_PTR: FbInfoCell = FbInfoCell(UnsafeCell::new(FbInfo {
    address: 0,
    width: 0,
    height: 0,
    stride: 0,
    is_bgr: 0,
}));

pub fn set_fb_info(info: FbInfo) {
    unsafe { *FB_INFO_PTR.0.get() = info };
}

// ---------------------------------------------------------------------------
// Surface info — off-screen surfaces for the compositor
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SurfaceInfo {
    pub address: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
}

const MAX_SURFACES: usize = 4;

pub struct SurfacesCell(pub UnsafeCell<[SurfaceInfo; MAX_SURFACES]>);
unsafe impl Sync for SurfacesCell {}

pub static SURFACES_PTR: SurfacesCell = SurfacesCell(UnsafeCell::new(
    [SurfaceInfo {
        address: 0,
        width: 0,
        height: 0,
        stride: 0,
    }; MAX_SURFACES],
));

static SURFACE_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn add_surface(info: SurfaceInfo) -> usize {
    let idx = SURFACE_COUNT.fetch_add(1, Ordering::SeqCst);
    unsafe { (*SURFACES_PTR.0.get())[idx] = info };
    idx
}

// ---------------------------------------------------------------------------
// Entry — called from exception.s
// ---------------------------------------------------------------------------

/// Syscall entry from exception.s. `frame` is the task's saved register frame
/// (save_all_regs layout: x0..x30 at 8*n, SP_EL0 at 248, ELR at 256, SPSR at 264).
/// Returns the frame to restore.
#[unsafe(no_mangle)]
extern "C" fn syscall_entry_arm(frame: u64) -> u64 {
    use crate::syscalls::Outcome;
    let regs = frame as *mut u64;
    let (nr, args) = unsafe {
        (*regs.add(8), [*regs.add(0), *regs.add(1), *regs.add(2), *regs.add(3), *regs.add(4), *regs.add(5)])
    };
    match crate::syscalls::dispatch(nr, args) {
        Outcome::Return(value) => {
            unsafe { *regs = value as u64 };
            frame
        }
        Outcome::Yield => {
            unsafe { *regs = 0 };
            super::context::switch_away(frame)
        }
        Outcome::Exited | Outcome::Blocked => super::context::switch_away(frame),
        Outcome::HandOff(task) => {
            unsafe { *regs = 0 };
            super::context::hand_off(frame, task)
        }
    }
}
