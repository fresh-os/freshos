/// System calls from EL0 (EL0 isolation spec, section 4).
///
/// Every pointer is checked against the caller's own address space before
/// use, and errors are returned, not fatal. What terminates a task is its own
/// hardware fault, handled in exceptions.rs.
use core::fmt::Write;

use freshos_abi::{Error, ExitReason, Handle, MAX_LOG, Message, Rights, sys};

use crate::arch::addrspace::{
    AddressSpace, check_writable, copy_from_user, read_user, write_user,
};
use crate::arch::context;

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

fn exit(reason: u64) -> Outcome {
    // A task may report Clean or Panic. Fault is the kernel's to give.
    let reason = match ExitReason::from_u64(reason) {
        Some(ExitReason::Panic) => ExitReason::Panic,
        _ => ExitReason::Clean,
    };
    context::retire_current(reason as u64);
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
    let _ = write!(out, "[{}] ", crate::task_names::name(context::current_task()));
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
