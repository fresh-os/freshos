/// Virtio-GPU 2D driver for x86_64 over PCI (Modern 1.0+).
///
/// This driver finds the Virtio-GPU device on the PCI bus, negotiates
/// features, sets up a command virtqueue, and provides dirty-rect
/// updates to the host.
use crate::arch::pci::{self, PciDevice};
use crate::frame_alloc;
use crate::framebuffer::Framebuffer;
use crate::serial::serial_println;

// ============================================================================
// PCI Virtio Capabilities
// ============================================================================

const PCI_CAP_VNDR: u8 = 0x09;

const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
const VIRTIO_PCI_CAP_ISR_CFG: u8 = 3;
const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;

#[derive(Debug, Clone, Copy)]
struct VirtioCap {
    cap_type: u8,
    bar: u8,
    offset: u32,
    length: u32,
    notify_off_multiplier: u32, // Only for NOTIFY_CFG
}

// ============================================================================
// Virtio 1.0 Common Config Structure (in a BAR)
// ============================================================================

#[repr(C)]
struct VirtioPciCommonCfg {
    device_feature_select: u32,
    device_feature: u32,
    driver_feature_select: u32,
    driver_feature: u32,
    config_msix_vector: u16,
    num_queues: u16,
    device_status: u8,
    config_generation: u8,
    queue_select: u16,
    queue_size: u16,
    queue_msix_vector: u16,
    queue_enable: u16,
    queue_notify_off: u16,
    queue_desc_lo: u32,
    queue_desc_hi: u32,
    queue_avail_lo: u32,
    queue_avail_hi: u32,
    queue_used_lo: u32,
    queue_used_hi: u32,
}

// ============================================================================
// Virtio-GPU command types (shared with aarch64)
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
// Virtqueue descriptor flags
// ============================================================================

const VRING_DESC_F_NEXT: u16 = 1;
const VRING_DESC_F_WRITE: u16 = 2;

// ============================================================================
// Memory layout within the single queue+cmd page (4 KiB)
// ============================================================================

const QUEUE_SIZE: usize = 16;
const DESC_OFFSET: usize = 0x000;
const AVAIL_OFFSET: usize = 0x100;
const USED_OFFSET: usize = 0x200;
const CMD_OFFSET: usize = 0x400;
const RESP_OFFSET: usize = 0x600;

#[repr(C)]
struct GpuCtrlHeader {
    cmd_type: u32,
    flags: u32,
    fence_id: u64,
    ctx_id: u32,
    padding: u32,
}

// ============================================================================
// VirtioGpu
// ============================================================================

pub struct VirtioGpu {
    common_cfg: usize, // Virtual address of common config structure
    device_cfg: usize, // Virtual address of device config area
    notify_base: usize,
    notify_off_multiplier: u32,
    queue_page: u64,    // Phys addr of 4 KiB queue+cmd page
    avail_idx: u16,     // running counter
    last_used_idx: u16, // last seen
    backing_phys: u64,
    width: u32,
    height: u32,
}

