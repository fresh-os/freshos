/// aarch64 scheduler — preemptive round-robin with per-task address spaces.
///
/// Supports both EL1 (kernel) and EL0 (user) tasks. Every task has a kernel
/// stack, which holds its saved frame and serves its EL0→EL1 transitions.
/// An EL0 task also owns an `AddressSpace`: its own top-level table, with its
/// code, data and stack in the private user window. EL1 tasks run on the
/// firmware's table.
///
/// The timer ISR (exception.s) saves all registers, calls
/// `scheduler_tick_arm(sp)`, and gets back the new SP. On a task switch the
/// scheduler also points TTBR0 at the next task's table.
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use freshos_abi::{
    Error, ExitReason, Handle, Rights, TaskRef, USER_BASE, USER_SIZE, USER_STACK_SIZE,
};

use crate::frame_alloc;
use crate::handles::HandleTable;
use crate::ipc::Message;
use crate::serial::serial_println;

use super::addrspace::{AddressSpace, Perm, PAGE};
use super::gic;
use super::timer;

pub const MAX_TASKS: usize = 16;
/// 32 KiB kernel stack per task. At 16 KiB the MCP bridge overflowed into
/// its neighbour (init) in a debug build, before `services_view` iterated
/// the registry by reference. Measured since then: the bridge peaks near
/// 14 KiB and init near 12 KiB (SPAWN's ELF load with a nested tick). The
/// rest is headroom for paths not measured: nested IRQ frames, Rhai in the
/// shell, deeper MCP views. The MCP `tasks` view reports each task's peak.
///
/// Kernel stacks have no guard page yet (that waits for later memory work),
/// so an overflow would silently corrupt the stack below. Instead each stack
/// ends in a canary that every switch checks: an overflow panics, naming the
/// task, at its next switch.
const KERNEL_STACK_SIZE: usize = 4096 * 8;
/// The canary: the lowest bytes of every kernel stack.
const CANARY: u64 = 0x57AC_C0DE_57AC_C0DE;
const CANARY_WORDS: usize = 8; // 64 bytes
/// The rest of a new kernel stack is painted with this byte, so the deepest
/// point it has reached can be read back (`stack_peak`).
const PAINT: u8 = 0xA5;

/// Size of the saved register frame that exception.s's save_all_regs pushes:
/// the GPRs, SP_EL0, ELR and SPSR at 0..272, then q0-q31, FPCR and FPSR,
/// then TPIDR_EL0 at 800 and 8 bytes of padding.
/// A new task's frame is all zeroes apart from the slots set below, so it
/// starts with zeroed FP/SIMD registers, FPCR = 0, FPSR = 0 and
/// TPIDR_EL0 = 0: nothing another task or the firmware left behind.
const FRAME_SIZE: u64 = 816;

#[derive(Clone, Copy, PartialEq)]
enum State {
    Free,
    /// Claimed by a spawn that is still building the task. The scheduler
    /// never runs it, and no other spawn can take it.
    Reserved,
    Ready,
    Running,
    Blocked,
}

/// An EL0 task blocked in the recv syscall: where its message goes, and
/// when it gives up.
#[derive(Clone, Copy)]
struct UserWait {
    channel: u32,
    buf: u64,
    deadline_ns: u64, // 0 = none
}

struct Task {
    sp: u64, // saved frame (kernel stack, after save_all_regs)
    kernel_stack_bottom: u64,
    kernel_stack_pages: usize,
    ttbr0: u64, // TTBR0_EL1 to load when this task runs
    space: Option<AddressSpace>,
    handles: HandleTable,
    wait: Option<UserWait>,
    state: State,
}

const EMPTY_TASK: Task = Task {
    sp: 0,
    kernel_stack_bottom: 0,
    kernel_stack_pages: 0,
    ttbr0: 0,
    space: None,
    handles: HandleTable::EMPTY,
    wait: None,
    state: State::Free,
};

#[derive(Clone, Copy)]
struct PendingFree {
    kernel_stack_bottom: u64,
    kernel_stack_pages: usize,
    used: bool,
}

