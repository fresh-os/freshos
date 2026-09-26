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

pub use freshos_abi::Message;

pub const MSG_PING: u32 = freshos_abi::tag::PING;
pub const MSG_PONG: u32 = freshos_abi::tag::PONG;
pub const MSG_IRQ: u32 = freshos_abi::tag::IRQ;
pub const MSG_MOUSE_RAW: u32 = freshos_abi::tag::MOUSE_RAW;
pub const MSG_MOUSE: u32 = freshos_abi::tag::MOUSE;
pub const MSG_KEY_DOWN: u32 = freshos_abi::tag::KEY_DOWN;
pub const MSG_KEY_UP: u32 = freshos_abi::tag::KEY_UP;

// ---------------------------------------------------------------------------
// Channel — bounded ring buffer with blocking receive
// ---------------------------------------------------------------------------

const MAX_CHANNELS: usize = 32;
const CHANNEL_CAP: usize = 16;

/// Channels the kernel creates at boot, in this order. Everything else is
/// created by init.
pub const KBD_EVENTS: u32 = 0;
pub const SHELL_KEYS: u32 = 1;
pub const INIT_INBOX: u32 = 2;

#[derive(Clone, Copy)]
struct Queued {
    msg: Message,
    sent_ns: u64,
}
const EMPTY_QUEUED: Queued = Queued { msg: Message::empty(), sent_ns: 0 };

#[derive(Clone, Copy)]
struct Channel {
    active: bool,
    buf: [Queued; CHANNEL_CAP],
    head: usize,
    tail: usize,
    count: usize,
    waiter: Option<usize>, // task blocked on recv
    consumer: u16,         // last task to recv on this channel (0xFFFF = none yet)
    receiver: u16,         // task holding RECV on this channel (0xFFFF = none)
    /// Where RECV returns when `receiver` exits: the task and handle slot it
    /// was moved from by spawn. None when the receiver holds it natively.
    recv_home: Option<(u16, u32)>,
}

const EMPTY_CHANNEL: Channel = Channel {
    active: false,
    buf: [EMPTY_QUEUED; CHANNEL_CAP],
    head: 0,
    tail: 0,
    count: 0,
    waiter: None,
    consumer: 0xFFFF,
    receiver: 0xFFFF,
    recv_home: None,
};

struct ChannelsCell(UnsafeCell<[Channel; MAX_CHANNELS]>);
unsafe impl Sync for ChannelsCell {}

static CHANNELS: ChannelsCell = ChannelsCell(UnsafeCell::new([EMPTY_CHANNEL; MAX_CHANNELS]));
static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

fn channels() -> *mut [Channel; MAX_CHANNELS] {
    CHANNELS.0.get()
}

/// Kernel notices waiting for room in a full channel, oldest first. Only
/// touched with IRQs masked. Sized for one exit notice per task slot, twice
/// over: init has at most one instance of each service in flight.
const BACKLOG_CAP: usize = 32;

struct Backlog {
    entries: [(u32, Message); BACKLOG_CAP],
    len: usize,
}

impl Backlog {
    fn pending(&self, channel_id: u32) -> bool {
        self.entries[..self.len].iter().any(|(c, _)| *c == channel_id)
    }

    fn push(&mut self, channel_id: u32, message: Message) -> bool {
        if self.len == BACKLOG_CAP {
            return false;
        }
        self.entries[self.len] = (channel_id, message);
        self.len += 1;
        true
    }

    fn pop(&mut self, channel_id: u32) -> Option<Message> {
        let index = self.entries[..self.len].iter().position(|(c, _)| *c == channel_id)?;
        let (_, message) = self.entries[index];
        self.entries.copy_within(index + 1..self.len, index);
        self.len -= 1;
        Some(message)
    }
}

struct BacklogCell(UnsafeCell<Backlog>);
unsafe impl Sync for BacklogCell {}

static BACKLOG: BacklogCell = BacklogCell(UnsafeCell::new(Backlog {
    entries: [(0, Message::empty()); BACKLOG_CAP],
    len: 0,
}));

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
/// If a task is blocked waiting to receive on this channel, it is woken. For
/// an EL0 task waiting in the recv syscall, the message is delivered straight
/// into its buffer and `Some(task)` is returned: that task is ready, and the
/// caller may run it next.
pub fn send(channel_id: u32, msg: &Message) -> Result<Option<usize>, Error> {
    send_from(crate::arch::current_task() as u16, channel_id, msg)
}

/// Send as the kernel itself (sender 0), e.g. TASK_EXITED. A kernel notice
/// is never lost to a full queue: it waits in the kernel's backlog, and moves
/// into the channel, in order, as the receiver makes room.
pub fn send_as_kernel(channel_id: u32, msg: &Message) -> Result<Option<usize>, Error> {
    let _irq = crate::arch::IrqGuard::mask();
    let ch = channel_mut(channel_id)?;
    let backlog = unsafe { &mut *BACKLOG.0.get() };
    if ch.count >= CHANNEL_CAP || backlog.pending(channel_id) {
        let mut stamped = *msg;
        stamped.sender = 0;
        if backlog.push(channel_id, stamped) {
            return Ok(None);
        }
        crate::serial::serial_println!(
            "ipc: kernel backlog full; notice tag {} on channel {} lost",
            msg.tag,
            channel_id
        );
        return Err(Error::Full);
    }
    send_from(0, channel_id, msg)
}