impl VirtioGpu {
    pub fn init() -> Option<(Self, Framebuffer)> {
        // 1. Find PCI device 1AF4:1050
        let dev = pci::find_device(0x1AF4, 0x1050)?;
        serial_println!(
            "  virtio-GPU (PCI) found at 00:{:02x}.{}",
            dev.slot,
            dev.func
        );

        // 2. Parse capabilities
        let mut caps = [VirtioCap {
            cap_type: 0,
            bar: 0,
            offset: 0,
            length: 0,
            notify_off_multiplier: 0,
        }; 5];
        // virtio_pci_cap layout (virtio 1.0 §4.1.4):
        //   u8  cap_vndr   (+0)   0x09 for vendor
        //   u8  cap_next   (+1)   next cap ptr
        //   u8  cap_len    (+2)
        //   u8  cfg_type   (+3)   COMMON/NOTIFY/ISR/DEVICE/PCI_CFG
        //   u8  bar        (+4)
        //   u8  pad[3]     (+5..+7)
        //   u32 offset     (+8)
        //   u32 length     (+12)
        //   u32 notify_off_multiplier (+16, NOTIFY_CFG only)
        let mut cap_ptr = dev.read_u8(0x34);
        while cap_ptr != 0 {
            let cap_id = dev.read_u8(cap_ptr.into());
            if cap_id == PCI_CAP_VNDR {
                let v_type = dev.read_u8((cap_ptr + 3).into());
                let v_bar = dev.read_u8((cap_ptr + 4).into());
                let v_off = dev.read_u32((cap_ptr + 8).into());
                let v_len_val = dev.read_u32((cap_ptr + 12).into());

                if v_type >= 1 && v_type <= 4 {
                    caps[v_type as usize] = VirtioCap {
                        cap_type: v_type,
                        bar: v_bar,
                        offset: v_off,
                        length: v_len_val,
                        notify_off_multiplier: if v_type == VIRTIO_PCI_CAP_NOTIFY_CFG {
                            dev.read_u32((cap_ptr + 16).into())
                        } else {
                            0
                        },
                    };
                }
            }
            cap_ptr = dev.read_u8((cap_ptr + 1).into());
        }

        // 3. Decode BARs — virtio-gpu-pci uses 64-bit memory BARs on q35 by
        // default, so we must combine two adjacent 32-bit registers.
        let get_addr = |cap: VirtioCap| -> usize {
            let bar_off = 0x10 + cap.bar * 4;
            let lo = dev.read_u32(bar_off.into());
            // BAR layout: bit 0 = type (0=mem, 1=io), bits 1-2 = locatable
            // (10b = 64-bit), bit 3 = prefetchable, bits 4-31 = base addr.
            let is_64 = (lo & 0b110) == 0b100;
            let base = (lo & 0xFFFF_FFF0) as u64;
            let full_base = if is_64 {
                let hi = dev.read_u32((bar_off + 4).into()) as u64;
                base | (hi << 32)
            } else {
                base
            };
            // The BAR may sit above the kernel's 4 GiB identity map. Map
            // the 1 GiB window containing it before any MMIO dereference.
            // (Safe to call repeatedly — subsequent calls are no-ops when
            // the entry already exists.)
            unsafe { crate::paging::map_mmio_1gib(full_base) };
            (full_base as usize) + cap.offset as usize
        };

        let common_cfg_addr = get_addr(caps[VIRTIO_PCI_CAP_COMMON_CFG as usize]);
        let device_cfg_addr = get_addr(caps[VIRTIO_PCI_CAP_DEVICE_CFG as usize]);
        let notify_cfg_addr = get_addr(caps[VIRTIO_PCI_CAP_NOTIFY_CFG as usize]);

        serial_println!(
            "  virtio-GPU caps: common={:#x}, device={:#x}, notify={:#x}",
            common_cfg_addr,
            device_cfg_addr,
            notify_cfg_addr
        );

        // 4. Reset + init
        let cfg = common_cfg_addr as *mut VirtioPciCommonCfg;
        unsafe {
            core::ptr::write_volatile(&mut (*cfg).device_status, 0); // reset
                                                                     // Spec §3.1.1: wait until the device reads back 0 before proceeding.
            let mut spin = 0u32;
            while core::ptr::read_volatile(&(*cfg).device_status) != 0 {
                spin += 1;
                if spin > 1_000_000 {
                    serial_println!("  virtio-GPU: reset never cleared");
                    return None;
                }
                core::hint::spin_loop();
            }
            core::ptr::write_volatile(&mut (*cfg).device_status, 1); // ACK
            core::ptr::write_volatile(&mut (*cfg).device_status, 1 | 2); // DRIVER

            // Acknowledge VIRTIO_F_VERSION_1 (feature bit 32) so the device
            // runs in modern (1.0+) mode, matching our config layout.
            core::ptr::write_volatile(&mut (*cfg).driver_feature_select, 0);
            core::ptr::write_volatile(&mut (*cfg).driver_feature, 0);
            core::ptr::write_volatile(&mut (*cfg).driver_feature_select, 1);
            core::ptr::write_volatile(&mut (*cfg).driver_feature, 1); // bit 32 = VERSION_1

            core::ptr::write_volatile(&mut (*cfg).device_status, 1 | 2 | 8); // FEATURES_OK
            if core::ptr::read_volatile(&(*cfg).device_status) & 8 == 0 {
                serial_println!("  virtio-GPU: FEATURES_OK rejected");
                return None;
            }

            // Queue setup
            core::ptr::write_volatile(&mut (*cfg).queue_select, 0);
            core::ptr::write_volatile(&mut (*cfg).queue_size, QUEUE_SIZE as u16);
        }

        let queue_page = frame_alloc::allocate_contiguous(1).expect("virtio queue");

        unsafe {
            core::ptr::write_bytes(queue_page as *mut u8, 0, 4096);

            core::ptr::write_volatile(
                &mut (*cfg).queue_desc_lo,
                (queue_page + DESC_OFFSET as u64) as u32,
            );
            core::ptr::write_volatile(
                &mut (*cfg).queue_desc_hi,
                ((queue_page + DESC_OFFSET as u64) >> 32) as u32,
            );
            core::ptr::write_volatile(
                &mut (*cfg).queue_avail_lo,
                (queue_page + AVAIL_OFFSET as u64) as u32,
            );
            core::ptr::write_volatile(
                &mut (*cfg).queue_avail_hi,
                ((queue_page + AVAIL_OFFSET as u64) >> 32) as u32,
            );
            core::ptr::write_volatile(
                &mut (*cfg).queue_used_lo,
                (queue_page + USED_OFFSET as u64) as u32,
            );
            core::ptr::write_volatile(
                &mut (*cfg).queue_used_hi,
                ((queue_page + USED_OFFSET as u64) >> 32) as u32,
            );
            core::ptr::write_volatile(&mut (*cfg).queue_enable, 1);

            core::ptr::write_volatile(&mut (*cfg).device_status, 1 | 2 | 8 | 4);
            // DRIVER_OK
        }

        let mut gpu = VirtioGpu {
            common_cfg: common_cfg_addr,
            device_cfg: device_cfg_addr,
            notify_base: notify_cfg_addr,
            notify_off_multiplier: caps[VIRTIO_PCI_CAP_NOTIFY_CFG as usize].notify_off_multiplier,
            queue_page: queue_page,
            avail_idx: 0,
            last_used_idx: 0,
            backing_phys: 0,
            width: 0,
            height: 0,
        };

        // 5. GPU Resources
        let (w, h) = gpu.get_display_info();
        gpu.width = w;
        gpu.height = h;
        serial_println!("  virtio-GPU: {}x{}", w, h);

        gpu.resource_create_2d(1, w, h);
        let backing_pages = (w as usize * h as usize * 4 + 4095) / 4096;
        let backing = frame_alloc::allocate_contiguous(backing_pages).expect("backing");
        unsafe { core::ptr::write_bytes(backing as *mut u8, 0, w as usize * h as usize * 4) };
        gpu.backing_phys = backing;

        gpu.resource_attach_backing(1, backing, w as u64 * h as u64 * 4);
        gpu.set_scanout(0, 1, w, h);

        gpu.transfer_rect(1, 0, 0, w, h);
        gpu.flush_rect(1, 0, 0, w, h);

        let fb = Framebuffer::new(backing as *mut u8, w as usize, h as usize, w as usize, true);
        Some((gpu, fb))
    }