const EMPTY_PENDING: PendingFree = PendingFree {
    kernel_stack_bottom: 0,
    kernel_stack_pages: 0,
    used: false,
};

struct TasksCell(UnsafeCell<[Task; MAX_TASKS]>);
unsafe impl Sync for TasksCell {}

static TASKS: TasksCell = TasksCell(UnsafeCell::new([EMPTY_TASK; MAX_TASKS]));
struct PendingCell(UnsafeCell<[PendingFree; MAX_TASKS]>);
unsafe impl Sync for PendingCell {}

static PENDING_FREES: PendingCell = PendingCell(UnsafeCell::new([EMPTY_PENDING; MAX_TASKS]));
static CURRENT: AtomicUsize = AtomicUsize::new(0);

/// The last direct hand-off: `DONOR` sent to `TARGET` and gave it the CPU.
/// NONE when there is no hand-off to return from. Only touched with IRQs
/// masked.
const NONE: usize = usize::MAX;
static DONOR: AtomicUsize = AtomicUsize::new(NONE);
static TARGET: AtomicUsize = AtomicUsize::new(NONE);
static COUNT: AtomicUsize = AtomicUsize::new(0);
/// The task running init (0 until the kernel starts it). Its exit halts the
/// system: nothing else supervises.
static INIT_TASK: AtomicUsize = AtomicUsize::new(0);
/// Each slot's generation, bumped on every install and never reset, so a
/// (slot, generation) pair names one task for as long as the kernel runs
/// (until 2^32 spawns into one slot).
static GENERATIONS: [AtomicU32; MAX_TASKS] = [const { AtomicU32::new(0) }; MAX_TASKS];

/// The task in slot `id` now (or last, if the slot is free).
pub fn task_ref(id: usize) -> TaskRef {
    let generation = GENERATIONS.get(id).map_or(0, |g| g.load(Ordering::SeqCst));
    TaskRef { id: id as u16, generation }
}

/// Paint a new kernel stack and write its canary.
fn prepare_stack(bottom: u64) {
    unsafe {
        core::ptr::write_bytes(bottom as *mut u8, PAINT, KERNEL_STACK_SIZE);
        for i in 0..CANARY_WORDS {
            *(bottom as *mut u64).add(i) = CANARY;
        }
    }
}

/// Panic if the outgoing task has overrun its kernel stack. Runs on every
/// switch, with IRQs masked.
fn check_canary(t: &[Task; MAX_TASKS], id: usize) {
    let task = &t[id];
    if task.state == State::Free || task.kernel_stack_pages == 0 {
        return;
    }
    let intact = (0..CANARY_WORDS)
        .all(|i| unsafe { *(task.kernel_stack_bottom as *const u64).add(i) } == CANARY);
    if !intact {
        panic!(
            "kernel stack overflow: task {} ({})",
            id,
            crate::registry::name(id).as_str()
        );
    }
}

/// The deepest `task` has reached into its kernel stack, in bytes, or None
/// for a free slot or the boot task (which runs on the firmware's stack).
pub fn stack_peak(task: usize) -> Option<usize> {
    let _irq = super::IrqGuard::mask();
    let t = unsafe { &*tasks() };
    let task = t.get(task)?;
    if matches!(task.state, State::Free | State::Reserved) || task.kernel_stack_pages == 0 {
        return None;
    }
    let painted = u64::from_ne_bytes([PAINT; 8]);
    let words = KERNEL_STACK_SIZE / 8;
    let base = task.kernel_stack_bottom as *const u64;
    let untouched = (CANARY_WORDS..words)
        .take_while(|&i| unsafe { *base.add(i) } == painted)
        .count();
    Some(KERNEL_STACK_SIZE - (CANARY_WORDS + untouched) * 8)
}

fn tasks() -> *mut [Task; MAX_TASKS] {
    TASKS.0.get()
}

fn pending_frees() -> *mut [PendingFree; MAX_TASKS] {
    PENDING_FREES.0.get()
}

