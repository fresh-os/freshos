/// Interrupt Descriptor Table — CPU exception handlers.
///
/// Without an IDT, any fault (page fault, invalid opcode, etc.) causes a
/// triple-fault and a CPU reset. We install handlers for the important
/// exceptions and log them to serial so QEMU's `-serial stdio` shows what
/// happened.
///
/// Hardware interrupts (timer, keyboard) come later once we configure the APIC.
///
/// # Safety
/// `init()` must be called exactly once, after `gdt::init()`.
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};
use x86_64::VirtAddr;

use crate::gdt;
use crate::serial::serial_println;

static mut IDT: InterruptDescriptorTable = InterruptDescriptorTable::new();

/// Register exception handlers and load the IDT.
///
/// # Safety
/// Must be called once during single-threaded boot, after `gdt::init()`.
pub unsafe fn init() {
    let idt = unsafe { &mut *core::ptr::addr_of_mut!(IDT) };

    idt.divide_error.set_handler_fn(divide_error);
    idt.debug.set_handler_fn(debug);
    idt.non_maskable_interrupt.set_handler_fn(nmi);
    idt.breakpoint.set_handler_fn(breakpoint);
    idt.overflow.set_handler_fn(overflow);
    idt.bound_range_exceeded.set_handler_fn(bound_range);
    idt.invalid_opcode.set_handler_fn(invalid_opcode);
    idt.device_not_available
        .set_handler_fn(device_not_available);

    unsafe {
        idt.double_fault
            .set_handler_fn(double_fault)
            .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
    }

    idt.invalid_tss.set_handler_fn(invalid_tss);
    idt.segment_not_present.set_handler_fn(segment_not_present);
    idt.stack_segment_fault.set_handler_fn(stack_segment);
    idt.general_protection_fault
        .set_handler_fn(general_protection);
    idt.page_fault.set_handler_fn(page_fault);
    idt.alignment_check.set_handler_fn(alignment_check);

    // Hardware interrupts (registered before PIC unmask, so safe)
    idt[33].set_handler_fn(crate::keyboard::irq_handler); // IRQ 1 = vector 33
    idt[44].set_handler_fn(crate::mouse::irq_handler); // IRQ 12 = vector 44

    idt.load();
}

/// Register a raw interrupt handler at the given vector (32–255).
/// The IDT is already loaded; changes take effect immediately.
///
/// # Safety
/// The handler must be a valid ISR that ends with `iretq`.
pub unsafe fn set_interrupt_handler(vector: u8, handler_addr: u64) {
    let idt = unsafe { &mut *core::ptr::addr_of_mut!(IDT) };
    unsafe { idt[vector].set_handler_addr(VirtAddr::new(handler_addr)) };
}

// ---------------------------------------------------------------------------
// Exception handlers
// ---------------------------------------------------------------------------

extern "x86-interrupt" fn divide_error(frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: divide error\n{:#?}", frame);
    halt();
}

extern "x86-interrupt" fn debug(frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: debug\n{:#?}", frame);
}

extern "x86-interrupt" fn nmi(frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: NMI\n{:#?}", frame);
}

extern "x86-interrupt" fn breakpoint(frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: breakpoint\n{:#?}", frame);
}

extern "x86-interrupt" fn overflow(frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: overflow\n{:#?}", frame);
    halt();
}

extern "x86-interrupt" fn bound_range(frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: bound range exceeded\n{:#?}", frame);
    halt();
}

extern "x86-interrupt" fn invalid_opcode(frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: invalid opcode\n{:#?}", frame);
    halt();
}

extern "x86-interrupt" fn device_not_available(frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: device not available\n{:#?}", frame);
    halt();
}

extern "x86-interrupt" fn double_fault(frame: InterruptStackFrame, error_code: u64) -> ! {
    serial_println!(
        "EXCEPTION: DOUBLE FAULT (error_code={:#x})\n{:#?}",
        error_code,
        frame
    );
    loop {
        unsafe { core::arch::asm!("cli; hlt") };
    }
}

extern "x86-interrupt" fn invalid_tss(frame: InterruptStackFrame, error_code: u64) {
    serial_println!(
        "EXCEPTION: invalid TSS (error_code={:#x})\n{:#?}",
        error_code,
        frame
    );
    halt();
}

extern "x86-interrupt" fn segment_not_present(frame: InterruptStackFrame, error_code: u64) {
    serial_println!(
        "EXCEPTION: segment not present (error_code={:#x})\n{:#?}",
        error_code,
        frame
    );
    halt();
}

extern "x86-interrupt" fn stack_segment(frame: InterruptStackFrame, error_code: u64) {
    serial_println!(
        "EXCEPTION: stack segment fault (error_code={:#x})\n{:#?}",
        error_code,
        frame
    );
    halt();
}

extern "x86-interrupt" fn general_protection(frame: InterruptStackFrame, error_code: u64) {
    serial_println!(
        "EXCEPTION: general protection fault (error_code={:#x})\n{:#?}",
        error_code,
        frame
    );
    halt();
}

extern "x86-interrupt" fn page_fault(frame: InterruptStackFrame, error_code: PageFaultErrorCode) {
    use x86_64::registers::control::Cr2;
    serial_println!(
        "EXCEPTION: page fault\n  accessed address: {:?}\n  error code: {:?}\n{:#?}",
        Cr2::read(),
        error_code,
        frame
    );
    halt();
}

extern "x86-interrupt" fn alignment_check(frame: InterruptStackFrame, error_code: u64) {
    serial_println!(
        "EXCEPTION: alignment check (error_code={:#x})\n{:#?}",
        error_code,
        frame
    );
    halt();
}

// ---------------------------------------------------------------------------

fn halt() -> ! {
    loop {
        unsafe { core::arch::asm!("cli; hlt") };
    }
}
