/// Inter-process communication — typed message-passing on channels.
///
/// This is the spine. Every feature in FreshOS is a consequence of this
/// working and being fast.
///
/// A **channel** is a bounded, unidirectional ring buffer. Tasks send
/// typed **messages** into a channel; another task receives them. If the
/// channel is empty, `recv` blocks the calling task until a message
/// arrives — the sender wakes the blocked receiver automatically.
///
/// Messages carry a type tag and 32 bytes of inline payload (4 × u64).
/// Enough for coordinates, commands, small data. Bulk data will use
/// shared-memory capabilities later — the message carries the handle,
/// not the bytes.
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

// ---------------------------------------------------------------------------
// Message — the unit of communication
// ---------------------------------------------------------------------------

pub const MSG_PING: u32 = 1;
pub const MSG_PONG: u32 = 2;
pub const MSG_IRQ: u32 = 10; // kernel → driver: raw interrupt data
pub const MSG_MOUSE_RAW: u32 = 11; // kernel → mouse driver: raw byte
pub const MSG_MOUSE: u32 = 12; // mouse driver → compositor: x, y, buttons
pub const MSG_KEY_DOWN: u32 = 20;
pub const MSG_KEY_UP: u32 = 21;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Message {
    pub tag: u32,
    pub sender: u16,
    pub len: u16,
    pub payload: [u64; 4], // 32 bytes inline
}

impl Message {
    pub const fn empty() -> Self {
        Self {
            tag: 0,
            sender: 0,
            len: 0,
            payload: [0; 4],
        }
    }

    pub fn new(tag: u32) -> Self {
        Self {
            tag,
            sender: crate::arch::current_task() as u16,
            len: 0,
            payload: [0; 4],
        }
    }

    pub fn with_data(mut self, slot: usize, value: u64) -> Self {
        if slot < 4 {
            self.payload[slot] = value;
            let end = ((slot + 1) * 8) as u16;
            if end > self.len {
                self.len = end;
            }
        }
        self
    }
}

// ---------------------------------------------------------------------------
// Channel — bounded ring buffer with blocking receive
// ---------------------------------------------------------------------------

const MAX_CHANNELS: usize = 32;
const CHANNEL_CAP: usize = 16;

#[derive(Clone, Copy)]
struct Channel {
    active: bool,
    buf: [Message; CHANNEL_CAP],
    head: usize,
    tail: usize,
    count: usize,
    waiter: Option<usize>, // task blocked on recv
    consumer: u16,         // last task to recv on this channel (0xFFFF = none yet)
}

const EMPTY_CHANNEL: Channel = Channel {
    active: false,
    buf: [Message::empty(); CHANNEL_CAP],
    head: 0,
    tail: 0,
    count: 0,
    waiter: None,
    consumer: 0xFFFF,
};

struct ChannelsCell(UnsafeCell<[Channel; MAX_CHANNELS]>);
unsafe impl Sync for ChannelsCell {}

static CHANNELS: ChannelsCell = ChannelsCell(UnsafeCell::new([EMPTY_CHANNEL; MAX_CHANNELS]));
static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