/// TTBR0 for tasks without their own space: the firmware's table, ASID 0.
static KERNEL_TTBR0: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Point TTBR0 at `ttbr0` if it isn't already. Kernel mappings are global
/// and identical in every table, so the kernel keeps running across the
/// switch; ASIDs mean no TLB flush is needed.
fn activate(ttbr0: u64) {
    let current: u64;
    unsafe { core::arch::asm!("mrs {}, TTBR0_EL1", out(reg) current, options(nomem, nostack)) };
    if current != ttbr0 {
        unsafe {
            core::arch::asm!("msr TTBR0_EL1, {}", "isb", in(reg) ttbr0, options(nostack));
        }
    }
}

/// Claim a free slot for a spawn. The claim is made with IRQs masked, so two
/// spawns (say, init restarting a service while the shell starts another)
/// can never pick the same slot. The spawn then fills the slot with
/// `install`, or gives it back with `release`.
fn claim_slot() -> Option<usize> {
    let _irq = super::IrqGuard::mask();
    let t = unsafe { &mut *tasks() };
    let id = (1..MAX_TASKS).find(|&id| t[id].state == State::Free)?;
    t[id].state = State::Reserved;
    Some(id)
}

/// Give back a slot claimed by `claim_slot` whose spawn failed.
fn release(id: usize) {
    let _irq = super::IrqGuard::mask();
    let t = unsafe { &mut *tasks() };
    debug_assert!(t[id].state == State::Reserved);
    t[id] = EMPTY_TASK;
}

/// Fill a claimed slot with a ready task, in one step the scheduler can't
/// interrupt. `bind` runs in that same step, before the task can run: it
/// records the task's name and service and moves its receive rights, so
/// even a task that exits on its first tick is charged to the right service
/// and gives its rights back to the right home.
fn install(id: usize, task: Task, bind: impl FnOnce(TaskRef)) -> TaskRef {
    let _irq = super::IrqGuard::mask();
    let t = unsafe { &mut *tasks() };
    debug_assert!(t[id].state == State::Reserved);
    t[id] = task;
    GENERATIONS[id].fetch_add(1, Ordering::SeqCst);
    let task = task_ref(id);
    bind(task);
    COUNT.store(task_count(), Ordering::SeqCst);
    task
}

fn queue_pending_free(bottom: u64, pages: usize) {
    if pages == 0 {
        return;
    }

    let pending = unsafe { &mut *pending_frees() };
    for slot in pending.iter_mut() {
        if !slot.used {
            *slot = PendingFree {
                kernel_stack_bottom: bottom,
                kernel_stack_pages: pages,
                used: true,
            };
            return;
        }
    }

    panic!("pending free queue full");
}

fn reap_pending_frees(current_stack_ptr: u64) {
    // Spawns reap too, from preemptible tasks; a tick mid-reap must not free
    // the same stack twice.
    let _irq = super::IrqGuard::mask();
    let pending = unsafe { &mut *pending_frees() };
    for slot in pending.iter_mut() {
        if !slot.used {
            continue;
        }

        let current_on_kernel_stack = slot.kernel_stack_pages > 0
            && current_stack_ptr >= slot.kernel_stack_bottom
            && current_stack_ptr < slot.kernel_stack_bottom + slot.kernel_stack_pages as u64 * 4096;
        if current_on_kernel_stack {
            continue;
        }

        if slot.kernel_stack_pages > 0 {
            unsafe {
                frame_alloc::deallocate_contiguous(
                    slot.kernel_stack_bottom,
                    slot.kernel_stack_pages,
                )
            };
        }
        *slot = EMPTY_PENDING;
    }
}

/// Called from exception.s on every timer IRQ.
///
/// Receives the current task's saved SP (after save_all_regs pushed FRAME_SIZE bytes).
/// Returns the next task's saved SP to restore.
#[unsafe(no_mangle)]
extern "C" fn scheduler_tick_arm(stack_ptr: u64) -> u64 {
    reap_pending_frees(stack_ptr);
    let intid = gic::acknowledge();
    timer::handle_irq();
    gic::end_of_interrupt(intid);
    expire_waits(super::time_ns());
    // A tick always rotates: no hand-off is returned from across one, so
    // sender-return never keeps a pair on the CPU past its time slice.
    forget_hand_off();
    switch_away(stack_ptr)
}

