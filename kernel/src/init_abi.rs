use core::slice;
use core::sync::atomic::{AtomicU64, Ordering};

use freshos_abi::Rights;

use crate::arch;
use crate::handles::{HandleTable, Slot};
use crate::serial::serial_println;

pub const SERVICE_KBD: u64 = 1;
pub const SERVICE_COMP: u64 = 2;
pub const SERVICE_SHELL: u64 = 3;
pub const SERVICE_DASH: u64 = 4;
pub const SERVICE_PING: u64 = 5;
pub const SERVICE_PONG: u64 = 6;
pub const SERVICE_PULSE: u64 = 7;
pub const SERVICE_FAULT: u64 = 8;
pub const SERVICE_MCP: u64 = 9;
pub const SERVICE_PROBE_BAD_KERNEL: u64 = 10;
pub const SERVICE_PROBE_BAD_UNMAPPED: u64 = 11;
pub const SERVICE_PROBE_BAD_CODE: u64 = 12;
pub const SERVICE_PROBE_BAD_STACK: u64 = 13;
pub const SERVICE_PROBE_ABI: u64 = 14;
pub const SERVICE_PROBE_CHAN: u64 = 15;

pub const SERVICE_FLAG_AUTOSTART: u64 = 1 << 0;
pub const SERVICE_FLAG_SUPERVISED: u64 = 1 << 1;

pub const SERVICE_STATE_RUNNING: u64 = 1 << 0;

pub const SERVICE_EXIT_NONE: u64 = 0;
pub const SERVICE_EXIT_CLEAN: u64 = 1;
pub const SERVICE_EXIT_FAULT: u64 = 2;

