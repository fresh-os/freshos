/// aarch64 page tables: the firmware's table, which the kernel keeps.
///
/// UEFI's identity map stays the kernel's map, EL1-only, in every address
/// space (see `addrspace`, which builds each EL0 task's own table from it).
/// `init` sets up the system registers for that: WXN off, PAN on, the ASID
/// taken from TTBR0, and every EL0-facing control set to a known value
/// rather than whatever the firmware left.
use crate::serial::serial_println;

const ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;

static mut TTBR0_ROOT: u64 = 0;
/// Whether `init` turned PAN on (the CPU has FEAT_PAN).
static PAN_ON: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Whether PAN is on: an EL1 access to EL0 memory then faults.
pub fn pan_enabled() -> bool {
    PAN_ON.load(core::sync::atomic::Ordering::SeqCst)
}

/// Turn WXN off and PAN on (where the CPU has it), deny EL0 the system
/// registers and instructions it doesn't need, clear TCR.A1, flush the TLB,
/// and hand the firmware's table to `addrspace`.
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
    }

    // PAN (FEAT_PAN, ARMv8.1) makes the kernel fault if it ever touches EL0
    // memory directly. The Pi 4's Cortex-A72 is ARMv8.0 and lacks it: there,
    // `msr PAN` is UNDEFINED and SCTLR.SPAN is RES1, so neither may be
    // touched. On such a CPU the rule "the kernel never dereferences a user
    // VA" rests on the copy_*_user discipline alone (see `addrspace`).
    let mmfr1: u64;
    unsafe {
        core::arch::asm!("mrs {}, ID_AA64MMFR1_EL1", out(reg) mmfr1, options(nomem, nostack));
    }
    let has_pan = (mmfr1 >> 20) & 0xF != 0;

    // WXN off (writable pages stay executable where the firmware said so).
    // With PAN: SPAN off, so PAN is set on every exception entry to EL1.
    let mut sctlr: u64;
    unsafe {
        core::arch::asm!("mrs {}, SCTLR_EL1", out(reg) sctlr, options(nomem, nostack));
    }
    // The EL0-facing bits as the firmware left them, logged below: on the
    // Pi 4 they are unverified, and this shows what the kernel overrode.
    let el0_bits: u64 = (1 << 9) | (1 << 14) | (1 << 15) | (1 << 26);
    let firmware_sctlr_el0 = sctlr & el0_bits;
    sctlr &= !(1 << 19); // WXN=0
    if has_pan {
        sctlr &= !(1 << 23); // SPAN=0
    }
    // What EL0 may do directly. Nothing in freshos-rt or any userbin needs
    // these (checked in the source and the disassembly), so each is denied
    // and traps to EL1, where it is an EL0 fault:
    sctlr &= !(1 << 9); // UMA=0: no `msr DAIFSet/DAIFClr` at EL0 (no masking IRQs)
    sctlr &= !(1 << 14); // DZE=0: no `dc zva` at EL0
    sctlr &= !(1 << 15); // UCT=0: no CTR_EL0 reads at EL0
    sctlr &= !(1 << 26); // UCI=0: no cache maintenance (dc cvau/civac/cvac, ic ivau) at EL0
    unsafe {
        core::arch::asm!("msr SCTLR_EL1, {}", in(reg) sctlr, options(nomem, nostack));
        core::arch::asm!("isb", options(nomem, nostack));
    }
    if has_pan {
        // PSTATE.PAN = 1 ("msr PAN, #1", encoded because the assembler may not
        // know PAN).
        unsafe {
            core::arch::asm!(".inst 0xd500419f", options(nomem, nostack));
            core::arch::asm!("isb", options(nomem, nostack));
        }
        PAN_ON.store(true, core::sync::atomic::Ordering::SeqCst);
    } else {
        serial_println!("[paging] PAN unavailable (ARMv8.0); user-copy discipline only");
    }

    // TPIDRRO_EL0 is readable at EL0 and holds whatever the firmware left.
    // Nothing in FreshOS uses it, and EL0 can't write it, so one write here
    // keeps it zero for every task; it is not part of the saved frame.
    // (TPIDR_EL0, which EL0 can write, is saved per task in exception.s.)
    unsafe {
        core::arch::asm!("msr TPIDRRO_EL0, xzr", options(nomem, nostack));
    }

    // CNTKCTL_EL1 = 0: EL0 may not read the physical or virtual counter
    // (EL0PCTEN, EL0VCTEN) or touch either timer (EL0PTEN, EL0VTEN), and no
    // event stream (EVNTEN). freshos-rt reads time through the TIME_NS
    // syscall, so EL0 needs none of it.
    let firmware_cntkctl: u64;
    unsafe {
        core::arch::asm!("mrs {}, CNTKCTL_EL1", out(reg) firmware_cntkctl, options(nomem, nostack));
        core::arch::asm!("msr CNTKCTL_EL1, xzr", "isb", options(nomem, nostack));
    }
    serial_println!(
        "  EL0 controls: firmware left CNTKCTL_EL1={:#x}, SCTLR_EL1 UMA/DZE/UCT/UCI bits={:#x}; all now 0",
        firmware_cntkctl,
        firmware_sctlr_el0
    );

    // TCR_EL1.A1 = 0: the ASID comes from TTBR0, where each task's lives.
    let tcr = tcr & !(1 << 22);
    unsafe {
        core::arch::asm!("msr TCR_EL1, {}", in(reg) tcr, options(nomem, nostack));
        core::arch::asm!("isb", options(nomem, nostack));
    }

    // Before the first user space exists: drop every TLB entry the firmware
    // may have left, under any ASID, so no stale translation can outlive the
    // switch to per-task ASIDs.
    unsafe {
        core::arch::asm!("dsb ishst", "tlbi vmalle1is", "dsb ish", "isb", options(nostack));
    }

    let ram_va = core::ptr::addr_of!(TTBR0_ROOT) as u64;
    if let Err(reason) = super::addrspace::init(ttbr0 & ADDR_MASK, t0sz, ram_va) {
        panic!("paging: {reason}");
    }

    serial_println!(
        "  Paging: T0SZ={}, WXN off, PAN {}, user window {:#x}",
        t0sz,
        if has_pan { "on" } else { "unavailable" },
        freshos_abi::USER_BASE
    );
    ttbr0
}

/// Make code just written through the data side at `[start, start + len)`
/// (a kernel VA: the identity map) visible to instruction fetch: clean the
/// D-cache to the point of unification by line, then invalidate the whole
/// I-cache. Apple cores are coherent (so HVF never needed this); the Pi 4's
/// Cortex-A72 is not, and would otherwise fetch stale instructions,
/// especially from a reused frame. `ic ialluis` rather than `ic ivau`, so
/// no I-cache alias of the frame can survive.
pub fn sync_icache(start: u64, len: u64) {
    if len == 0 {
        return;
    }
    // CTR_EL0.DminLine (bits 19:16): log2 of the smallest D-cache line, in words.
    let ctr: u64;
    unsafe { core::arch::asm!("mrs {}, CTR_EL0", out(reg) ctr, options(nomem, nostack)) };
    let line = 4u64 << ((ctr >> 16) & 0xF);
    let end = start + len;
    let mut addr = start & !(line - 1);
    while addr < end {
        unsafe { core::arch::asm!("dc cvau, {}", in(reg) addr, options(nostack)) };
        addr += line;
    }
    unsafe {
        core::arch::asm!("dsb ish", "ic ialluis", "dsb ish", "isb", options(nostack));
    }
}