/// Save the current task's frame and pick the next task to run. Returns the
/// frame to restore. Runs with IRQs masked (exception context).
///
/// Sender-return: if the current task was run by a hand-off and has now
/// blocked, and its sender is still ready, the sender runs next. A
/// request/response pair then goes straight back to the requester instead
/// of waiting up to a tick behind every task idling in wfi. Otherwise (the
/// task yielded, exited or was preempted) the choice is round-robin.
pub fn switch_away(frame: u64) -> u64 {
    let t = unsafe { &mut *tasks() };
    let cur = CURRENT.load(Ordering::SeqCst);
    check_canary(t, cur);
    if t[cur].state != State::Free {
        t[cur].sp = frame;
    }
    if t[cur].state == State::Running {
        t[cur].state = State::Ready;
    }
    let donor = DONOR.load(Ordering::SeqCst);
    let returning = TARGET.load(Ordering::SeqCst) == cur
        && t[cur].state == State::Blocked
        && donor < MAX_TASKS
        && t[donor].state == State::Ready;
    forget_hand_off();
    if returning {
        return run(t, donor);
    }
    let next = (1..=MAX_TASKS)
        .map(|step| (cur + step) % MAX_TASKS)
        .find(|&candidate| t[candidate].state == State::Ready)
        .unwrap_or(0);
    run(t, next)
}

/// Run `target` next (direct hand-off). The current task stays ready.
pub fn hand_off(frame: u64, target: usize) -> u64 {
    let t = unsafe { &mut *tasks() };
    // Only a ready task has a live saved frame; a Free slot's `sp` is stale.
    // Anything else gets an ordinary switch instead.
    if target >= MAX_TASKS || t[target].state != State::Ready {
        return switch_away(frame);
    }
    let cur = CURRENT.load(Ordering::SeqCst);
    check_canary(t, cur);
    if t[cur].state != State::Free {
        t[cur].sp = frame;
    }
    if t[cur].state == State::Running {
        t[cur].state = State::Ready;
    }
    DONOR.store(cur, Ordering::SeqCst);
    TARGET.store(target, Ordering::SeqCst);
    run(t, target)
}

fn forget_hand_off() {
    DONOR.store(NONE, Ordering::SeqCst);
    TARGET.store(NONE, Ordering::SeqCst);
}

fn run(t: &mut [Task; MAX_TASKS], next: usize) -> u64 {
    t[next].state = State::Running;
    CURRENT.store(next, Ordering::SeqCst);
    activate(t[next].ttbr0);
    t[next].sp
}

/// The current task's address space (None for kernel tasks).
pub fn current_space() -> Option<&'static AddressSpace> {
    let t = unsafe { &*tasks() };
    t[CURRENT.load(Ordering::SeqCst)].space.as_ref()
}

/// The current task's handle table (None for kernel tasks, which have none).
pub fn current_handles() -> Option<&'static HandleTable> {
    let t = unsafe { &*tasks() };
    let task = &t[CURRENT.load(Ordering::SeqCst)];
    task.space.as_ref().map(|_| &task.handles)
}

/// Block the current task in recv: the frame is saved by switch_away.
pub fn set_user_wait(channel: u32, buf: u64, deadline_ns: u64) {
    let t = unsafe { &mut *tasks() };
    let cur = CURRENT.load(Ordering::SeqCst);
    t[cur].wait = Some(UserWait { channel, buf, deadline_ns });
    t[cur].state = State::Blocked;
}

pub fn has_user_wait(task: usize) -> bool {
    let t = unsafe { &*tasks() };
    task < MAX_TASKS && t[task].wait.is_some()
}

/// Finish `task`'s recv: copy `message` into its buffer (validated when it
/// blocked) through the task's own address space, set its result, and make
/// it ready. Callers run with IRQs masked.
pub fn complete_user_recv(task: usize, message: &Message) {
    let t = unsafe { &mut *tasks() };
    let Some(wait) = t[task].wait.take() else { return };
    let result = match t[task].space.as_ref() {
        Some(space) => match super::addrspace::write_user(space, wait.buf, message) {
            Ok(()) => 0i64,
            Err(e) => e as i64,
        },
        None => Error::NotPermitted as i64,
    };
    set_saved_x0(t[task].sp, result);
    t[task].state = State::Ready;
}