const SERVICE_COUNT: usize = 15;
const SERVICE_NAME_BYTES: usize = 16;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ServiceInfo {
    pub id: u64,
    pub flags: u64,
    pub restart_period_ticks: u64,
    pub name: [u8; SERVICE_NAME_BYTES],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ServiceStatus {
    pub task_id: u64,
    pub restart_count: u64,
    pub exit_count: u64,
    pub last_exit_reason: u64,
    pub state: u64,
}

#[repr(C)]
pub struct InitApi {
    pub log: extern "C" fn(ptr: *const u8, len: usize),
    pub spawn_service: extern "C" fn(service_id: u64) -> i64,
    pub service_count: extern "C" fn() -> u64,
    pub service_info: extern "C" fn(index: u64, out: *mut ServiceInfo) -> i64,
    pub service_status: extern "C" fn(service_id: u64, out: *mut ServiceStatus) -> i64,
    pub yield_now: extern "C" fn(),
}

#[derive(Clone, Copy)]
pub struct ServiceRecord {
    pub id: u64,
    pub name: &'static str,
    pub flags: u64,
    pub restart_period_ticks: u64,
}

#[derive(Clone, Copy)]
struct ServiceDefinition {
    id: u64,
    name: &'static str,
    flags: u64,
    restart_period_ticks: u64,
    /// Start the service; `None` if it couldn't be started.
    spawn: fn() -> Option<usize>,
}

fn spawn_kbd_service() -> Option<usize> {
    Some(arch::context::spawn(crate::arm_tasks::keyboard_el1))
}

fn spawn_comp_service() -> Option<usize> {
    Some(arch::context::spawn(crate::arm_tasks::compositor_el1))
}

fn spawn_shell_service() -> Option<usize> {
    Some(arch::context::spawn(crate::arm_tasks::shell_el1))
}

fn spawn_dash_service() -> Option<usize> {
    Some(arch::context::spawn(crate::arm_tasks::dashboard_el1))
}

fn spawn_mcp_service() -> Option<usize> {
    Some(arch::context::spawn(crate::mcp::bridge_el1))
}

/// Start `binary` from the ESP at EL0 with `handles`, of which the first
/// `handle_count` are its granted handles 0.., and `arg` for its `main`. A
/// missing file is silent (the probes are only staged for test boots).
fn spawn_image(binary: &str, handles: HandleTable, handle_count: u64, arg: u64) -> Option<usize> {
    let image = crate::boot_images::find(binary)?;
    match arch::context::spawn_el0(image, handles, handle_count, arg) {
        Ok(id) => Some(id),
        Err(arch::context::SpawnError::BadImage(reason)) => {
            serial_println!("[init] cannot start {}: bad image: {}", binary, reason);
            None
        }
        Err(err) => {
            serial_println!("[init] cannot start {}: {:?}", binary, err);
            None
        }
    }
}

/// A handle table granting `slots`, in order, as handles 0...
fn grants(slots: &[(u32, Rights)]) -> HandleTable {
    let mut table = HandleTable::EMPTY;
    for &(channel, rights) in slots {
        let _ = table.insert(Slot { channel, rights });
    }
    table
}

/// ping sends on channel 2 and receives on channel 3; pong the reverse.
fn spawn_ping_service() -> Option<usize> {
    let id = spawn_image("PING.ELF", grants(&[(2, Rights::SEND), (3, Rights::RECV)]), 2, 0)?;
    crate::ipc::set_receiver(3, id);
    Some(id)
}

fn spawn_pong_service() -> Option<usize> {
    let id = spawn_image("PONG.ELF", grants(&[(2, Rights::RECV), (3, Rights::SEND)]), 2, 0)?;
    crate::ipc::set_receiver(2, id);
    Some(id)
}

/// Test-only (PROBECHA.ELF is staged only by the test harness): sends on the
/// sink, channel 4, which nobody receives on, and receives on channel 5.
fn spawn_probe_chan() -> Option<usize> {
    let id = spawn_image("PROBECHA.ELF", grants(&[(4, Rights::SEND), (5, Rights::RECV)]), 2, 0)?;
    crate::ipc::set_receiver(5, id);
    Some(id)
}

fn spawn_fault_service() -> Option<usize> {
    if crate::boot_images::find("FAULT.ELF").is_none() {
        serial_println!("[init] FAULT.ELF missing");
    }
    spawn_image("FAULT.ELF", HandleTable::EMPTY, 0, 0)
}

fn spawn_pulse_service() -> Option<usize> {
    spawn_image("PULSE.ELF", HandleTable::EMPTY, 0, 0)
}

// Test-only (PROBEBAD.ELF is staged only by the test harness). Each attempts
// one forbidden access and must end in a contained fault. 0x4000_0000 is the
// start of RAM on QEMU virt (the kernel heap); the kernel's map of RAM, which
// also covers every other task's frames, is EL1-only, so that one probe
// stands for both "kernel memory" and "another service's frames".
fn spawn_probe_bad_kernel() -> Option<usize> {
    spawn_image("PROBEBAD.ELF", HandleTable::EMPTY, 0, 0x4000_0000)
}

fn spawn_probe_bad_unmapped() -> Option<usize> {
    spawn_image("PROBEBAD.ELF", HandleTable::EMPTY, 0, 0x4_2000_0000)
}

fn spawn_probe_bad_code() -> Option<usize> {
    spawn_image("PROBEBAD.ELF", HandleTable::EMPTY, 0, 0)
}

fn spawn_probe_bad_stack() -> Option<usize> {
    spawn_image("PROBEBAD.ELF", HandleTable::EMPTY, 0, 1)
}

/// Test-only: checks log sanitising, bad pointers, unknown syscalls and
/// FP/SIMD state across switches, then exits cleanly.
fn spawn_probe_abi() -> Option<usize> {
    spawn_image("PROBEBAD.ELF", HandleTable::EMPTY, 0, 2)
}

static SERVICES: [ServiceDefinition; SERVICE_COUNT] = [
    ServiceDefinition {
        id: SERVICE_KBD,
        name: "kbd",
        flags: SERVICE_FLAG_AUTOSTART,
        restart_period_ticks: 0,
        spawn: spawn_kbd_service,
    },
    ServiceDefinition {
        id: SERVICE_COMP,
        name: "comp",
        flags: SERVICE_FLAG_AUTOSTART,
        restart_period_ticks: 0,
        spawn: spawn_comp_service,
    },
    ServiceDefinition {
        id: SERVICE_SHELL,
        name: "shell",
        flags: SERVICE_FLAG_AUTOSTART,
        restart_period_ticks: 0,
        spawn: spawn_shell_service,
    },
    ServiceDefinition {
        id: SERVICE_DASH,
        name: "dash",
        flags: SERVICE_FLAG_AUTOSTART,
        restart_period_ticks: 0,
        spawn: spawn_dash_service,
    },
    ServiceDefinition {
        id: SERVICE_PING,
        name: "ping",
        flags: SERVICE_FLAG_AUTOSTART,
        restart_period_ticks: 0,
        spawn: spawn_ping_service,
    },
    ServiceDefinition {
        id: SERVICE_PONG,
        name: "pong",
        flags: SERVICE_FLAG_AUTOSTART,
        restart_period_ticks: 0,
        spawn: spawn_pong_service,
    },
    ServiceDefinition {
        id: SERVICE_PULSE,
        name: "pulse",
        flags: SERVICE_FLAG_AUTOSTART | SERVICE_FLAG_SUPERVISED,
        restart_period_ticks: 250,
        spawn: spawn_pulse_service,
    },
    ServiceDefinition {
        id: SERVICE_FAULT,
        name: "fault",
        flags: SERVICE_FLAG_AUTOSTART | SERVICE_FLAG_SUPERVISED,
        restart_period_ticks: 200,
        spawn: spawn_fault_service,
    },
    ServiceDefinition {
        id: SERVICE_MCP,
        name: "mcp",
        flags: SERVICE_FLAG_AUTOSTART,
        restart_period_ticks: 0,
        spawn: spawn_mcp_service,
    },
    ServiceDefinition {
        id: SERVICE_PROBE_BAD_KERNEL,
        name: "probe-bad-kernel",
        flags: SERVICE_FLAG_AUTOSTART,
        restart_period_ticks: 0,
        spawn: spawn_probe_bad_kernel,
    },
    ServiceDefinition {
        id: SERVICE_PROBE_BAD_UNMAPPED,
        name: "probe-bad-unmapped",
        flags: SERVICE_FLAG_AUTOSTART,
        restart_period_ticks: 0,
        spawn: spawn_probe_bad_unmapped,
    },
    ServiceDefinition {
        id: SERVICE_PROBE_BAD_CODE,
        name: "probe-bad-code",
        flags: SERVICE_FLAG_AUTOSTART,
        restart_period_ticks: 0,
        spawn: spawn_probe_bad_code,
    },
    ServiceDefinition {
        id: SERVICE_PROBE_BAD_STACK,
        name: "probe-bad-stack",
        flags: SERVICE_FLAG_AUTOSTART,
        restart_period_ticks: 0,
        spawn: spawn_probe_bad_stack,
    },
    ServiceDefinition {
        id: SERVICE_PROBE_ABI,
        name: "probe-abi",
        flags: SERVICE_FLAG_AUTOSTART,
        restart_period_ticks: 0,
        spawn: spawn_probe_abi,
    },
    ServiceDefinition {
        id: SERVICE_PROBE_CHAN,
        name: "probe-chan",
        flags: SERVICE_FLAG_AUTOSTART,
        restart_period_ticks: 0,
        spawn: spawn_probe_chan,
    },
];

static STARTED_SERVICES: AtomicU64 = AtomicU64::new(0);
static SERVICE_TASKS: [AtomicU64; SERVICE_COUNT] = [const { AtomicU64::new(0) }; SERVICE_COUNT];
static SERVICE_RESTARTS: [AtomicU64; SERVICE_COUNT] = [const { AtomicU64::new(0) }; SERVICE_COUNT];
static SERVICE_EXITS: [AtomicU64; SERVICE_COUNT] = [const { AtomicU64::new(0) }; SERVICE_COUNT];
static SERVICE_LAST_EXITS: [AtomicU64; SERVICE_COUNT] =
    [const { AtomicU64::new(SERVICE_EXIT_NONE) }; SERVICE_COUNT];
static TASK_SERVICES: [AtomicU64; crate::arch::context::MAX_TASKS] =
    [const { AtomicU64::new(0) }; crate::arch::context::MAX_TASKS];

fn service_index(service_id: u64) -> Option<usize> {
    SERVICES.iter().position(|service| service.id == service_id)
}

fn service_by_id(service_id: u64) -> Option<&'static ServiceDefinition> {
    service_index(service_id).map(|idx| &SERVICES[idx])
}

