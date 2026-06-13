#![no_std]
#![no_main]
#![cfg_attr(target_arch = "x86_64", feature(abi_x86_interrupt))]

extern crate alloc;

mod arch;
#[cfg(target_arch = "aarch64")]
mod elf;
mod font;
mod font_aa;
mod frame_alloc;
mod framebuffer;
mod heap;
pub mod ipc;
mod metrics;
mod serial;
mod task_names;

// x86_64-specific modules — not compiled on aarch64
#[cfg(target_arch = "x86_64")]
pub(crate) mod gdt;
#[cfg(target_arch = "x86_64")]
mod idt;
#[cfg(target_arch = "x86_64")]
mod keyboard;
#[cfg(target_arch = "x86_64")]
mod mouse;
#[cfg(target_arch = "x86_64")]
mod paging;
#[cfg(target_arch = "x86_64")]
mod pic;
#[cfg(target_arch = "x86_64")]
pub mod scheduler;
#[cfg(target_arch = "x86_64")]
mod speaker;
#[cfg(target_arch = "x86_64")]
pub mod syscall;
#[cfg(target_arch = "x86_64")]
mod tsc;

// Scripting works on both architectures
mod scripting;

// aarch64 userspace tasks (EL0)
#[cfg(target_arch = "aarch64")]
mod arm_tasks;
#[cfg(target_arch = "aarch64")]
mod init_abi;
#[cfg(target_arch = "aarch64")]
mod service_abi;

use frame_alloc::MemRegion;
use framebuffer::{Color, Framebuffer};
use serial::serial_println;
use uefi::boot;
use uefi::mem::memory_map::{MemoryMap, MemoryType};
use uefi::prelude::*;
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat};
#[cfg(target_arch = "aarch64")]
use uefi::proto::media::file::{File, FileAttribute, FileInfo, FileMode};
#[cfg(target_arch = "aarch64")]
use uefi::{cstr16, CStr16};

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    serial_println!("KERNEL PANIC: {}", info);
    loop {
        arch::interrupt_disable();
        arch::halt();
    }
}

// ============================================================================
// Constants
// ============================================================================

const BG: Color = Color::new(0x0D, 0x11, 0x17);
const TITLE: Color = Color::new(0x58, 0xA6, 0xFF);
const SUBTLE: Color = Color::new(0x8B, 0x94, 0x9E);
const TEXT: Color = Color::new(0xE6, 0xED, 0xF3);
const GREEN: Color = Color::new(0x3F, 0xB9, 0x50);
const DIM: Color = Color::new(0x30, 0x36, 0x3D);
const SEP: Color = Color::new(0x21, 0x26, 0x2D);
const ORANGE: Color = Color::new(0xFF, 0xA6, 0x58);

// Surfaces: each workspace is 620x400, fitting side by side above a taskbar
const SURF_W: usize = 620;
const SURF_H: usize = 400;
// (Old taskbar constants replaced by TBAR_H / TBAR_Y in compositor section)
const SURF_BYTES: usize = SURF_W * SURF_H * 4;
const SURF_PAGES: usize = (SURF_BYTES + 4095) / 4096;
const EL0_FAULT_REGION_BYTES: usize = 2 * 1024 * 1024;
const EL0_FAULT_REGION_PAGES: usize = EL0_FAULT_REGION_BYTES / 4096;

// ============================================================================
// Boot info
// ============================================================================

struct BootInfo {
    fb_ptr: *mut u8,
    width: usize,
    height: usize,
    stride: usize,
    is_bgr: bool,
    usable_mb: u64,
    total_mb: u64,
    acpi_rsdp: u64,
    regions: [MemRegion; 128],
    region_count: usize,
}

// ============================================================================
// IPC channel layout
// ============================================================================
//
//   Ch 0: kernel IRQ handler → keyboard driver   (raw scancodes)
//   Ch 1: keyboard driver → compositor           (key events)
//   Ch 2: compositor → shell workspace           (forwarded key events)

const CH_KBD_RAW: u32 = 0;
const CH_KBD_EVENTS: u32 = 1;
const CH_SHELL_KEYS: u32 = 2;
const CH_MOUSE_RAW: u32 = 3;
const CH_MOUSE_EVENTS: u32 = 4;

#[cfg(target_arch = "aarch64")]
struct BootFile {
    ptr: *mut u8,
    len: usize,
}

#[cfg(target_arch = "aarch64")]
fn load_esp_file(path: &CStr16) -> Option<BootFile> {
    let mut fs = match boot::get_image_file_system(boot::image_handle()) {
        Ok(fs) => fs,
        Err(err) => {
            serial_println!("  init fs unavailable: {:?}", err.status());
            return None;
        }
    };
    let mut root = match fs.open_volume() {
        Ok(root) => root,
        Err(err) => {
            serial_println!("  init volume open failed: {:?}", err.status());
            return None;
        }
    };
    let handle = match root.open(path, FileMode::Read, FileAttribute::empty()) {
        Ok(file) => file,
        Err(err) => {
            serial_println!("  init open failed: {:?}", err.status());
            return None;
        }
    };
    let mut file = match handle.into_regular_file() {
        Some(file) => file,
        None => {
            serial_println!("  init path is not a regular file");
            return None;
        }
    };

    let mut info_buf = [0u8; 512];
    let info = match file.get_info::<FileInfo>(&mut info_buf) {
        Ok(info) => info,
        Err(_) => {
            serial_println!("  init file info unavailable");
            return None;
        }
    };
    let len = info.file_size() as usize;
    let pool = match boot::allocate_pool(MemoryType::LOADER_DATA, len.max(1)) {
        Ok(ptr) => ptr,
        Err(err) => {
            serial_println!("  init buffer alloc failed: {:?}", err.status());
            return None;
        }
    };
    let buf = unsafe { core::slice::from_raw_parts_mut(pool.as_ptr(), len) };
    match file.read(buf) {
        Ok(read) if read == len => Some(BootFile {
            ptr: pool.as_ptr(),
            len,
        }),
        Ok(read) => {
            serial_println!("  init short read: {}/{}", read, len);
            None
        }
        Err(err) => {
            serial_println!("  init read failed: {:?}", err.status());
            None
        }
    }
}

// ============================================================================
// aarch64 entry point — graphical boot + preemptive scheduling
// ============================================================================

