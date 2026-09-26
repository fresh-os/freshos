#![no_std]
#![no_main]

use freshos_rt::{Error, Handle, Message, Startup, abi, entry, exit, log, recv, send, spawn, try_recv};

entry!(main);

fn report(case: &str, result: Result<(), Error>) {
    match result {
        Ok(()) => log!("[test] {case} result=ok code=0"),
        Err(e) => log!("[test] {case} result=refused code={}", e as i64),
    }
}

/// Raw syscall, for deliberately bad arguments the safe wrappers can't express.
fn raw(nr: u64, a0: u64, a1: u64) -> Result<(), Error> {
    let ret: i64;
    unsafe {
        core::arch::asm!("svc #0", in("x8") nr, inlateout("x0") a0 as i64 => ret, in("x1") a1, in("x2") 0u64, options(nostack));
    }
    if ret < 0 { Err(Error::from_code(ret)) } else { Ok(()) }
}

/// Test-only. Chosen by its table argument: 0 = every way of using channels
/// and pointers wrongly, each reported; 1 and 2 = the two halves of the
/// buffering-across-a-restart check.
fn main(start: Startup) -> ! {
    match start.arg() {
        1 => buffer_receiver(start.handle(0)),
        2 => buffer_sender(start.handle(0)),
        _ => misuse(start),
    }
}

/// Log every message; exit after the first, so seq 2 and 3 can only reach a
/// restarted instance.
fn buffer_receiver(channel: Handle) -> ! {
    loop {
        match recv(channel) {
            Ok(message) => {
                log!("[buf] got seq={}", message.payload[0]);
                if message.payload[0] == 1 {
                    exit()
                }
            }
            Err(e) => log!("[buf] recv failed: {e:?}"),
        }
    }
}

/// Send seq 1, which ends the receiver's first run, then seq 2 and 3, and
/// exit. Nothing here waits: the channel is FIFO, so whenever 2 and 3 are
/// sent, they are queued behind 1 and outlive the instance that takes 1.
fn buffer_sender(channel: Handle) -> ! {
    for seq in 1..=3 {
        match send(channel, &Message::new(0).with_data(0, seq)) {
            Ok(()) => log!("[buf] sent seq={seq}"),
            Err(e) => log!("[buf] send seq={seq} failed: {e:?}"),
        }
    }
    exit()
}

fn misuse(start: Startup) -> ! {
    let sink = start.handle(0);
    let own = start.handle(1);
    // An untyped message (tag 0), so the trace never shows it as a PING.
    let message = Message::new(0);
    let msg_ptr = &message as *const Message as u64;

    report("ungranted-send", send(Handle(5), &message));
    report("recv-on-send-only", try_recv(sink).map(|_| ()));
    report("send-from-kernel-memory", raw(abi::sys::SEND, sink.0 as u64, 0x4000_0000));
    report("send-from-null", raw(abi::sys::SEND, sink.0 as u64, 0));
    report("send-unaligned", raw(abi::sys::SEND, sink.0 as u64, msg_ptr + 1));
    report(
        "send-straddles-window-end",
        raw(abi::sys::SEND, sink.0 as u64, abi::USER_BASE + abi::USER_SIZE - 8),
    );
    report("recv-into-code", raw(abi::sys::TRY_RECV, own.0 as u64, main as *const () as u64));
    report("log-from-kernel-memory", raw(abi::sys::LOG, 0x4000_0000, 16));

    // Nobody receives on the sink: 16 sends queue, the 17th is refused. If any
    // refused send above had queued a message, fewer than 16 would succeed.
    let mut sent = 0;
    let mut result = Ok(());
    for _ in 0..17 {
        result = send(sink, &message);
        if result.is_err() {
            break;
        }
        sent += 1;
    }
    match result {
        Ok(()) => log!("[test] queue-full result=ok code=0 after={sent}"),
        Err(e) => log!("[test] queue-full result=refused code={} after={sent}", e as i64),
    }

    report("spawn-not-init", spawn("x", "PONG.ELF", &[], 0).map(|_| ()));

    let long = [b'L'; 300];
    freshos_rt::log(core::str::from_utf8(&long).unwrap_or(""));
    let bad_utf8: &[u8] = b"bad\xffutf8";
    let _ = raw(abi::sys::LOG, bad_utf8.as_ptr() as u64, bad_utf8.len() as u64);
    freshos_rt::log("fake\n[init] spoof");

    log!("[test] done");
    exit()
}