fn send_from(sender: u16, channel_id: u32, msg: &Message) -> Result<Option<usize>, Error> {
    // Built-ins send with IRQs enabled. Delivery must not interleave with a
    // tick's deadline expiry, or a message could be dequeued for a wait that
    // has just timed out, and lost.
    let _irq = crate::arch::IrqGuard::mask();
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
        None if ch.receiver != 0xFFFF => ch.receiver as usize,
        None if ch.consumer != 0xFFFF => ch.consumer as usize,
        None => 0xFFFF,
    };
    let now_ns = crate::arch::time_ns();

    // The kernel decides who sent a message, never the sender (spec §4).
    let mut stamped = *msg;
    stamped.sender = sender;
    ch.buf[ch.head] = Queued { msg: stamped, sent_ns: now_ns };
    ch.head = (ch.head + 1) % CHANNEL_CAP;
    ch.count += 1;

    // Trace this message
    trace_record(TraceEntry {
        timestamp_ns: now_ns,
        from_task: sender,
        to_task: receiver as u16,
        channel: channel_id as u16,
        tag: msg.tag as u16,
    });

    // Wake the waiting receiver. A user task waiting in recv gets the message
    // delivered straight into its buffer and is ready to run; the caller may
    // hand the CPU to it.
    if let Some(task_id) = ch.waiter.take() {
        crate::metrics::note_task_unblocked(task_id, now_ns);
        if crate::arch::context::has_user_wait(task_id)
            && let Some(message) = dequeue(ch)
        {
            crate::arch::context::complete_user_recv(task_id, &message);
            return Ok(Some(task_id));
        }
        crate::arch::unblock_task(task_id);
    }
    Ok(None)
}

/// Take the oldest message, recording how long it waited to be delivered.
fn dequeue(ch: &mut Channel) -> Option<Message> {
    if ch.count == 0 {
        return None;
    }
    let queued = ch.buf[ch.tail];
    ch.tail = (ch.tail + 1) % CHANNEL_CAP;
    ch.count -= 1;
    let now = crate::arch::time_ns();
    crate::metrics::record_ipc_delivery_ns(now.saturating_sub(queued.sent_ns));
    Some(queued.msg)
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

        if let Some(msg) = dequeue(ch) {
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
    dequeue(ch)
}

/// Take a message without blocking (for syscalls).
pub fn try_dequeue(channel_id: u32) -> Result<Option<Message>, Error> {
    let ch = channel_mut(channel_id)?;
    ch.consumer = crate::arch::current_task() as u16;
    let message = dequeue(ch);
    if message.is_some() {
        refill_from_backlog(channel_id);
    }
    Ok(message)
}

/// The receiver made room: move waiting kernel notices into the channel.
fn refill_from_backlog(channel_id: u32) {
    let backlog = unsafe { &mut *BACKLOG.0.get() };
    while backlog.pending(channel_id) {
        let Ok(ch) = channel_mut(channel_id) else { return };
        if ch.count >= CHANNEL_CAP {
            return;
        }
        let Some(message) = backlog.pop(channel_id) else { return };
        let _ = send_from(0, channel_id, &message);
    }
}

/// Record `task` as waiting on `channel_id` (it must hold the channel's RECV).
pub fn register_waiter(channel_id: u32, task: usize) -> Result<(), Error> {
    channel_mut(channel_id)?.waiter = Some(task);
    Ok(())
}

/// Forget `task`'s wait on `channel_id` (deadline passed, or the task exited).
pub fn cancel_waiter(channel_id: u32, task: usize) {
    if let Ok(ch) = channel_mut(channel_id)
        && ch.waiter == Some(task)
    {
        ch.waiter = None;
    }
}

/// `task` holds RECV on `channel_id` natively (it created the channel, or the
/// kernel gave it the right at boot).
pub fn set_receiver(channel_id: u32, task: usize) {
    if let Ok(ch) = channel_mut(channel_id) {
        ch.receiver = task as u16;
        ch.recv_home = None;
    }
}

/// `task` now holds RECV on `channel_id`, moved from `home_task`'s slot `home_slot`.
pub fn set_receiver_with_home(channel_id: u32, task: usize, home_task: usize, home_slot: u32) {
    if let Ok(ch) = channel_mut(channel_id) {
        ch.receiver = task as u16;
        ch.recv_home = Some((home_task as u16, home_slot));
    }
}

/// The receiver of `channel_id`, if any.
pub fn receiver(channel_id: u32) -> Option<usize> {
    channel_mut(channel_id)
        .ok()
        .and_then(|ch| (ch.receiver != 0xFFFF).then_some(ch.receiver as usize))
}

/// `task`, the receiver of `channel_id`, exited. Queued messages stay; the
/// right goes back to where it came from. Returns that home (task, slot), or
/// None if the right had no home, in which case the channel has no receiver.
pub fn receiver_exited(channel_id: u32, task: usize) -> Option<(usize, u32)> {
    let ch = channel_mut(channel_id).ok()?;
    if ch.receiver != task as u16 {
        return None;
    }
    if ch.waiter == Some(task) {
        ch.waiter = None;
    }
    match ch.recv_home.take() {
        Some((home_task, home_slot)) => {
            ch.receiver = home_task;
            Some((home_task as usize, home_slot))
        }
        None => {
            ch.receiver = 0xFFFF;
            None
        }
    }
}

fn channel_mut(channel_id: u32) -> Result<&'static mut Channel, Error> {
    let id = channel_id as usize;
    if id >= MAX_CHANNELS {
        return Err(Error::InvalidChannel);
    }
    let ch = unsafe { &mut (*channels())[id] };
    if ch.active { Ok(ch) } else { Err(Error::InvalidChannel) }
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
