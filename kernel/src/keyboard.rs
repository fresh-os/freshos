/// Keyboard IRQ handler (ring 0).
///
/// The kernel doesn't know what a keyboard is. This handler reads one byte
/// from port 0x60 (the PS/2 data register), wraps it in a typed message,
/// and drops it into an IPC channel. A userspace driver does the decoding.
///
/// This is the microkernel pattern: the kernel delivers interrupts as
/// messages. Drivers live in userspace.
use x86_64::structures::idt::InterruptStackFrame;

use crate::ipc;
use crate::pic;

/// IPC channel that receives raw scancodes from the keyboard IRQ.
/// Must be created before interrupts are enabled.
pub const RAW_CHANNEL: u32 = 0;

pub extern "x86-interrupt" fn irq_handler(_frame: InterruptStackFrame) {
    let scancode: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            out("al") scancode,
            in("dx") 0x60u16,
            options(nomem, nostack),
        );
    }

    // Timestamp the IRQ with nanosecond precision — this is the start
    // of the input-to-photon measurement.
    let irq_ns = crate::tsc::now_ns();

    let msg = ipc::Message {
        tag: ipc::MSG_IRQ,
        sender: 0,
        len: 2,
        payload: [scancode as u64, irq_ns, 0, 0],
    };

    // Best-effort send — if the channel is full, drop the scancode.
    // A real driver would handle backpressure.
    let _ = ipc::send(RAW_CHANNEL, &msg);

    pic::eoi(1);
}
