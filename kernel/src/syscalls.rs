/// System calls from EL0 (EL0 isolation spec, section 4).
///
/// Every pointer is checked against the caller's own address space before
/// use, and errors are returned, not fatal. What terminates a task is its own
/// hardware fault, handled in exceptions.rs.
use core::fmt::Write;

use freshos_abi::{
    Error, ExitReason, Grant, Handle, MAX_BINARY_NAME, MAX_HANDLES, MAX_LOG, Message, NAME_LEN,
    Rights, SpawnRequest, sys,
};

use crate::arch::addrspace::{
    AddressSpace, check_writable, copy_from_user, read_user, write_user,
};
use crate::arch::context;
use crate::handles::{HandleTable, Slot};
use crate::serial::serial_println;

pub enum Outcome {
    /// Resume the caller with this value in x0.
    Return(i64),
    /// The caller gave up the CPU; it resumes with 0.
    Yield,
    /// The caller exited; never resume it.
    Exited,
    /// The caller is waiting; its x0 is written when it's woken.
    Blocked,
    /// A send woke this task: run it next. The caller resumes with 0.
    HandOff(usize),
}

fn err(e: Error) -> Outcome {
    Outcome::Return(e as i64)
}

fn space() -> Result<&'static AddressSpace, Error> {
    context::current_space().ok_or(Error::NotPermitted)
}

pub fn dispatch(nr: u64, a: [u64; 6]) -> Outcome {
    match nr {
        sys::SEND => send(a[0], a[1]),
        sys::RECV => recv(a[0], a[1], a[2], true),
        sys::TRY_RECV => recv(a[0], a[1], 0, false),
        sys::YIELD => Outcome::Yield,
        sys::EXIT => exit(a[0]),
        sys::TIME_NS => Outcome::Return(crate::arch::time_ns() as i64),
        sys::LOG => log(a[0], a[1]),
        sys::CHANNEL_CREATE => channel_create(),
        sys::SPAWN => spawn(a[0]),
        _ => err(Error::NoSuchSyscall),
    }
}

/// The channel behind the caller's `handle`, if the handle carries `need`.
fn channel(handle: u64, need: Rights) -> Result<u32, Error> {
    let handles = context::current_handles().ok_or(Error::NotPermitted)?;
    let handle = u32::try_from(handle).map_err(|_| Error::NoSuchHandle)?;
    handles.lookup(Handle(handle), need)
}

fn send(handle: u64, ptr: u64) -> Outcome {
    let run = || -> Result<Outcome, Error> {
        let channel = channel(handle, Rights::SEND)?;
        let message: Message = read_user(space()?, ptr)?;
        match crate::ipc::send(channel, &message) {
            Ok(Some(woken)) => Ok(Outcome::HandOff(woken)),
            Ok(None) => Ok(Outcome::Return(0)),
            Err(crate::ipc::Error::Full) => Err(Error::Full),
            Err(_) => Err(Error::NoSuchHandle),
        }
    };
    run().unwrap_or_else(err)
}

/// Receive into `buf`. Blocking with nothing waiting, the caller switches
/// away; a later send delivers straight into `buf`, or the tick after
/// `deadline_ns` (0 = none) wakes it with Timeout.
fn recv(handle: u64, buf: u64, deadline_ns: u64, blocking: bool) -> Outcome {
    let run = || -> Result<Outcome, Error> {
        let channel = channel(handle, Rights::RECV)?;
        let space = space()?;
        // Validate the destination first, so a bad buffer is refused even
        // when a message is waiting.
        check_writable::<Message>(space, buf)?;
        match crate::ipc::try_dequeue(channel).map_err(|_| Error::NoSuchHandle)? {
            Some(message) => {
                write_user(space, buf, &message)?;
                Ok(Outcome::Return(0))
            }
            None if !blocking => Err(Error::WouldBlock),
            None if deadline_ns != 0 && crate::arch::time_ns() >= deadline_ns => Err(Error::Timeout),
            None => {
                crate::ipc::register_waiter(channel, context::current_task())
                    .map_err(|_| Error::NoSuchHandle)?;
                context::set_user_wait(channel, buf, deadline_ns);
                Ok(Outcome::Blocked)
            }
        }
    };
    run().unwrap_or_else(err)
}