    pub fn present_rect(&mut self, x: u32, y: u32, w: u32, h: u32) {
        // Transfer
        self.post_transfer(0, 1, x, y, w, h);
        // Flush
        self.post_flush(2, 3, x, y, w, h);

        self.notify(0);
        self.poll_used(2);
        self.check_response(0, "transfer");
        self.check_response(1, "flush");
    }

    pub fn present_full(&mut self) {
        self.present_rect(0, 0, self.width, self.height);
    }

    // ========================================================================
    // Implementation (mostly same as aarch64 but using PCI MMIO)
    // ========================================================================

    fn get_display_info(&mut self) -> (u32, u32) {
        let cmd = self.cmd_ptr();
        unsafe {
            core::ptr::write_bytes(cmd, 0, 512);
            let hdr = cmd as *mut GpuCtrlHeader;
            (*hdr).cmd_type = CMD_GET_DISPLAY_INFO;
        }
        self.send_command(0, 1, 24, 408);
        let resp = self.resp_ptr();
        let resp_type = unsafe { core::ptr::read_volatile(resp as *const u32) };
        if resp_type != RESP_OK_DISPLAY_INFO {
            return (1280, 800);
        }
        let w = unsafe { core::ptr::read_volatile((resp as usize + 24 + 8) as *const u32) };
        let h = unsafe { core::ptr::read_volatile((resp as usize + 24 + 12) as *const u32) };
        (w, h)
    }

