/// aarch64 exception vector table — install and unhandled-exception handler.
use crate::serial::serial_println;

// Pull in the assembly vector table
core::arch::global_asm!(include_str!("exception.s"));

unsafe extern "C" {
    /// The 2048-byte aligned vector table defined in exception.s.
    fn exception_vectors();
}

/// Install the exception vector table at VBAR_EL1.
///
/// # Safety
/// Must be called once during early boot, before enabling interrupts.
pub unsafe fn init() {
    let vectors = exception_vectors as *const () as u64;
    unsafe {
        core::arch::asm!(
            "msr VBAR_EL1, {v}",
            "isb",
            v = in(reg) vectors,
            options(nomem, nostack),
        );
    }
    serial_println!("  Exception vectors installed at {:#x}", vectors);
}

/// Called from exception.s for any unhandled exception.
/// Prints diagnostics and halts.
fn log_exception(esr: u64, elr: u64, far: u64) -> u64 {
    let ec = (esr >> 26) & 0x3F; // exception class
    let iss = esr & 0x1FF_FFFF; // instruction-specific syndrome

    serial_println!(
        "  ESR_EL1:  {:#018x} (EC={:#04x}, ISS={:#09x})",
        esr,
        ec,
        iss
    );
    serial_println!("  ELR_EL1:  {:#018x}", elr);
    serial_println!("  FAR_EL1:  {:#018x}", far);

    // Show active TTBR0 at time of fault
    let ttbr0: u64;
    unsafe { core::arch::asm!("mrs {}, TTBR0_EL1", out(reg) ttbr0, options(nomem, nostack)) };
    serial_println!("  TTBR0:    {:#018x}", ttbr0);

    match ec {
        0x15 => serial_println!("  Type: SVC from AArch64"),
        0x18 => serial_println!("  Type: MSR/MRS trap"),
        0x20 => serial_println!("  Type: Instruction abort (lower EL)"),
        0x21 => serial_println!("  Type: Instruction abort (same EL)"),
        0x24 => serial_println!("  Type: Data abort (lower EL)"),
        0x25 => serial_println!("  Type: Data abort (same EL)"),
        0x26 => serial_println!("  Type: SP alignment fault"),
        0x2C => serial_println!("  Type: FP/SIMD trap"),
        _ => serial_println!("  Type: unknown (EC={:#04x})", ec),
    }
    ec
}

#[unsafe(no_mangle)]
extern "C" fn exception_panic(esr: u64, elr: u64, far: u64) -> ! {
    // Disable interrupts immediately to prevent timer from interrupting our output
    super::interrupt_disable();

    serial_println!("*** EXCEPTION ***");
    log_exception(esr, elr, far);

    loop {
        super::interrupt_disable();
        super::halt();
    }
}

#[unsafe(no_mangle)]
extern "C" fn exception_current_sync(esr: u64, elr: u64, far: u64) -> ! {
    super::interrupt_disable();

    let task_id = super::current_task();
    if task_id == 0 {
        serial_println!("*** EXCEPTION ***");
        log_exception(esr, elr, far);
        loop {
            super::interrupt_disable();
            super::halt();
        }
    }

    serial_println!("*** TASK FAULT ***");
    serial_println!("  Task: {}", task_id);
    log_exception(esr, elr, far);
    serial_println!("  Action: terminate faulting task");
    super::context::terminate_current_with_reason(crate::init_abi::SERVICE_EXIT_FAULT)
}

#[unsafe(no_mangle)]
extern "C" fn exception_lower_sync(esr: u64, elr: u64, far: u64) -> ! {
    super::interrupt_disable();

    let task_id = super::current_task();
    if task_id == 0 {
        serial_println!("*** EXCEPTION ***");
        log_exception(esr, elr, far);
        loop {
            super::interrupt_disable();
            super::halt();
        }
    }

    serial_println!("*** EL0 TASK FAULT ***");
    serial_println!("  Task: {}", task_id);
    log_exception(esr, elr, far);
    serial_println!("  Action: terminate faulting task");
    super::context::terminate_current_with_reason(crate::init_abi::SERVICE_EXIT_FAULT)
}
