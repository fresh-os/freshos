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
use core::sync::atomic::{AtomicUsize, Ordering};

use freshos_abi::{USER_BASE, USER_SIZE, USER_STACK_SIZE};

use crate::frame_alloc;
use crate::serial::serial_println;

use super::addrspace::{AddressSpace, Perm, PAGE};
use super::gic;
use super::timer;

pub const MAX_TASKS: usize = 16;
const KERNEL_STACK_SIZE: usize = 4096 * 4; // 16 KiB kernel stack per task

/// Size of the saved register frame that exception.s's save_all_regs pushes:
/// the GPRs, SP_EL0, ELR and SPSR at 0..272, then q0-q31, FPCR and FPSR.
/// A new task's frame is all zeroes apart from the slots set below, so it
/// starts with zeroed FP/SIMD registers, FPCR = 0 and FPSR = 0.
const FRAME_SIZE: u64 = 800;

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

struct Task {
    sp: u64, // saved frame (kernel stack, after save_all_regs)
    kernel_stack_bottom: u64,
    kernel_stack_pages: usize,
    ttbr0: u64, // TTBR0_EL1 to load when this task runs
    space: Option<AddressSpace>,
    state: State,
}

const EMPTY_TASK: Task = Task {
    sp: 0,
    kernel_stack_bottom: 0,
    kernel_stack_pages: 0,
    ttbr0: 0,
    space: None,
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
static COUNT: AtomicUsize = AtomicUsize::new(0);

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
/// interrupt.
fn install(id: usize, task: Task) {
    let _irq = super::IrqGuard::mask();
    let t = unsafe { &mut *tasks() };
    debug_assert!(t[id].state == State::Reserved);
    t[id] = task;
    COUNT.store(task_count(), Ordering::SeqCst);
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
    switch_away(stack_ptr)
}

/// Save the current task's frame and pick the next ready task, round-robin.
/// Returns the frame to restore. Runs with IRQs masked (exception context).
pub fn switch_away(frame: u64) -> u64 {
    let t = unsafe { &mut *tasks() };
    let cur = CURRENT.load(Ordering::SeqCst);
    if t[cur].state != State::Free {
        t[cur].sp = frame;
    }
    if t[cur].state == State::Running {
        t[cur].state = State::Ready;
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
    debug_assert!(target < MAX_TASKS && t[target].state == State::Ready);
    let cur = CURRENT.load(Ordering::SeqCst);
    if t[cur].state != State::Free {
        t[cur].sp = frame;
    }
    if t[cur].state == State::Running {
        t[cur].state = State::Ready;
    }
    run(t, target)
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

/// Spawn a kernel-mode task (EL1) — used when EL0 isn't available (HVF).
///
/// Seeds a fake exception frame with SPSR_EL1 = EL1h + IRQs enabled.
pub fn spawn(entry: fn() -> !) -> usize {
    spawn_with_arg(entry as *const () as u64, 0)
}

pub fn spawn_with_arg(entry_addr: u64, arg0: u64) -> usize {
    reap_pending_frees(0);
    let id = claim_slot().expect("too many tasks");

    let stack_bottom =
        frame_alloc::allocate_contiguous(KERNEL_STACK_SIZE / 4096).expect("task stack");
    let stack_top = stack_bottom + KERNEL_STACK_SIZE as u64;

    let frame_base = stack_top - FRAME_SIZE;
    unsafe {
        core::ptr::write_bytes(frame_base as *mut u8, 0, FRAME_SIZE as usize);
        let slots = frame_base as *mut u64;
        *slots.add(0) = arg0; // x0
        *slots.add(30) = entry_addr; // x30 (LR)
        *slots.add(32) = entry_addr; // ELR_EL1
        *slots.add(33) = 0x0000_0005; // SPSR: EL1h, IRQs enabled
    }

    install(
        id,
        Task {
            sp: frame_base,
            kernel_stack_bottom: stack_bottom,
            kernel_stack_pages: KERNEL_STACK_SIZE / 4096,
            ttbr0: KERNEL_TTBR0.load(Ordering::SeqCst),
            state: State::Ready,
            ..EMPTY_TASK
        },
    );

    serial_println!(
        "    task {} @ {:#x}, stack {:#x}..{:#x}",
        id,
        entry_addr,
        stack_bottom,
        stack_top
    );
    id
}

#[derive(Debug)]
pub enum SpawnError {
    NoSlot,
    OutOfMemory,
    BadImage(&'static str),
}

/// Start an EL0 task from an ELF image, in its own address space. `arg0` and
/// `arg1` arrive in x0 and x1 at the entry point (freshos-rt passes them to
/// `main` as the handle count and the service's argument).
pub fn spawn_el0(image: &[u8], arg0: u64, arg1: u64) -> Result<usize, SpawnError> {
    reap_pending_frees(0);
    let id = claim_slot().ok_or(SpawnError::NoSlot)?;
    match build_el0(id, image, arg0, arg1) {
        Ok((task, entry)) => {
            install(id, task);
            serial_println!("    task {} el0 asid={} entry={:#x}", id, id, entry);
            Ok(id)
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
fn build_el0(id: usize, image: &[u8], arg0: u64, arg1: u64) -> Result<(Task, u64), SpawnError> {
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
/// free its slot, and record its exit. The caller is still running on that
/// stack and must switch away (a syscall returns through the scheduler; a
/// fault waits for the next tick).
///
/// The order matters, and the whole body runs with IRQs masked: the slot
/// only becomes Free after TTBR0 has left the space and the space's ASID has
/// been flushed (by its drop). Otherwise a tick could hand the slot, and so
/// the ASID, to a new task while stale translations for it remain.
pub fn retire_current(reason: u64) {
    let _irq = super::IrqGuard::mask();
    let cur = CURRENT.load(Ordering::SeqCst);
    if cur == 0 {
        return;
    }
    let t = unsafe { &mut *tasks() };
    // Never keep running on a table that is about to be freed.
    activate(KERNEL_TTBR0.load(Ordering::SeqCst));
    drop(t[cur].space.take());
    queue_pending_free(t[cur].kernel_stack_bottom, t[cur].kernel_stack_pages);
    t[cur] = EMPTY_TASK;
    COUNT.store(task_count(), Ordering::SeqCst);
    crate::init_abi::task_exited(cur, reason);
}

pub fn terminate_current_with_reason(reason: u64) -> ! {
    retire_current(reason);
    loop {
        super::interrupt_enable();
        super::halt();
    }
}

pub fn terminate_current() -> ! {
    terminate_current_with_reason(crate::init_abi::SERVICE_EXIT_FAULT)
}