#[cfg(target_arch = "aarch64")]
#[entry]
fn main() -> Status {
    use serial::serial_println;

    // ---- UEFI: grab framebuffer and memory map (same protocol as x86) ----
    let gop_handle = boot::get_handle_for_protocol::<GraphicsOutput>().expect("GOP not available");
    let mut gop =
        boot::open_protocol_exclusive::<GraphicsOutput>(gop_handle).expect("failed to open GOP");
    let mode = gop.current_mode_info();
    let (width, height) = mode.resolution();
    let stride = mode.stride();
    let pixel_format = mode.pixel_format();
    let fb_ptr = gop.frame_buffer().as_mut_ptr();
    drop(gop);

    let is_bgr = matches!(pixel_format, PixelFormat::Bgr);

    let init_file = load_esp_file(cstr16!("\\EFI\\FreshOS\\INIT.ELF"));
    if let Some(file) = &init_file {
        serial_println!("  Boot init: {} bytes from ESP", file.len);
    } else {
        serial_println!("  Boot init: missing, will use built-in launch path");
    }

    let pong_file = load_esp_file(cstr16!("\\EFI\\FreshOS\\PONG.ELF"));
    if let Some(file) = &pong_file {
        serial_println!("  Boot pong: {} bytes from ESP", file.len);
    } else {
        serial_println!("  Boot pong: missing, will use built-in service");
    }

    let pulse_file = load_esp_file(cstr16!("\\EFI\\FreshOS\\PULSE.ELF"));
    if let Some(file) = &pulse_file {
        serial_println!("  Boot pulse: {} bytes from ESP", file.len);
    } else {
        serial_println!("  Boot pulse: missing, will use built-in service");
    }

    let fault_file = load_esp_file(cstr16!("\\EFI\\FreshOS\\FAULT.ELF"));
    if let Some(file) = &fault_file {
        serial_println!("  Boot fault: {} bytes from ESP", file.len);
    } else {
        serial_println!("  Boot fault: missing, will use built-in service");
    }

    let mmap = boot::memory_map(MemoryType::LOADER_DATA).expect("memory map");
    let mut usable_pages: u64 = 0;
    let mut regions = [MemRegion {
        start: 0,
        pages: 0,
        usable: false,
    }; 128];
    let mut region_count: usize = 0;
    for desc in mmap.entries() {
        let usable = desc.ty == MemoryType::CONVENTIONAL;
        if usable {
            usable_pages += desc.page_count;
        }
        if region_count < regions.len() {
            regions[region_count] = MemRegion {
                start: desc.phys_start,
                pages: desc.page_count,
                usable,
            };
            region_count += 1;
        }
    }
    drop(mmap);

    // ---- Exit boot services ----
    let _ = unsafe { boot::exit_boot_services(MemoryType::LOADER_DATA) };

    // ---- Kernel init ----
    serial_println!("FreshOS booting on aarch64...");
    serial_println!("  Serial: PL011 UART");

    unsafe { arch::exceptions::init() };
    unsafe { arch::gic::init() };

    unsafe { frame_alloc::init(&regions, region_count) };
    serial_println!(
        "  Frames: {} free ({} MB)",
        frame_alloc::free_count(),
        frame_alloc::free_mb()
    );

    unsafe { heap::init() };

    serial_println!(
        "  Display: {}x{} ({})",
        width,
        height,
        if is_bgr { "BGR" } else { "RGB" }
    );

    // ---- Render boot screen to framebuffer ----
    let mut fb = Framebuffer::new(fb_ptr, width, height, stride, is_bgr);
    fb.clear(BG);

    // Menu bar
    let menu_bg = Color::new(0x10, 0x14, 0x1C);
    fb.draw_rect(0, 0, width, 26, menu_bg);
    fb.draw_aa_string(12, 5, "FreshOS", TITLE, menu_bg);
    fb.draw_aa_string(100, 5, "|  aarch64", DIM, menu_bg);

    // Title
    fb.draw_aa_string_2x(width / 2 - 70, 80, "FreshOS", TITLE, BG);
    fb.draw_aa_string(
        width / 2 - 100,
        120,
        "Running on aarch64 with HVF",
        SUBTLE,
        BG,
    );

    // System info
    let mut y = 180;
    fb.draw_aa_string(80, y, "Architecture  aarch64 (Apple Silicon)", TEXT, BG);
    y += 24;
    fb.draw_aa_string(80, y, "Acceleration  HVF (near-native)", TEXT, BG);
    y += 24;
    {
        let mut buf = [0u8; 64];
        let s = fmt_simple(
            &mut buf,
            "Memory        ",
            frame_alloc::free_mb() as u64,
            " MB free",
        );
        fb.draw_aa_string(80, y, s, TEXT, BG);
    }
    y += 24;
    {
        let mut buf = [0u8; 64];
        let s = fmt_simple(&mut buf, "Display       ", width as u64, "");
        fb.draw_aa_string(80, y, s, TEXT, BG);
        let xpos = 80 + s.len() * font_aa::GLYPH_W;
        fb.draw_aa_char(xpos, y, 'x', TEXT, BG);
        let mut buf2 = [0u8; 16];
        let hs = fmt_u64_str(height as u64, &mut buf2);
        fb.draw_aa_string(xpos + font_aa::GLYPH_W, y, hs, TEXT, BG);
    }
    y += 24;
    fb.draw_aa_string(80, y, "Heap          1024 KiB", TEXT, BG);

    y += 48;
    fb.draw_aa_string(80, y, "The architecture is perceptible.", DIM, BG);

    serial_println!("  Desktop rendered");

    // ---- Page tables: patch UEFI's tables for EL0 access ----
    let ttbr0 = unsafe { arch::paging::init() };

    // ---- Syscall support ----
    arch::syscall::set_fb_info(arch::syscall::FbInfo {
        address: fb_ptr as u64,
        width: width as u32,
        height: height as u32,
        stride: stride as u32,
        is_bgr: if is_bgr { 1 } else { 0 },
    });

    // ---- Allocate compositor surfaces ----
    let surf0_addr = frame_alloc::allocate_contiguous(SURF_PAGES).expect("surface 0");
    let surf1_addr = frame_alloc::allocate_contiguous(SURF_PAGES).expect("surface 1");
    unsafe {
        core::ptr::write_bytes(surf0_addr as *mut u8, 0, SURF_BYTES);
        core::ptr::write_bytes(surf1_addr as *mut u8, 0, SURF_BYTES);
    }
    arch::syscall::add_surface(arch::syscall::SurfaceInfo {
        address: surf0_addr,
        width: SURF_W as u32,
        height: SURF_H as u32,
        stride: SURF_W as u32,
    });
    arch::syscall::add_surface(arch::syscall::SurfaceInfo {
        address: surf1_addr,
        width: SURF_W as u32,
        height: SURF_H as u32,
        stride: SURF_W as u32,
    });
    serial_println!(
        "  Surfaces: {}x{} x2 at {:#x}, {:#x}",
        SURF_W,
        SURF_H,
        surf0_addr,
        surf1_addr
    );

    // ---- IPC channels ----
    let _ = ipc::create().expect("ch0: kbd events");
    let _ = ipc::create().expect("ch1: shell keys");
    let _ = ipc::create().expect("ch2: probe ping");
    let _ = ipc::create().expect("ch3: probe pong");
    serial_println!("  {} IPC channels", ipc::channel_count());

    // ---- Scheduler: spawn tasks ----
    // HVF limitation: all tlbi instructions are trapped, making EL0 page
    // table management impossible (can't grant user access without TLB
    // invalidation). Run tasks at EL1 with direct syscall dispatch instead.
    // The SVC path and EL0 infrastructure is ready for bare-metal targets.
    arch::context::init(ttbr0);

    let loaded_init = init_file.as_ref().and_then(|file| {
        let bytes = unsafe { core::slice::from_raw_parts(file.ptr, file.len) };
        match elf::load_image(bytes) {
            Ok(image) => {
                arch::paging::make_executable(image.base, image.size as u64);
                serial_println!(
                    "  Init ELF loaded: base={:#x} size={} entry={:#x}",
                    image.base,
                    image.size,
                    image.entry
                );
                Some(image)
            }
            Err(err) => {
                serial_println!("  Init ELF load failed: {}", err);
                None
            }
        }
    });

    if let Some(file) = pong_file.as_ref() {
        let bytes = unsafe { core::slice::from_raw_parts(file.ptr, file.len) };
        match elf::load_image(bytes) {
            Ok(image) => {
                arch::paging::make_executable(image.base, image.size as u64);
                service_abi::register_external_pong(image.entry);
                serial_println!(
                    "  Pong ELF loaded: base={:#x} size={} entry={:#x}",
                    image.base,
                    image.size,
                    image.entry
                );
            }
            Err(err) => {
                serial_println!("  Pong ELF load failed: {}", err);
            }
        }
    }

    if let Some(file) = pulse_file.as_ref() {
        let bytes = unsafe { core::slice::from_raw_parts(file.ptr, file.len) };
        match elf::load_image(bytes) {
            Ok(image) => {
                arch::paging::make_executable(image.base, image.size as u64);
                service_abi::register_external_pulse(image.entry);
                serial_println!(
                    "  Pulse ELF loaded: base={:#x} size={} entry={:#x}",
                    image.base,
                    image.size,
                    image.entry
                );
            }
            Err(err) => {
                serial_println!("  Pulse ELF load failed: {}", err);
            }
        }
    }

    if let Some(file) = fault_file.as_ref() {
        let bytes = unsafe { core::slice::from_raw_parts(file.ptr, file.len) };
        let fault_region = frame_alloc::allocate_contiguous_aligned(
            EL0_FAULT_REGION_PAGES,
            EL0_FAULT_REGION_PAGES,
        );
        match fault_region {
            Some(region_base) => match elf::load_image_into(
                bytes,
                region_base,
                EL0_FAULT_REGION_BYTES - arch::context::USER_STACK_BYTES as usize,
            ) {
                Ok(image) => {
                    let user_stack_bottom = region_base
                        + (EL0_FAULT_REGION_BYTES as u64 - arch::context::USER_STACK_BYTES);
                    unsafe {
                        core::ptr::write_bytes(
                            user_stack_bottom as *mut u8,
                            0,
                            arch::context::USER_STACK_BYTES as usize,
                        );
                    }
                    arch::paging::grant_user_access(region_base, EL0_FAULT_REGION_BYTES as u64);
                    arch::paging::make_executable(image.base, image.size as u64);
                    service_abi::register_external_fault(image.entry, user_stack_bottom);
                    serial_println!(
                        "  Fault ELF loaded: base={:#x} size={} entry={:#x} ustack={:#x} region={:#x}..{:#x}",
                        image.base,
                        image.size,
                        image.entry,
                        user_stack_bottom,
                        region_base,
                        region_base + EL0_FAULT_REGION_BYTES as u64,
                    );
                }
                Err(err) => {
                    serial_println!("  Fault ELF load failed: {}", err);
                }
            },
            None => {
                serial_println!("  Fault ELF load failed: no aligned EL0 region");
            }
        }
    }

    if let Some(image) = loaded_init {
        arch::context::spawn_with_arg(image.entry, init_abi::api_ptr() as u64);
    } else {
        arch::context::spawn(arm_tasks::keyboard_el1);
        arch::context::spawn(arm_tasks::compositor_el1);
        arch::context::spawn(arm_tasks::shell_el1);
        arch::context::spawn(arm_tasks::dashboard_el1);
        arch::context::spawn(arm_tasks::ipc_probe_ping_el1);
        arch::context::spawn(arm_tasks::ipc_probe_pong_el1);
    }
    serial_println!("  {} tasks ready", arch::context::task_count());

    unsafe { arch::context::start() };

    // Task 0 (boot/idle) — loops here, preempted by timer
    loop {
        arch::halt();
    }
}

// Helper for simple "label N suffix" formatting without alloc
fn fmt_simple<'a>(buf: &'a mut [u8; 64], prefix: &str, n: u64, suffix: &str) -> &'a str {
    let mut i = 0;
    for &b in prefix.as_bytes() {
        if i < 60 {
            buf[i] = b;
            i += 1;
        }
    }
    // Number
    if n == 0 {
        buf[i] = b'0';
        i += 1;
    } else {
        let mut digits = [0u8; 20];
        let mut d = 0;
        let mut v = n;
        while v > 0 {
            digits[d] = b'0' + (v % 10) as u8;
            v /= 10;
            d += 1;
        }
        while d > 0 {
            d -= 1;
            buf[i] = digits[d];
            i += 1;
        }
    }
    for &b in suffix.as_bytes() {
        if i < 64 {
            buf[i] = b;
            i += 1;
        }
    }
    core::str::from_utf8(&buf[..i]).unwrap_or("?")
}

fn fmt_u64_str<'a>(n: u64, buf: &'a mut [u8; 16]) -> &'a str {
    if n == 0 {
        buf[0] = b'0';
        return core::str::from_utf8(&buf[..1]).unwrap_or("0");
    }
    let mut i = 0;
    let mut v = n;
    let mut digits = [0u8; 16];
    let mut d = 0;
    while v > 0 {
        digits[d] = b'0' + (v % 10) as u8;
        v /= 10;
        d += 1;
    }
    while d > 0 {
        d -= 1;
        buf[i] = digits[d];
        i += 1;
    }
    core::str::from_utf8(&buf[..i]).unwrap_or("?")
}

// ============================================================================
// x86_64 entry point + all task code
// ============================================================================