fn service_bit(service_idx: usize) -> u64 {
    1u64 << service_idx
}

pub(crate) fn exit_reason_name(reason: u64) -> &'static str {
    freshos_abi::ExitReason::from_u64(reason).map_or("unknown", freshos_abi::ExitReason::as_str)
}

fn remember_service_task(service_idx: usize, service_id: u64, task_id: usize) {
    SERVICE_TASKS[service_idx].store(task_id as u64, Ordering::SeqCst);
    if let Some(slot) = TASK_SERVICES.get(task_id) {
        slot.store(service_id, Ordering::SeqCst);
    }
}

fn fill_service_name(name: &str) -> [u8; SERVICE_NAME_BYTES] {
    let mut out = [0u8; SERVICE_NAME_BYTES];
    let bytes = name.as_bytes();
    let len = bytes.len().min(SERVICE_NAME_BYTES);
    out[..len].copy_from_slice(&bytes[..len]);
    out
}

extern "C" fn init_log(ptr: *const u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }

    let bytes = unsafe { slice::from_raw_parts(ptr, len.min(256)) };
    if let Ok(s) = core::str::from_utf8(bytes) {
        serial_println!("{}", s);
    } else {
        serial_println!("[init] <non-utf8:{} bytes>", len);
    }
}

extern "C" fn init_yield_now() {
    arch::interrupt_enable();
    arch::halt();
}

