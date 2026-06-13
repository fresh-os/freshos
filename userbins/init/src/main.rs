#![no_std]
#![no_main]

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ServiceInfo {
    id: u64,
    flags: u64,
    restart_period_ticks: u64,
    name: [u8; 16],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ServiceStatus {
    task_id: u64,
    restart_count: u64,
    exit_count: u64,
    last_exit_reason: u64,
    state: u64,
}

#[repr(C)]
pub struct InitApi {
    log: extern "C" fn(ptr: *const u8, len: usize),
    spawn_service: extern "C" fn(service_id: u64) -> i64,
    service_count: extern "C" fn() -> u64,
    service_info: extern "C" fn(index: u64, out: *mut ServiceInfo) -> i64,
    service_status: extern "C" fn(service_id: u64, out: *mut ServiceStatus) -> i64,
    yield_now: extern "C" fn(),
}

const MAX_SERVICES: usize = 16;
const SERVICE_FLAG_AUTOSTART: u64 = 1 << 0;
const SERVICE_FLAG_SUPERVISED: u64 = 1 << 1;
const SERVICE_STATE_RUNNING: u64 = 1 << 0;
const SERVICE_EXIT_NONE: u64 = 0;
const NEVER_RESTART_TICK: u64 = u64::MAX;

const EMPTY_SERVICE_INFO: ServiceInfo = ServiceInfo {
    id: 0,
    flags: 0,
    restart_period_ticks: 0,
    name: [0; 16],
};

const EMPTY_SERVICE_STATUS: ServiceStatus = ServiceStatus {
    task_id: 0,
    restart_count: 0,
    exit_count: 0,
    last_exit_reason: 0,
    state: 0,
};

#[derive(Clone, Copy)]
struct ServiceRuntime {
    info: ServiceInfo,
    last_seen_exit_count: u64,
    next_restart_tick: u64,
}

const EMPTY_SERVICE_RUNTIME: ServiceRuntime = ServiceRuntime {
    info: EMPTY_SERVICE_INFO,
    last_seen_exit_count: 0,
    next_restart_tick: NEVER_RESTART_TICK,
};

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

fn log(api: &InitApi, s: &str) {
    (api.log)(s.as_ptr(), s.len());
}

fn spawn(api: &InitApi, service_id: u64) -> i64 {
    (api.spawn_service)(service_id)
}

fn status(api: &InitApi, service_id: u64) -> Option<ServiceStatus> {
    let mut out = EMPTY_SERVICE_STATUS;
    if (api.service_status)(service_id, &mut out as *mut ServiceStatus) == 0 {
        Some(out)
    } else {
        None
    }
}

fn load_services(api: &InitApi, out: &mut [ServiceRuntime; MAX_SERVICES]) -> usize {
    let count = (api.service_count)() as usize;
    let count = count.min(MAX_SERVICES);

    for (idx, slot) in out.iter_mut().take(count).enumerate() {
        let mut info = EMPTY_SERVICE_INFO;
        if (api.service_info)(idx as u64, &mut info as *mut ServiceInfo) != 0 {
            *slot = EMPTY_SERVICE_RUNTIME;
            continue;
        }
        *slot = ServiceRuntime {
            info,
            last_seen_exit_count: status(api, info.id).map(|s| s.exit_count).unwrap_or(0),
            next_restart_tick: NEVER_RESTART_TICK,
        };
    }

    count
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(api: *const InitApi) -> ! {
    let api = unsafe { &*api };
    let mut services = [EMPTY_SERVICE_RUNTIME; MAX_SERVICES];
    let service_count = load_services(api, &mut services);

    log(api, "[init] starting services");
    for service in services.iter_mut().take(service_count) {
        if service.info.flags & SERVICE_FLAG_AUTOSTART != 0 {
            spawn(api, service.info.id);
        }
    }
    log(api, "[init] services launched");

    let mut ticks: u64 = 0;
    loop {
        (api.yield_now)();
        ticks += 1;

        for service in services.iter_mut().take(service_count) {
            if service.info.flags & SERVICE_FLAG_SUPERVISED == 0 {
                continue;
            }
            if service.info.restart_period_ticks == 0 {
                continue;
            }

            let Some(current) = status(api, service.info.id) else {
                continue;
            };
            if current.state & SERVICE_STATE_RUNNING != 0 {
                service.next_restart_tick = NEVER_RESTART_TICK;
                continue;
            }
            if current.last_exit_reason == SERVICE_EXIT_NONE {
                continue;
            }
            if current.exit_count != service.last_seen_exit_count {
                service.last_seen_exit_count = current.exit_count;
                service.next_restart_tick = ticks.wrapping_add(service.info.restart_period_ticks);
            }
            if service.next_restart_tick == NEVER_RESTART_TICK || ticks < service.next_restart_tick {
                continue;
            }

            if spawn(api, service.info.id) >= 0 {
                service.next_restart_tick = NEVER_RESTART_TICK;
            }
        }
    }
}
