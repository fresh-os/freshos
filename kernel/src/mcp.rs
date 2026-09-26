/// MCP bridge — agents can read FreshOS's state over MCP (decision 0005).
///
/// Speaks MCP's stdio transport (newline-delimited JSON-RPC 2.0) over the
/// board's second PL011 UART. On QEMU, `run-arm.sh` exposes that UART as the
/// Unix socket `mcp.sock`, so any MCP client can connect with `nc -U mcp.sock`.
///
/// Each piece of state is offered twice: as a tool (what agents call most
/// readily) and as a resource at `freshos://<name>` (what 0005 names it).
///
/// This first version is read-only: every tool reports kernel state and none
/// changes it. Write tools wait for capabilities (roadmap M5). Until then the
/// bridge runs as a built-in EL1 service, like the rest of the desktop on this
/// path, and reads kernel state directly rather than through granted handles.
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use serde_json::{Value, json};

use crate::arch;
use crate::arch::board;
use crate::ipc;
use crate::serial::serial_println;

/// Longest request line accepted. Longer lines are discarded whole.
const MAX_LINE: usize = 16 * 1024;

/// Protocol version offered when the client doesn't name one.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// One piece of observable state, offered as both an MCP tool and a resource.
struct View {
    name: &'static str,
    description: &'static str,
    read: fn() -> Value,
}

const VIEWS: &[View] = &[
    View {
        name: "system",
        description: "Board, uptime, task and IPC channel counts, and kernel heap usage.",
        read: system_view,
    },
    View {
        name: "services",
        description: "Every service started so far, from the kernel's registry: its task, whether it is running, restart and exit counts, and why it last exited.",
        read: services_view,
    },
    View {
        name: "tasks",
        description: "Running tasks by id, with the names the kernel registered for them.",
        read: tasks_view,
    },
    View {
        name: "message_trace",
        description: "The last 64 IPC messages, oldest first: sender, receiver, channel and message type. The same data the dashboard's flow view draws.",
        read: trace_view,
    },
    View {
        name: "metrics",
        description: "Latency metrics in microseconds (latest and max): compositor frame phases, input-to-photon and scheduler wake-up; IPC delivery time in nanoseconds.",
        read: metrics_view,
    },
];

pub fn bridge_el1() -> ! {
    let Some(base) = board::MCP_UART_BASE else {
        serial_println!("[mcp] no MCP UART on {}; bridge not started", board::NAME);
        arch::context::terminate_current(freshos_abi::ExitReason::Clean);
    };
    arch::pl011_enable(base);
    serial_println!("[mcp] listening on the second UART");

    let mut line: Vec<u8> = Vec::new();
    let mut discarding = false;
    loop {
        // Drain everything waiting before yielding, so a request arrives in
        // one pass rather than one byte per timer tick.
        while let Some(byte) = arch::pl011_try_read(base) {
            match byte {
                b'\n' => {
                    if discarding {
                        discarding = false;
                        reply(base, &error(Value::Null, -32700, "request too long"));
                    } else if let Some(response) = handle_line(&line) {
                        reply(base, &response);
                    }
                    line.clear();
                }
                b'\r' => {}
                _ if discarding => {}
                _ if line.len() >= MAX_LINE => {
                    discarding = true;
                    line.clear();
                }
                _ => line.push(byte),
            }
        }
        arch::interrupt_enable();
        arch::halt();
    }
}

/// Handle one JSON-RPC message. Returns the response, or `None` for
/// notifications and for anything that isn't a request.
fn handle_line(line: &[u8]) -> Option<Value> {
    if line.iter().all(u8::is_ascii_whitespace) {
        return None;
    }
    let Ok(message) = serde_json::from_slice::<Value>(line) else {
        return Some(error(Value::Null, -32700, "parse error"));
    };
    let method = message.get("method")?.as_str()?;
    // Notifications carry no id and never get a reply.
    let id = message.get("id")?.clone();
    let params = message.get("params").cloned().unwrap_or(Value::Null);

    serial_println!("[mcp] {}", method);
    Some(match dispatch(method, &params) {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, text)) => error(id, code, text),
    })
}

