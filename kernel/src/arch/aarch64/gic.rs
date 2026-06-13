/// GICv2 driver for QEMU virt machine under HVF.
///
/// QEMU virt with HVF defaults to GICv2 (not GICv3). GICv2 has no ICC_*
/// system registers — the CPU interface is memory-mapped at GICC.
///
/// QEMU virt GICv2 memory map:
///   GICD: 0x0800_0000 (distributor)
///   GICC: 0x0801_0000 (CPU interface)
use crate::serial::serial_println;

// GICD — distributor
const GICD_BASE: usize = 0x0800_0000;
const GICD_CTLR: *mut u32 = GICD_BASE as *mut u32;
const GICD_ISENABLER0: *mut u32 = (GICD_BASE + 0x100) as *mut u32;

// GICC — CPU interface (memory-mapped, GICv2)
const GICC_BASE: usize = 0x0801_0000;
const GICC_CTLR: *mut u32 = GICC_BASE as *mut u32;
const GICC_PMR: *mut u32 = (GICC_BASE + 0x04) as *mut u32;
const GICC_IAR: *const u32 = (GICC_BASE + 0x0C) as *const u32;
const GICC_EOIR: *mut u32 = (GICC_BASE + 0x10) as *mut u32;

/// Virtual timer PPI interrupt ID (used under HVF).
pub const TIMER_INTID: u32 = 27;

unsafe fn mmio_read(addr: *const u32) -> u32 {
    unsafe { core::ptr::read_volatile(addr) }
}

unsafe fn mmio_write(addr: *mut u32, val: u32) {
    unsafe { core::ptr::write_volatile(addr, val) }
}

/// Initialise GICv2: distributor + CPU interface.
///
/// # Safety
/// Must be called once during early boot with interrupts disabled.
pub unsafe fn init() {
    // --- Distributor ---
    // Disable while configuring
    unsafe {
        mmio_write(GICD_CTLR, 0);
        dsb_sy();
    }

    // Enable virtual timer PPI (INTID 27) — PPIs are in ISENABLER0
    unsafe {
        mmio_write(GICD_ISENABLER0, 1 << TIMER_INTID);
        dsb_sy();
    }

    // Set timer priority to 0 (highest).
    // INTID 27: byte 27 in IPRIORITYR, register at offset 0x400 + 24,
    // byte 3 within that word.
    unsafe {
        let pri_reg = (GICD_BASE + 0x400 + (TIMER_INTID as usize & !3)) as *mut u32;
        let shift = (TIMER_INTID % 4) * 8;
        let mut val = mmio_read(pri_reg);
        val &= !(0xFF << shift); // priority 0
        mmio_write(pri_reg, val);
        dsb_sy();
    }

    // Enable the distributor (EnableGrp0 + EnableGrp1)
    unsafe {
        mmio_write(GICD_CTLR, 0x3);
        dsb_sy();
    }

    // --- CPU interface (GICC, memory-mapped) ---
    // Set priority mask to accept all (0xFF)
    unsafe {
        mmio_write(GICC_PMR, 0xFF);
        dsb_sy();
    }

    // Enable the CPU interface (EnableGrp0 + EnableGrp1)
    unsafe {
        mmio_write(GICC_CTLR, 0x3);
        dsb_sy();
    }

    serial_println!("  GICv2 initialised (timer PPI {} enabled)", TIMER_INTID);
}

/// Acknowledge an interrupt — returns the INTID.
#[inline]
pub fn acknowledge() -> u32 {
    let intid = unsafe { mmio_read(GICC_IAR) };
    intid & 0x3FF // bottom 10 bits are the INTID
}

/// Signal end-of-interrupt for the given INTID.
#[inline]
pub fn end_of_interrupt(intid: u32) {
    unsafe {
        mmio_write(GICC_EOIR, intid);
    }
}

#[inline(always)]
fn dsb_sy() {
    unsafe { core::arch::asm!("dsb sy", options(nomem, nostack)) };
}