#[cfg(target_arch = "x86_64")]
#[entry]
fn main() -> Status {
    // ---- UEFI ----
    let gop_handle = boot::get_handle_for_protocol::<GraphicsOutput>().expect("GOP not available");
    let mut gop =
        boot::open_protocol_exclusive::<GraphicsOutput>(gop_handle).expect("failed to open GOP");
    let mode = gop.current_mode_info();
    let (width, height) = mode.resolution();
    let stride = mode.stride();
    let pixel_format = mode.pixel_format();
    let fb_ptr = gop.frame_buffer().as_mut_ptr();
    drop(gop);

    let mmap = boot::memory_map(MemoryType::LOADER_DATA).expect("memory map");
    let mut usable_pages: u64 = 0;
    let mut total_pages: u64 = 0;
    let mut regions = [MemRegion {
        start: 0,
        pages: 0,
        usable: false,
    }; 128];
    let mut region_count: usize = 0;
    for desc in mmap.entries() {
        total_pages += desc.page_count;
        let usable = desc.ty == MemoryType::CONVENTIONAL;
        if usable {
            usable_pages += desc.page_count;
        }
        if region_count < regions.len() {
            regions[region_count] = MemRegion {
                start: desc.phys_start,
                pages: desc.page_count,
                usable,
            };
            region_count += 1;
        }
    }
    drop(mmap);

    let acpi_rsdp = uefi::system::with_config_table(|entries| {
        let mut found = 0u64;
        for entry in entries {
            if entry.guid == uefi::table::cfg::ACPI2_GUID {
                found = entry.address as u64;
                break;
            }
        }
        if found == 0 {
            for entry in entries {
                if entry.guid == uefi::table::cfg::ACPI_GUID {
                    found = entry.address as u64;
                    break;
                }
            }
        }
        found
    });

    let info = BootInfo {
        fb_ptr,
        width,
        height,
        stride,
        is_bgr: matches!(pixel_format, PixelFormat::Bgr),
        usable_mb: usable_pages * 4096 / (1024 * 1024),
        total_mb: total_pages * 4096 / (1024 * 1024),
        acpi_rsdp,
        regions,
        region_count,
    };

    let _ = unsafe { boot::exit_boot_services(MemoryType::LOADER_DATA) };

    // ---- Kernel init ----
    serial_println!("FreshOS booting...");

    unsafe { gdt::init() };
    serial_println!("  GDT loaded");

    unsafe { idt::init() };
    serial_println!("  IDT loaded");

    unsafe { frame_alloc::init(&info.regions, info.region_count) };
    serial_println!(
        "  Frames: {} free ({} MB)",
        frame_alloc::free_count(),
        frame_alloc::free_mb()
    );

    let kernel_pml4 = unsafe { paging::init() };

    unsafe { heap::init() };

    let pci_ready = if let Some(mcfg) = arch::acpi::init(info.acpi_rsdp) {
        arch::pci::init(mcfg);
        true
    } else {
        serial_println!("  ACPI: MCFG not found, falling back to GOP framebuffer");
        false
    };

    unsafe { pic::init() };
    serial_println!("  PIC remapped");

    unsafe { tsc::calibrate() };

    unsafe { syscall::init() };

    // Try virtio-GPU first; if a virtio-gpu device is present, its backing
    // buffer replaces the GOP framebuffer so the compositor draws straight
    // into the resource the GPU will scan out. Otherwise we keep the GOP fb.
    let gop_fb = syscall::FbInfo {
        address: info.fb_ptr as u64,
        width: info.width as u32,
        height: info.height as u32,
        stride: info.stride as u32,
        is_bgr: if info.is_bgr { 1 } else { 0 },
    };

    // The x86 virtio-GPU driver is present but currently dormant: resetting
    // the device destroys the scanout OVMF configured for us, and rebuilding
    // one from scratch is not yet working (resource_create / set_scanout
    // succeed in isolation but the display stays on the old frame). Until
    // that's sorted, use the GOP framebuffer OVMF set up — which on q35 is
    // itself a virtio-GPU resource, maintained for us across boot. The
    // SYS_PRESENT_RECT syscall stays wired for the day we take the device
    // over: today it is a no-op because set_virtio_gpu is never called.
    let _ = pci_ready;
    let active_fb = gop_fb;

    syscall::set_fb_info(active_fb);

    // ---- Allocate compositor surfaces ----
    let surf0_addr = frame_alloc::allocate_contiguous(SURF_PAGES).expect("surface 0");
    let surf1_addr = frame_alloc::allocate_contiguous(SURF_PAGES).expect("surface 1");
    // Zero them
    unsafe {
        core::ptr::write_bytes(surf0_addr as *mut u8, 0, SURF_BYTES);
        core::ptr::write_bytes(surf1_addr as *mut u8, 0, SURF_BYTES);
    }
    syscall::add_surface(syscall::SurfaceInfo {
        address: surf0_addr,
        width: SURF_W as u32,
        height: SURF_H as u32,
        stride: SURF_W as u32,
    });
    syscall::add_surface(syscall::SurfaceInfo {
        address: surf1_addr,
        width: SURF_W as u32,
        height: SURF_H as u32,
        stride: SURF_W as u32,
    });
    serial_println!(
        "  Surfaces: {}x{} x2 at {:#x}, {:#x}",
        SURF_W,
        SURF_H,
        surf0_addr,
        surf1_addr
    );

    unsafe { mouse::init() };

    // ---- IPC channels ----
    let _ = ipc::create().expect("ch0: kbd raw");
    let _ = ipc::create().expect("ch1: kbd events");
    let _ = ipc::create().expect("ch2: shell keys");
    let _ = ipc::create().expect("ch3: mouse raw");
    let _ = ipc::create().expect("ch4: mouse events");
    serial_println!("  {} IPC channels", ipc::channel_count());

    // ---- Scripting demo ----
    let _script_ch = ipc::create().expect("script channel");
    let _ = scripting::run(
        r#"
        let msg = 42;
        send(3, 1, msg);
        print("sent " + msg + " on channel 3");
    "#,
    );

    // ---- Tasks ----
    scheduler::init(kernel_pml4);

    let fb_region = (syscall::fb_address(), syscall::fb_size());
    let s0_region = (surf0_addr, SURF_BYTES as u64);
    let s1_region = (surf1_addr, SURF_BYTES as u64);

    scheduler::spawn_user(x86_tasks::user_kbd_driver as *const () as u64, &[]); // 1
    scheduler::spawn_user(x86_tasks::user_mouse_driver as *const () as u64, &[]); // 2
    scheduler::spawn_user(
        x86_tasks::user_compositor as *const () as u64,
        &[fb_region, s0_region, s1_region],
    ); // 3
    scheduler::spawn_user(x86_tasks::user_shell as *const () as u64, &[s0_region]); // 4
    scheduler::spawn_user(x86_tasks::user_clock as *const () as u64, &[s1_region]); // 5
    scheduler::spawn_user(x86_tasks::user_chiptune as *const () as u64, &[]); // 6

    serial_println!(
        "  {} tasks (kbd, mouse, comp, shell, dash, music)",
        scheduler::task_count()
    );

    // ---- Boot chime ----
    speaker::boot_chime();
    serial_println!("  Boot chime played");

    // ---- Show boot screen briefly ----
    {
        let mut fb = Framebuffer::new(
            info.fb_ptr,
            info.width,
            info.height,
            info.stride,
            info.is_bgr,
        );
        fb.clear(BG);
        fb.draw_string(80, 350, "FreshOS compositor starting...", SUBTLE, 2);
    }

    // ---- Start ----
    syscall::set_kernel_rsp(gdt::syscall_stack_top());
    pic::unmask(1); // keyboard
    pic::unmask(2); // cascade (slave PIC)
    pic::unmask(12); // mouse
    unsafe { scheduler::start() };

    loop {
        unsafe { core::arch::asm!("hlt") };
    }
}

// ============================================================================
// Everything below is x86_64-specific until the arch port is complete.
// Syscall wrappers use asm!("syscall"), task functions reference x86 modules.
// ============================================================================
#[cfg(target_arch = "x86_64")]
mod x86_tasks {
    use super::*;
    use crate::font_aa;

    // ============================================================================
    // Userspace syscall helpers
    // ============================================================================

    #[inline(always)]
    fn user_send(ch: u32, msg: &ipc::Message) -> i64 {
        let r: i64;
        unsafe {
            core::arch::asm!("syscall", in("rax") syscall::SYS_SEND, in("rdx") ch as u64, in("r8") msg as *const _ as u64, lateout("rax") r, out("rcx") _, out("r11") _, options(nostack));
        }
        r
    }

    #[inline(always)]
    fn user_recv(ch: u32, buf: &mut ipc::Message) -> i64 {
        let r: i64;
        unsafe {
            core::arch::asm!("syscall", in("rax") syscall::SYS_RECV, in("rdx") ch as u64, in("r8") buf as *mut _ as u64, lateout("rax") r, out("rcx") _, out("r11") _, options(nostack));
        }
        r
    }

    #[inline(always)]
    fn user_debug_char(ch: u8) {
        unsafe {
            core::arch::asm!("syscall", in("rax") syscall::SYS_DEBUG, in("rdx") ch as u64, lateout("rax") _, out("rcx") _, out("r11") _, options(nostack));
        }
    }

    #[inline(always)]
    fn user_time_ns() -> u64 {
        let r: i64;
        unsafe {
            core::arch::asm!("syscall", in("rax") syscall::SYS_TIME, lateout("rax") r, out("rcx") _, out("r11") _, out("rdx") _, options(nostack));
        }
        r as u64
    }

    #[inline(always)]
    fn user_fbinfo(buf: &mut syscall::FbInfo) -> i64 {
        let r: i64;
        unsafe {
            core::arch::asm!("syscall", in("rax") syscall::SYS_FBINFO, in("rdx") buf as *mut _ as u64, lateout("rax") r, out("rcx") _, out("r11") _, options(nostack));
        }
        r
    }

    #[inline(always)]
    fn user_surface_info(idx: u32, buf: &mut syscall::SurfaceInfo) -> i64 {
        let r: i64;
        unsafe {
            core::arch::asm!("syscall", in("rax") syscall::SYS_SURFACE_INFO, in("rdx") idx as u64, in("r8") buf as *mut _ as u64, lateout("rax") r, out("rcx") _, out("r11") _, options(nostack));
        }
        r
    }

