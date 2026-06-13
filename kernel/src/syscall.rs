/// System call interface — the gate between user mode and the kernel.
///
/// User tasks invoke `syscall` with:
///   RAX = syscall number
///   RCX = (clobbered by CPU — holds return RIP)
///   RDX, R8, R9, R10 = arguments (up to 4)
///   R11 = (clobbered by CPU — holds saved RFLAGS)
///
/// The `syscall` instruction:
///   1. Saves RIP in RCX, RFLAGS in R11
///   2. Masks RFLAGS with the IA32_FMASK MSR (we clear IF to disable interrupts)
///   3. Loads CS from STAR[47:32], SS from STAR[47:32]+8
///   4. Loads RIP from IA32_LSTAR
///   → We're now in ring 0, on the kernel stack, interrupts disabled.
///
/// The handler dispatches based on RAX, then `sysretq` returns to user mode.
///
/// Syscall numbers:
///   0 = send(channel: u32, msg_ptr: *const Message) -> i64
///   1 = recv(channel: u32) -> i64  (blocks; message written to caller's stack)
///   2 = yield — voluntarily give up the timeslice
///   3 = exit  — terminate the current task
use crate::ipc;
use crate::scheduler;
use crate::serial::serial_println;

// Syscall numbers
pub const SYS_SEND: u64 = 0;
pub const SYS_RECV: u64 = 1;
pub const SYS_YIELD: u64 = 2;
pub const SYS_EXIT: u64 = 3;
pub const SYS_FBINFO: u64 = 4;
pub const SYS_TIME: u64 = 5;
pub const SYS_TRACE: u64 = 6;
pub const SYS_SURFACE_INFO: u64 = 7;
pub const SYS_BEEP: u64 = 8; // play a tone: freq_hz, duration_ms
pub const SYS_PORT_IN8: u64 = 10;
pub const SYS_PORT_OUT8: u64 = 11; // write byte to I/O port
pub const SYS_PORT_INS16: u64 = 12; // read N 16-bit words from port into buffer
pub const SYS_PORT_OUTS16: u64 = 13; // write N 16-bit words from buffer to port
pub const SYS_PRESENT_RECT: u64 = 14; // virtio-GPU transfer+flush of a rect: x, y, w, h
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

use core::cell::UnsafeCell;

struct FbInfoCell(UnsafeCell<FbInfo>);
unsafe impl Sync for FbInfoCell {}

static FB_INFO: FbInfoCell = FbInfoCell(UnsafeCell::new(FbInfo {
    address: 0,
    width: 0,
    height: 0,
    stride: 0,
    is_bgr: 0,
}));

/// Store framebuffer info for the SYS_FBINFO syscall. Call once at boot.
pub fn set_fb_info(info: FbInfo) {
    unsafe { *FB_INFO.0.get() = info };
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

struct SurfacesCell(UnsafeCell<[SurfaceInfo; MAX_SURFACES]>);
unsafe impl Sync for SurfacesCell {}

static SURFACES: SurfacesCell = SurfacesCell(UnsafeCell::new(
    [SurfaceInfo {
        address: 0,
        width: 0,
        height: 0,
        stride: 0,
    }; MAX_SURFACES],
));

static SURFACE_COUNT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Register a surface. Call during boot before tasks start.
pub fn add_surface(info: SurfaceInfo) -> usize {
    let idx = SURFACE_COUNT.fetch_add(1, core::sync::atomic::Ordering::SeqCst);
    unsafe { (*SURFACES.0.get())[idx] = info };
    idx
}

/// Number of registered surfaces.
pub fn surface_count() -> usize {
    SURFACE_COUNT.load(core::sync::atomic::Ordering::SeqCst)
}

/// The framebuffer's physical base address (for page table mapping).
pub fn fb_address() -> u64 {
    unsafe { (*FB_INFO.0.get()).address }
}

/// Framebuffer size in bytes.
pub fn fb_size() -> u64 {
    let info = unsafe { &*FB_INFO.0.get() };
    info.stride as u64 * info.height as u64 * 4
}

// x86-64 MSRs for syscall/sysret
const IA32_STAR: u32 = 0xC000_0081;
const IA32_LSTAR: u32 = 0xC000_0082;
const IA32_FMASK: u32 = 0xC000_0084;
const IA32_EFER: u32 = 0xC000_0080;
const EFER_SCE: u64 = 1 << 0; // syscall enable bit

fn wrmsr(msr: u32, value: u64) {
    let lo = value as u32;
    let hi = (value >> 32) as u32;
    unsafe {
        core::arch::asm!("wrmsr", in("ecx") msr, in("eax") lo, in("edx") hi, options(nomem, nostack));
    }
}

fn rdmsr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack));
    }
    (hi as u64) << 32 | lo as u64
}

