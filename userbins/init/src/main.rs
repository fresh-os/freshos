#![no_std]
#![no_main]

//! init: the one place system policy lives. It creates the channels, starts
//! every service with exactly the handles its table grants, and restarts
//! supervised services when the kernel reports they've exited.

use freshos_rt::{
    Error, ExitReason, Grant, Handle, Message, Rights, Startup, TaskRef, channel_create, entry,
    log, recv_until, spawn, tag, time_ns,
};

entry!(main);

// Channels init creates, by index into `channels`.
const PING: usize = 0;
const PONG: usize = 1;
const SINK: usize = 2;
const PROBE: usize = 3;
const BUF: usize = 4;
const CHANNELS: usize = 5;

struct Service {
    name: &'static str,
    binary: &'static str,
    grants: &'static [(usize, Rights)],
    arg: u64,
    restart_after_ms: Option<u64>,
    optional: bool,
}

const fn service(name: &'static str, binary: &'static str) -> Service {
    Service { name, binary, grants: &[], arg: 0, restart_after_ms: None, optional: false }
}

const SEND: Rights = Rights::SEND;
const RECV: Rights = Rights::RECV;

const TABLE: &[Service] = &[
    // In-kernel built-ins, until each moves out (decision 0006).
    service("mcp", "builtin:mcp"),
    service("kbd", "builtin:kbd"),
    service("comp", "builtin:comp"),
    service("shell", "builtin:shell"),
    service("dash", "builtin:dash"),
    // EL0 services.
    Service { grants: &[(PING, RECV), (PONG, SEND)], ..service("pong", "PONG.ELF") },
    Service { grants: &[(PING, SEND), (PONG, RECV)], ..service("ping", "PING.ELF") },
    Service { restart_after_ms: Some(250), ..service("pulse", "PULSE.ELF") },
    Service { restart_after_ms: Some(200), ..service("fault", "FAULT.ELF") },
    // Test-only, present only when a test stages their binaries.
    Service { arg: 0x4000_0000, optional: true, ..service("probe-bad-kernel", "PROBEBAD.ELF") },
    Service { arg: 0x4_2000_0000, optional: true, ..service("probe-bad-unmap", "PROBEBAD.ELF") },
    Service { arg: 0, optional: true, ..service("probe-bad-code", "PROBEBAD.ELF") },
    Service { arg: 1, optional: true, ..service("probe-bad-stack", "PROBEBAD.ELF") },
    Service { arg: 2, optional: true, ..service("probe-abi", "PROBEBAD.ELF") },
    Service { grants: &[(SINK, SEND), (PROBE, RECV)], optional: true, ..service("probe-chan", "PROBECHA.ELF") },
    Service { grants: &[(PING, RECV)], optional: true, ..service("probe-dup-recv", "PROBECHA.ELF") },
    Service { optional: true, ..service("probe-badelf", "BADELF.ELF") },
    // A supervised receiver that exits after its first message, and a sender
    // that sends more while it is down: they must wait for the restart.
    Service {
        grants: &[(BUF, RECV)],
        arg: 1,
        restart_after_ms: Some(300),
        optional: true,
        ..service("probe-buf-rx", "PROBECHA.ELF")
    },
    Service { grants: &[(BUF, SEND)], arg: 2, optional: true, ..service("probe-buf-tx", "PROBECHA.ELF") },
];

#[derive(Clone, Copy)]
struct Runtime {
    /// The running instance, exactly: a task id alone may already belong to
    /// a newer task by the time its exit notice arrives.
    task: Option<TaskRef>,
    restart_at: Option<u64>,
}

fn main(start: Startup) -> ! {
    let inbox = start.handle(0);
    // A channel that couldn't be created stays None, and every service
    // granted it is skipped: a placeholder handle would name the inbox.
    let mut channels = [None; CHANNELS];
    for channel in channels.iter_mut() {
        match channel_create() {
            Ok(handle) => *channel = Some(handle),
            Err(e) => log!("cannot create a channel: {e:?}"),
        }
    }

    let mut state = [Runtime { task: None, restart_at: None }; TABLE.len()];
    log!("starting services");
    for (index, runtime) in state.iter_mut().enumerate() {
        runtime.task = start_service(index, &channels);
    }
    log!("services launched");

    loop {
        let deadline = state.iter().filter_map(|s| s.restart_at).min().unwrap_or(0);
        match recv_until(inbox, deadline) {
            Ok(message) => handle_message(&message, &mut state, &channels),
            Err(Error::Timeout) => {}
            Err(e) => log!("inbox: {e:?}"),
        }
        let now = time_ns();
        for index in 0..TABLE.len() {
            if state[index].restart_at.is_some_and(|at| now >= at) {
                state[index].restart_at = None;
                log!("restarting {}", TABLE[index].name);
                state[index].task = start_service(index, &channels);
            }
        }
    }
}

fn handle_message(message: &Message, state: &mut [Runtime], channels: &[Option<Handle>; CHANNELS]) {
    match message.tag {
        // Only the kernel (sender 0) reports exits.
        tag::TASK_EXITED if message.sender == 0 => {
            let task = TaskRef {
                id: message.payload[0] as u16,
                generation: message.payload[2] as u32,
            };
            let Some(index) = state.iter().position(|s| s.task == Some(task)) else { return };
            state[index].task = None;
            let reason = ExitReason::from_u64(message.payload[1]).map(|r| r.as_str()).unwrap_or("?");
            log!("{} exited ({})", TABLE[index].name, reason);
            if let Some(ms) = TABLE[index].restart_after_ms {
                state[index].restart_at = Some(time_ns() + ms * 1_000_000);
            }
        }
        tag::RESTART_REQUEST => {
            let (bytes, len) = message.name();
            let name = core::str::from_utf8(&bytes[..len]).unwrap_or("");
            match TABLE.iter().position(|s| s.name == name) {
                None => log!("restart request for unknown service {name}"),
                Some(index) if state[index].task.is_some() => log!("{name} is already running"),
                Some(index) => {
                    log!("restarting {name} (requested)");
                    // It's starting now, so any scheduled restart is moot.
                    state[index].restart_at = None;
                    state[index].task = start_service(index, channels);
                }
            }
        }
        _ => {}
    }
}

fn start_service(index: usize, channels: &[Option<Handle>; CHANNELS]) -> Option<TaskRef> {
    let service = &TABLE[index];
    let mut grants = [Grant { handle: Handle(0), rights: Rights::NONE }; 16];
    for (grant, &(channel, rights)) in grants.iter_mut().zip(service.grants) {
        let Some(handle) = channels[channel] else {
            log!("cannot start {}: channel {} missing", service.name, channel);
            return None;
        };
        *grant = Grant { handle, rights };
    }
    match spawn(service.name, service.binary, &grants[..service.grants.len()], service.arg) {
        Ok(task) => Some(task),
        Err(Error::NotFound) if service.optional => None,
        Err(e) => {
            log!("cannot start {}: {:?}", service.name, e);
            None
        }
    }
}
