use core::slice;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch;
use crate::ipc;
use crate::serial::serial_println;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ServiceMessage {
    pub tag: u32,
    pub sender: u16,
    pub len: u16,
    pub payload: [u64; 4],
}

#[repr(C)]
pub struct ServiceApi {
    pub log: extern "C" fn(ptr: *const u8, len: usize),
    pub send: extern "C" fn(channel_id: u32, msg: *const ServiceMessage) -> i64,
    pub recv: extern "C" fn(channel_id: u32, out: *mut ServiceMessage) -> i64,
    pub yield_now: extern "C" fn(),
    pub exit_now: extern "C" fn() -> !,
}

static EXTERNAL_PONG_ENTRY: AtomicU64 = AtomicU64::new(0);
static EXTERNAL_PULSE_ENTRY: AtomicU64 = AtomicU64::new(0);
static EXTERNAL_FAULT_ENTRY: AtomicU64 = AtomicU64::new(0);
static EXTERNAL_FAULT_USER_STACK: AtomicU64 = AtomicU64::new(0);

extern "C" fn service_log(ptr: *const u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }

    let bytes = unsafe { slice::from_raw_parts(ptr, len.min(256)) };
    if let Ok(s) = core::str::from_utf8(bytes) {
        serial_println!("{}", s);
    } else {
        serial_println!("[service] <non-utf8:{} bytes>", len);
    }
}

extern "C" fn service_send(channel_id: u32, msg: *const ServiceMessage) -> i64 {
    if msg.is_null() {
        return -1;
    }

    let msg = unsafe { &*msg };
    let kernel_msg = ipc::Message {
        tag: msg.tag,
        sender: arch::current_task() as u16,
        len: msg.len.min(32),
        payload: msg.payload,
    };

    match ipc::send(channel_id, &kernel_msg) {
        Ok(()) => 0,
        Err(ipc::Error::InvalidChannel) => -2,
        Err(ipc::Error::Full) => -3,
        Err(ipc::Error::NoCapacity) => -4,
    }
}

extern "C" fn service_recv(channel_id: u32, out: *mut ServiceMessage) -> i64 {
    if out.is_null() {
        return -1;
    }

    match ipc::recv(channel_id) {
        Ok(msg) => {
            unsafe {
                *out = ServiceMessage {
                    tag: msg.tag,
                    sender: msg.sender,
                    len: msg.len,
                    payload: msg.payload,
                };
            }
            0
        }
        Err(ipc::Error::InvalidChannel) => -2,
        Err(ipc::Error::Full) => -3,
        Err(ipc::Error::NoCapacity) => -4,
    }
}

extern "C" fn service_yield_now() {
    arch::interrupt_enable();
    arch::halt();
}

extern "C" fn service_exit_now() -> ! {
    serial_println!("task {} exited", arch::current_task());
    arch::context::terminate_current_with_reason(crate::init_abi::SERVICE_EXIT_CLEAN)
}

static PROBE_API: ServiceApi = ServiceApi {
    log: service_log,
    send: service_send,
    recv: service_recv,
    yield_now: service_yield_now,
    exit_now: service_exit_now,
};

pub fn register_external_pong(entry: u64) {
    EXTERNAL_PONG_ENTRY.store(entry, Ordering::SeqCst);
}

pub fn external_pong_entry() -> Option<u64> {
    match EXTERNAL_PONG_ENTRY.load(Ordering::SeqCst) {
        0 => None,
        entry => Some(entry),
    }
}

pub fn register_external_pulse(entry: u64) {
    EXTERNAL_PULSE_ENTRY.store(entry, Ordering::SeqCst);
}

pub fn external_pulse_entry() -> Option<u64> {
    match EXTERNAL_PULSE_ENTRY.load(Ordering::SeqCst) {
        0 => None,
        entry => Some(entry),
    }
}

pub fn register_external_fault(entry: u64, user_stack_bottom: u64) {
    EXTERNAL_FAULT_ENTRY.store(entry, Ordering::SeqCst);
    EXTERNAL_FAULT_USER_STACK.store(user_stack_bottom, Ordering::SeqCst);
}

pub fn external_fault_entry() -> Option<u64> {
    match EXTERNAL_FAULT_ENTRY.load(Ordering::SeqCst) {
        0 => None,
        entry => Some(entry),
    }
}

pub fn external_fault_user_stack() -> Option<u64> {
    match EXTERNAL_FAULT_USER_STACK.load(Ordering::SeqCst) {
        0 => None,
        stack => Some(stack),
    }
}

pub fn probe_api_ptr() -> *const ServiceApi {
    &PROBE_API
}
