#![no_std]
#![no_main]

extern crate alloc;

mod arch;
#[cfg(target_arch = "aarch64")]
mod boot_images;
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

// Rhai scripting
mod scripting;

// aarch64 userspace tasks (EL0)
#[cfg(target_arch = "aarch64")]
mod arm_tasks;
#[cfg(target_arch = "aarch64")]
mod init_abi;
#[cfg(target_arch = "aarch64")]
mod mcp;
#[cfg(target_arch = "aarch64")]
mod service_abi;

use frame_alloc::MemRegion;
use framebuffer::{Color, Framebuffer};
use serial::serial_println;
use uefi::boot;
use uefi::mem::memory_map::{MemoryMap, MemoryType};
use uefi::prelude::*;
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat};

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

// Surfaces: each workspace is 620x480, fitting side by side above a taskbar
const SURF_W: usize = 620;
const SURF_H: usize = 480;
// (Old taskbar constants replaced by TBAR_H / TBAR_Y in compositor section)
const SURF_BYTES: usize = SURF_W * SURF_H * 4;
const SURF_PAGES: usize = (SURF_BYTES + 4095) / 4096;

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

    // The firmware default is small (800x600 on QEMU). Switch to the largest
    // mode the firmware offers, within a cap that keeps the compositor's
    // framebuffer allocations sane, for a roomier desktop.
    let best_mode = gop
        .modes()
        .filter(|m| {
            let (w, h) = m.info().resolution();
            w <= 1920 && h <= 1200
        })
        .max_by_key(|m| {
            let (w, h) = m.info().resolution();
            w * h
        });
    if let Some(target) = best_mode {
        let _ = gop.set_mode(&target);
    }

    let mode = gop.current_mode_info();
    let (width, height) = mode.resolution();
    let stride = mode.stride();
    let pixel_format = mode.pixel_format();
    let fb_ptr = gop.frame_buffer().as_mut_ptr();
    drop(gop);

    let is_bgr = matches!(pixel_format, PixelFormat::Bgr);

    boot_images::load_all();

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
    serial_println!("  Board: {}", arch::board::NAME);
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
    // Tasks run at EL1 with direct syscall dispatch, sharing one page table.
    // That began as an HVF limitation: older QEMU hung on every tlbi, so page
    // tables couldn't be managed safely. QEMU 11 runs tlbi correctly, so
    // per-task EL0 isolation is now the goal here too (v1 rung 1). The SVC
    // path and EL0 infrastructure already exist.
    arch::context::init(ttbr0);

    let loaded_init = boot_images::find("INIT.ELF").and_then(|bytes| {
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

    if let Some(bytes) = boot_images::find("PONG.ELF") {
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

    if let Some(bytes) = boot_images::find("PULSE.ELF") {
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

    if let Some(image) = loaded_init {
        arch::context::spawn_with_arg(image.entry, init_abi::api_ptr() as u64);
    } else {
        serial_println!("INIT.ELF missing from \\EFI\\FreshOS — nothing to run");
        loop {
            arch::interrupt_disable();
            arch::halt();
        }
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