    #[inline(always)]
    fn user_yield() {
        unsafe {
            core::arch::asm!("syscall", in("rax") syscall::SYS_YIELD, lateout("rax") _, out("rcx") _, out("r11") _, out("rdx") _, options(nostack));
        }
    }

    #[inline(always)]
    fn user_beep(freq: u32, ms: u32) {
        unsafe {
            core::arch::asm!("syscall", in("rax") syscall::SYS_BEEP, in("rdx") freq as u64, in("r8") ms as u64, lateout("rax") _, out("rcx") _, out("r11") _, options(nostack));
        }
    }

    #[inline(always)]
    fn user_present_rect(x: u32, y: u32, w: u32, h: u32) {
        // arg0 packs (y << 32) | x, arg1 packs (h << 32) | w.
        let arg0 = ((y as u64) << 32) | (x as u64);
        let arg1 = ((h as u64) << 32) | (w as u64);
        unsafe {
            core::arch::asm!(
                "syscall",
                in("rax") syscall::SYS_PRESENT_RECT,
                in("rdx") arg0,
                in("r8") arg1,
                lateout("rax") _,
                out("rcx") _,
                out("r11") _,
                options(nostack),
            );
        }
    }

    #[inline(always)]
    fn user_trace(buf: &mut [ipc::TraceEntry]) -> usize {
        let r: i64;
        unsafe {
            core::arch::asm!("syscall", in("rax") syscall::SYS_TRACE, in("rdx") buf.as_mut_ptr() as u64, in("r8") buf.len() as u64, lateout("rax") r, out("rcx") _, out("r11") _, options(nostack));
        }
        r.max(0) as usize
    }

    fn user_print(s: &[u8]) {
        for &b in s {
            user_debug_char(b);
        }
    }

    fn user_print_u64(mut n: u64) {
        if n == 0 {
            user_debug_char(b'0');
            return;
        }
        let mut buf = [0u8; 20];
        let mut i = 0;
        while n > 0 {
            buf[i] = b'0' + (n % 10) as u8;
            n /= 10;
            i += 1;
        }
        while i > 0 {
            i -= 1;
            user_debug_char(buf[i]);
        }
    }

    /// Get a Framebuffer for a surface by index.
    fn get_surface(idx: u32) -> Framebuffer {
        let mut si = syscall::SurfaceInfo {
            address: 0,
            width: 0,
            height: 0,
            stride: 0,
        };
        user_surface_info(idx, &mut si);
        Framebuffer::new(
            si.address as *mut u8,
            si.width as usize,
            si.height as usize,
            si.stride as usize,
            true,
        )
    }

    /// Get a Framebuffer for the real display.
    fn get_display() -> (Framebuffer, syscall::FbInfo) {
        let mut fbi = syscall::FbInfo {
            address: 0,
            width: 0,
            height: 0,
            stride: 0,
            is_bgr: 0,
        };
        user_fbinfo(&mut fbi);
        let fb = Framebuffer::new(
            fbi.address as *mut u8,
            fbi.width as usize,
            fbi.height as usize,
            fbi.stride as usize,
            fbi.is_bgr != 0,
        );
        (fb, fbi)
    }

    // ============================================================================
    // Keyboard driver (ring 3) — unchanged
    // ============================================================================

    const fn scancode_table() -> [u8; 128] {
        let mut t = [0u8; 128];
        t[0x02] = b'1';
        t[0x03] = b'2';
        t[0x04] = b'3';
        t[0x05] = b'4';
        t[0x06] = b'5';
        t[0x07] = b'6';
        t[0x08] = b'7';
        t[0x09] = b'8';
        t[0x0A] = b'9';
        t[0x0B] = b'0';
        t[0x0C] = b'-';
        t[0x0D] = b'=';
        t[0x10] = b'q';
        t[0x11] = b'w';
        t[0x12] = b'e';
        t[0x13] = b'r';
        t[0x14] = b't';
        t[0x15] = b'y';
        t[0x16] = b'u';
        t[0x17] = b'i';
        t[0x18] = b'o';
        t[0x19] = b'p';
        t[0x1A] = b'[';
        t[0x1B] = b']';
        t[0x1E] = b'a';
        t[0x1F] = b's';
        t[0x20] = b'd';
        t[0x21] = b'f';
        t[0x22] = b'g';
        t[0x23] = b'h';
        t[0x24] = b'j';
        t[0x25] = b'k';
        t[0x26] = b'l';
        t[0x27] = b';';
        t[0x28] = b'\'';
        t[0x29] = b'`';
        t[0x2B] = b'\\';
        t[0x2C] = b'z';
        t[0x2D] = b'x';
        t[0x2E] = b'c';
        t[0x2F] = b'v';
        t[0x30] = b'b';
        t[0x31] = b'n';
        t[0x32] = b'm';
        t[0x33] = b',';
        t[0x34] = b'.';
        t[0x35] = b'/';
        t[0x0E] = 0x08;
        t[0x0F] = b'\t';
        t[0x1C] = b'\n';
        t[0x39] = b' ';
        t
    }
    static SCANCODES: [u8; 128] = scancode_table();

    pub fn user_kbd_driver() -> ! {
        loop {
            let mut msg = ipc::Message::empty();
            if user_recv(CH_KBD_RAW, &mut msg) < 0 {
                continue;
            }
            let sc = msg.payload[0] as u8;
            let released = sc & 0x80 != 0;
            let idx = (sc & 0x7F) as usize;
            if idx >= 128 {
                continue;
            }
            let ascii = SCANCODES[idx];
            if ascii == 0 {
                continue;
            }
            let tag = if released {
                ipc::MSG_KEY_UP
            } else {
                ipc::MSG_KEY_DOWN
            };
            let irq_ns = msg.payload[1];
            let event = ipc::Message {
                tag,
                sender: 0,
                len: 3,
                payload: [ascii as u64, sc as u64, irq_ns, 0],
            };
            // Send to compositor (it decides where keys go)
            let _ = user_send(CH_KBD_EVENTS, &event);
        }
    }

    // ============================================================================
    // Mouse driver (ring 3) — assembles 3-byte PS/2 packets, tracks position
    // ============================================================================

    pub fn user_mouse_driver() -> ! {
        let mut packet = [0u8; 3];
        let mut byte_idx: usize = 0;
        let mut mx: i32 = 640;
        let mut my: i32 = 400;

        loop {
            let mut msg = ipc::Message::empty();
            if user_recv(CH_MOUSE_RAW, &mut msg) < 0 {
                continue;
            }

            let byte = msg.payload[0] as u8;

            // Sync: first byte must have bit 3 set
            if byte_idx == 0 && byte & 0x08 == 0 {
                continue;
            }

            packet[byte_idx] = byte;
            byte_idx += 1;

            if byte_idx == 3 {
                byte_idx = 0;

                let buttons = packet[0] & 0x07;
                let dx = if packet[0] & 0x10 != 0 {
                    packet[1] as i32 - 256 // sign extend
                } else {
                    packet[1] as i32
                };
                let dy = if packet[0] & 0x20 != 0 {
                    -(packet[2] as i32 - 256) // sign extend + invert Y
                } else {
                    -(packet[2] as i32) // invert Y (PS/2 Y is bottom-up)
                };

                mx = (mx + dx).clamp(0, 1279);
                my = (my + dy).clamp(0, 799);

                let event = ipc::Message {
                    tag: ipc::MSG_MOUSE,
                    sender: 0,
                    len: 3,
                    payload: [mx as u64, my as u64, buttons as u64, 0],
                };
                let _ = user_send(CH_MOUSE_EVENTS, &event);
            }
        }
    }

    // ============================================================================
    // Cursor bitmap (12x16 arrow pointer)
    // ============================================================================

    #[rustfmt::skip]
const CURSOR: [[u8; 12]; 16] = [
    [1,0,0,0,0,0,0,0,0,0,0,0],
    [1,1,0,0,0,0,0,0,0,0,0,0],
    [1,2,1,0,0,0,0,0,0,0,0,0],
    [1,2,2,1,0,0,0,0,0,0,0,0],
    [1,2,2,2,1,0,0,0,0,0,0,0],
    [1,2,2,2,2,1,0,0,0,0,0,0],
    [1,2,2,2,2,2,1,0,0,0,0,0],
    [1,2,2,2,2,2,2,1,0,0,0,0],
    [1,2,2,2,2,2,2,2,1,0,0,0],
    [1,2,2,2,2,1,1,1,1,0,0,0],
    [1,2,2,1,2,2,1,0,0,0,0,0],
    [1,2,1,0,1,2,2,1,0,0,0,0],
    [1,1,0,0,1,2,2,1,0,0,0,0],
    [1,0,0,0,0,1,2,2,1,0,0,0],
    [0,0,0,0,0,1,2,1,0,0,0,0],
    [0,0,0,0,0,0,1,0,0,0,0,0],
];
    const CURSOR_W: usize = 12;
    const CURSOR_H: usize = 16;

    fn draw_cursor(fb: &mut Framebuffer, cx: usize, cy: usize) {
        // Drop shadow (offset 1,1, dark)
        for row in 0..CURSOR_H {
            for col in 0..CURSOR_W {
                if CURSOR[row][col] == 1 {
                    fb.put_pixel(cx + col + 1, cy + row + 1, Color::new(0, 0, 0));
                }
            }
        }
        // Main cursor with slight glow on the white fill
        for row in 0..CURSOR_H {
            for col in 0..CURSOR_W {
                match CURSOR[row][col] {
                    1 => fb.put_pixel(cx + col, cy + row, Color::new(10, 10, 10)),
                    2 => fb.put_pixel(cx + col, cy + row, Color::new(255, 255, 255)),
                    _ => {}
                }
            }
        }
    }

    // ============================================================================
    // Visual helpers — gradient, alpha blending, formatting
    // ============================================================================

    // Menu bar height
    const MENU_H: usize = 26;
    // Bottom taskbar
    const TBAR_H: usize = 36;
    const TBAR_Y: usize = 800 - TBAR_H; // y=764

