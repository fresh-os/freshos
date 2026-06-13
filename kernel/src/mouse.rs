/// PS/2 mouse — initialization and IRQ 12 handler.
///
/// Like the keyboard, the kernel just reads raw bytes and sends them as
/// IPC messages. A userspace driver assembles 3-byte packets and computes
/// absolute cursor position.
use x86_64::structures::idt::InterruptStackFrame;

use crate::ipc;
use crate::pic;

pub const RAW_CHANNEL: u32 = 3;

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

fn wait_write() {
    for _ in 0..100_000 {
        if inb(0x64) & 0x02 == 0 {
            return;
        }
        core::hint::spin_loop();
    }
}

fn wait_read() {
    for _ in 0..100_000 {
        if inb(0x64) & 0x01 != 0 {
            return;
        }
        core::hint::spin_loop();
    }
}

fn mouse_cmd(byte: u8) {
    wait_write();
    outb(0x64, 0xD4); // next byte → auxiliary device
    wait_write();
    outb(0x60, byte);
    wait_read();
    let _ = inb(0x60); // ACK
}

/// Initialize the PS/2 mouse. Call during boot with interrupts disabled.
///
/// # Safety
/// Must be called once, after PIC init.
pub unsafe fn init() {
    // Enable auxiliary device
    wait_write();
    outb(0x64, 0xA8);

    // Enable aux interrupt in controller config
    wait_write();
    outb(0x64, 0x20); // read config
    wait_read();
    let config = inb(0x60);
    wait_write();
    outb(0x64, 0x60); // write config
    wait_write();
    outb(0x60, config | 0x02); // bit 1 = enable aux IRQ

    // Set defaults and enable streaming
    mouse_cmd(0xF6); // set defaults
    mouse_cmd(0xF4); // enable data reporting

    crate::serial::serial_println!("  PS/2 mouse initialized");
}

pub extern "x86-interrupt" fn irq_handler(_frame: InterruptStackFrame) {
    let byte: u8 = inb(0x60);

    let msg = ipc::Message {
        tag: ipc::MSG_MOUSE_RAW,
        sender: 0,
        len: 1,
        payload: [byte as u64, 0, 0, 0],
    };
    let _ = ipc::send(RAW_CHANNEL, &msg);

    pic::eoi(12);
}