/// Configure MSRs for syscall/sysret and install the entry point.
///
/// # Safety
/// GDT must be loaded first. Call once during boot.
pub unsafe fn init() {
    // Enable the syscall instruction (EFER.SCE)
    let efer = rdmsr(IA32_EFER);
    wrmsr(IA32_EFER, efer | EFER_SCE);

    // STAR: segment selectors for syscall/sysret
    //   [47:32] = kernel CS base (0x08) — syscall loads CS=base, SS=base+8
    //   [63:48] = user CS base (0x10)  — sysret loads CS=base+16, SS=base+8
    //
    // With our GDT layout:
    //   syscall: CS=0x08 (kernel code), SS=0x10 (kernel data) ✓
    //   sysret:  CS=0x10+16=0x20|3 (user code), SS=0x10+8=0x18|3 (user data) ✓
    let star = ((0x10u64) << 48) | ((0x08u64) << 32);
    wrmsr(IA32_STAR, star);

    // LSTAR: the RIP loaded on syscall — our assembly entry point
    wrmsr(IA32_LSTAR, syscall_entry_stub as *const () as u64);

    // FMASK: bits cleared in RFLAGS on syscall entry.
    // Clear IF (bit 9) so interrupts are disabled on entry.
    wrmsr(IA32_FMASK, 0x200);

    serial_println!("  syscall/sysret configured");
}

// ---------------------------------------------------------------------------
// Syscall entry stub (assembly)
//
// On entry (from `syscall` instruction):
//   RCX = user RIP (return address)
//   R11 = user RFLAGS
//   RAX = syscall number
//   RDX, R8, R9, R10 = arguments
//   RSP = still user RSP (!)
//
// We must:
//   1. Switch to the kernel stack (from TSS or a known location)
//   2. Save user RSP
//   3. Dispatch to the Rust handler
//   4. Restore user RSP
//   5. sysretq
// ---------------------------------------------------------------------------

core::arch::global_asm!(
    ".global syscall_entry_stub",
    "syscall_entry_stub:",
    // Swap to kernel stack. Save user RSP in R12 (callee-saved).
    "    mov  r12, rsp",                // save user RSP
    "    mov  rsp, [rip + kernel_rsp]", // load kernel stack pointer
    "",
    // Save the values we need to restore for sysretq
    "    push r12", // user RSP
    "    push rcx", // user RIP (for sysretq)
    "    push r11", // user RFLAGS (for sysretq)
    "",
    // Set up args for Rust handler: syscall_dispatch(nr, arg0, arg1, arg2)
    // RAX=nr already in place as first arg (MS ABI: rcx). Move things around.
    // MS ABI: rcx=arg0, rdx=arg1, r8=arg2, r9=arg3
    "    mov  r11, rdx", // save arg0 (was in rdx)
    "    mov  rcx, rax", // arg0 = syscall number
    "    mov  rdx, r11", // arg1 = original rdx (channel_id or similar)
    // r8 = arg2 (already there)
    // r9 = arg3 (already there)
    "",
    "    sub  rsp, 32",          // shadow space
    "    call syscall_dispatch", // returns result in RAX
    "    add  rsp, 32",
    "",
    // Restore for sysretq
    "    pop  r11", // user RFLAGS
    "    pop  rcx", // user RIP
    "    pop  r12", // user RSP
    "    mov  rsp, r12",
    "",
    "    sysretq",
    "",
    // Kernel stack pointer — written by scheduler on context switch
    ".align 8",
    ".global kernel_rsp",
    "kernel_rsp:",
    "    .quad 0",
);

unsafe extern "C" {
    fn syscall_entry_stub();
    static mut kernel_rsp: u64;
}

/// Set the kernel RSP used by the syscall entry stub.
/// Called during init and could be called on task switch if per-task kernel stacks are needed.
pub fn set_kernel_rsp(rsp: u64) {
    unsafe { *core::ptr::addr_of_mut!(kernel_rsp) = rsp };
}