/// Wake every task whose recv deadline has passed, with Timeout.
fn expire_waits(now_ns: u64) {
    let t = unsafe { &mut *tasks() };
    for (id, task) in t.iter_mut().enumerate().skip(1) {
        let Some(wait) = task.wait else { continue };
        if wait.deadline_ns != 0 && now_ns >= wait.deadline_ns {
            crate::ipc::cancel_waiter(wait.channel, id);
            task.wait = None;
            set_saved_x0(task.sp, Error::Timeout as i64);
            task.state = State::Ready;
        }
    }
}

/// Write a blocked task's syscall result into x0 of its saved frame (frame
/// offset 0), which lives on its kernel stack.
fn set_saved_x0(frame: u64, value: i64) {
    unsafe { *(frame as *mut u64) = value as u64 };
}

/// `task`'s handle table. The caller must be the only one changing it: init
/// (in its own syscalls, which run with IRQs masked while they touch it) or
/// the retirement of a task whose receive rights go home to it.
pub fn handles_mut(task: usize) -> &'static mut HandleTable {
    unsafe { &mut (*tasks())[task].handles }
}

pub fn set_init_task(id: usize) {
    INIT_TASK.store(id, Ordering::SeqCst);
}

pub fn init_task() -> usize {
    INIT_TASK.load(Ordering::SeqCst)
}

/// Initialise the scheduler with task 0 (the boot/idle task).
pub fn init(ttbr0: u64) {
    KERNEL_TTBR0.store(ttbr0, Ordering::SeqCst);
    let t = unsafe { &mut *tasks() };
    for task in t.iter_mut() {
        *task = EMPTY_TASK;
    }
    t[0] = Task {
        ttbr0,
        state: State::Running,
        ..EMPTY_TASK
    };
    COUNT.store(1, Ordering::SeqCst);
    CURRENT.store(0, Ordering::SeqCst);
}

/// Spawn a kernel-mode task (EL1): one of the in-kernel built-ins.
///
/// Seeds a fake exception frame with SPSR_EL1 = EL1h + IRQs enabled. `bind`
/// runs before the task can run (see `install`).
pub fn spawn(entry: fn() -> !, bind: impl FnOnce(TaskRef)) -> Result<TaskRef, SpawnError> {
    let entry_addr = entry as *const () as u64;
    reap_pending_frees(0);
    let id = claim_slot().ok_or(SpawnError::NoSlot)?;

    let Some(stack_bottom) = frame_alloc::allocate_contiguous(KERNEL_STACK_SIZE / 4096) else {
        release(id);
        return Err(SpawnError::OutOfMemory);
    };
    let stack_top = stack_bottom + KERNEL_STACK_SIZE as u64;

    let frame_base = stack_top - FRAME_SIZE;
    prepare_stack(stack_bottom);
    unsafe {
        core::ptr::write_bytes(frame_base as *mut u8, 0, FRAME_SIZE as usize);
        let slots = frame_base as *mut u64;
        *slots.add(30) = entry_addr; // x30 (LR)
        *slots.add(32) = entry_addr; // ELR_EL1
        *slots.add(33) = 0x0000_0005; // SPSR: EL1h, IRQs enabled
    }

    let task = install(
        id,
        Task {
            sp: frame_base,
            kernel_stack_bottom: stack_bottom,
            kernel_stack_pages: KERNEL_STACK_SIZE / 4096,
            ttbr0: KERNEL_TTBR0.load(Ordering::SeqCst),
            state: State::Ready,
            ..EMPTY_TASK
        },
        bind,
    );

    serial_println!(
        "    task {} @ {:#x}, stack {:#x}..{:#x}",
        id,
        entry_addr,
        stack_bottom,
        stack_top
    );
    Ok(task)
}