fn dispatch(method: &str, params: &Value) -> Result<Value, (i64, &'static str)> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(PROTOCOL_VERSION),
            "capabilities": { "tools": {}, "resources": {} },
            "serverInfo": { "name": "freshos", "version": env!("CARGO_PKG_VERSION") },
            "instructions": "A read-only view of a running FreshOS kernel. Every tool reports current state; none changes it.",
        })),
        "ping" => Ok(json!({})),
        "tools/list" => {
            let tools: Vec<Value> = VIEWS
                .iter()
                .map(|view| {
                    json!({
                        "name": view.name,
                        "description": view.description,
                        "inputSchema": { "type": "object", "properties": {} },
                        "annotations": { "readOnlyHint": true },
                    })
                })
                .collect();
            Ok(json!({ "tools": tools }))
        }
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or((-32602, "missing tool name"))?;
            let view = find_view(name).ok_or((-32602, "unknown tool"))?;
            Ok(json!({
                "content": [{ "type": "text", "text": render(view) }],
                "isError": false,
            }))
        }
        "resources/list" => {
            let resources: Vec<Value> = VIEWS
                .iter()
                .map(|view| {
                    json!({
                        "uri": uri(view),
                        "name": view.name,
                        "description": view.description,
                        "mimeType": "application/json",
                    })
                })
                .collect();
            Ok(json!({ "resources": resources }))
        }
        "resources/read" => {
            let requested = params
                .get("uri")
                .and_then(Value::as_str)
                .ok_or((-32602, "missing uri"))?;
            let view = requested
                .strip_prefix("freshos://")
                .and_then(find_view)
                .ok_or((-32002, "resource not found"))?;
            Ok(json!({
                "contents": [{ "uri": requested, "mimeType": "application/json", "text": render(view) }],
            }))
        }
        _ => Err((-32601, "method not found")),
    }
}

fn find_view(name: &str) -> Option<&'static View> {
    VIEWS.iter().find(|view| view.name == name)
}

fn uri(view: &View) -> String {
    format!("freshos://{}", view.name)
}

fn render(view: &View) -> String {
    serde_json::to_string(&(view.read)()).unwrap_or_default()
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn reply(base: usize, message: &Value) {
    let Ok(text) = serde_json::to_string(message) else {
        return;
    };
    for byte in text.bytes() {
        arch::pl011_write_byte(base, byte);
    }
    arch::pl011_write_byte(base, b'\n');
}

// ============================================================================
// Views
// ============================================================================

fn system_view() -> Value {
    json!({
        "board": board::NAME,
        "uptime_ns": arch::time_ns(),
        "tasks": arch::context::task_count(),
        "ipc_channels": ipc::channel_count(),
        "frames_free": crate::frame_alloc::free_count(),
        "heap": { "used_bytes": crate::heap::used(), "total_bytes": crate::heap::total() },
    })
}

fn services_view() -> Value {
    // Iterate the snapshot by reference: moving the 32-entry array through
    // iterator adapters costs several copies of it per layer in a debug
    // build, which overflowed the bridge's 16 KiB kernel stack.
    let snapshot = crate::registry::services();
    let services: Vec<Value> = snapshot
        .iter()
        .flatten()
        .map(|s| {
            json!({
                "name": s.name.as_str(),
                "task": s.task,
                "running": s.task.is_some(),
                "restarts": s.starts.saturating_sub(1),
                "exits": s.exits,
                "last_exit": s.last_exit.map(|r| r.as_str()),
            })
        })
        .collect();
    json!(services)
}

fn tasks_view() -> Value {
    let tasks: Vec<Value> = (0..arch::context::MAX_TASKS)
        .filter_map(|id| {
            let name = crate::registry::name(id);
            (!name.is_empty()).then(|| json!({ "id": id, "name": name.as_str() }))
        })
        .collect();
    json!(tasks)
}

fn trace_view() -> Value {
    const EMPTY: ipc::TraceEntry = ipc::TraceEntry {
        timestamp_ns: 0,
        from_task: 0,
        to_task: 0,
        channel: 0,
        tag: 0,
    };
    const TRACE_LEN: usize = 64; // the kernel's trace ring size
    let mut entries = [EMPTY; TRACE_LEN];
    let count = ipc::trace_read(&mut entries, TRACE_LEN);
    let messages: Vec<Value> = entries[..count]
        .iter()
        .map(|entry| {
            // 0xFFFF: no receiver was attributed when the message was sent.
            let to = (entry.to_task != 0xFFFF).then(|| task_json(entry.to_task as usize));
            json!({
                "timestamp_ns": entry.timestamp_ns,
                "from": task_json(entry.from_task as usize),
                "to": to,
                "channel": entry.channel,
                "type": crate::arm_tasks::tag_label(entry.tag),
                "tag": entry.tag,
            })
        })
        .collect();
    json!(messages)
}

fn task_json(id: usize) -> Value {
    let name = crate::registry::name(id);
    json!({ "id": id, "name": if name.is_empty() { Value::Null } else { json!(name.as_str()) } })
}

fn metrics_view() -> Value {
    let snapshot = crate::metrics::snapshot();
    let sample = |s: crate::metrics::MetricSample| json!({ "latest_us": s.latest, "max_us": s.max });
    json!({
        "frame": sample(snapshot.frame_us),
        "present": sample(snapshot.present_us),
        "background": sample(snapshot.background_us),
        "windows": sample(snapshot.windows_us),
        "chrome": sample(snapshot.chrome_us),
        "input_to_photon": sample(snapshot.input_to_photon_us),
        "ipc_delivery": { "latest_ns": snapshot.ipc_delivery_ns.latest, "max_ns": snapshot.ipc_delivery_ns.max },
        "scheduler_wake": sample(snapshot.sched_wake_us),
    })
}
