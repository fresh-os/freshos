/// Minimal ACPI parser to find the MCFG table for ECAM.
use crate::serial::serial_println;

#[repr(C, packed)]
struct Rsdp {
    signature: [u8; 8],
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,
    rsdt_addr: u32,
    // v2+
    length: u32,
    xsdt_addr: u64,
    extended_checksum: u8,
    reserved: [u8; 3],
}

#[repr(C, packed)]
struct TableHeader {
    signature: [u8; 4],
    length: u32,
    revision: u8,
    checksum: u8,
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    creator_id: u32,
    creator_revision: u32,
}

pub fn init(rsdp_addr: u64) -> Option<u64> {
    if rsdp_addr == 0 {
        return None;
    }

    let rsdp = unsafe { &*(rsdp_addr as *const Rsdp) };
    if &rsdp.signature != b"RSD PTR " {
        return None;
    }

    serial_println!("  ACPI: RSDP v{} found at {:#x}", rsdp.revision, rsdp_addr);

    // Prefer XSDT (v2+)
    if rsdp.revision >= 2 && rsdp.xsdt_addr != 0 {
        return find_in_xsdt(rsdp.xsdt_addr);
    } else {
        return find_in_rsdt(rsdp.rsdt_addr as u64);
    }
}

fn find_in_rsdt(rsdt_addr: u64) -> Option<u64> {
    let rsdt = unsafe { &*(rsdt_addr as *const TableHeader) };
    let entries = (rsdt.length as usize - core::mem::size_of::<TableHeader>()) / 4;
    let ptr = (rsdt_addr + core::mem::size_of::<TableHeader>() as u64) as *const u32;

    for i in 0..entries {
        let table_addr = unsafe { ptr.add(i).read_unaligned() } as u64;
        let hdr = unsafe { &*(table_addr as *const TableHeader) };
        if &hdr.signature == b"MCFG" {
            serial_println!("  ACPI: MCFG found in RSDT at {:#x}", table_addr);
            return Some(table_addr);
        }
    }
    None
}

fn find_in_xsdt(xsdt_addr: u64) -> Option<u64> {
    let xsdt = unsafe { &*(xsdt_addr as *const TableHeader) };
    let entries = (xsdt.length as usize - core::mem::size_of::<TableHeader>()) / 8;
    let ptr = (xsdt_addr + core::mem::size_of::<TableHeader>() as u64) as *const u64;

    for i in 0..entries {
        // XSDT entries follow a 36-byte header, so the u64 array is
        // 4-byte-aligned, not 8-byte. Use an unaligned read.
        let table_addr = unsafe { ptr.add(i).read_unaligned() };
        let hdr = unsafe { &*(table_addr as *const TableHeader) };
        if &hdr.signature == b"MCFG" {
            serial_println!("  ACPI: MCFG found in XSDT at {:#x}", table_addr);
            return Some(table_addr);
        }
    }
    None
}