fn channels() -> *mut [Channel; MAX_CHANNELS] {
    CHANNELS.0.get()
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum Error {
    InvalidChannel,
    Full,
    NoCapacity,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Create a new channel. Returns the channel ID.
pub fn create() -> Result<u32, Error> {
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    if id >= MAX_CHANNELS {
        return Err(Error::NoCapacity);
    }
    let ch = unsafe { &mut (*channels())[id] };
    *ch = EMPTY_CHANNEL;
    ch.active = true;
    Ok(id as u32)
}

/// Send a message on a channel. Non-blocking.
///
/// If a task is blocked waiting to receive on this channel, it is woken.
pub fn send(channel_id: u32, msg: &Message) -> Result<(), Error> {
    let id = channel_id as usize;
    if id >= MAX_CHANNELS {
        return Err(Error::InvalidChannel);
    }
    let ch = unsafe { &mut (*channels())[id] };
    if !ch.active {
        return Err(Error::InvalidChannel);
    }
    if ch.count >= CHANNEL_CAP {
        return Err(Error::Full);
    }

    // Record who will receive (before we take() the waiter). Prefer the task
    // currently blocked on recv; otherwise fall back to the channel's learned
    // consumer, so the trace attributes a destination even when delivery was
    // buffered rather than handed to a blocked waiter.
    let receiver = match ch.waiter {
        Some(w) => w,
        None if ch.consumer != 0xFFFF => ch.consumer as usize,
        None => 0xFFFF,
    };
    let now_ns = crate::arch::time_ns();

    ch.buf[ch.head] = *msg;
    ch.head = (ch.head + 1) % CHANNEL_CAP;
    ch.count += 1;

    // Trace this message
    trace_record(TraceEntry {
        timestamp_ns: now_ns,
        from_task: crate::arch::current_task() as u16,
        to_task: receiver as u16,
        channel: channel_id as u16,
        tag: msg.tag as u16,
    });

    // Wake the blocked receiver
    if let Some(task_id) = ch.waiter.take() {
        crate::metrics::note_task_unblocked(task_id, now_ns);
        crate::arch::unblock_task(task_id);
    }

    Ok(())
}

/// Receive a message from a channel. Blocks if the channel is empty.
///
/// The calling task sleeps until a message is available, consuming zero
/// CPU while blocked. The sender's `send()` call wakes the receiver.
pub fn recv(channel_id: u32) -> Result<Message, Error> {
    let id = channel_id as usize;
    if id >= MAX_CHANNELS {
        return Err(Error::InvalidChannel);
    }

    loop {
        // CLI: prevent timer interrupt between empty-check and block
        crate::arch::interrupt_disable();

        let ch = unsafe { &mut (*channels())[id] };
        if !ch.active {
            crate::arch::interrupt_enable();
            return Err(Error::InvalidChannel);
        }

        // Learn this channel's consumer so future sends can attribute a
        // destination even when no task is blocked waiting.
        ch.consumer = crate::arch::current_task() as u16;

        if ch.count > 0 {
            let msg = ch.buf[ch.tail];
            ch.tail = (ch.tail + 1) % CHANNEL_CAP;
            ch.count -= 1;
            crate::arch::interrupt_enable();
            return Ok(msg);
        }

        // Empty — register as waiter and sleep.
        // block_current() does STI+HLT atomically, so we can't miss a
        // wakeup between registering and sleeping.
        ch.waiter = Some(crate::arch::current_task());
        crate::arch::block_current_task();
        crate::metrics::note_task_running(crate::arch::current_task(), crate::arch::time_ns());
        // Woken — loop and check again
    }
}

/// Try to receive without blocking. Returns None if empty.
pub fn try_recv(channel_id: u32) -> Option<Message> {
    let id = channel_id as usize;
    if id >= MAX_CHANNELS {
        return None;
    }
    let ch = unsafe { &mut (*channels())[id] };
    if !ch.active {
        return None;
    }
    ch.consumer = crate::arch::current_task() as u16;
    if ch.count == 0 {
        return None;
    }
    let msg = ch.buf[ch.tail];
    ch.tail = (ch.tail + 1) % CHANNEL_CAP;
    ch.count -= 1;
    Some(msg)
}

/// How many channels are active.
pub fn channel_count() -> usize {
    NEXT_ID.load(Ordering::SeqCst)
}

// ---------------------------------------------------------------------------
// Trace buffer — records every message for the visualisation
// ---------------------------------------------------------------------------

const TRACE_SIZE: usize = 64;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TraceEntry {
    pub timestamp_ns: u64,
    pub from_task: u16,
    pub to_task: u16, // 0xFFFF = no receiver was waiting
    pub channel: u16,
    pub tag: u16,
}

const EMPTY_TRACE: TraceEntry = TraceEntry {
    timestamp_ns: 0,
    from_task: 0,
    to_task: 0,
    channel: 0,
    tag: 0,
};

struct TraceCell(UnsafeCell<TraceBuffer>);
unsafe impl Sync for TraceCell {}

struct TraceBuffer {
    entries: [TraceEntry; TRACE_SIZE],
    head: usize,
    count: usize,
}

static TRACE: TraceCell = TraceCell(UnsafeCell::new(TraceBuffer {
    entries: [EMPTY_TRACE; TRACE_SIZE],
    head: 0,
    count: 0,
}));

fn trace_record(entry: TraceEntry) {
    let tb = unsafe { &mut *TRACE.0.get() };
    tb.entries[tb.head] = entry;
    tb.head = (tb.head + 1) % TRACE_SIZE;
    if tb.count < TRACE_SIZE {
        tb.count += 1;
    }
}

/// Copy the last `max` trace entries into `buf` (oldest first).
/// Returns the number written.
pub fn trace_read(buf: &mut [TraceEntry], max: usize) -> usize {
    let tb = unsafe { &*TRACE.0.get() };
    let n = tb.count.min(max).min(buf.len());
    if n == 0 {
        return 0;
    }
    // Start index (oldest entry in the window)
    let start = if tb.count >= TRACE_SIZE {
        tb.head // buffer full, head points to oldest
    } else {
        0
    };
    // Copy the last `n` entries, oldest first
    let skip = tb.count - n;
    for i in 0..n {
        let idx = (start + skip + i) % TRACE_SIZE;
        buf[i] = tb.entries[idx];
    }
    n
}
