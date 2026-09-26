//! The FreshOS kernel/user ABI: every number and type that crosses the
//! boundary, defined once and used by the kernel and every userbin
//! (decision 0006). Nothing here may be redeclared by hand elsewhere.
#![no_std]

/// Syscall numbers, passed in `x8` to `svc #0`.
pub mod sys {
    pub const SEND: u64 = 0;
    pub const RECV: u64 = 1;
    pub const TRY_RECV: u64 = 2;
    pub const YIELD: u64 = 3;
    pub const EXIT: u64 = 4;
    pub const TIME_NS: u64 = 5;
    pub const LOG: u64 = 6;
    pub const CHANNEL_CREATE: u64 = 7;
    pub const SPAWN: u64 = 8;
}

/// Syscall errors, returned in `x0` as negative numbers.
#[repr(i64)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NoSuchSyscall = -1,
    NoSuchHandle = -2,
    NoRight = -3,
    BadPointer = -4,
    WouldBlock = -5,
    Timeout = -6,
    NotFound = -7,
    NotPermitted = -8,
    TableFull = -9,
    ReceiverTaken = -10,
    Full = -11,
    Invalid = -12,
    OutOfMemory = -13,
}

impl Error {
    pub const ALL: [Error; 13] = [
        Error::NoSuchSyscall,
        Error::NoSuchHandle,
        Error::NoRight,
        Error::BadPointer,
        Error::WouldBlock,
        Error::Timeout,
        Error::NotFound,
        Error::NotPermitted,
        Error::TableFull,
        Error::ReceiverTaken,
        Error::Full,
        Error::Invalid,
        Error::OutOfMemory,
    ];

    /// The error for a negative syscall result. Unknown codes map to `Invalid`.
    pub fn from_code(code: i64) -> Error {
        Error::ALL
            .into_iter()
            .find(|e| *e as i64 == code)
            .unwrap_or(Error::Invalid)
    }
}

/// Message tags.
pub mod tag {
    pub const PING: u32 = 1;
    pub const PONG: u32 = 2;
    pub const IRQ: u32 = 10;
    pub const MOUSE_RAW: u32 = 11;
    pub const MOUSE: u32 = 12;
    pub const KEY_DOWN: u32 = 20;
    pub const KEY_UP: u32 = 21;
    /// Kernel → init: payload[0] = task id, payload[1] = `ExitReason`.
    pub const TASK_EXITED: u32 = 100;
    /// Anyone with SEND on init's inbox → init: the service name, packed by `with_name`.
    pub const RESTART_REQUEST: u32 = 101;
}

/// A message: a tag and 32 bytes of inline payload. The kernel always
/// overwrites `sender` with the real sending task (0 = the kernel).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Message {
    pub tag: u32,
    pub sender: u16,
    pub len: u16,
    pub payload: [u64; 4],
}

impl Message {
    pub const fn empty() -> Self {
        Self { tag: 0, sender: 0, len: 0, payload: [0; 4] }
    }

    pub const fn new(tag: u32) -> Self {
        Self { tag, sender: 0, len: 0, payload: [0; 4] }
    }

    pub fn with_data(mut self, slot: usize, value: u64) -> Self {
        if slot < 4 {
            self.payload[slot] = value;
            let end = ((slot + 1) * 8) as u16;
            if end > self.len {
                self.len = end;
            }
        }
        self
    }

    /// Pack a name of up to `NAME_LEN` bytes into payload[0..2]; longer names are cut.
    pub fn with_name(mut self, name: &str) -> Self {
        let bytes = name.as_bytes();
        let n = bytes.len().min(NAME_LEN);
        let mut packed = [0u8; NAME_LEN];
        packed[..n].copy_from_slice(&bytes[..n]);
        self.payload[0] = u64::from_le_bytes(packed[0..8].try_into().unwrap_or([0; 8]));
        self.payload[1] = u64::from_le_bytes(packed[8..16].try_into().unwrap_or([0; 8]));
        self.len = n as u16;
        self
    }

    /// The name packed by `with_name`: the bytes and their length.
    pub fn name(&self) -> ([u8; NAME_LEN], usize) {
        let mut bytes = [0u8; NAME_LEN];
        bytes[0..8].copy_from_slice(&self.payload[0].to_le_bytes());
        bytes[8..16].copy_from_slice(&self.payload[1].to_le_bytes());
        (bytes, (self.len as usize).min(NAME_LEN))
    }
}

/// A slot in the calling task's handle table.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handle(pub u32);

/// What a handle allows.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rights(pub u32);

impl Rights {
    pub const NONE: Rights = Rights(0);
    pub const SEND: Rights = Rights(1);
    pub const RECV: Rights = Rights(2);

    pub const fn contains(self, other: Rights) -> bool {
        self.0 & other.0 == other.0
    }
    pub const fn union(self, other: Rights) -> Rights {
        Rights(self.0 | other.0)
    }
    pub const fn without(self, other: Rights) -> Rights {
        Rights(self.0 & !other.0)
    }
}

/// One handle for `spawn` to copy (SEND) or move (RECV) into the child.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grant {
    pub handle: Handle,
    pub rights: Rights,
}

/// `spawn`'s argument. The child's handles are the grants, in order, and its
/// `main` receives `arg`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SpawnRequest {
    pub name_ptr: u64,
    pub name_len: u64,
    pub binary_ptr: u64,
    pub binary_len: u64,
    pub grants_ptr: u64,
    pub grants_len: u64,
    pub arg: u64,
}

#[repr(u64)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    Clean = 1,
    Fault = 2,
    Panic = 3,
}

impl ExitReason {
    pub fn from_u64(value: u64) -> Option<ExitReason> {
        match value {
            1 => Some(ExitReason::Clean),
            2 => Some(ExitReason::Fault),
            3 => Some(ExitReason::Panic),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            ExitReason::Clean => "clean",
            ExitReason::Fault => "fault",
            ExitReason::Panic => "panic",
        }
    }
}

/// Every task's private window: 1 GiB at 16 GiB.
pub const USER_BASE: u64 = 0x4_0000_0000;
pub const USER_SIZE: u64 = 1 << 30;
pub const USER_STACK_SIZE: u64 = 64 * 1024;
pub const MAX_HANDLES: usize = 16;
pub const NAME_LEN: usize = 16;
pub const MAX_BINARY_NAME: usize = 32;
pub const MAX_LOG: usize = 256;
