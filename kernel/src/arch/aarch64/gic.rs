/// Interrupt controller driver: GICv2 or GICv3, as the board says.
///
/// QEMU virt under HVF has a GICv3 (QEMU 11 refuses GICv2 there); the Pi 4
/// has a GIC-400, which is GICv2. Both paths do the same job: enable the
/// virtual timer PPI, accept every priority, and acknowledge and end
/// interrupts for the scheduler tick.
use super::board::{self, Gic};
use crate::serial::serial_println;

/// Virtual timer PPI interrupt ID (used under HVF).
pub const TIMER_INTID: u32 = 27;

// GICD — distributor (both versions)
const GICD_CTLR: usize = 0x000;
const GICD_ISENABLER0: usize = 0x100;
const GICD_IPRIORITYR: usize = 0x400;

// GICC — CPU interface (GICv2, memory-mapped)
const GICC_CTLR: usize = 0x00;
const GICC_PMR: usize = 0x04;
const GICC_IAR: usize = 0x0C;
const GICC_EOIR: usize = 0x10;

// GICR — redistributor (GICv3). SGIs and PPIs live in its second 64 KiB frame.
const GICR_WAKER: usize = 0x0014;
const GICR_SGI_BASE: usize = 0x1_0000;
const GICR_IGROUPR0: usize = GICR_SGI_BASE + 0x080;
const GICR_ISENABLER0: usize = GICR_SGI_BASE + 0x100;
const GICR_IPRIORITYR: usize = GICR_SGI_BASE + 0x400;

unsafe fn mmio_read(addr: usize) -> u32 {
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

unsafe fn mmio_write(addr: usize, val: u32) {
    unsafe { core::ptr::write_volatile(addr as *mut u32, val) }
}

/// Initialise the board's GIC for the timer interrupt.
///
/// # Safety
/// Must be called once during early boot with interrupts disabled.
pub unsafe fn init() {
    match board::GIC {
        Gic::V2 { gicd, gicc } => unsafe { init_v2(gicd, gicc) },
        Gic::V3 { gicd, gicr } => unsafe { init_v3(gicd, gicr) },
    }
}

unsafe fn init_v2(gicd: usize, gicc: usize) {
    unsafe {
        // Distributor: disable while configuring, enable the timer PPI at
        // priority 0 (highest), then enable Group 0 and Group 1.
        mmio_write(gicd + GICD_CTLR, 0);
        dsb_sy();
        mmio_write(gicd + GICD_ISENABLER0, 1 << TIMER_INTID);
        set_priority_zero(gicd + GICD_IPRIORITYR);
        mmio_write(gicd + GICD_CTLR, 0x3);
        dsb_sy();

        // CPU interface: accept every priority, enable both groups.
        mmio_write(gicc + GICC_PMR, 0xFF);
        mmio_write(gicc + GICC_CTLR, 0x3);
        dsb_sy();
    }
    serial_println!("  GICv2 initialised (timer PPI {} enabled)", TIMER_INTID);
}

unsafe fn init_v3(gicd: usize, gicr: usize) {
    unsafe {
        // Distributor: affinity routing on, both groups enabled. Bits 0, 1
        // and 4 cover both the single- and two-security-state layouts.
        mmio_write(gicd + GICD_CTLR, (1 << 4) | (1 << 1) | 1);
        dsb_sy();

        // Wake this CPU's redistributor: clear ProcessorSleep, then wait for
        // ChildrenAsleep to clear.
        let waker = mmio_read(gicr + GICR_WAKER);
        mmio_write(gicr + GICR_WAKER, waker & !(1 << 1));
        while mmio_read(gicr + GICR_WAKER) & (1 << 2) != 0 {
            core::hint::spin_loop();
        }

        // Timer PPI: Group 1 (delivered as an IRQ), priority 0, enabled.
        let group = mmio_read(gicr + GICR_IGROUPR0);
        mmio_write(gicr + GICR_IGROUPR0, group | (1 << TIMER_INTID));
        set_priority_zero(gicr + GICR_IPRIORITYR);
        mmio_write(gicr + GICR_ISENABLER0, 1 << TIMER_INTID);
        dsb_sy();

        // CPU interface (system registers): enable them, accept every
        // priority, enable Group 1.
        let sre: u64;
        core::arch::asm!("mrs {}, S3_0_C12_C12_5", out(reg) sre, options(nomem, nostack)); // ICC_SRE_EL1
        core::arch::asm!("msr S3_0_C12_C12_5, {}", in(reg) sre | 1, options(nomem, nostack));
        core::arch::asm!("isb", options(nomem, nostack));
        core::arch::asm!("msr S3_0_C4_C6_0, {}", in(reg) 0xFFu64, options(nomem, nostack)); // ICC_PMR_EL1
        core::arch::asm!("msr S3_0_C12_C12_7, {}", in(reg) 1u64, options(nomem, nostack)); // ICC_IGRPEN1_EL1
        core::arch::asm!("isb", options(nomem, nostack));
    }
    serial_println!("  GICv3 initialised (timer PPI {} enabled)", TIMER_INTID);
}

/// Set the timer PPI's priority byte to 0 in a priority register bank.
unsafe fn set_priority_zero(ipriorityr: usize) {
    let reg = ipriorityr + (TIMER_INTID as usize & !3);
    let shift = (TIMER_INTID % 4) * 8;
    unsafe {
        let val = mmio_read(reg);
        mmio_write(reg, val & !(0xFF << shift));
    }
}

/// Acknowledge an interrupt — returns the INTID.
#[inline]
pub fn acknowledge() -> u32 {
    match board::GIC {
        Gic::V2 { gicc, .. } => unsafe { mmio_read(gicc + GICC_IAR) & 0x3FF },
        Gic::V3 { .. } => {
            let iar: u64;
            unsafe {
                core::arch::asm!("mrs {}, S3_0_C12_C12_0", out(reg) iar, options(nomem, nostack)); // ICC_IAR1_EL1
            }
            (iar & 0xFF_FFFF) as u32
        }
    }
}

/// Signal end-of-interrupt for the given INTID.
#[inline]
pub fn end_of_interrupt(intid: u32) {
    match board::GIC {
        Gic::V2 { gicc, .. } => unsafe { mmio_write(gicc + GICC_EOIR, intid) },
        Gic::V3 { .. } => unsafe {
            core::arch::asm!("msr S3_0_C12_C12_1, {}", in(reg) intid as u64, options(nomem, nostack)); // ICC_EOIR1_EL1
        },
    }
}

#[inline(always)]
fn dsb_sy() {
    unsafe { core::arch::asm!("dsb sy", options(nomem, nostack)) };
}
