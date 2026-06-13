#![no_std]
#![no_main]

#[repr(C)]
#[derive(Clone, Copy)]
struct Message {
    tag: u32,
    sender: u16,
    len: u16,
    payload: [u64; 4],
}

#[repr(C)]
pub struct ServiceApi {
    log: extern "C" fn(ptr: *const u8, len: usize),
    send: extern "C" fn(channel_id: u32, msg: *const Message) -> i64,
    recv: extern "C" fn(channel_id: u32, out: *mut Message) -> i64,
    yield_now: extern "C" fn(),
    exit_now: extern "C" fn() -> !,
}

const CH_IPC_PROBE_PING: u32 = 2;
const CH_IPC_PROBE_PONG: u32 = 3;
const MSG_PONG: u32 = 2;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

fn log(api: &ServiceApi, s: &str) {
    (api.log)(s.as_ptr(), s.len());
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(api: *const ServiceApi) -> ! {
    let api = unsafe { &*api };
    let mut incoming = Message {
        tag: 0,
        sender: 0,
        len: 0,
        payload: [0; 4],
    };

    log(api, "[probe] pong ext");

    loop {
        if (api.recv)(CH_IPC_PROBE_PING, &mut incoming) == 0 {
            let reply = Message {
                tag: MSG_PONG,
                sender: 0,
                len: 8,
                payload: [incoming.payload[0], 0, 0, 0],
            };
            let _ = (api.send)(CH_IPC_PROBE_PONG, &reply);
        } else {
            (api.yield_now)();
        }
    }
}