    // Gradient background colours (top-left to bottom-right)
    const GRAD_TL: (u16, u16, u16) = (0x08, 0x0C, 0x1A); // deep navy
    const GRAD_TR: (u16, u16, u16) = (0x12, 0x0A, 0x22); // dark purple
    const GRAD_BL: (u16, u16, u16) = (0x06, 0x14, 0x28); // deep blue
    const GRAD_BR: (u16, u16, u16) = (0x18, 0x10, 0x30); // purple-blue

    // Window styling
    const WIN_TITLE_H: usize = 28;
    const WIN_BORDER: usize = 2;
    const ACTIVE_ALPHA: u8 = 230; // ~0.90 opacity
    const INACTIVE_ALPHA: u8 = 150; // ~0.59 opacity

    // Menu/taskbar colours
    const MENU_BG: Color = Color::new(0x10, 0x14, 0x1C);
    const TBAR_BG_COL: Color = Color::new(0x10, 0x14, 0x1C);
    const ACCENT: Color = Color::new(0x60, 0x9B, 0xFF); // bright blue accent
    const ACCENT_DIM: Color = Color::new(0x30, 0x50, 0x80); // dimmed accent

    /// Compute the gradient colour at position (x, y) for a screen of (w, h).
    /// Uses bilinear interpolation between four corner colours.
    #[inline]
    fn gradient_at(x: usize, y: usize, w: usize, h: usize) -> Color {
        let fx = (x as u16).min(w as u16 - 1);
        let fy = (y as u16).min(h as u16 - 1);
        let ww = w as u16;
        let hh = h as u16;

        // Bilinear interpolation: top = lerp(TL, TR, fx/w), bottom = lerp(BL, BR, fx/w), result = lerp(top, bottom, fy/h)
        // Using integer math: val = (TL*(w-x)*(h-y) + TR*x*(h-y) + BL*(w-x)*y + BR*x*y) / (w*h)
        // To avoid overflow with u16, we use u32 intermediates.
        let iwx = (ww - fx) as u32;
        let ix = fx as u32;
        let ihy = (hh - fy) as u32;
        let iy = fy as u32;
        let denom = ww as u32 * hh as u32;

        let r = (GRAD_TL.0 as u32 * iwx * ihy
            + GRAD_TR.0 as u32 * ix * ihy
            + GRAD_BL.0 as u32 * iwx * iy
            + GRAD_BR.0 as u32 * ix * iy)
            / denom;
        let g = (GRAD_TL.1 as u32 * iwx * ihy
            + GRAD_TR.1 as u32 * ix * ihy
            + GRAD_BL.1 as u32 * iwx * iy
            + GRAD_BR.1 as u32 * ix * iy)
            / denom;
        let b = (GRAD_TL.2 as u32 * iwx * ihy
            + GRAD_TR.2 as u32 * ix * ihy
            + GRAD_BL.2 as u32 * iwx * iy
            + GRAD_BR.2 as u32 * ix * iy)
            / denom;
        Color::new(r as u8, g as u8, b as u8)
    }

    /// Alpha-blend foreground colour over background. alpha 0=transparent, 255=opaque.
    #[inline]
    fn blend(fg: Color, bg: Color, alpha: u8) -> Color {
        let a = alpha as u16;
        let ia = 255 - a;
        Color::new(
            ((fg.r as u16 * a + bg.r as u16 * ia) / 255) as u8,
            ((fg.g as u16 * a + bg.g as u16 * ia) / 255) as u8,
            ((fg.b as u16 * a + bg.b as u16 * ia) / 255) as u8,
        )
    }

    /// Redraw desktop background in a region.
    fn draw_gradient_region(
        fb: &mut Framebuffer,
        x0: usize,
        y0: usize,
        w: usize,
        h: usize,
        _sw: usize,
        _sh: usize,
    ) {
        let desktop_bg = Color::new(0x0C, 0x10, 0x20);
        fb.draw_rect(x0, y0, w, h, desktop_bg);
    }

    /// Draw a filled rectangle with alpha blending over the gradient background.
    fn draw_rect_alpha(
        fb: &mut Framebuffer,
        x0: usize,
        y0: usize,
        w: usize,
        h: usize,
        color: Color,
        alpha: u8,
        sw: usize,
        sh: usize,
    ) {
        for dy in 0..h {
            let y = y0 + dy;
            for dx in 0..w {
                let x = x0 + dx;
                let bg = gradient_at(x, y, sw, sh);
                fb.put_pixel(x, y, blend(color, bg, alpha));
            }
        }
    }

    /// Blit a surface onto the framebuffer with alpha blending against the gradient.
    /// Blit a surface — always fast memcpy. Both active and inactive use opaque blit.
    /// Inactive windows appear dimmed because the dashboard task draws with darker colors.
    fn blit_window(
        fb: &mut Framebuffer,
        src: &Framebuffer,
        dest_x: usize,
        dest_y: usize,
        _is_active: bool,
    ) {
        fb.blit(src, dest_x, dest_y);
    }

    /// Draw a horizontal line with alpha.
    fn hline_alpha(
        fb: &mut Framebuffer,
        x0: usize,
        y: usize,
        w: usize,
        c: Color,
        alpha: u8,
        sw: usize,
        sh: usize,
    ) {
        for dx in 0..w {
            let x = x0 + dx;
            let bg = gradient_at(x, y, sw, sh);
            fb.put_pixel(x, y, blend(c, bg, alpha));
        }
    }

    /// Format a number into a decimal string in a buffer. Returns slice length.
    fn fmt_u64(n: u64, buf: &mut [u8]) -> usize {
        if n == 0 {
            buf[0] = b'0';
            return 1;
        }
        let mut v = n;
        let mut d = [0u8; 20];
        let mut di = 0;
        while v > 0 {
            d[di] = b'0' + (v % 10) as u8;
            v /= 10;
            di += 1;
        }
        let len = di.min(buf.len());
        for i in 0..len {
            buf[i] = d[di - 1 - i];
        }
        len
    }

    fn fmt_latency(ms: u64, frac: u64, us: u64, buf: &mut [u8; 10]) -> usize {
        let mut i = 0;
        if ms > 0 || us >= 1000 {
            let mut n = ms;
            if n == 0 {
                buf[i] = b'0';
                i += 1;
            } else {
                let mut d = [0u8; 6];
                let mut di = 0;
                while n > 0 {
                    d[di] = b'0' + (n % 10) as u8;
                    n /= 10;
                    di += 1;
                }
                while di > 0 {
                    di -= 1;
                    buf[i] = d[di];
                    i += 1;
                }
            }
            buf[i] = b'.';
            i += 1;
            buf[i] = b'0' + (frac % 10) as u8;
            i += 1;
            buf[i] = b'm';
            i += 1;
            buf[i] = b's';
            i += 1;
        } else {
            let mut n = us;
            if n == 0 {
                buf[i] = b'0';
                i += 1;
            } else {
                let mut d = [0u8; 6];
                let mut di = 0;
                while n > 0 {
                    d[di] = b'0' + (n % 10) as u8;
                    n /= 10;
                    di += 1;
                }
                while di > 0 {
                    di -= 1;
                    buf[i] = d[di];
                    i += 1;
                }
            }
            buf[i] = b'u';
            i += 1;
            buf[i] = b's';
            i += 1;
        }
        i
    }

    /// Format uptime HH:MM:SS into an 8-byte buffer.
    fn fmt_hms(ns: u64, buf: &mut [u8; 8]) {
        let secs = ns / 1_000_000_000;
        let mins = secs / 60;
        let hrs = mins / 60;
        buf[0] = b'0' + (hrs / 10 % 10) as u8;
        buf[1] = b'0' + (hrs % 10) as u8;
        buf[2] = b':';
        buf[3] = b'0' + (mins % 60 / 10) as u8;
        buf[4] = b'0' + (mins % 60 % 10) as u8;
        buf[5] = b':';
        buf[6] = b'0' + (secs % 60 / 10) as u8;
        buf[7] = b'0' + (secs % 60 % 10) as u8;
    }

    // ============================================================================
    // Menu bar — dark translucent strip at top
    // ============================================================================

    fn draw_menu_bar(
        fb: &mut Framebuffer,
        sw: usize,
        sh: usize,
        ns: u64,
        latency_us: u64,
        task_count: u64,
    ) {
        // Semi-transparent dark bar
        draw_rect_alpha(fb, 0, 0, sw, MENU_H, MENU_BG, 210, sw, sh);

        // Subtle bottom edge
        hline_alpha(fb, 0, MENU_H, sw, ACCENT_DIM, 80, sw, sh);

        // Left: FreshOS logo text
        fb.draw_aa_string(12, 5, "FreshOS", ACCENT, MENU_BG);

        // Separator dot
        fb.draw_aa_string(90, 5, "|", DIM, MENU_BG);

        // Version / tag
        fb.draw_aa_string(100, 5, "microkernel", DIM, MENU_BG);

        // Right side: clock
        let mut hms = [0u8; 8];
        fmt_hms(ns, &mut hms);
        let time_str = core::str::from_utf8(&hms).unwrap_or("??:??:??");
        // Clock at right edge
        let clock_x = sw - 8 * font_aa::GLYPH_W - 12;
        fb.draw_aa_string(clock_x, 5, time_str, TEXT, MENU_BG);

        // Status blobs left of clock
        let mut sx = clock_x - 12;

        // Latency blob
        if latency_us > 0 {
            let ms = latency_us / 1000;
            let frac = (latency_us % 1000) / 100;
            let mut lbuf = [0u8; 10];
            let llen = fmt_latency(ms, frac, latency_us, &mut lbuf);
            let lstr = core::str::from_utf8(&lbuf[..llen]).unwrap_or("?");
            let lw = llen * font_aa::GLYPH_W;
            sx -= lw + 8;
            let lat_col = if latency_us < 2000 {
                GREEN
            } else if latency_us < 5000 {
                ORANGE
            } else {
                Color::new(0xFF, 0x44, 0x44)
            };
            fb.draw_aa_string(sx, 5, lstr, lat_col, MENU_BG);
            sx -= 4;
        }

        // Task count
        {
            let mut tbuf = [0u8; 16];
            let tlen = fmt_u64(task_count, &mut tbuf);
            // "6 tasks"
            let mut full = [b' '; 10];
            for i in 0..tlen.min(4) {
                full[i] = tbuf[i];
            }
            full[tlen.min(4)] = b' ';
            full[tlen.min(4) + 1] = b't';
            full[tlen.min(4) + 2] = b'a';
            full[tlen.min(4) + 3] = b's';
            full[tlen.min(4) + 4] = b'k';
            full[tlen.min(4) + 5] = b's';
            let slen = tlen.min(4) + 6;
            let s = core::str::from_utf8(&full[..slen]).unwrap_or("?");
            let tw = slen * font_aa::GLYPH_W;
            sx -= tw + 8;
            fb.draw_aa_string(sx, 5, s, SUBTLE, MENU_BG);
        }
    }

