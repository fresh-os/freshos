/// Serial output — portable formatting layer.
///
/// The actual byte output (`write_byte`) is delegated to the arch module.
/// This file provides the `Serial` struct, `core::fmt::Write` impl, and
/// the `serial_println!` macro. All modules use this — it's the most
/// cross-cutting dependency in the kernel.
use core::fmt::{self, Write};

pub struct Serial;

impl Serial {
    /// Public single-byte write for the debug syscall.
    pub fn write_byte_raw(byte: u8) {
        crate::arch::serial_write_byte(byte);
    }
}

impl Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &byte in s.as_bytes() {
            if byte == b'\n' {
                crate::arch::serial_write_byte(b'\r');
            }
            crate::arch::serial_write_byte(byte);
        }
        Ok(())
    }
}

/// Write a formatted string to the serial port, followed by a newline.
macro_rules! serial_println {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let _ = writeln!($crate::serial::Serial, $($arg)*);
    }};
}

pub(crate) use serial_println;
