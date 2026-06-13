/// PCI Express Enhanced Configuration Access Mechanism (ECAM) for x86_64.
///
/// Instead of legacy I/O ports 0xCF8/0xCFC, we map the entire PCI
/// configuration space into the physical memory map.
use crate::serial::serial_println;
use core::sync::atomic::{AtomicU64, Ordering};

static ECAM_BASE: AtomicU64 = AtomicU64::new(0);

#[repr(C, packed)]
struct McfgHeader {
    signature: [u8; 4],
    length: u32,
    revision: u8,
    checksum: u8,
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    creator_id: u32,
    creator_revision: u32,
    reserved: u64,
}

#[repr(C, packed)]
struct McfgEntry {
    base_addr: u64,
    pci_segment: u16,
    start_bus: u8,
    end_bus: u8,
    reserved: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct PciDevice {
    pub bus: u8,
    pub slot: u8,
    pub func: u8,
    pub vendor_id: u16,
    pub device_id: u16,
}

impl PciDevice {
    /// Get the memory-mapped address for a configuration register.
    fn config_addr(&self, offset: u16) -> usize {
        let base = ECAM_BASE.load(Ordering::SeqCst) as usize;
        if base == 0 {
            panic!("PCI: ECAM not initialised");
        }
        base | ((self.bus as usize) << 20)
            | ((self.slot as usize) << 15)
            | ((self.func as usize) << 12)
            | (offset as usize)
    }

    pub fn read_u32(&self, offset: u16) -> u32 {
        let addr = self.config_addr(offset);
        // PCI capability walking passes arbitrary byte offsets, so the
        // pointer isn't always aligned to its element size. Use the
        // unaligned-read intrinsics; ECAM is strongly-ordered MMIO where
        // alignment only matters for Rust's safety contract, not the bus.
        unsafe { (addr as *const u32).read_unaligned() }
    }

    pub fn read_u16(&self, offset: u16) -> u16 {
        let addr = self.config_addr(offset);
        unsafe { (addr as *const u16).read_unaligned() }
    }

    pub fn read_u8(&self, offset: u16) -> u8 {
        let addr = self.config_addr(offset);
        unsafe { core::ptr::read_volatile(addr as *const u8) }
    }

    /// Get the BAR0 address for this device.
    pub fn get_bar0(&self) -> u32 {
        self.read_u32(0x10)
    }
}

pub fn init(mcfg_addr: u64) {
    let hdr = unsafe { &*(mcfg_addr as *const McfgHeader) };
    let entry_count = (hdr.length as usize - core::mem::size_of::<McfgHeader>())
        / core::mem::size_of::<McfgEntry>();
    let entry_ptr = (mcfg_addr + core::mem::size_of::<McfgHeader>() as u64) as *const McfgEntry;

    if entry_count > 0 {
        let entry = unsafe { &*entry_ptr };
        let base_addr = entry.base_addr;
        ECAM_BASE.store(base_addr, Ordering::SeqCst);
        serial_println!(
            "  PCI: ECAM enabled at {:#x} (buses {}-{})",
            base_addr,
            entry.start_bus,
            entry.end_bus
        );
    }
}

/// Scan for a specific vendor/device ID using ECAM.
pub fn find_device(vendor_id: u16, device_id: u16) -> Option<PciDevice> {
    if ECAM_BASE.load(Ordering::SeqCst) == 0 {
        return None;
    }

    for bus in 0..8 {
        // scan first 8 buses
        for slot in 0..32 {
            for func in 0..8 {
                let dev = PciDevice {
                    bus,
                    slot,
                    func,
                    vendor_id: 0,
                    device_id: 0,
                };
                let v = dev.read_u16(0x00);
                if v == 0xFFFF {
                    if func == 0 {
                        break;
                    }
                    continue;
                }
                let d = dev.read_u16(0x02);
                if v == vendor_id && d == device_id {
                    return Some(PciDevice {
                        bus,
                        slot,
                        func,
                        vendor_id: v,
                        device_id: d,
                    });
                }
            }
        }
    }
    None
}

pub fn scan_bus() {
    serial_println!("PCI Scan (ECAM):");
    for bus in 0..4 {
        for slot in 0..32 {
            for func in 0..8 {
                let dev = PciDevice {
                    bus,
                    slot,
                    func,
                    vendor_id: 0,
                    device_id: 0,
                };
                let v = dev.read_u16(0x00);
                if v == 0xFFFF {
                    if func == 0 {
                        break;
                    }
                    continue;
                }
                let d = dev.read_u16(0x02);
                serial_println!("  {:02x}:{:02x}.{} -> {:04x}:{:04x}", bus, slot, func, v, d);
            }
        }
    }
}