// ---------------------------------------------------------------------------
// Rust syscall dispatcher
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
extern "C" fn syscall_dispatch(nr: u64, arg0: u64, arg1: u64, arg2: u64) -> i64 {
    match nr {
        SYS_SEND => sys_send(arg0 as u32, arg1),
        SYS_RECV => sys_recv(arg0 as u32, arg1),
        SYS_YIELD => {
            // Re-enable interrupts and halt — next timer tick picks another task
            unsafe { core::arch::asm!("sti; hlt", options(nomem, nostack)) };
            0
        }
        SYS_EXIT => {
            serial_println!("task {} exited", scheduler::current_task());
            // For now just block forever
            loop {
                unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack)) };
            }
        }
        SYS_FBINFO => {
            let buf = arg0 as *mut FbInfo;
            unsafe { *buf = *FB_INFO.0.get() };
            0
        }
        SYS_TIME => crate::tsc::now_ns() as i64,
        SYS_SURFACE_INFO => {
            // arg0 = surface index, arg1 = pointer to SurfaceInfo buffer
            let idx = arg0 as usize;
            if idx >= SURFACE_COUNT.load(core::sync::atomic::Ordering::SeqCst) {
                return -1;
            }
            let buf = arg1 as *mut SurfaceInfo;
            unsafe { *buf = (*SURFACES.0.get())[idx] };
            0
        }
        SYS_TRACE => {
            // arg0 = pointer to TraceEntry array, arg1 = max entries
            let buf = arg0 as *mut ipc::TraceEntry;
            let max = arg1 as usize;
            let slice = unsafe { core::slice::from_raw_parts_mut(buf, max) };
            ipc::trace_read(slice, max) as i64
        }
        SYS_BEEP => {
            let freq = arg0 as u32;
            let ms = (arg1 as u32).min(200); // cap duration
            crate::speaker::beep(freq, ms);
            0
        }
        SYS_PORT_IN8 => {
            let port = arg0 as u16;
            if !is_port_allowed(port) {
                return -1;
            }
            let val: u8;
            unsafe {
                core::arch::asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack))
            };
            val as i64
        }
        SYS_PORT_OUT8 => {
            let port = arg0 as u16;
            let val = arg1 as u8;
            if !is_port_allowed(port) {
                return -1;
            }
            unsafe {
                core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack))
            };
            0
        }
        SYS_PORT_INS16 => {
            // arg0=port, arg1=buffer ptr, arg2=word count
            let port = arg0 as u16;
            let buf = arg1 as *mut u16;
            let count = arg2 as usize;
            if !is_port_allowed(port) {
                return -1;
            }
            for i in 0..count {
                let val: u16;
                unsafe {
                    core::arch::asm!("in ax, dx", out("ax") val, in("dx") port, options(nomem, nostack))
                };
                unsafe { *buf.add(i) = val };
            }
            count as i64
        }
        SYS_PORT_OUTS16 => {
            let port = arg0 as u16;
            let buf = arg1 as *const u16;
            let count = arg2 as usize;
            if !is_port_allowed(port) {
                return -1;
            }
            for i in 0..count {
                let val = unsafe { *buf.add(i) };
                unsafe {
                    core::arch::asm!("out dx, ax", in("dx") port, in("ax") val, options(nomem, nostack))
                };
            }
            count as i64
        }
        SYS_DEBUG => {
            crate::serial::Serial::write_byte_raw(arg0 as u8);
            0
        }
        SYS_PRESENT_RECT => {
            // arg0 packs x (low 32) and y (high 32); arg1 packs w and h.
            let x = arg0 as u32;
            let y = (arg0 >> 32) as u32;
            let w = arg1 as u32;
            let h = (arg1 >> 32) as u32;
            present_rect(x, y, w, h);
            0
        }
        _ => -1,
    }
}

// ---------------------------------------------------------------------------
// Virtio-GPU handle — owned by the kernel, driven via SYS_PRESENT_RECT
// ---------------------------------------------------------------------------

use crate::arch::virtio_gpu::VirtioGpu;

struct VirtioGpuCell(UnsafeCell<Option<VirtioGpu>>);
unsafe impl Sync for VirtioGpuCell {}

static VIRTIO_GPU: VirtioGpuCell = VirtioGpuCell(UnsafeCell::new(None));

/// Take ownership of the initialised driver. Call once during boot.
pub fn set_virtio_gpu(gpu: VirtioGpu) {
    unsafe { *VIRTIO_GPU.0.get() = Some(gpu) };
}

fn present_rect(x: u32, y: u32, w: u32, h: u32) {
    let slot = unsafe { &mut *VIRTIO_GPU.0.get() };
    if let Some(gpu) = slot.as_mut() {
        gpu.present_rect(x, y, w, h);
    }
    // No virtio-GPU → no-op; the GOP framebuffer is scanned out directly.
}

/// Capability check: which I/O ports is the current task allowed to access?
/// For now, only ATA controller ports. The kernel decides; user tasks ask.
fn is_port_allowed(port: u16) -> bool {
    match port {
        0x1F0..=0x1F7 | 0x3F6 => true, // primary ATA
        0x170..=0x177 | 0x376 => true, // secondary ATA
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Individual syscall implementations
// ---------------------------------------------------------------------------

fn sys_send(channel_id: u32, msg_ptr: u64) -> i64 {
    // Read the message from user memory.
    // TODO: validate that msg_ptr is in user address space
    let msg = unsafe { *(msg_ptr as *const ipc::Message) };
    match ipc::send(channel_id, &msg) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

fn sys_recv(channel_id: u32, buf_ptr: u64) -> i64 {
    // Enable interrupts so the scheduler can run while we block
    unsafe { core::arch::asm!("sti", options(nomem, nostack)) };

    match ipc::recv(channel_id) {
        Ok(msg) => {
            // Write the message to user memory.
            // TODO: validate that buf_ptr is in user address space
            unsafe { *(buf_ptr as *mut ipc::Message) = msg };
            0
        }
        Err(_) => -1,
    }
}
