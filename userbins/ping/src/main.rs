#![no_std]
#![no_main]

use freshos_rt::{Error, Message, Startup, entry, log, recv, recv_until, send, tag, time_ns};

entry!(main);

const SAMPLES: usize = 100;

/// Measures ping/pong round trips in batches of 100 and logs each batch's
/// median, then waits a second. Handles: 0 = SEND ping, 1 = RECV pong.
fn main(start: Startup) -> ! {
    let pings = start.handle(0);
    let pongs = start.handle(1);
    loop {
        let mut rtts = [0u64; SAMPLES];
        for rtt in rtts.iter_mut() {
            let sent = time_ns();
            if send(pings, &Message::new(tag::PING).with_data(0, sent)).is_err() {
                continue;
            }
            if let Ok(reply) = recv(pongs) {
                *rtt = time_ns().saturating_sub(reply.payload[0]);
            }
        }
        rtts.sort_unstable();
        log!("rtt_median_ns={} samples={}", rtts[SAMPLES / 2], SAMPLES);
        // Nothing arrives on pongs between batches, so this is a one-second sleep.
        match recv_until(pongs, time_ns() + 1_000_000_000) {
            Ok(_) | Err(Error::Timeout) => {}
            Err(e) => log!("wait failed: {e:?}"),
        }
    }
}