extern "C" fn init_spawn_service(service_id: u64) -> i64 {
    let Some(service_idx) = service_index(service_id) else {
        return -1;
    };

    let bit = service_bit(service_idx);
    if STARTED_SERVICES.fetch_or(bit, Ordering::SeqCst) & bit != 0 {
        return SERVICE_TASKS[service_idx].load(Ordering::SeqCst) as i64;
    }

    let definition = &SERVICES[service_idx];
    let had_previous_exit =
        SERVICE_LAST_EXITS[service_idx].load(Ordering::SeqCst) != SERVICE_EXIT_NONE;
    // Record the task before it can run. With IRQs masked, no tick can start
    // it until its name and its service are both registered, so its first
    // log line carries its own name (never the slot's previous occupant's),
    // and an exit, however early, is charged to the right service.
    let irq = arch::IrqGuard::mask();
    let Some(task_id) = (definition.spawn)() else {
        drop(irq);
        // Leave the service stopped so a later request can try again.
        STARTED_SERVICES.fetch_and(!bit, Ordering::SeqCst);
        return -1;
    };
    crate::task_names::register(task_id, definition.name);
    if had_previous_exit {
        SERVICE_RESTARTS[service_idx].fetch_add(1, Ordering::SeqCst);
    }
    remember_service_task(service_idx, service_id, task_id);
    drop(irq);
    task_id as i64
}

extern "C" fn init_service_count() -> u64 {
    SERVICE_COUNT as u64
}

extern "C" fn init_service_info(index: u64, out: *mut ServiceInfo) -> i64 {
    if out.is_null() {
        return -1;
    }

    let Some(definition) = SERVICES.get(index as usize) else {
        return -2;
    };

    unsafe {
        *out = ServiceInfo {
            id: definition.id,
            flags: definition.flags,
            restart_period_ticks: definition.restart_period_ticks,
            name: fill_service_name(definition.name),
        };
    }
    0
}