    // ============================================================================
    // Bottom taskbar — centred workspace pills
    // ============================================================================

    fn draw_taskbar(fb: &mut Framebuffer, sw: usize, sh: usize, active_ws: usize) {
        // Opaque dark bar (fast)
        fb.draw_rect(0, TBAR_Y, sw, TBAR_H, TBAR_BG_COL);
        // Top edge line
        fb.draw_rect(0, TBAR_Y, sw, 1, ACCENT_DIM);

        // Centred workspace pills
        let pw = 100;
        let ph = 24;
        let gap = 12;
        let total = pw * 2 + gap;
        let px = (sw - total) / 2;
        let py = TBAR_Y + (TBAR_H - ph) / 2;

        // Shell pill
        {
            let is_active = active_ws == 0;
            let border_c = if is_active { GREEN } else { DIM };
            let fill_c = if is_active {
                Color::new(0x1A, 0x2E, 0x1A)
            } else {
                Color::new(0x18, 0x1C, 0x22)
            };
            let fill_a: u8 = if is_active { 220 } else { 180 };
            let text_c = if is_active { GREEN } else { SUBTLE };

            // Rounded-ish pill: border then fill
            fb.draw_rect(px, py, pw, ph, border_c);
            fb.draw_rect(px + 1, py + 1, pw - 2, ph - 2, fill_c);
            if is_active {
                fb.draw_rect(px + pw / 2 - 2, py + ph - 4, 4, 2, GREEN);
            }
            fb.draw_aa_string(px + pw / 2 - 25, py + 4, "Shell", text_c, fill_c);
        }

        // Dashboard pill
        {
            let is_active = active_ws == 1;
            let border_c = if is_active { ORANGE } else { DIM };
            let fill_c = if is_active {
                Color::new(0x2E, 0x22, 0x1A)
            } else {
                Color::new(0x18, 0x1C, 0x22)
            };
            let text_c = if is_active { ORANGE } else { SUBTLE };
            let px1 = px + pw + gap;

            fb.draw_rect(px1, py, pw, ph, border_c);
            fb.draw_rect(px1 + 1, py + 1, pw - 2, ph - 2, fill_c);
            if is_active {
                fb.draw_rect(px1 + pw / 2 - 2, py + ph - 4, 4, 2, ORANGE);
            }
            fb.draw_aa_string(px1 + pw / 2 - 43, py + 4, "Dashboard", text_c, fill_c);
        }
    }

    // ============================================================================
    // System stats overlay — right side below menu bar
    // ============================================================================

    fn draw_stats_overlay(
        fb: &mut Framebuffer,
        sw: usize,
        sh: usize,
        ns: u64,
        latency_hist: &[u64; 16],
        hist_idx: usize,
    ) {
        let panel_w: usize = 200;
        let panel_h: usize = 140;
        let panel_x = sw - panel_w - 16;
        let panel_y = MENU_H + 12;

        // Opaque panel background (fast)
        fb.draw_rect(
            panel_x,
            panel_y,
            panel_w,
            panel_h,
            Color::new(0x08, 0x0C, 0x14),
        );
        fb.draw_rect(panel_x, panel_y, panel_w, 1, ACCENT_DIM);
        fb.draw_rect(panel_x, panel_y + panel_h - 1, panel_w, 1, DIM);

        let lx = panel_x + 10;
        let mut y = panel_y + 8;

        // Title
        fb.draw_aa_string(lx, y, "System", ACCENT, Color::new(0x08, 0x0C, 0x14));
        y += 18;

        // Uptime
        let mut hms = [0u8; 8];
        fmt_hms(ns, &mut hms);
        let time_str = core::str::from_utf8(&hms).unwrap_or("??:??:??");
        fb.draw_aa_string(lx, y, "Uptime", SUBTLE, Color::new(0x08, 0x0C, 0x14));
        fb.draw_aa_string(lx + 80, y, time_str, TEXT, Color::new(0x08, 0x0C, 0x14));
        y += 16;

        // Tasks
        fb.draw_aa_string(lx, y, "Tasks", SUBTLE, Color::new(0x08, 0x0C, 0x14));
        fb.draw_aa_string(lx + 80, y, "6 ring-3", TEXT, Color::new(0x08, 0x0C, 0x14));
        y += 16;

        // Scheduler
        fb.draw_aa_string(lx, y, "Sched", SUBTLE, Color::new(0x08, 0x0C, 0x14));
        fb.draw_aa_string(lx + 80, y, "1 kHz", TEXT, Color::new(0x08, 0x0C, 0x14));
        y += 20;

        // Latency graph — bar chart of last 16 values
        fb.draw_aa_string(lx, y, "Latency", SUBTLE, Color::new(0x08, 0x0C, 0x14));
        y += 14;

        let bar_area_w = panel_w - 20;
        let bar_h: usize = 30;
        let num_bars: usize = 16;
        let bar_w = bar_area_w / num_bars;

        // Find max for scaling
        let mut max_val: u64 = 1;
        for i in 0..num_bars {
            if latency_hist[i] > max_val {
                max_val = latency_hist[i];
            }
        }

        for i in 0..num_bars {
            // Draw bars oldest to newest
            let idx = (hist_idx + i) % num_bars;
            let val = latency_hist[idx];
            let h = if val > 0 {
                ((val as usize * bar_h) / max_val as usize).max(1)
            } else {
                0
            };
            let bx = lx + i * bar_w;
            let by = y + bar_h - h;

            // Color based on value
            let c = if val < 2000 {
                GREEN
            } else if val < 5000 {
                ORANGE
            } else {
                Color::new(0xFF, 0x44, 0x44)
            };

            if h > 0 {
                for dy in 0..h {
                    for dx in 0..bar_w.saturating_sub(1) {
                        fb.put_pixel(bx + dx, by + dy, c);
                    }
                }
            }
        }
    }

    // ============================================================================
    // Window title bar drawing — gradient title bar with text
    // ============================================================================

    fn draw_window_frame(
        fb: &mut Framebuffer,
        wx: usize,
        wy: usize,
        ww: usize,
        wh: usize,
        title: &str,
        accent: Color,
        is_active: bool,
        _sw: usize,
        _sh: usize,
    ) {
        let border_c = if is_active { accent } else { DIM };
        let title_y = wy.saturating_sub(WIN_TITLE_H);

        // Simple opaque border (no alpha — fast)
        let bw = WIN_BORDER;
        // Top
        fb.draw_rect(
            wx.saturating_sub(bw),
            title_y.saturating_sub(bw),
            ww + bw * 2,
            bw,
            border_c,
        );
        // Bottom
        fb.draw_rect(wx.saturating_sub(bw), wy + wh, ww + bw * 2, bw, border_c);
        // Left
        fb.draw_rect(
            wx.saturating_sub(bw),
            title_y,
            bw,
            wh + WIN_TITLE_H,
            border_c,
        );
        // Right
        fb.draw_rect(wx + ww, title_y, bw, wh + WIN_TITLE_H, border_c);

        // Title bar — opaque dark fill
        let bar_c = if is_active {
            Color::new(0x18, 0x1C, 0x24)
        } else {
            Color::new(0x10, 0x13, 0x18)
        };
        fb.draw_rect(wx, title_y, ww, WIN_TITLE_H, bar_c);

        // Title text
        let text_c = if is_active { TEXT } else { SUBTLE };
        fb.draw_aa_string(wx + 10, title_y + 6, title, text_c, bar_c);

        // Accent dot
        if is_active {
            let dot_x = wx + 10 + title.len() * font_aa::GLYPH_W + 8;
            let dot_y = title_y + 10;
            fb.draw_rect(dot_x, dot_y, 4, 4, accent);
        }

        // Separator line under title
        fb.draw_rect(
            wx,
            title_y + WIN_TITLE_H - 1,
            ww,
            1,
            if is_active { accent } else { DIM },
        );
    }

    // ============================================================================
    // Desktop icons — small sprites drawn on the gradient
    // ============================================================================

    // 16x16 disk icon (1=dark outline, 2=fill, 3=highlight, 4=label area)
    #[rustfmt::skip]
const DISK_ICON: [[u8; 16]; 16] = [
    [0,0,1,1,1,1,1,1,1,1,1,1,1,1,0,0],
    [0,1,3,3,3,3,3,3,3,3,3,3,3,3,1,0],
    [1,3,2,2,2,2,2,2,2,2,2,2,2,2,3,1],
    [1,3,2,4,4,4,4,4,4,4,4,4,4,2,3,1],
    [1,3,2,4,4,4,4,4,4,4,4,4,4,2,3,1],
    [1,3,2,4,4,4,4,4,4,4,4,4,4,2,3,1],
    [1,3,2,2,2,2,2,2,2,2,2,2,2,2,3,1],
    [1,3,2,2,2,2,2,2,2,2,2,2,2,2,3,1],
    [1,3,2,2,2,2,2,2,2,2,2,2,2,2,3,1],
    [1,3,2,2,2,2,2,2,2,2,2,2,2,2,3,1],
    [1,3,2,2,2,2,2,2,2,2,2,2,2,2,3,1],
    [1,3,2,2,1,1,1,1,1,1,2,2,2,2,3,1],
    [1,3,2,2,1,3,3,3,3,1,2,2,2,2,3,1],
    [1,3,2,2,1,3,3,3,3,1,2,2,2,2,3,1],
    [0,1,3,3,1,3,3,3,3,1,3,3,3,3,1,0],
    [0,0,1,1,1,1,1,1,1,1,1,1,1,1,0,0],
];

