/// aarch64 syscall dispatch — handles SVC #0 from EL0.
///
/// Syscall ABI (AAPCS64-inspired):
///   x8  = syscall number
///   x0-x5 = arguments
///   x0  = return value
///
/// Called from exception.s's lower_sync_entry after saving all registers.
use crate::ipc;
use crate::serial::serial_println;

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

// Syscall numbers (must match userspace wrappers)
pub const SYS_SEND: u64 = 0;
pub const SYS_RECV: u64 = 1;
pub const SYS_YIELD: u64 = 2;
pub const SYS_EXIT: u64 = 3;
pub const SYS_FBINFO: u64 = 4;
pub const SYS_TIME: u64 = 5;
pub const SYS_TRACE: u64 = 6;
pub const SYS_SURFACE_INFO: u64 = 7;
pub const SYS_DEBUG: u64 = 99;

// ---------------------------------------------------------------------------
// Framebuffer info — set by kernel at boot, read by SYS_FBINFO
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

pub fn fb_address() -> u64 {
    unsafe { (*FB_INFO_PTR.0.get()).address }
}

pub fn fb_size() -> u64 {
    let info = unsafe { &*FB_INFO_PTR.0.get() };
    info.stride as u64 * info.height as u64 * 4
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
// Dispatch — called from exception.s
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
extern "C" fn syscall_dispatch_arm(
    nr: u64,
    arg0: u64,
    arg1: u64,
    _arg2: u64,
    _arg3: u64,
    _arg4: u64,
) -> i64 {
    match nr {
        SYS_SEND => sys_send(arg0 as u32, arg1),
        SYS_RECV => sys_recv(arg0 as u32, arg1),
        SYS_YIELD => {
            // Enable interrupts and wait — timer will preempt us
            super::interrupt_enable();
            super::halt();
            0
        }
        SYS_EXIT => {
            serial_println!("task {} exited", super::context::current_task());
            super::context::terminate_current_with_reason(crate::init_abi::SERVICE_EXIT_CLEAN)
        }
        SYS_FBINFO => {
            let buf = arg0 as *mut FbInfo;
            unsafe { *buf = *FB_INFO_PTR.0.get() };
            0
        }
        SYS_TIME => super::timer::time_ns() as i64,
        SYS_SURFACE_INFO => {
            let idx = arg0 as usize;
            if idx >= SURFACE_COUNT.load(Ordering::SeqCst) {
                return -1;
            }
            let buf = arg1 as *mut SurfaceInfo;
            unsafe { *buf = (*SURFACES_PTR.0.get())[idx] };
            0
        }
        SYS_TRACE => {
            let buf = arg0 as *mut ipc::TraceEntry;
            let max = arg1 as usize;
            let slice = unsafe { core::slice::from_raw_parts_mut(buf, max) };
            ipc::trace_read(slice, max) as i64
        }
        SYS_DEBUG => {
            crate::serial::Serial::write_byte_raw(arg0 as u8);
            0
        }
        _ => -1,
    }
}

fn sys_send(channel_id: u32, msg_ptr: u64) -> i64 {
    let msg = unsafe { *(msg_ptr as *const ipc::Message) };
    match ipc::send(channel_id, &msg) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

fn sys_recv(channel_id: u32, buf_ptr: u64) -> i64 {
    // Enable interrupts so the scheduler can run while we block
    super::interrupt_enable();

    match ipc::recv(channel_id) {
        Ok(msg) => {
            unsafe { *(buf_ptr as *mut ipc::Message) = msg };
            0
        }
        Err(_) => -1,
    }
}