    fn resource_create_2d(&mut self, resource_id: u32, w: u32, h: u32) {
        let cmd = self.cmd_ptr();
        unsafe {
            core::ptr::write_bytes(cmd, 0, 512);
            let hdr = cmd as *mut GpuCtrlHeader;
            (*hdr).cmd_type = CMD_RESOURCE_CREATE_2D;
            let body = (cmd as usize + 24) as *mut u32;
            body.write_volatile(resource_id);
            body.add(1).write_volatile(FORMAT_B8G8R8A8_UNORM);
            body.add(2).write_volatile(w);
            body.add(3).write_volatile(h);
        }
        self.send_command(0, 1, 24 + 16, 24);
        self.check_single_response("resource_create_2d");
    }

    fn resource_attach_backing(&mut self, resource_id: u32, addr: u64, len: u64) {
        let cmd = self.cmd_ptr();
        unsafe {
            core::ptr::write_bytes(cmd, 0, 512);
            let hdr = cmd as *mut GpuCtrlHeader;
            (*hdr).cmd_type = CMD_RESOURCE_ATTACH_BACKING;
            let body = (cmd as usize + 24) as *mut u32;
            body.write_volatile(resource_id);
            body.add(1).write_volatile(1); // 1 entry
            let entry = (cmd as usize + 24 + 8) as *mut u64;
            entry.write_volatile(addr);
            let entry_len = (cmd as usize + 24 + 8 + 8) as *mut u32;
            entry_len.write_volatile(len as u32);
        }
        self.send_command(0, 1, 24 + 8 + 16, 24);
        self.check_single_response("attach_backing");
    }

