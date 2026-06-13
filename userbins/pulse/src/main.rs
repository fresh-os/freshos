#![no_std]
#![no_main]

#[repr(C)]
pub struct ServiceApi {
    log: extern "C" fn(ptr: *const u8, len: usize),
    send: extern "C" fn(channel_id: u32, msg: *const u8) -> i64,
    recv: extern "C" fn(channel_id: u32, out: *mut u8) -> i64,
    yield_now: extern "C" fn(),
    exit_now: extern "C" fn() -> !,
}

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

    log(api, "[pulse] ext start");
    for msg in ["[pulse] ext beat 1", "[pulse] ext beat 2", "[pulse] ext beat 3"] {
        for _ in 0..500 {
            (api.yield_now)();
        }
        log(api, msg);
    }
    log(api, "[pulse] ext exit");
    (api.exit_now)()
}
