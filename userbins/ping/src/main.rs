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
        let mut samples = 0;
        let mut failures = 0;
        for _ in 0..SAMPLES {
            let sent = time_ns();
            if send(pings, &Message::new(tag::PING).with_data(0, sent)).is_err() {
                failures += 1;
                continue;
            }
            match recv(pongs) {
                Ok(reply) => {
                    rtts[samples] = time_ns().saturating_sub(reply.payload[0]);
                    samples += 1;
                }
                Err(_) => failures += 1,
            }
        }
        // Only successful round trips count; a failure must not drag the
        // median towards zero.
        let rtts = &mut rtts[..samples];
        rtts.sort_unstable();
        let median = rtts.get(samples / 2).copied().unwrap_or(0);
        log!("rtt_median_ns={} samples={} failures={}", median, samples, failures);
        // Nothing arrives on pongs between batches, so this is a one-second sleep.
        match recv_until(pongs, time_ns() + 1_000_000_000) {
            Ok(_) | Err(Error::Timeout) => {}
            Err(e) => log!("wait failed: {e:?}"),
        }
    }
}
