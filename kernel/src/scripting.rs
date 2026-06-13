/// Scripting integration — Rhai with native access to the message layer.
///
/// "Every component in the system exposes a message-based automation
/// interface by design. A lightweight scripting runtime where any two
/// components can be glued together in three lines."
///                                          — FreshOS Manifesto
///
/// The scripting engine runs in ring 0 (kernel context) for now. Scripts
/// have access to `send()`, `recv()`, `time()`, and `print()`. A future
/// version would run scripts in ring 3 with syscall bindings.
use alloc::format;
use alloc::string::String;
use rhai::{Engine, EvalAltResult, Scope};

use crate::arch;
use crate::ipc;
use crate::serial::serial_println;

/// Create a Rhai engine with FreshOS bindings.
pub fn create_engine() -> Engine {
    let mut engine = Engine::new();

    // send(channel, tag, payload0) — send a typed message
    engine.register_fn("send", |channel: i64, tag: i64, data: i64| -> i64 {
        let msg = ipc::Message {
            tag: tag as u32,
            sender: 0,
            len: 8,
            payload: [data as u64, 0, 0, 0],
        };
        match ipc::send(channel as u32, &msg) {
            Ok(()) => 0,
            Err(_) => -1,
        }
    });

    // recv(channel) — blocking receive, returns the first payload word
    engine.register_fn("recv", |channel: i64| -> i64 {
        match ipc::recv(channel as u32) {
            Ok(msg) => msg.payload[0] as i64,
            Err(_) => -1,
        }
    });

    // recv_tag(channel) — blocking receive, returns the tag
    engine.register_fn("recv_tag", |channel: i64| -> i64 {
        match ipc::recv(channel as u32) {
            Ok(msg) => msg.tag as i64,
            Err(_) => -1,
        }
    });

    // time() — current time in microseconds
    engine.register_fn("time", || -> i64 { (arch::time_ns() / 1000) as i64 });

    // print(message) — output to serial
    engine.register_fn("print", |s: String| {
        serial_println!("[script] {}", s);
    });

    // print(number)
    engine.register_fn("print", |n: i64| {
        serial_println!("[script] {}", n);
    });

    engine
}

/// Run a script string. Returns Ok or the error message.
pub fn run(script: &str) -> Result<(), String> {
    let engine = create_engine();
    let mut scope = Scope::new();

    serial_println!("  Running script...");

    match engine.eval_with_scope::<()>(&mut scope, script) {
        Ok(()) => {
            serial_println!("  Script completed.");
            Ok(())
        }
        Err(e) => {
            let msg = format!("Script error: {}", e);
            serial_println!("  {}", msg);
            Err(msg)
        }
    }
}