    fn draw_icon(fb: &mut Framebuffer, ix: usize, iy: usize, accent: Color, sw: usize, sh: usize) {
        let scale = 2; // draw at 2x (32x32 on screen)
        for row in 0..16 {
            for col in 0..16 {
                let c = match DISK_ICON[row][col] {
                    1 => Color::new(0x20, 0x25, 0x30), // outline
                    2 => blend(
                        accent,
                        gradient_at(ix + col * scale, iy + row * scale, sw, sh),
                        80,
                    ),
                    3 => blend(Color::new(0xFF, 0xFF, 0xFF), accent, 40),
                    4 => Color::new(0x18, 0x1C, 0x24), // label area
                    _ => continue,
                };
                for sy in 0..scale {
                    for sx in 0..scale {
                        fb.put_pixel(ix + col * scale + sx, iy + row * scale + sy, c);
                    }
                }
            }
        }
    }

    fn draw_desktop_icons(fb: &mut Framebuffer, sw: usize, sh: usize) {
        // System disk — top left below menu bar
        let ix = 30;
        let iy = 50;
        draw_icon(fb, ix, iy, ACCENT, sw, sh);
        fb.draw_aa_string(
            ix - 4,
            iy + 36,
            "System",
            SUBTLE,
            gradient_at(ix, iy + 36, sw, sh),
        );

        // Storage disk — below system
        let iy2 = 120;
        draw_icon(fb, ix, iy2, ORANGE, sw, sh);
        fb.draw_aa_string(
            ix - 4,
            iy2 + 36,
            "Disk 0",
            SUBTLE,
            gradient_at(ix, iy2 + 36, sw, sh),
        );
    }

    // ============================================================================
    // Chiptune melody — plays as a background task
    // ============================================================================

    // A simple looping melody (frequency in Hz, duration in ms, 0 = rest)
    const MELODY: [(u32, u32); 32] = [
        (523, 150),
        (0, 50), // C5
        (587, 150),
        (0, 50), // D5
        (659, 300),
        (0, 100), // E5
        (523, 150),
        (0, 50), // C5
        (659, 150),
        (0, 50), // E5
        (784, 300),
        (0, 100), // G5
        (659, 150),
        (0, 50), // E5
        (784, 150),
        (0, 50), // G5
        (1047, 300),
        (0, 100), // C6
        (784, 150),
        (0, 50), // G5
        (659, 300),
        (0, 200), // E5
        (523, 150),
        (0, 50), // C5
        (440, 150),
        (0, 50), // A4
        (523, 300),
        (0, 100), // C5
        (440, 150),
        (0, 50), // A4
        (392, 600),
        (0, 400), // G4 (long, rest before repeat)
    ];

    pub fn user_chiptune() -> ! {
        // Wait a moment after boot before starting the melody
        for _ in 0..200 {
            user_yield();
        }

        loop {
            for &(freq, dur) in &MELODY {
                if freq > 0 {
                    user_beep(freq, dur);
                } else {
                    // Rest: just yield for the duration
                    let start = user_time_ns();
                    let target = dur as u64 * 1_000_000;
                    while user_time_ns().wrapping_sub(start) < target {
                        user_yield();
                    }
                }
            }
        }
    }

    // ============================================================================
    // Compositor (ring 3) — owns the framebuffer, composites surfaces
    // ============================================================================

    pub fn user_compositor() -> ! {
        let (mut fb, fbi) = get_display();
        let s0 = get_surface(0);
        let s1 = get_surface(1);

        let sw = fbi.width as usize;
        let sh = fbi.height as usize;

        // The kernel chose the backing buffer (virtio-GPU resource or GOP
        // framebuffer) and exposed it via SYS_FBINFO. We draw straight into
        // it; SYS_PRESENT_RECT below flushes to the display when needed.
        let mut draw_fb = fb.clone();

        let mut active_ws: usize = 0;
        let mut last_latency_us: u64 = 0;
        let mut mouse_x: usize = sw / 2;
        let mut mouse_y: usize = sh / 2;
        let mut prev_left_btn = false;

        // Latency history for the graph (ring buffer)
        let mut latency_hist = [0u64; 16];
        let mut hist_idx: usize = 0;

        // Frame counter for pulsing effects
        let mut frame: u64 = 0;

        // Entry sound
        user_beep(523, 30);

        // Window positions — the two workspaces as overlapping windows
        // Active window is centred, inactive is offset behind
        let win_w = SURF_W;
        let win_h = SURF_H;

        // Solid background — one fb.clear, instant
        let desktop_bg = Color::new(0x0C, 0x10, 0x20);
        fb.clear(desktop_bg);

        // Desktop icons
        draw_desktop_icons(&mut draw_fb, sw, sh);
        // Also draw the menu bar and taskbar immediately so they appear fast
        draw_menu_bar(&mut draw_fb, sw, sh, 0, 0, 7);
        draw_taskbar(&mut draw_fb, sw, sh, 0);

        // Initial presentation — flush the full screen once.
        user_present_rect(0, 0, sw as u32, sh as u32);

        // Track what needs redrawing
        let mut needs_full_redraw = true;
        let mut prev_active_ws: usize = 99; // force first draw

        loop {
            let ns = user_time_ns();
            frame += 1;

            // Flags for dirty zones
            let mut menu_dirty = true; // For now redraw all chrome every frame
            let mut stats_dirty = true;
            let mut taskbar_dirty = true;
            let mut content_dirty = false;

            // Compute window positions based on active workspace
            // Active window: centred in the content area (between menu and taskbar)
            let content_top = MENU_H + 4;
            let content_bot = TBAR_Y - 4;
            let content_h = content_bot - content_top;

            let active_x = (sw - win_w) / 2;
            let active_y = content_top + (content_h - win_h - WIN_TITLE_H) / 2 + WIN_TITLE_H;

            // Inactive window: peeking out behind, offset to the side
            let inactive_x = if active_ws == 0 {
                active_x + 60
            } else {
                active_x - 60
            };
            let inactive_y = active_y + 20;

            // Detect workspace switch — redraw gradient in affected regions
            if active_ws != prev_active_ws || needs_full_redraw {
                // Redraw gradient over the entire content area
                draw_gradient_region(&mut draw_fb, 0, content_top, sw, content_h, sw, sh);
                prev_active_ws = active_ws;
                needs_full_redraw = false;
                content_dirty = true;
            } else {
                // Skip gradient redraw on non-switch frames — windows overwrite their area
                // (In a real compositor, we'd check if any window has damage)
                content_dirty = true; // Force redraw content for simplicity for now
            }

            // Draw inactive window first (behind)
            {
                let (surf, title, accent) = if active_ws == 0 {
                    (&s1, "Dashboard", ORANGE)
                } else {
                    (&s0, "Shell", GREEN)
                };
                draw_window_frame(
                    &mut draw_fb,
                    inactive_x,
                    inactive_y,
                    win_w,
                    win_h,
                    title,
                    accent,
                    false,
                    sw,
                    sh,
                );
                blit_window(&mut draw_fb, surf, inactive_x, inactive_y, false);
            }

            // Draw active window on top
            {
                let (surf, title, accent) = if active_ws == 0 {
                    (&s0, "Shell", GREEN)
                } else {
                    (&s1, "Dashboard", ORANGE)
                };
                draw_window_frame(
                    &mut draw_fb,
                    active_x,
                    active_y,
                    win_w,
                    win_h,
                    title,
                    accent,
                    true,
                    sw,
                    sh,
                );
                blit_window(&mut draw_fb, surf, active_x, active_y, true);
            }

            // -- Menu bar (always on top) --
            draw_menu_bar(&mut draw_fb, sw, sh, ns, last_latency_us, 6);

            // -- System stats overlay --
            draw_stats_overlay(&mut draw_fb, sw, sh, ns, &latency_hist, hist_idx);

            // -- Bottom taskbar --
            draw_taskbar(&mut draw_fb, sw, sh, active_ws);

            // -- Handle keyboard input --
            if let Some(msg) = ipc::try_recv(CH_KBD_EVENTS) {
                if msg.tag == ipc::MSG_KEY_DOWN {
                    let irq_ns = msg.payload[2];
                    if irq_ns > 0 {
                        let now = user_time_ns();
                        if now > irq_ns {
                            last_latency_us = (now - irq_ns) / 1000;
                            latency_hist[hist_idx] = last_latency_us;
                            hist_idx = (hist_idx + 1) % 16;
                        }
                    }

                    let ch = msg.payload[0] as u8;
                    if ch == b'\t' {
                        active_ws = 1 - active_ws;
                        needs_full_redraw = true;
                        user_beep(if active_ws == 0 { 440 } else { 523 }, 20);
                    } else if ch == b'1' {
                        if active_ws != 0 {
                            needs_full_redraw = true;
                        }
                        active_ws = 0;
                    } else if ch == b'2' {
                        if active_ws != 1 {
                            needs_full_redraw = true;
                        }
                        active_ws = 1;
                    } else if active_ws == 0 {
                        let _ = user_send(CH_SHELL_KEYS, &msg);
                    }
                }
            }

            // -- Handle mouse input --
            if let Some(msg) = ipc::try_recv(CH_MOUSE_EVENTS) {
                if msg.tag == ipc::MSG_MOUSE {
                    mouse_x = (msg.payload[0] as usize).min(sw - 1);
                    mouse_y = (msg.payload[1] as usize).min(sh - 1);
                    let left_btn = msg.payload[2] & 1 != 0;

                    if prev_left_btn && !left_btn {
                        // Click on taskbar pills to switch workspace
                        if mouse_y >= TBAR_Y {
                            let pw = 100usize;
                            let gap = 12usize;
                            let total = pw * 2 + gap;
                            let px = (sw - total) / 2;
                            if mouse_x >= px && mouse_x < px + pw {
                                if active_ws != 0 {
                                    needs_full_redraw = true;
                                }
                                active_ws = 0;
                                user_beep(440, 20);
                            } else if mouse_x >= px + pw + gap && mouse_x < px + total {
                                if active_ws != 1 {
                                    needs_full_redraw = true;
                                }
                                active_ws = 1;
                                user_beep(523, 20);
                            }
                        } else {
                            // Click anywhere else toggles workspace
                            active_ws = 1 - active_ws;
                            needs_full_redraw = true;
                            user_beep(if active_ws == 0 { 440 } else { 523 }, 20);
                        }
                    }
                    prev_left_btn = left_btn;
                }
            }

            // -- Cursor (always on top of everything) --
            draw_cursor(&mut draw_fb, mouse_x, mouse_y);

            // -- Final Presentation --
            // SYS_PRESENT_RECT is a no-op when no virtio-GPU is present (the
            // GOP framebuffer is scanned out directly), so no branching here.
            if menu_dirty {
                user_present_rect(0, 0, sw as u32, MENU_H as u32 + 1);
            }
            if content_dirty {
                user_present_rect(0, content_top as u32, sw as u32, content_h as u32);
            }
            if stats_dirty {
                let stats_panel_w: usize = 200;
                let stats_panel_h: usize = 140;
                let stats_panel_x = sw - stats_panel_w - 16;
                let stats_panel_y = MENU_H + 12;
                user_present_rect(
                    stats_panel_x as u32,
                    stats_panel_y as u32,
                    stats_panel_w as u32,
                    stats_panel_h as u32,
                );
            }
            if taskbar_dirty {
                user_present_rect(0, TBAR_Y as u32, sw as u32, TBAR_H as u32);
            }
            // Cursor dirty rect (simplified: redraw a 32x32 area)
            user_present_rect(mouse_x as u32, mouse_y as u32, 32, 32);

            user_yield();
        }
    }

