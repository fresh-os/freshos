//! What a FreshOS userbin links against: the entry point, a panic handler,
//! safe syscall wrappers and `log!`. Userbins have no allocator.
#![no_std]

mod syscall;

pub use freshos_abi::{self as abi, Error, ExitReason, Grant, Handle, Message, Rights, tag};
use freshos_abi::{SpawnRequest, sys};
use syscall::{check, svc};

/// What a service is started with: its granted handles (slots 0..count, in
/// the order init's table lists them) and its table argument.
#[derive(Clone, Copy)]
pub struct Startup {
    handles: u32,
    arg: u64,
}

impl Startup {
    #[doc(hidden)]
    pub fn __new(handles: u64, arg: u64) -> Self {
        Startup { handles: handles as u32, arg }
    }

    /// The `index`-th granted handle. Panics if it wasn't granted.
    pub fn handle(&self, index: u32) -> Handle {
        assert!(index < self.handles, "handle {index} was not granted");
        Handle(index)
    }

    pub fn handle_count(&self) -> u32 {
        self.handles
    }

    pub fn arg(&self) -> u64 {
        self.arg
    }
}

/// Declare the userbin's `main(Startup) -> !`.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn _start(handle_count: u64, arg: u64) -> ! {
            let main: fn($crate::Startup) -> ! = $main;
            main($crate::Startup::__new(handle_count, arg))
        }
    };
}

pub fn send(handle: Handle, message: &Message) -> Result<(), Error> {
    check(svc(sys::SEND, handle.0 as u64, message as *const Message as u64, 0)).map(|_| ())
}

/// Wait for a message.
pub fn recv(handle: Handle) -> Result<Message, Error> {
    recv_until(handle, 0)
}

/// Wait for a message until `deadline_ns` (from `time_ns`); `Timeout` if it passes.
/// A deadline of 0 waits forever.
pub fn recv_until(handle: Handle, deadline_ns: u64) -> Result<Message, Error> {
    let mut message = Message::empty();
    check(svc(sys::RECV, handle.0 as u64, &mut message as *mut Message as u64, deadline_ns))?;
    Ok(message)
}

/// Take a waiting message, or `WouldBlock`.
pub fn try_recv(handle: Handle) -> Result<Message, Error> {
    let mut message = Message::empty();
    check(svc(sys::TRY_RECV, handle.0 as u64, &mut message as *mut Message as u64, 0))?;
    Ok(message)
}

pub fn yield_now() {
    svc(sys::YIELD, 0, 0, 0);
}

pub fn exit() -> ! {
    exit_with(ExitReason::Clean)
}

fn exit_with(reason: ExitReason) -> ! {
    svc(sys::EXIT, reason as u64, 0, 0);
    loop {
        core::hint::spin_loop();
    }
}

pub fn time_ns() -> u64 {
    svc(sys::TIME_NS, 0, 0, 0) as u64
}

/// Write one log line. The kernel prefixes it with this task's name.
pub fn log(text: &str) {
    svc(sys::LOG, text.as_ptr() as u64, text.len() as u64, 0);
}

/// init only: create a channel; the handle carries SEND and RECV.
pub fn channel_create() -> Result<Handle, Error> {
    check(svc(sys::CHANNEL_CREATE, 0, 0, 0)).map(|h| Handle(h as u32))
}

/// init only: start `binary` (an ESP file name, or "builtin:<name>") as the
/// service `name`, with `grants` as its handles 0.., and `arg` for its `main`.
pub fn spawn(name: &str, binary: &str, grants: &[Grant], arg: u64) -> Result<u32, Error> {
    let request = SpawnRequest {
        name_ptr: name.as_ptr() as u64,
        name_len: name.len() as u64,
        binary_ptr: binary.as_ptr() as u64,
        binary_len: binary.len() as u64,
        grants_ptr: grants.as_ptr() as u64,
        grants_len: grants.len() as u64,
        arg,
    };
    check(svc(sys::SPAWN, &request as *const SpawnRequest as u64, 0, 0)).map(|id| id as u32)
}

/// A fixed buffer for `log!`; longer text is cut at `MAX_LOG` bytes.
pub struct LineBuf {
    bytes: [u8; freshos_abi::MAX_LOG],
    len: usize,
}

impl LineBuf {
    pub const fn new() -> Self {
        LineBuf { bytes: [0; freshos_abi::MAX_LOG], len: 0 }
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("?")
    }
}

impl Default for LineBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Write for LineBuf {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for ch in s.chars() {
            let mut utf8 = [0u8; 4];
            let encoded = ch.encode_utf8(&mut utf8).as_bytes();
            if self.len + encoded.len() > self.bytes.len() {
                break;
            }
            self.bytes[self.len..self.len + encoded.len()].copy_from_slice(encoded);
            self.len += encoded.len();
        }
        Ok(())
    }
}

/// `log!("x = {}", x)`: format into a stack buffer and log it.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {{
        let mut line = $crate::LineBuf::new();
        let _ = core::fmt::Write::write_fmt(&mut line, format_args!($($arg)*));
        $crate::log(line.as_str());
    }};
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    log!("panic: {}", info.message());
    exit_with(ExitReason::Panic)
}