extern "C" fn init_service_status(service_id: u64, out: *mut ServiceStatus) -> i64 {
    if out.is_null() {
        return -1;
    }

    let Some(service_idx) = service_index(service_id) else {
        return -2;
    };

    let state = if STARTED_SERVICES.load(Ordering::SeqCst) & service_bit(service_idx) != 0 {
        SERVICE_STATE_RUNNING
    } else {
        0
    };

    unsafe {
        *out = ServiceStatus {
            task_id: SERVICE_TASKS[service_idx].load(Ordering::SeqCst),
            restart_count: SERVICE_RESTARTS[service_idx].load(Ordering::SeqCst),
            exit_count: SERVICE_EXITS[service_idx].load(Ordering::SeqCst),
            last_exit_reason: SERVICE_LAST_EXITS[service_idx].load(Ordering::SeqCst),
            state,
        };
    }
    0
}

static INIT_API: InitApi = InitApi {
    log: init_log,
    spawn_service: init_spawn_service,
    service_count: init_service_count,
    service_info: init_service_info,
    service_status: init_service_status,
    yield_now: init_yield_now,
};

pub fn api_ptr() -> *const InitApi {
    &INIT_API
}

pub fn service_count() -> usize {
    SERVICE_COUNT
}

pub fn service_record(index: usize) -> Option<ServiceRecord> {
    SERVICES.get(index).map(|service| ServiceRecord {
        id: service.id,
        name: service.name,
        flags: service.flags,
        restart_period_ticks: service.restart_period_ticks,
    })
}

pub fn find_service(name: &str) -> Option<ServiceRecord> {
    SERVICES
        .iter()
        .find(|service| service.name == name)
        .map(|service| ServiceRecord {
            id: service.id,
            name: service.name,
            flags: service.flags,
            restart_period_ticks: service.restart_period_ticks,
        })
}

pub fn service_status(service_id: u64) -> Option<ServiceStatus> {
    let service_idx = service_index(service_id)?;
    let state = if STARTED_SERVICES.load(Ordering::SeqCst) & service_bit(service_idx) != 0 {
        SERVICE_STATE_RUNNING
    } else {
        0
    };

    Some(ServiceStatus {
        task_id: SERVICE_TASKS[service_idx].load(Ordering::SeqCst),
        restart_count: SERVICE_RESTARTS[service_idx].load(Ordering::SeqCst),
        exit_count: SERVICE_EXITS[service_idx].load(Ordering::SeqCst),
        last_exit_reason: SERVICE_LAST_EXITS[service_idx].load(Ordering::SeqCst),
        state,
    })
}

pub fn spawn_service(service_id: u64) -> Result<usize, i64> {
    let task_id = init_spawn_service(service_id);
    if task_id < 0 {
        Err(task_id)
    } else {
        Ok(task_id as usize)
    }
}

pub fn task_exited(task_id: usize, reason: u64) {
    let Some(slot) = TASK_SERVICES.get(task_id) else {
        return;
    };
    let service_id = slot.swap(0, Ordering::SeqCst);
    if service_id == 0 {
        return;
    }

    let Some(service_idx) = service_index(service_id) else {
        return;
    };

    SERVICE_TASKS[service_idx].store(0, Ordering::SeqCst);
    SERVICE_EXITS[service_idx].fetch_add(1, Ordering::SeqCst);
    SERVICE_LAST_EXITS[service_idx].store(reason, Ordering::SeqCst);
    STARTED_SERVICES.fetch_and(!service_bit(service_idx), Ordering::SeqCst);

    let service_name = service_by_id(service_id)
        .map(|service| service.name)
        .unwrap_or("unknown");
    serial_println!(
        "[init] service {} exited (task {}, reason={})",
        service_name,
        task_id,
        exit_reason_name(reason)
    );
}
