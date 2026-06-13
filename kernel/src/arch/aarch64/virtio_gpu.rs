/// Virtio-GPU 2D driver over MMIO transport.
///
/// Single file: MMIO transport + GPU 2D commands + dirty-rect presentation.
/// Split into virtio.rs + gpu.rs only when a second virtio device arrives.
///
/// Barrier choice: DMB OSHST (store-only, outer-shareable) orders our stores
/// relative to the device. The GIC driver uses DSB SY for stricter ordering
/// required by interrupt controller configuration. See ARM ARM B2.7.3.
use crate::frame_alloc;
use crate::framebuffer::Framebuffer;
use crate::serial::serial_println;

// ============================================================================
// Virtio MMIO v2 register offsets
// ============================================================================

const MAGIC_VALUE: usize = 0x000;
const VERSION: usize = 0x004;
const DEVICE_ID: usize = 0x008;
const DEVICE_FEATURES: usize = 0x010;
const DEVICE_FEATURES_SEL: usize = 0x014;
const DRIVER_FEATURES: usize = 0x020;
const DRIVER_FEATURES_SEL: usize = 0x024;
const QUEUE_SEL: usize = 0x030;
const QUEUE_NUM_MAX: usize = 0x034;
const QUEUE_NUM: usize = 0x038;
const QUEUE_READY: usize = 0x044;
const QUEUE_NOTIFY: usize = 0x050;
const INTERRUPT_STATUS: usize = 0x060;
const INTERRUPT_ACK: usize = 0x064;
const STATUS: usize = 0x070;
const QUEUE_DESC_LOW: usize = 0x080;
const QUEUE_DESC_HIGH: usize = 0x084;
const QUEUE_DRIVER_LOW: usize = 0x090;
const QUEUE_DRIVER_HIGH: usize = 0x094;
const QUEUE_DEVICE_LOW: usize = 0x0A0;
const QUEUE_DEVICE_HIGH: usize = 0x0A4;

// Device status bits
const STATUS_ACKNOWLEDGE: u32 = 1;
const STATUS_DRIVER: u32 = 2;
const STATUS_DRIVER_OK: u32 = 4;
const STATUS_FEATURES_OK: u32 = 8;

// Virtqueue descriptor flags
const VRING_DESC_F_NEXT: u16 = 1;
const VRING_DESC_F_WRITE: u16 = 2;

// ============================================================================
// Virtio-GPU command types
// ============================================================================

const CMD_GET_DISPLAY_INFO: u32 = 0x0100;
const CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
const CMD_SET_SCANOUT: u32 = 0x0103;
const CMD_RESOURCE_FLUSH: u32 = 0x0104;
const CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;

const RESP_OK_NODATA: u32 = 0x1100;
const RESP_OK_DISPLAY_INFO: u32 = 0x1101;

const FORMAT_B8G8R8A8_UNORM: u32 = 1;

// ============================================================================
// Memory layout within the single queue+cmd page (4 KiB)
// ============================================================================

const QUEUE_SIZE: usize = 16;
const DESC_OFFSET: usize = 0x000; // 16 × 16 = 256 bytes
const AVAIL_OFFSET: usize = 0x100; // 4 + 16×2 = 36 bytes
const USED_OFFSET: usize = 0x200; // 4 + 16×8 = 132 bytes
const CMD_OFFSET: usize = 0x400; // 512 bytes
const RESP_OFFSET: usize = 0x600; // 512 bytes

// Compile-time layout assertions
const USED_END: usize = USED_OFFSET + 4 + QUEUE_SIZE * 8;
const _: () = assert!(USED_END <= CMD_OFFSET, "used ring overlaps command buffer");
const _: () = assert!(
    CMD_OFFSET + 512 <= RESP_OFFSET,
    "command buffer overlaps response"
);
const _: () = assert!(RESP_OFFSET + 512 <= 4096, "response buffer exceeds page");

// ============================================================================
// GPU command header (24 bytes, shared by all commands)
// ============================================================================

#[repr(C)]
struct GpuCtrlHeader {
    cmd_type: u32,
    flags: u32,
    fence_id: u64,
    ctx_id: u32,
    padding: u32,
}