fn caller_is_init() -> Result<usize, Error> {
    let cur = context::current_task();
    if cur != 0 && cur == context::init_task() { Ok(cur) } else { Err(Error::NotPermitted) }
}

/// init only: a new channel, with SEND and RECV in one handle.
fn channel_create() -> Outcome {
    let run = || -> Result<Outcome, Error> {
        let init = caller_is_init()?;
        let handles = context::handles_mut(init);
        // Check for room first: a channel, once created, is never freed.
        if handles.is_full() {
            return Err(Error::TableFull);
        }
        let channel = crate::ipc::create().map_err(|_| Error::TableFull)?;
        let handle = handles.insert(Slot { channel, rights: Rights::SEND.union(Rights::RECV) })?;
        crate::ipc::set_receiver(channel, init);
        Ok(Outcome::Return(handle.0 as i64))
    };
    run().unwrap_or_else(err)
}

/// A service name becomes the "[name]" prefix of the task's log lines, so it
/// must be printable and unable to fake a prefix.
fn valid_name(name: &[u8]) -> bool {
    name.iter().all(|&b| b.is_ascii_graphic() && b != b'[' && b != b']')
}

/// init only: start a service (EL0 isolation spec, section 5). SEND grants
/// are copied from init's handles; RECV grants move out of init's slot into
/// the child, and come home when the child exits (`context::retire_current`).
fn spawn(request_ptr: u64) -> Outcome {
    let run = || -> Result<Outcome, Error> {
        let init = caller_is_init()?;
        let space = space()?;
        let request: SpawnRequest = read_user(space, request_ptr)?;
        if request.name_len == 0
            || request.name_len > NAME_LEN as u64
            || request.binary_len == 0
            || request.binary_len > MAX_BINARY_NAME as u64
            || request.grants_len > MAX_HANDLES as u64
        {
            return Err(Error::Invalid);
        }
        let mut name = [0u8; NAME_LEN];
        let name = &mut name[..request.name_len as usize];
        copy_from_user(space, request.name_ptr, name)?;
        if !valid_name(name) {
            return Err(Error::Invalid);
        }
        let name: &[u8] = name;
        let mut binary = [0u8; MAX_BINARY_NAME];
        let binary = &mut binary[..request.binary_len as usize];
        copy_from_user(space, request.binary_ptr, binary)?;
        let binary = core::str::from_utf8(binary).map_err(|_| Error::Invalid)?;
        let count = request.grants_len as usize;

        // Built-ins still in the kernel: started by init's policy, run at EL1.
        // They hold no handles, so there is nothing to grant them.
        if let Some(builtin) = binary.strip_prefix("builtin:") {
            if count != 0 {
                return Err(Error::Invalid);
            }
            let entry = crate::arm_tasks::builtin(builtin.as_bytes()).ok_or(Error::NotFound)?;
            let id = context::spawn(entry, |id| crate::registry::on_spawn(id, name))
                .map_err(spawn_error)?;
            return Ok(Outcome::Return(id as i64));
        }

        let image = crate::boot_images::find(binary).ok_or(Error::NotFound)?;
        let mut grants = [Grant { handle: Handle(0), rights: Rights::NONE }; MAX_HANDLES];
        for (i, grant) in grants[..count].iter_mut().enumerate() {
            let offset = (i * core::mem::size_of::<Grant>()) as u64;
            let va = request.grants_ptr.checked_add(offset).ok_or(Error::BadPointer)?;
            *grant = read_user(space, va)?;
        }
        let grants = &grants[..count];

        // Resolve every grant against init's table before anything changes.
        // A grant may only carry rights init holds on that handle.
        let init_handles = context::handles_mut(init);
        let mut child = HandleTable::EMPTY;
        for (i, grant) in grants.iter().enumerate() {
            if !Rights::SEND.union(Rights::RECV).contains(grant.rights) {
                return Err(Error::Invalid);
            }
            let slot = *init_handles.get_mut(grant.handle).ok_or(Error::NoSuchHandle)?;
            if grant.rights.contains(Rights::RECV) {
                // One receiver per channel: not twice in one request, and not
                // while an earlier child still holds it.
                let duplicate = grants[..i].iter().any(|g| {
                    g.rights.contains(Rights::RECV)
                        && init_handles.get_mut(g.handle).map(|s| s.channel) == Some(slot.channel)
                });
                if duplicate || !slot.rights.contains(Rights::RECV) {
                    let taken = crate::ipc::receiver(slot.channel).is_some_and(|r| r != init);
                    return Err(if duplicate || taken { Error::ReceiverTaken } else { Error::NoRight });
                }
            }
            if grant.rights.contains(Rights::SEND) && !slot.rights.contains(Rights::SEND) {
                return Err(Error::NoRight);
            }
            child.insert(Slot { channel: slot.channel, rights: grant.rights })?;
        }

        // Load with IRQs on: an ELF load is too long to hold the CPU. The
        // task can't run, and nothing else can change what was checked above
        // (only init spawns, and an exit only gives RECV back to init), until
        // `bind` has run inside the install. It moves the receive rights and
        // names the task before the task can run, so even an instant exit
        // returns them and is charged to this service.
        let bind = |id: usize| {
            for grant in grants.iter().filter(|g| g.rights.contains(Rights::RECV)) {
                if let Some(slot) = context::handles_mut(init).get_mut(grant.handle) {
                    slot.rights = slot.rights.without(Rights::RECV);
                    crate::ipc::set_receiver_with_home(slot.channel, id, init, grant.handle.0);
                }
            }
            crate::registry::on_spawn(id, name);
        };
        crate::arch::interrupt_enable();
        let spawned = context::spawn_el0(image, child, count as u64, request.arg, bind);
        crate::arch::interrupt_disable();
        let id = spawned.map_err(|e| {
            if let context::SpawnError::BadImage(reason) = e {
                serial_println!("refused {}: {}", binary, reason);
            }
            spawn_error(e)
        })?;
        Ok(Outcome::Return(id as i64))
    };
    run().unwrap_or_else(err)
}

