#![no_std]
#![no_main]

use freshos_rt::{Message, Startup, entry, recv, send, tag};

entry!(main);

/// Echoes every PING back as a PONG. Handles: 0 = RECV ping, 1 = SEND pong.
fn main(start: Startup) -> ! {
    let pings = start.handle(0);
    let pongs = start.handle(1);
    loop {
        if let Ok(ping) = recv(pings) {
            let _ = send(pongs, &Message::new(tag::PONG).with_data(0, ping.payload[0]));
        }
    }
}