// ============================================================================
// MMIO helpers
// ============================================================================

#[inline]
fn read32(base: usize, offset: usize) -> u32 {
    unsafe { core::ptr::read_volatile((base + offset) as *const u32) }
}

#[inline]
fn write32(base: usize, offset: usize, val: u32) {
    unsafe { core::ptr::write_volatile((base + offset) as *mut u32, val) }
}

#[inline]
fn dmb_oshst() {
    unsafe { core::arch::asm!("dmb oshst", options(nomem, nostack)) }
}

// ============================================================================
// VirtioGpu
// ============================================================================

pub struct VirtioGpu {
    mmio_base: usize,
    queue_page: u64,    // phys addr of the 4 KiB queue+cmd page
    avail_idx: u16,     // running counter for available ring
    last_used_idx: u16, // last seen used ring idx
    backing_phys: u64,
    width: u32,
    height: u32,
}

impl VirtioGpu {
    /// Probe MMIO bus, init device, create GPU resource, attach backing.
    /// Returns (gpu, framebuffer pointing at the backing store).
    pub fn init() -> Option<(Self, Framebuffer)> {
        // --- Probe ---
        let mmio_base = probe_gpu()?;
        serial_println!("  virtio-GPU found at {:#x}", mmio_base);

        // --- Reset + negotiate ---
        write32(mmio_base, STATUS, 0); // reset
        write32(mmio_base, STATUS, STATUS_ACKNOWLEDGE);
        write32(mmio_base, STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

        // No optional features needed
        write32(mmio_base, DEVICE_FEATURES_SEL, 0);
        let _features = read32(mmio_base, DEVICE_FEATURES);
        write32(mmio_base, DRIVER_FEATURES_SEL, 0);
        write32(mmio_base, DRIVER_FEATURES, 0);

        write32(
            mmio_base,
            STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK,
        );
        let status = read32(mmio_base, STATUS);
        if status & STATUS_FEATURES_OK == 0 {
            serial_println!("  virtio-GPU: FEATURES_OK rejected");
            return None;
        }

        // --- Set up virtqueue 0 ---
        write32(mmio_base, QUEUE_SEL, 0);
        let max = read32(mmio_base, QUEUE_NUM_MAX);
        if max < QUEUE_SIZE as u32 {
            serial_println!("  virtio-GPU: queue too small (max={})", max);
            return None;
        }
        write32(mmio_base, QUEUE_NUM, QUEUE_SIZE as u32);

        let queue_page = frame_alloc::allocate_contiguous(1).expect("virtio queue page");
        unsafe { core::ptr::write_bytes(queue_page as *mut u8, 0, 4096) }

        let desc_phys = queue_page + DESC_OFFSET as u64;
        let avail_phys = queue_page + AVAIL_OFFSET as u64;
        let used_phys = queue_page + USED_OFFSET as u64;

        write32(mmio_base, QUEUE_DESC_LOW, desc_phys as u32);
        write32(mmio_base, QUEUE_DESC_HIGH, (desc_phys >> 32) as u32);
        write32(mmio_base, QUEUE_DRIVER_LOW, avail_phys as u32);
        write32(mmio_base, QUEUE_DRIVER_HIGH, (avail_phys >> 32) as u32);
        write32(mmio_base, QUEUE_DEVICE_LOW, used_phys as u32);
        write32(mmio_base, QUEUE_DEVICE_HIGH, (used_phys >> 32) as u32);
        write32(mmio_base, QUEUE_READY, 1);

        // --- DRIVER_OK ---
        write32(
            mmio_base,
            STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK,
        );
        serial_println!("  virtio-GPU: DRIVER_OK");

        let mut gpu = VirtioGpu {
            mmio_base,
            queue_page,
            avail_idx: 0,
            last_used_idx: 0,
            backing_phys: 0,
            width: 0,
            height: 0,
        };

        // --- GET_DISPLAY_INFO ---
        let (w, h) = gpu.get_display_info();
        gpu.width = w;
        gpu.height = h;
        serial_println!("  virtio-GPU: display {}x{}", w, h);

        // --- Create resource + backing ---
        gpu.resource_create_2d(1, w, h);
        let backing_pages = (w as usize * h as usize * 4 + 4095) / 4096;
        let backing = frame_alloc::allocate_contiguous(backing_pages).expect("GPU backing");
        unsafe { core::ptr::write_bytes(backing as *mut u8, 0, w as usize * h as usize * 4) }
        gpu.backing_phys = backing;

        gpu.resource_attach_backing(1, backing, w as u64 * h as u64 * 4);
        gpu.set_scanout(0, 1, w, h);

        // Initial full-frame transfer to show something
        gpu.transfer_rect(1, 0, 0, w, h);
        gpu.flush_rect(1, 0, 0, w, h);

        serial_println!("  virtio-GPU: resource 1, backing at {:#x}", backing);

        let fb = Framebuffer::new(
            backing as *mut u8,
            w as usize,
            h as usize,
            w as usize,
            true, // B8G8R8A8 = BGR
        );

        Some((gpu, fb))
    }

    /// Transfer + flush a rectangular region. Batches both into one notify.
    pub fn present_rect(&mut self, x: u32, y: u32, w: u32, h: u32) {
        // Cache clean the dirty region before transfer
        let stride = self.width as u64 * 4;
        let start = self.backing_phys + y as u64 * stride + x as u64 * 4;
        let len = h as u64 * stride; // conservative: clean full rows
        clean_cache_range(start, len);

        // Post transfer command (desc 0+1)
        self.post_transfer(0, 1, x, y, w, h);
        // Post flush command (desc 2+3)
        self.post_flush(2, 3, x, y, w, h);

        // Barrier + single notify
        dmb_oshst();
        write32(self.mmio_base, QUEUE_NOTIFY, 0);

        // Wait for both completions
        self.poll_used(2);

        // Check responses
        self.check_response(0, "transfer");
        self.check_response(1, "flush");
    }

    /// Transfer + flush the entire screen.
    pub fn present_full(&mut self) {
        self.present_rect(0, 0, self.width, self.height);
    }

    // ========================================================================
    // GPU commands
    // ========================================================================

    fn get_display_info(&mut self) -> (u32, u32) {
        let cmd = self.cmd_ptr();
        let hdr = cmd as *mut GpuCtrlHeader;
        unsafe {
            core::ptr::write_bytes(cmd, 0, 512);
            (*hdr).cmd_type = CMD_GET_DISPLAY_INFO;
        }

        self.send_command(0, 1, 24, 408);

        // Parse response: header (24 bytes) + display[0]: {x,y,w,h,enabled,flags}
        let resp = self.resp_ptr();
        let resp_type = unsafe { core::ptr::read_volatile(resp as *const u32) };
        if resp_type != RESP_OK_DISPLAY_INFO {
            serial_println!("  virtio-GPU: GET_DISPLAY_INFO failed ({:#x})", resp_type);
            return (800, 600); // fallback
        }

        // First display: offset 24 in response, rect is {x: u32, y: u32, w: u32, h: u32}
        let w = unsafe { core::ptr::read_volatile((resp as usize + 24 + 8) as *const u32) };
        let h = unsafe { core::ptr::read_volatile((resp as usize + 24 + 12) as *const u32) };
        if w == 0 || h == 0 {
            (800, 600)
        } else {
            (w, h)
        }
    }

    fn resource_create_2d(&mut self, resource_id: u32, width: u32, height: u32) {
        let cmd = self.cmd_ptr();
        unsafe {
            core::ptr::write_bytes(cmd, 0, 512);
            let hdr = cmd as *mut GpuCtrlHeader;
            (*hdr).cmd_type = CMD_RESOURCE_CREATE_2D;
            // Body: resource_id, format, width, height (after 24-byte header)
            let body = (cmd as usize + 24) as *mut u32;
            body.write_volatile(resource_id);
            body.add(1).write_volatile(FORMAT_B8G8R8A8_UNORM);
            body.add(2).write_volatile(width);
            body.add(3).write_volatile(height);
        }
        self.send_command(0, 1, 24 + 16, 24);
        self.check_single_response("resource_create_2d");
    }

    fn resource_attach_backing(&mut self, resource_id: u32, addr: u64, length: u64) {
        let cmd = self.cmd_ptr();
        unsafe {
            core::ptr::write_bytes(cmd, 0, 512);
            let hdr = cmd as *mut GpuCtrlHeader;
            (*hdr).cmd_type = CMD_RESOURCE_ATTACH_BACKING;
            // Body: resource_id(u32), nr_entries(u32), then [{addr: u64, length: u32, pad: u32}]
            let body = (cmd as usize + 24) as *mut u32;
            body.write_volatile(resource_id);
            body.add(1).write_volatile(1); // nr_entries = 1
                                           // Entry at offset 24 + 8
            let entry = (cmd as usize + 24 + 8) as *mut u64;
            entry.write_volatile(addr);
            let entry_len = (cmd as usize + 24 + 8 + 8) as *mut u32;
            entry_len.write_volatile(length as u32);
            entry_len.add(1).write_volatile(0); // padding
        }
        self.send_command(0, 1, 24 + 8 + 16, 24); // header + body + 1 entry
        self.check_single_response("resource_attach_backing");
    }

    fn set_scanout(&mut self, scanout_id: u32, resource_id: u32, w: u32, h: u32) {
        let cmd = self.cmd_ptr();
        unsafe {
            core::ptr::write_bytes(cmd, 0, 512);
            let hdr = cmd as *mut GpuCtrlHeader;
            (*hdr).cmd_type = CMD_SET_SCANOUT;
            // Body: rect{x,y,w,h}, scanout_id, resource_id
            let body = (cmd as usize + 24) as *mut u32;
            body.write_volatile(0); // x
            body.add(1).write_volatile(0); // y
            body.add(2).write_volatile(w);
            body.add(3).write_volatile(h);
            body.add(4).write_volatile(scanout_id);
            body.add(5).write_volatile(resource_id);
        }
        self.send_command(0, 1, 24 + 24, 24);
        self.check_single_response("set_scanout");
    }

    fn transfer_rect(&mut self, resource_id: u32, x: u32, y: u32, w: u32, h: u32) {
        let cmd = self.cmd_ptr();
        let offset = (y as u64 * self.width as u64 + x as u64) * 4;
        unsafe {
            core::ptr::write_bytes(cmd, 0, 512);
            let hdr = cmd as *mut GpuCtrlHeader;
            (*hdr).cmd_type = CMD_TRANSFER_TO_HOST_2D;
            let body = (cmd as usize + 24) as *mut u32;
            body.write_volatile(x);
            body.add(1).write_volatile(y);
            body.add(2).write_volatile(w);
            body.add(3).write_volatile(h);
            let off_ptr = (cmd as usize + 24 + 16) as *mut u64;
            off_ptr.write_volatile(offset);
            let res_ptr = (cmd as usize + 24 + 24) as *mut u32;
            res_ptr.write_volatile(resource_id);
            res_ptr.add(1).write_volatile(0); // padding
        }
        self.send_command(0, 1, 24 + 32, 24);
        self.check_single_response("transfer_to_host_2d");
    }

    fn flush_rect(&mut self, resource_id: u32, x: u32, y: u32, w: u32, h: u32) {
        let cmd = self.cmd_ptr();
        unsafe {
            core::ptr::write_bytes(cmd, 0, 512);
            let hdr = cmd as *mut GpuCtrlHeader;
            (*hdr).cmd_type = CMD_RESOURCE_FLUSH;
            let body = (cmd as usize + 24) as *mut u32;
            body.write_volatile(x);
            body.add(1).write_volatile(y);
            body.add(2).write_volatile(w);
            body.add(3).write_volatile(h);
            body.add(4).write_volatile(resource_id);
            body.add(5).write_volatile(0); // padding
        }
        self.send_command(0, 1, 24 + 24, 24);
        self.check_single_response("resource_flush");
    }

    // ========================================================================
    // Virtqueue helpers
    // ========================================================================

    /// Post a single command and wait for completion.
    fn send_command(&mut self, desc0: u16, desc1: u16, cmd_len: u32, resp_len: u32) {
        self.setup_desc_pair(desc0, desc1, cmd_len, resp_len);
        self.push_avail(desc0);
        dmb_oshst();
        write32(self.mmio_base, QUEUE_NOTIFY, 0);
        self.poll_used(1);
    }

    /// Post a transfer+flush as two chains and wait for both (used by present_rect).
    fn post_transfer(&mut self, desc0: u16, desc1: u16, x: u32, y: u32, w: u32, h: u32) {
        let cmd = self.cmd_ptr();
        let offset = (y as u64 * self.width as u64 + x as u64) * 4;
        unsafe {
            core::ptr::write_bytes(cmd, 0, 128); // clear command area
            let hdr = cmd as *mut GpuCtrlHeader;
            (*hdr).cmd_type = CMD_TRANSFER_TO_HOST_2D;
            let body = (cmd as usize + 24) as *mut u32;
            body.write_volatile(x);
            body.add(1).write_volatile(y);
            body.add(2).write_volatile(w);
            body.add(3).write_volatile(h);
            let off_ptr = (cmd as usize + 24 + 16) as *mut u64;
            off_ptr.write_volatile(offset);
            let res_ptr = (cmd as usize + 24 + 24) as *mut u32;
            res_ptr.write_volatile(1); // resource_id
        }
        self.setup_desc_pair(desc0, desc1, 24 + 32, 24);
        self.push_avail(desc0);
    }

    fn post_flush(&mut self, desc0: u16, desc1: u16, x: u32, y: u32, w: u32, h: u32) {
        // Use a separate area in the command buffer for the flush command
        let cmd2 = (self.queue_page + CMD_OFFSET as u64 + 128) as *mut u8;
        let resp2 = (self.queue_page + RESP_OFFSET as u64 + 64) as *mut u8;
        unsafe {
            core::ptr::write_bytes(cmd2, 0, 128);
            let hdr = cmd2 as *mut GpuCtrlHeader;
            (*hdr).cmd_type = CMD_RESOURCE_FLUSH;
            let body = (cmd2 as usize + 24) as *mut u32;
            body.write_volatile(x);
            body.add(1).write_volatile(y);
            body.add(2).write_volatile(w);
            body.add(3).write_volatile(h);
            body.add(4).write_volatile(1); // resource_id
        }

        // Set up descriptors pointing to cmd2/resp2
        let desc_base = (self.queue_page + DESC_OFFSET as u64) as *mut u8;
        unsafe {
            let d0 = desc_base.add(desc0 as usize * 16);
            (d0 as *mut u64).write_volatile(cmd2 as u64); // addr
            (d0.add(8) as *mut u32).write_volatile(24 + 24); // len
            (d0.add(12) as *mut u16).write_volatile(VRING_DESC_F_NEXT); // flags
            (d0.add(14) as *mut u16).write_volatile(desc1); // next

            let d1 = desc_base.add(desc1 as usize * 16);
            (d1 as *mut u64).write_volatile(resp2 as u64); // addr
            (d1.add(8) as *mut u32).write_volatile(24); // len
            (d1.add(12) as *mut u16).write_volatile(VRING_DESC_F_WRITE); // flags
            (d1.add(14) as *mut u16).write_volatile(0); // next (unused)
        }
        self.push_avail(desc0);
    }

    fn setup_desc_pair(&mut self, d0: u16, d1: u16, cmd_len: u32, resp_len: u32) {
        let desc_base = (self.queue_page + DESC_OFFSET as u64) as *mut u8;
        let cmd_phys = self.queue_page + CMD_OFFSET as u64;
        let resp_phys = self.queue_page + RESP_OFFSET as u64;

        unsafe {
            // Clear response area
            core::ptr::write_bytes(resp_phys as *mut u8, 0, resp_len as usize);

            // Descriptor 0: command (readable)
            let p0 = desc_base.add(d0 as usize * 16);
            (p0 as *mut u64).write_volatile(cmd_phys);
            (p0.add(8) as *mut u32).write_volatile(cmd_len);
            (p0.add(12) as *mut u16).write_volatile(VRING_DESC_F_NEXT);
            (p0.add(14) as *mut u16).write_volatile(d1);

            // Descriptor 1: response (writable)
            let p1 = desc_base.add(d1 as usize * 16);
            (p1 as *mut u64).write_volatile(resp_phys);
            (p1.add(8) as *mut u32).write_volatile(resp_len);
            (p1.add(12) as *mut u16).write_volatile(VRING_DESC_F_WRITE);
            (p1.add(14) as *mut u16).write_volatile(0);
        }
    }

    fn push_avail(&mut self, desc_idx: u16) {
        let avail_base = (self.queue_page + AVAIL_OFFSET as u64) as *mut u8;
        let ring_idx = self.avail_idx % QUEUE_SIZE as u16;
        unsafe {
            // avail.ring[ring_idx] = desc_idx
            let ring_entry = avail_base.add(4 + ring_idx as usize * 2) as *mut u16;
            ring_entry.write_volatile(desc_idx);
        }
        dmb_oshst();
        self.avail_idx = self.avail_idx.wrapping_add(1);
        unsafe {
            // avail.idx = self.avail_idx
            let idx_ptr = avail_base.add(2) as *mut u16;
            idx_ptr.write_volatile(self.avail_idx);
        }
        dmb_oshst();
    }

    fn poll_used(&mut self, count: u16) {
        let used_base = (self.queue_page + USED_OFFSET as u64) as *mut u8;
        let target = self.last_used_idx.wrapping_add(count);

        for attempt in 0..1_000_000u32 {
            let used_idx = unsafe { core::ptr::read_volatile(used_base.add(2) as *const u16) };
            if used_idx == target {
                self.last_used_idx = target;
                return;
            }
            if attempt % 100_000 == 99_999 {
                core::hint::spin_loop();
            }
        }
        panic!(
            "virtio-GPU: device not responding (last_used={}, target={})",
            self.last_used_idx, target
        );
    }

    fn check_single_response(&self, cmd_name: &str) {
        let resp = self.resp_ptr();
        let resp_type = unsafe { core::ptr::read_volatile(resp as *const u32) };
        if resp_type != RESP_OK_NODATA {
            panic!("virtio-GPU: {} failed ({:#x})", cmd_name, resp_type);
        }
    }

    fn check_response(&self, idx: u32, cmd_name: &str) {
        // idx 0 = primary response at RESP_OFFSET
        // idx 1 = secondary response at RESP_OFFSET + 64
        let resp = if idx == 0 {
            self.resp_ptr()
        } else {
            (self.queue_page + RESP_OFFSET as u64 + 64) as *mut u8
        };
        let resp_type = unsafe { core::ptr::read_volatile(resp as *const u32) };
        if resp_type != RESP_OK_NODATA {
            panic!("virtio-GPU: {} failed ({:#x})", cmd_name, resp_type);
        }
    }

    fn cmd_ptr(&self) -> *mut u8 {
        (self.queue_page + CMD_OFFSET as u64) as *mut u8
    }

    fn resp_ptr(&self) -> *mut u8 {
        (self.queue_page + RESP_OFFSET as u64) as *mut u8
    }
}

// ============================================================================
// Bus probe
// ============================================================================

fn probe_gpu() -> Option<usize> {
    const MMIO_BASE: usize = 0x0a00_0000;
    const MMIO_STRIDE: usize = 0x200;
    const VIRTIO_MAGIC: u32 = 0x7472_6976; // "virt"
    const GPU_DEVICE_ID: u32 = 16;

    for slot in 0..32 {
        let base = MMIO_BASE + slot * MMIO_STRIDE;
        let magic = read32(base, MAGIC_VALUE);
        if magic != VIRTIO_MAGIC {
            continue;
        }
        let device_id = read32(base, DEVICE_ID);
        if device_id == GPU_DEVICE_ID {
            return Some(base);
        }
    }
    None
}

// ============================================================================
// Cache maintenance
// ============================================================================

/// Clean data cache for a range of physical addresses.
/// Apple Silicon cache lines are 64 bytes.
fn clean_cache_range(start: u64, len: u64) {
    if len == 0 {
        return;
    }
    let mut addr = start & !63;
    let end = start + len;
    while addr < end {
        unsafe {
            core::arch::asm!("dc civac, {}", in(reg) addr, options(nomem));
        }
        addr += 64;
    }
    unsafe {
        core::arch::asm!("dsb ish", options(nomem, nostack));
    }
}