#[derive(Debug)]
pub enum SpawnError {
    NoSlot,
    OutOfMemory,
    BadImage(&'static str),
}

/// Start an EL0 task from an ELF image, in its own address space. `arg0` and
/// `arg1` arrive in x0 and x1 at the entry point (freshos-rt passes them to
/// `main` as the handle count and the service's argument). `bind` runs
/// before the task can run (see `install`).
///
/// Only the slot claim and the install mask IRQs; the ELF load in between
/// runs with whatever mask the caller has (the SPAWN syscall unmasks it).
pub fn spawn_el0(
    image: &[u8],
    handles: HandleTable,
    arg0: u64,
    arg1: u64,
    bind: impl FnOnce(TaskRef),
) -> Result<TaskRef, SpawnError> {
    reap_pending_frees(0);
    let id = claim_slot().ok_or(SpawnError::NoSlot)?;
    match build_el0(id, image, handles, arg0, arg1) {
        Ok((task, entry)) => {
            let task = install(id, task, bind);
            serial_println!("    task {} el0 asid={} entry={:#x}", id, id, entry);
            Ok(task)
        }
        Err(err) => {
            release(id);
            Err(err)
        }
    }
}

/// Build the task for slot `id` (claimed, so its ASID is ours). On error,
/// everything allocated here is freed: the space by its drop, and the
/// kernel stack is allocated last.
fn build_el0(
    id: usize,
    image: &[u8],
    handles: HandleTable,
    arg0: u64,
    arg1: u64,
) -> Result<(Task, u64), SpawnError> {
    // The ASID is the task slot: unique among live tasks, and flushed when
    // the space is dropped, before the slot can be reused.
    let mut space = AddressSpace::new(id as u16).map_err(|_| SpawnError::OutOfMemory)?;
    let entry = crate::elf::load_into(image, &mut space).map_err(SpawnError::BadImage)?;

    // Stack at the top of the window. The page below it is never mapped:
    // that's the guard page.
    let stack_top = USER_BASE + USER_SIZE;
    let mut va = stack_top - USER_STACK_SIZE;
    while va < stack_top {
        space.map_new_page(va, Perm::ReadWrite).map_err(|_| SpawnError::OutOfMemory)?;
        va += PAGE;
    }
    space.publish();

    let kernel_stack_bottom = frame_alloc::allocate_contiguous(KERNEL_STACK_SIZE / 4096)
        .ok_or(SpawnError::OutOfMemory)?;
    let kernel_stack_top = kernel_stack_bottom + KERNEL_STACK_SIZE as u64;

    // Seed a frame for restore_all_regs + eret into EL0.
    let frame_base = kernel_stack_top - FRAME_SIZE;
    prepare_stack(kernel_stack_bottom);
    unsafe {
        core::ptr::write_bytes(frame_base as *mut u8, 0, FRAME_SIZE as usize);
        let slots = frame_base as *mut u64;
        *slots.add(0) = arg0; // x0
        *slots.add(1) = arg1; // x1
        *slots.add(31) = stack_top; // SP_EL0
        *slots.add(32) = entry; // ELR_EL1
        *slots.add(33) = 0; // SPSR_EL1: EL0t, interrupts unmasked
    }

    let ttbr0 = space.ttbr0();
    let task = Task {
        sp: frame_base,
        kernel_stack_bottom,
        kernel_stack_pages: KERNEL_STACK_SIZE / 4096,
        ttbr0,
        space: Some(space),
        handles,
        wait: None,
        state: State::Ready,
    };
    Ok((task, entry))
}

/// Start the scheduler: enable the timer and interrupts.
///
/// # Safety
/// GIC, exception vectors, and paging must be initialised before calling this.
pub unsafe fn start() {
    unsafe { timer::init(1000) };
    super::interrupt_enable();
    serial_println!(
        "  Scheduler started (timer @ 1000 Hz, {} tasks)",
        task_count(),
    );
}

pub fn task_count() -> usize {
    let t = unsafe { &*tasks() };
    let mut count = 0;
    for task in t.iter() {
        if !matches!(task.state, State::Free | State::Reserved) {
            count += 1;
        }
    }
    count
}

pub fn current_task() -> usize {
    CURRENT.load(Ordering::SeqCst)
}

pub fn block_current() {
    let t = unsafe { &mut *tasks() };
    let cur = CURRENT.load(Ordering::SeqCst);
    t[cur].state = State::Blocked;
    // Enable interrupts and wait — the timer will preempt us
    unsafe {
        core::arch::asm!("msr DAIFClr, #0x2", options(nostack));
        core::arch::asm!("wfi", options(nomem, nostack));
    }
}

pub fn unblock(task_id: usize) {
    let t = unsafe { &mut *tasks() };
    if task_id < MAX_TASKS && t[task_id].state == State::Blocked {
        t[task_id].state = State::Ready;
    }
}

/// Remove the current task: free its address space, queue its kernel stack,
/// return its receive rights to where they came from, free its slot, record
/// its exit in the registry, and tell init. The caller is still running on
/// that stack and must switch away (a syscall returns through the
/// scheduler; a fault waits for the next tick).
///
/// The order matters, and the whole body runs with IRQs masked: the slot
/// only becomes Free after TTBR0 has left the space and the space's ASID has
/// been flushed (by its drop). Otherwise a tick could hand the slot, and so
/// the ASID, to a new task while stale translations for it remain. The
/// registry and init hear of the exit in the same masked step, so no spawn
/// can reuse the slot before the old task's exit is recorded.
///
/// init's own exit halts the system: nothing is left to supervise it.
pub fn retire_current(reason: ExitReason) {
    let _irq = super::IrqGuard::mask();
    let cur = CURRENT.load(Ordering::SeqCst);
    if cur == 0 {
        return;
    }
    if cur == INIT_TASK.load(Ordering::SeqCst) {
        serial_println!(
            "init exited ({}): nothing supervises the system — halting",
            reason.as_str()
        );
        loop {
            super::interrupt_disable();
            super::halt();
        }
    }
    let t = unsafe { &mut *tasks() };
    // An exiting task is never returned to, and never returns to its sender
    // (its slot may be reused before the next switch).
    if DONOR.load(Ordering::SeqCst) == cur || TARGET.load(Ordering::SeqCst) == cur {
        forget_hand_off();
    }
    // A task that exits (or faults) while waiting leaves no waiter behind.
    if let Some(wait) = t[cur].wait.take() {
        crate::ipc::cancel_waiter(wait.channel, cur);
    }
    // Receive rights go home; messages queued meanwhile wait for the next
    // receiver. A right with no home leaves the channel without a receiver.
    let handles = t[cur].handles;
    for (_, slot) in handles.slots() {
        if !slot.rights.contains(Rights::RECV) {
            continue;
        }
        // The channel names its receiver only once the home slot has
        // taken the right back; otherwise it's left with no receiver.
        crate::ipc::receiver_exited(slot.channel, cur, |home, home_slot| {
            if home >= MAX_TASKS || home == cur {
                return false;
            }
            match t[home].handles.get_mut(Handle(home_slot)) {
                Some(home_slot) if home_slot.channel == slot.channel => {
                    home_slot.rights = home_slot.rights.union(Rights::RECV);
                    true
                }
                _ => false,
            }
        });
    }
    // Never keep running on a table that is about to be freed.
    activate(KERNEL_TTBR0.load(Ordering::SeqCst));
    drop(t[cur].space.take());
    queue_pending_free(t[cur].kernel_stack_bottom, t[cur].kernel_stack_pages);
    t[cur] = EMPTY_TASK;
    COUNT.store(task_count(), Ordering::SeqCst);
    let task = task_ref(cur);
    crate::registry::on_exit(task, reason);
    let _ = crate::ipc::send_as_kernel(
        crate::ipc::INIT_INBOX,
        &Message::new(freshos_abi::tag::TASK_EXITED)
            .with_data(0, cur as u64)
            .with_data(1, reason as u64)
            .with_data(2, task.generation as u64),
    );
}

pub fn terminate_current(reason: ExitReason) -> ! {
    retire_current(reason);
    loop {
        super::interrupt_enable();
        super::halt();
    }
}