fn spawn_error(e: context::SpawnError) -> Error {
    match e {
        context::SpawnError::NoSlot => Error::TableFull,
        context::SpawnError::OutOfMemory => Error::OutOfMemory,
        context::SpawnError::BadImage(_) => Error::Invalid,
    }
}

fn exit(reason: u64) -> Outcome {
    // A task may report Clean or Panic. Fault is the kernel's to give.
    let reason = match ExitReason::from_u64(reason) {
        Some(ExitReason::Panic) => ExitReason::Panic,
        _ => ExitReason::Clean,
    };
    context::retire_current(reason);
    Outcome::Exited
}

/// Print one line as "[name] text". The text is capped at MAX_LOG bytes, and
/// invalid UTF-8, control characters (newlines included) and the Unicode line
/// and paragraph separators become '?', so a log can never forge another
/// task's line.
fn log(ptr: u64, len: u64) -> Outcome {
    let space = match space() {
        Ok(space) => space,
        Err(e) => return err(e),
    };
    let n = len.min(MAX_LOG as u64) as usize;
    let mut buf = [0u8; MAX_LOG];
    if let Err(e) = copy_from_user(space, ptr, &mut buf[..n]) {
        return err(e);
    }
    let mut out = crate::serial::Serial;
    let _ = write!(out, "[{}] ", crate::registry::name(context::current_task()).as_str());
    for chunk in buf[..n].utf8_chunks() {
        for ch in chunk.valid().chars() {
            let forged_break = ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}');
            let _ = out.write_char(if forged_break { '?' } else { ch });
        }
        if !chunk.invalid().is_empty() {
            let _ = out.write_char('?');
        }
    }
    if len > MAX_LOG as u64 {
        let _ = out.write_str("…");
    }
    let _ = out.write_str("\n");
    Outcome::Return(0)
}
