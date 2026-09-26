/// aarch64 page tables: the firmware's table, which the kernel keeps.
///
/// UEFI's identity map stays the kernel's map, EL1-only, in every address
/// space (see `addrspace`, which builds each EL0 task's own table from it).
/// `init` sets up the system registers for that: WXN off, PAN on, and the
/// ASID taken from TTBR0.
///
/// `make_executable` edits the firmware's leaf entries in place so built-in
/// services loaded at EL1 can run. The architecture doesn't require
/// break-before-make for permission-only changes, but the TLB may still hold
/// the old permissions, so every batch of edits ends with a TLB invalidation
/// (`flush_tlb`).
use crate::serial::serial_println;

const VALID: u64 = 1 << 0;
const TABLE: u64 = 1 << 1;
const PXN: u64 = 1 << 53;
const UXN: u64 = 1 << 54;
const PXN_TABLE: u64 = 1 << 59;
const UXN_TABLE: u64 = 1 << 60;
const ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;

static mut TTBR0_ROOT: u64 = 0;
static mut START_LEVEL: u32 = 1;

/// Turn WXN off and PAN on, clear TCR.A1, and hand the firmware's table to
/// `addrspace`.
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

    // WXN off (writable pages stay executable where the firmware said so);
    // SPAN off, so PAN is set on every exception entry to EL1.
    let mut sctlr: u64;
    unsafe {
        core::arch::asm!("mrs {}, SCTLR_EL1", out(reg) sctlr, options(nomem, nostack));
    }
    sctlr &= !(1 << 19); // WXN=0
    sctlr &= !(1 << 23); // SPAN=0
    unsafe {
        core::arch::asm!("msr SCTLR_EL1, {}", in(reg) sctlr, options(nomem, nostack));
        core::arch::asm!("isb", options(nomem, nostack));
        // PSTATE.PAN = 1 ("msr PAN, #1", encoded because the assembler may not
        // know PAN): the kernel faults if it ever touches EL0 memory directly.
        core::arch::asm!(".inst 0xd500419f", options(nomem, nostack));
        core::arch::asm!("isb", options(nomem, nostack));
    }

    // TCR_EL1.A1 = 0: the ASID comes from TTBR0, where each task's lives.
    let tcr = tcr & !(1 << 22);
    unsafe {
        core::arch::asm!("msr TCR_EL1, {}", in(reg) tcr, options(nomem, nostack));
        core::arch::asm!("isb", options(nomem, nostack));
    }

    let ram_va = core::ptr::addr_of!(TTBR0_ROOT) as u64;
    if let Err(reason) = super::addrspace::init(ttbr0 & ADDR_MASK, t0sz, ram_va) {
        panic!("paging: {reason}");
    }

    serial_println!(
        "  Paging: T0SZ={}, WXN off, PAN on, user window {:#x}",
        t0sz,
        freshos_abi::USER_BASE
    );
    ttbr0
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

    flush_tlb();
    unsafe {
        core::arch::asm!("ic iallu", options(nomem, nostack));
        core::arch::asm!("dsb ish", options(nomem, nostack));
        core::arch::asm!("isb", options(nomem, nostack));
    }
}

/// No-op — all tasks share UEFI's page tables.
#[inline]
pub fn switch_ttbr0(_: u64) {}

// ---------------------------------------------------------------------------
// Page table walker — find and patch a single leaf entry
// ---------------------------------------------------------------------------

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

/// Make page-table edits visible: order the writes before the invalidation,
/// drop every EL1&0 translation, then wait for it to complete.
///
/// Older QEMU hung the guest on any `tlbi` under HVF; QEMU 11 runs it and it
/// invalidates correctly (verified 2026-09-25: a remapped page kept its stale
/// translation until `tlbi`).
fn flush_tlb() {
    unsafe {
        core::arch::asm!("dsb ishst", "tlbi vmalle1is", "dsb ish", "isb", options(nostack));
    }
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
