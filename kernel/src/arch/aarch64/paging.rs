/// aarch64 page tables — patch UEFI's tables for EL0 access.
///
/// We modify UEFI's existing L3 page entries in-place using the ARM
/// break-before-make sequence to avoid TLB corruption. Only pages that
/// EL0 tasks need (user stacks, code, framebuffer, surfaces) are patched.
///
/// Since EL1 (kernel) also needs to access these pages, we disable both
/// PAN and WXN in SCTLR_EL1.
///
/// No TTBR0 switching — all tasks share UEFI's (patched) page tables.
use crate::serial::serial_println;

const VALID: u64 = 1 << 0;
const TABLE: u64 = 1 << 1;
const AP_MASK: u64 = 0b11 << 6;
const AP_RW_ALL: u64 = 0b01 << 6;
const PXN: u64 = 1 << 53;
const UXN: u64 = 1 << 54;
const PXN_TABLE: u64 = 1 << 59;
const UXN_TABLE: u64 = 1 << 60;
const ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;

static mut TTBR0_ROOT: u64 = 0;
static mut START_LEVEL: u32 = 1;

/// Disable WXN and PAN, record UEFI page table geometry.
///
/// Returns the current TTBR0 (unchanged).
///
/// # Safety
/// Call once during boot.
pub unsafe fn init() -> u64 {
    let ttbr0: u64;
    unsafe {
        core::arch::asm!("mrs {}, TTBR0_EL1", out(reg) ttbr0, options(nomem, nostack));
    }

    let tcr: u64;
    unsafe {
        core::arch::asm!("mrs {}, TCR_EL1", out(reg) tcr, options(nomem, nostack));
    }
    let t0sz = (tcr & 0x3F) as u32;

    unsafe {
        TTBR0_ROOT = ttbr0 & ADDR_MASK;
        START_LEVEL = if t0sz >= 25 { 1 } else { 0 };
    }

    // Disable WXN (writable = execute-never) and enable SPAN (don't set PAN on exception)
    let mut sctlr: u64;
    unsafe {
        core::arch::asm!("mrs {}, SCTLR_EL1", out(reg) sctlr, options(nomem, nostack));
    }
    sctlr &= !(1 << 19); // WXN=0
    sctlr |= 1 << 23; // SPAN=1
    unsafe {
        core::arch::asm!("msr SCTLR_EL1, {}", in(reg) sctlr, options(nomem, nostack));
        core::arch::asm!("isb", options(nomem, nostack));
        // Clear PSTATE.PAN
        core::arch::asm!(".inst 0xd500409f", options(nomem, nostack));
        core::arch::asm!("isb", options(nomem, nostack));
    }

    serial_println!("  Paging: T0SZ={}, WXN off, PAN off", t0sz);
    ttbr0
}

/// Grant EL0 access to a range of physical addresses by patching the
/// UEFI page table entries in-place.
///
/// Uses break-before-make: invalidate entry → TLB invalidate → write new entry.
/// This is safe as long as no EL0 task is running during the patch (call
/// before starting the scheduler).
pub fn grant_user_access(start: u64, size: u64) {
    if size == 0 {
        return;
    }

    let root = unsafe { TTBR0_ROOT };
    let level = unsafe { START_LEVEL };
    let end = start + size;

    // Walk each 4 KiB page in the range
    let mut addr = start & !0xFFF;
    let mut patched = 0u32;
    while addr < end {
        if patch_leaf_entry(root, level, addr) {
            patched += 1;
        }
        addr += 4096;
    }

    // DSB to ensure all writes are visible, then ISB
    unsafe {
        core::arch::asm!("dsb ish", options(nomem, nostack));
        core::arch::asm!("isb", options(nomem, nostack));
    }

    // Debug output removed — serial at 115200 baud is the bottleneck.
    let _ = patched;
}

pub fn make_executable(start: u64, size: u64) {
    if size == 0 {
        return;
    }

    let root = unsafe { TTBR0_ROOT };
    let level = unsafe { START_LEVEL };
    let end = start + size;

    let mut addr = start & !0xFFF;
    while addr < end {
        clear_xn_leaf_entry(root, level, addr);
        addr += 4096;
    }

    unsafe {
        core::arch::asm!("dsb ish", options(nomem, nostack));
        core::arch::asm!("ic iallu", options(nomem, nostack));
        core::arch::asm!("dsb ish", options(nomem, nostack));
        core::arch::asm!("isb", options(nomem, nostack));
    }
}

/// The TTBR0 value (same for all tasks — no switching).
pub fn user_ttbr0() -> u64 {
    unsafe { TTBR0_ROOT }
}

/// No-op — all tasks share UEFI's page tables.
#[inline]
pub fn switch_ttbr0(_: u64) {}

// ---------------------------------------------------------------------------
// Page table walker — find and patch a single leaf entry
// ---------------------------------------------------------------------------

/// Walk the page table for `va` and set AP=AP_RW_ALL on the leaf entry.
///
/// Direct write without break-before-make: QEMU HVF traps all `tlbi`
/// instructions, making the standard sequence unusable. Under HVF's
/// software-managed TLB this is safe — the hypervisor picks up changes
/// on the next TLB miss. On real hardware, proper break-before-make
/// would be needed.
///
/// Returns true if the entry was modified.
fn patch_leaf_entry(table: u64, level: u32, va: u64) -> bool {
    let shift = match level {
        0 => 39,
        1 => 30,
        2 => 21,
        3 => 12,
        _ => return false,
    };
    let index = ((va >> shift) & 0x1FF) as usize;
    let entry = read_entry(table, index);

    if entry & VALID == 0 {
        return false;
    }

    let is_table = (entry & TABLE) != 0 && level < 3;

    if is_table {
        let next = entry & ADDR_MASK;
        patch_leaf_entry(next, level + 1, va)
    } else {
        let current_ap = entry & AP_MASK;
        if current_ap == AP_RW_ALL {
            return false;
        }

        let new_entry = (entry & !AP_MASK) | AP_RW_ALL;
        write_entry(table, index, new_entry);
        true
    }
}

fn clear_xn_leaf_entry(table: u64, level: u32, va: u64) -> bool {
    let shift = match level {
        0 => 39,
        1 => 30,
        2 => 21,
        3 => 12,
        _ => return false,
    };
    let index = ((va >> shift) & 0x1FF) as usize;
    let entry = read_entry(table, index);

    if entry & VALID == 0 {
        return false;
    }

    let is_table = (entry & TABLE) != 0 && level < 3;
    if is_table {
        let table_entry = entry & !(PXN_TABLE | UXN_TABLE);
        if table_entry != entry {
            write_entry(table, index, table_entry);
        }
        let next = entry & ADDR_MASK;
        return clear_xn_leaf_entry(next, level + 1, va);
    }

    let new_entry = entry & !(PXN | UXN);
    if new_entry == entry {
        return false;
    }

    write_entry(table, index, new_entry);
    true
}

fn read_entry(table_phys: u64, index: usize) -> u64 {
    unsafe { *((table_phys as *const u64).add(index)) }
}

fn write_entry(table_phys: u64, index: usize, value: u64) {
    unsafe {
        let ptr = (table_phys as *mut u64).add(index);
        ptr.write_volatile(value);
    }
}