    fn set_scanout(&mut self, scanout_id: u32, resource_id: u32, w: u32, h: u32) {
        let cmd = self.cmd_ptr();
        unsafe {
            core::ptr::write_bytes(cmd, 0, 512);
            let hdr = cmd as *mut GpuCtrlHeader;
            (*hdr).cmd_type = CMD_SET_SCANOUT;
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
        }
        self.send_command(0, 1, 24 + 32, 24);
        self.check_single_response("transfer");
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
        }
        self.send_command(0, 1, 24 + 24, 24);
        self.check_single_response("flush");
    }

    // ========================================================================
    // Virtqueue Helpers
    // ========================================================================

    fn send_command(&mut self, d0: u16, d1: u16, cmd_len: u32, resp_len: u32) {
        self.setup_desc_pair(d0, d1, cmd_len, resp_len);
        self.push_avail(d0);
        self.notify(0);
        self.poll_used(1);
    }

    fn setup_desc_pair(&mut self, d0: u16, d1: u16, cmd_len: u32, resp_len: u32) {
        let desc_base = (self.queue_page + DESC_OFFSET as u64) as *mut u8;
        let cmd_phys = self.queue_page + CMD_OFFSET as u64;
        let resp_phys = self.queue_page + RESP_OFFSET as u64;
        unsafe {
            core::ptr::write_bytes(resp_phys as *mut u8, 0, resp_len as usize);
            let p0 = desc_base.add(d0 as usize * 16);
            (p0 as *mut u64).write_volatile(cmd_phys);
            (p0.add(8) as *mut u32).write_volatile(cmd_len);
            (p0.add(12) as *mut u16).write_volatile(VRING_DESC_F_NEXT);
            (p0.add(14) as *mut u16).write_volatile(d1);
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
            let entry = avail_base.add(4 + ring_idx as usize * 2) as *mut u16;
            entry.write_volatile(desc_idx);
            self.avail_idx = self.avail_idx.wrapping_add(1);
            let idx_ptr = avail_base.add(2) as *mut u16;
            idx_ptr.write_volatile(self.avail_idx);
        }
    }

    fn notify(&self, queue_idx: u16) {
        // Modern Virtio-PCI: notify_base + (queue_notify_off * notify_off_multiplier)
        let cfg = self.common_cfg as *const VirtioPciCommonCfg;
        unsafe {
            core::ptr::write_volatile(
                &mut (*(self.common_cfg as *mut VirtioPciCommonCfg)).queue_select,
                queue_idx,
            );
            let off = core::ptr::read_volatile(&(*cfg).queue_notify_off);
            let addr = self.notify_base + (off as u32 * self.notify_off_multiplier) as usize;
            core::ptr::write_volatile(addr as *mut u16, queue_idx);
        }
    }

    fn poll_used(&mut self, count: u16) {
        let used_base = (self.queue_page + USED_OFFSET as u64) as *mut u8;
        let target = self.last_used_idx.wrapping_add(count);
        for _ in 0..1_000_000 {
            let used_idx = unsafe { core::ptr::read_volatile(used_base.add(2) as *const u16) };
            if used_idx == target {
                self.last_used_idx = target;
                return;
            }
            core::hint::spin_loop();
        }
        panic!("virtio-GPU: timeout");
    }

    fn post_transfer(&mut self, d0: u16, d1: u16, x: u32, y: u32, w: u32, h: u32) {
        let cmd = self.cmd_ptr();
        let offset = (y as u64 * self.width as u64 + x as u64) * 4;
        unsafe {
            core::ptr::write_bytes(cmd, 0, 128);
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
            res_ptr.write_volatile(1);
        }
        self.setup_desc_pair(d0, d1, 24 + 32, 24);
        self.push_avail(d0);
    }

    fn post_flush(&mut self, d0: u16, d1: u16, x: u32, y: u32, w: u32, h: u32) {
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
            body.add(4).write_volatile(1);

            let desc_base = (self.queue_page + DESC_OFFSET as u64) as *mut u8;
            let p0 = desc_base.add(d0 as usize * 16);
            (p0 as *mut u64).write_volatile(cmd2 as u64);
            (p0.add(8) as *mut u32).write_volatile(24 + 24);
            (p0.add(12) as *mut u16).write_volatile(VRING_DESC_F_NEXT);
            (p0.add(14) as *mut u16).write_volatile(d1);
            let p1 = desc_base.add(d1 as usize * 16);
            (p1 as *mut u64).write_volatile(resp2 as u64);
            (p1.add(8) as *mut u32).write_volatile(24);
            (p1.add(12) as *mut u16).write_volatile(VRING_DESC_F_WRITE);
            (p1.add(14) as *mut u16).write_volatile(0);
        }
        self.push_avail(d0);
    }

    fn check_single_response(&self, _name: &str) {
        let resp = self.resp_ptr();
        let resp_type = unsafe { core::ptr::read_volatile(resp as *const u32) };
        if resp_type != RESP_OK_NODATA {
            panic!("virtio-GPU: fail");
        }
    }

    fn check_response(&self, idx: u32, _name: &str) {
        let resp = if idx == 0 {
            self.resp_ptr()
        } else {
            (self.queue_page + RESP_OFFSET as u64 + 64) as *mut u8
        };
        let resp_type = unsafe { core::ptr::read_volatile(resp as *const u32) };
        if resp_type != RESP_OK_NODATA {
            panic!("virtio-GPU: fail");
        }
    }

    fn cmd_ptr(&self) -> *mut u8 {
        (self.queue_page + CMD_OFFSET as u64) as *mut u8
    }
    fn resp_ptr(&self) -> *mut u8 {
        (self.queue_page + RESP_OFFSET as u64) as *mut u8
    }
}