    // ============================================================================
    // Shell workspace (ring 3) — draws to surface 0
    // ============================================================================

    /// Shell background colour — dark but distinct from the gradient so it reads well through translucency.
    const SHELL_BG: Color = Color::new(0x0A, 0x0E, 0x14);

    pub fn user_shell() -> ! {
        let mut surf = get_surface(0);
        surf.clear(SHELL_BG);

        // Subtle header area
        surf.draw_rect(0, 0, SURF_W, 38, Color::new(0x0D, 0x12, 0x18));
        surf.draw_aa_string(14, 10, "~  shell", GREEN, Color::new(0x0D, 0x12, 0x18));
        surf.draw_rect(0, 38, SURF_W, 1, SEP);

        let left = 14;
        let char_w = font_aa::GLYPH_W;
        let line_h = font_aa::GLYPH_H + 3;
        let start_y = 48;
        let max_y = SURF_H - line_h;
        let mut cx = left;
        let mut cy = start_y;

        // Prompt
        let prompt_color = ACCENT;
        surf.draw_aa_char(cx, cy, '>', prompt_color, SHELL_BG);
        cx += char_w;
        surf.draw_aa_char(cx, cy, ' ', prompt_color, SHELL_BG);
        cx += char_w;

        loop {
            let mut msg = ipc::Message::empty();
            if user_recv(CH_SHELL_KEYS, &mut msg) < 0 {
                continue;
            }
            if msg.tag != ipc::MSG_KEY_DOWN {
                continue;
            }

            let ch = msg.payload[0] as u8;
            match ch {
                b'\n' => {
                    cy += line_h;
                    cx = left;
                    if cy >= max_y {
                        cy = start_y;
                        surf.draw_rect(left, start_y, SURF_W - left, max_y - start_y, SHELL_BG);
                    }
                    surf.draw_aa_char(cx, cy, '>', prompt_color, SHELL_BG);
                    cx += char_w;
                    surf.draw_aa_char(cx, cy, ' ', prompt_color, SHELL_BG);
                    cx += char_w;
                }
                0x08 => {
                    if cx > left + 2 * char_w {
                        cx -= char_w;
                        surf.draw_rect(cx, cy, char_w, font_aa::GLYPH_H, SHELL_BG);
                    }
                }
                _ => {
                    surf.draw_aa_char(cx, cy, ch as char, GREEN, SHELL_BG);
                    cx += char_w;
                }
            }
        }
    }

    // ============================================================================
    // Dashboard/clock workspace (ring 3) — draws to surface 1
    // ============================================================================

    /// Tag name for the message trace display.
    fn tag_label(tag: u16) -> &'static str {
        match tag as u32 {
            ipc::MSG_IRQ => "IRQ",
            ipc::MSG_KEY_DOWN => "KEY",
            ipc::MSG_KEY_UP => "KEY_UP",
            ipc::MSG_PING => "PING",
            ipc::MSG_PONG => "PONG",
            _ => "MSG",
        }
    }

    const TASK_LABELS: [&str; 7] = ["idle", "kbd", "mouse", "comp", "shell", "dash", "music"];
    const TASK_COLORS: [Color; 7] = [
        DIM,
        TITLE,
        SUBTLE,
        SUBTLE,
        GREEN,
        ORANGE,
        Color::new(0xD2, 0xA8, 0xFF),
    ];
    const DASH_BG: Color = Color::new(0x0A, 0x0E, 0x14);

    pub fn user_clock() -> ! {
        let mut surf = get_surface(1);
        let mut trace_buf = [ipc::TraceEntry {
            timestamp_ns: 0,
            from_task: 0,
            to_task: 0,
            channel: 0,
            tag: 0,
        }; 16];

        loop {
            surf.clear(DASH_BG);

            // Header area
            surf.draw_rect(0, 0, SURF_W, 38, Color::new(0x0D, 0x12, 0x18));
            surf.draw_aa_string(14, 10, "~  dashboard", ORANGE, Color::new(0x0D, 0x12, 0x18));
            surf.draw_rect(0, 38, SURF_W, 1, SEP);

            let ns = user_time_ns();
            let secs = ns / 1_000_000_000;
            let mins = secs / 60;
            let hrs = mins / 60;

            let mut y = 50;

            // -- Big clock --
            let mut hms = [0u8; 8];
            fmt_hms(ns, &mut hms);
            let time_str = core::str::from_utf8(&hms).unwrap_or("??:??:??");
            surf.draw_aa_string_2x(14, y, time_str, TEXT, DASH_BG);
            y += 40;

            // Uptime label
            surf.draw_aa_string(14, y, "uptime", DIM, DASH_BG);
            y += 22;

            // Separator
            surf.draw_rect(14, y, SURF_W - 28, 1, SEP);
            y += 10;

            // -- System info --
            surf.draw_aa_string(14, y, "Tasks", SUBTLE, DASH_BG);
            surf.draw_aa_string(110, y, "6 ring-3 @ 1 kHz", TEXT, DASH_BG);
            y += 18;

            surf.draw_aa_string(14, y, "Arch", SUBTLE, DASH_BG);
            surf.draw_aa_string(110, y, "x86_64 microkernel", TEXT, DASH_BG);
            y += 18;

            surf.draw_aa_string(14, y, "IPC", SUBTLE, DASH_BG);
            surf.draw_aa_string(110, y, "5 channels", TEXT, DASH_BG);
            y += 26;

            // -- Message Flow --
            surf.draw_aa_string(14, y, "Message Flow", ORANGE, DASH_BG);
            y += 14;
            surf.draw_rect(14, y, SURF_W - 28, 1, SEP);
            y += 8;

            let now = user_time_ns();
            let count = user_trace(&mut trace_buf);

            if count == 0 {
                surf.draw_aa_string(14, y, "(waiting for messages...)", DIM, DASH_BG);
            } else {
                let show = count.min(8);
                let start = if count > 8 { count - 8 } else { 0 };

                for row in 0..show {
                    let e = &trace_buf[start + row];
                    let age_ms = if now > e.timestamp_ns {
                        (now - e.timestamp_ns) / 1_000_000
                    } else {
                        0
                    };

                    let color = if age_ms < 300 {
                        GREEN
                    } else if age_ms < 2000 {
                        SUBTLE
                    } else {
                        DIM
                    };

                    let mut rx = 14;
                    let gw = font_aa::GLYPH_W;

                    // Age (right-aligned 4 chars)
                    let mut abuf = [b' '; 4];
                    let mut av = age_ms;
                    let mut ai = 3;
                    loop {
                        abuf[ai] = b'0' + (av % 10) as u8;
                        av /= 10;
                        if av == 0 || ai == 0 {
                            break;
                        }
                        ai -= 1;
                    }
                    for &b in &abuf {
                        surf.draw_aa_char(rx, y, b as char, DIM, DASH_BG);
                        rx += gw;
                    }
                    surf.draw_aa_string(rx, y, "ms", DIM, DASH_BG);
                    rx += gw * 3;

                    let fi = (e.from_task as usize).min(6);
                    let ti = (e.to_task as usize).min(6);
                    surf.draw_aa_string(rx, y, TASK_LABELS[fi], TASK_COLORS[fi], DASH_BG);
                    rx += TASK_LABELS[fi].len() * gw;
                    surf.draw_aa_string(rx, y, ">", color, DASH_BG);
                    rx += gw;
                    if e.to_task < 7 {
                        surf.draw_aa_string(rx, y, TASK_LABELS[ti], TASK_COLORS[ti], DASH_BG);
                        rx += TASK_LABELS[ti].len() * gw + gw;
                    } else {
                        surf.draw_aa_string(rx, y, "?", DIM, DASH_BG);
                        rx += gw * 2;
                    }

                    surf.draw_aa_string(rx, y, tag_label(e.tag), color, DASH_BG);

                    y += 14;
                }
            }

            // -- Footer --
            surf.draw_aa_string(
                14,
                SURF_H - 22,
                "The architecture is perceptible.",
                DIM,
                DASH_BG,
            );

            // Pace: ~10 fps for the dashboard
            for _ in 0..10 {
                user_yield();
            }
        }
    }
} // mod x86_tasks
