/// 8259 PIC — remap hardware interrupts to IDT vectors 32–47.
///
/// UEFI leaves the PIC in an undefined state. We reinitialise both chips
/// (master + slave) so IRQ 0 (PIT timer) arrives at vector 32, safely
/// above the CPU exception range (0–31).

pub const TIMER_VECTOR: u8 = 32; // IRQ 0 after remapping

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack));
    }
}

fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe {
        core::arch::asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack));
    }
    val
}

fn io_wait() {
    outb(0x80, 0); // write to unused port as delay
}

/// Reinitialise both PICs and mask all IRQs.
///
/// # Safety
/// Must be called once during boot.
pub unsafe fn init() {
    // ICW1: init + ICW4 needed
    outb(PIC1_CMD, 0x11);
    io_wait();
    outb(PIC2_CMD, 0x11);
    io_wait();

    // ICW2: vector offsets
    outb(PIC1_DATA, 32);
    io_wait();
    outb(PIC2_DATA, 40);
    io_wait();

    // ICW3: cascade wiring
    outb(PIC1_DATA, 4);
    io_wait(); // slave on IRQ 2
    outb(PIC2_DATA, 2);
    io_wait(); // cascade identity

    // ICW4: 8086 mode
    outb(PIC1_DATA, 0x01);
    io_wait();
    outb(PIC2_DATA, 0x01);
    io_wait();

    // Mask everything
    outb(PIC1_DATA, 0xFF);
    outb(PIC2_DATA, 0xFF);
}

/// Unmask a specific IRQ line (0–15).
pub fn unmask(irq: u8) {
    if irq < 8 {
        let mask = inb(PIC1_DATA);
        outb(PIC1_DATA, mask & !(1 << irq));
    } else {
        let mask = inb(PIC2_DATA);
        outb(PIC2_DATA, mask & !(1 << (irq - 8)));
    }
}

/// Send End-of-Interrupt for a given IRQ.
pub fn eoi(irq: u8) {
    if irq >= 8 {
        outb(PIC2_CMD, 0x20);
    }
    outb(PIC1_CMD, 0x20);
}
