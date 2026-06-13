/// aarch64 scheduler — preemptive round-robin with per-task page tables.
///
/// Supports both EL1 (kernel) and EL0 (user) tasks. Each user task has:
///   - Its own TTBR0 page table (user-space address mapping)
///   - A dedicated kernel stack (used during EL0→EL1 transitions)
///   - A user stack (mapped USER in its page table)
///
/// The timer ISR (exception.s) saves all registers, calls
/// `scheduler_tick_arm(sp)`, and gets back the new SP. On task switch,
/// we also switch TTBR0 and update SP_EL1 for the next EL0→EL1 transition.
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::frame_alloc;
use crate::serial::serial_println;

use super::gic;
use super::paging;
use super::timer;

pub const MAX_TASKS: usize = 16;
const USER_STACK_SIZE: usize = 4096 * 4; // 16 KiB user stack
const KERNEL_STACK_SIZE: usize = 4096 * 4; // 16 KiB kernel stack per task
pub const USER_STACK_BYTES: u64 = USER_STACK_SIZE as u64;

#[derive(Clone, Copy, PartialEq)]
enum State {
    Free,
    Ready,
    Running,
    Blocked,
}

#[derive(Clone, Copy)]
struct Task {
    sp: u64, // saved stack pointer (kernel stack, after save_all_regs)
    user_stack_bottom: u64,
    user_stack_pages: usize,
    kernel_stack_bottom: u64,
    kernel_stack_pages: usize,
    kernel_stack_top: u64, // top of per-task kernel stack (for SP_EL1)
    ttbr0: u64,            // TTBR0_EL1 value (0 = use kernel page table)
    state: State,
}

const EMPTY_TASK: Task = Task {
    sp: 0,
    user_stack_bottom: 0,
    user_stack_pages: 0,
    kernel_stack_bottom: 0,
    kernel_stack_pages: 0,
    kernel_stack_top: 0,
    ttbr0: 0,
    state: State::Free,
};

#[derive(Clone, Copy)]
struct PendingFree {
    kernel_stack_bottom: u64,
    kernel_stack_pages: usize,
    user_stack_bottom: u64,
    user_stack_pages: usize,
    used: bool,
}

const EMPTY_PENDING: PendingFree = PendingFree {
    kernel_stack_bottom: 0,
    kernel_stack_pages: 0,
    user_stack_bottom: 0,
    user_stack_pages: 0,
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

fn allocate_slot(t: &[Task; MAX_TASKS]) -> usize {
    for id in 1..MAX_TASKS {
        if t[id].state == State::Free {
            return id;
        }
    }
    panic!("too many tasks");
}

fn queue_pending_free(task: Task) {
    if task.kernel_stack_pages == 0 && task.user_stack_pages == 0 {
        return;
    }

    let pending = unsafe { &mut *pending_frees() };
    for slot in pending.iter_mut() {
        if !slot.used {
            *slot = PendingFree {
                kernel_stack_bottom: task.kernel_stack_bottom,
                kernel_stack_pages: task.kernel_stack_pages,
                user_stack_bottom: task.user_stack_bottom,
                user_stack_pages: task.user_stack_pages,
                used: true,
            };
            return;
        }
    }

    panic!("pending free queue full");
}

fn reap_pending_frees(current_stack_ptr: u64) {
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
        if slot.user_stack_pages > 0 {
            unsafe {
                frame_alloc::deallocate_contiguous(slot.user_stack_bottom, slot.user_stack_pages)
            };
        }
        *slot = EMPTY_PENDING;
    }
}

/// Called from exception.s on every timer IRQ.
///
/// Receives the current task's saved SP (after save_all_regs pushed 272 bytes).
/// Returns the next task's saved SP to restore.
#[unsafe(no_mangle)]
extern "C" fn scheduler_tick_arm(stack_ptr: u64) -> u64 {
    reap_pending_frees(stack_ptr);

    // Acknowledge the GIC interrupt and rearm the timer
    let intid = gic::acknowledge();
    timer::handle_irq();
    gic::end_of_interrupt(intid);

    let t = unsafe { &mut *tasks() };
    let cur = CURRENT.load(Ordering::SeqCst);

    // Save outgoing task
    if t[cur].state != State::Free {
        t[cur].sp = stack_ptr;
    }
    if t[cur].state == State::Running {
        t[cur].state = State::Ready;
    }

    // Round-robin: find next Ready task
    let mut next = cur;
    let mut found = false;
    for step in 1..=MAX_TASKS {
        let candidate = (cur + step) % MAX_TASKS;
        if t[candidate].state == State::Ready {
            next = candidate;
            found = true;
            break;
        }
    }
    if !found && t[cur].state == State::Ready {
        next = cur;
        found = true;
    }
    if !found {
        next = 0;
    }

    t[next].state = State::Running;
    CURRENT.store(next, Ordering::SeqCst);

    // All tasks share UEFI's (patched) page tables — no TTBR0 switch needed.

    t[next].sp
}

/// Initialise the scheduler with task 0 (the boot/idle task).
pub fn init(ttbr0: u64) {
    let t = unsafe { &mut *tasks() };
    for task in t.iter_mut() {
        *task = EMPTY_TASK;
    }
    t[0] = Task {
        sp: 0,
        user_stack_bottom: 0,
        user_stack_pages: 0,
        kernel_stack_bottom: 0,
        kernel_stack_pages: 0,
        kernel_stack_top: 0,
        ttbr0,
        state: State::Running,
    };
    COUNT.store(1, Ordering::SeqCst);
    CURRENT.store(0, Ordering::SeqCst);
}

/// Spawn a user-mode task (EL0) with its own page tables.
///
/// `extra_regions` are additional `(addr, size)` pairs to map as USER
/// in the task's page table (e.g. the framebuffer, surfaces).
pub fn spawn_user(entry: u64, extra_regions: &[(u64, u64)]) -> usize {
    reap_pending_frees(0);
    let t = unsafe { &mut *tasks() };
    let id = allocate_slot(t);

    // Allocate user stack
    let user_stack_bottom =
        frame_alloc::allocate_contiguous(USER_STACK_SIZE / 4096).expect("user stack");
    let user_stack_top = user_stack_bottom + USER_STACK_SIZE as u64;

    // Allocate kernel stack (used during EL0→EL1 transitions)
    let kernel_stack_bottom =
        frame_alloc::allocate_contiguous(KERNEL_STACK_SIZE / 4096).expect("kernel stack");
    let kernel_stack_top = kernel_stack_bottom + KERNEL_STACK_SIZE as u64;

    // Grant EL0 access to user stack, code, and extra regions
    paging::grant_user_access(user_stack_bottom, USER_STACK_SIZE as u64);
    let code_base = entry & !0xFFF; // page-align
    paging::grant_user_access(code_base, 8 * 1024 * 1024); // 8 MiB of code/data
    for &(addr, size) in extra_regions {
        if size > 0 {
            paging::grant_user_access(addr, size);
        }
    }

    let ttbr0 = paging::user_ttbr0();

    // Seed the kernel stack with a fake exception frame.
    // restore_all_regs will pop 272 bytes, then eret enters EL0.
    //
    // Layout:
    //   [sp+0..232]  x0-x29 = 0
    //   [sp+240]     x30 (LR) = 0
    //   [sp+248]     SP_EL0 = user_stack_top (user stack pointer)
    //   [sp+256]     ELR_EL1 = entry (where to start executing)
    //   [sp+264]     SPSR_EL1 = 0x0 (EL0t, IRQs enabled)
    let frame_base = kernel_stack_top - 272;
    unsafe {
        let p = frame_base as *mut u8;
        core::ptr::write_bytes(p, 0, 272);

        let slots = frame_base as *mut u64;
        // SP_EL0 = user stack top
        *slots.add(31) = user_stack_top;
        // ELR_EL1 = entry point
        *slots.add(32) = entry;
        // SPSR_EL1 = EL0t (0x0), all DAIF clear (interrupts enabled)
        *slots.add(33) = 0x0000_0000;
    }

    t[id] = Task {
        sp: frame_base,
        user_stack_bottom,
        user_stack_pages: USER_STACK_SIZE / 4096,
        kernel_stack_bottom,
        kernel_stack_pages: KERNEL_STACK_SIZE / 4096,
        kernel_stack_top,
        ttbr0,
        state: State::Ready,
    };
    COUNT.store(task_count(), Ordering::SeqCst);

    serial_println!(
        "    task {} @ {:#x}, ustack {:#x}, kstack {:#x}, ttbr0 {:#x}",
        id,
        entry,
        user_stack_bottom,
        kernel_stack_bottom,
        ttbr0,
    );
    id
}

pub fn spawn_user_pregranted(entry: u64, user_stack_bottom: u64) -> usize {
    reap_pending_frees(0);
    let t = unsafe { &mut *tasks() };
    let id = allocate_slot(t);
    let user_stack_top = user_stack_bottom + USER_STACK_SIZE as u64;

    let kernel_stack_bottom =
        frame_alloc::allocate_contiguous(KERNEL_STACK_SIZE / 4096).expect("kernel stack");
    let kernel_stack_top = kernel_stack_bottom + KERNEL_STACK_SIZE as u64;

    let ttbr0 = paging::user_ttbr0();
    let frame_base = kernel_stack_top - 272;
    unsafe {
        let p = frame_base as *mut u8;
        core::ptr::write_bytes(p, 0, 272);

        let slots = frame_base as *mut u64;
        *slots.add(31) = user_stack_top;
        *slots.add(32) = entry;
        *slots.add(33) = 0x0000_0000;
    }

    t[id] = Task {
        sp: frame_base,
        user_stack_bottom,
        user_stack_pages: 0,
        kernel_stack_bottom,
        kernel_stack_pages: KERNEL_STACK_SIZE / 4096,
        kernel_stack_top,
        ttbr0,
        state: State::Ready,
    };
    COUNT.store(task_count(), Ordering::SeqCst);

    serial_println!(
        "    task {} @ {:#x}, ustack {:#x}, kstack {:#x}, ttbr0 {:#x}",
        id,
        entry,
        user_stack_bottom,
        kernel_stack_bottom,
        ttbr0,
    );
    id
}

/// Spawn a kernel-mode task (EL1) — used when EL0 isn't available (HVF).
///
/// Seeds a fake exception frame with SPSR_EL1 = EL1h + IRQs enabled.
pub fn spawn(entry: fn() -> !) -> usize {
    spawn_with_arg(entry as *const () as u64, 0)
}

pub fn spawn_with_arg(entry_addr: u64, arg0: u64) -> usize {
    reap_pending_frees(0);
    let t = unsafe { &mut *tasks() };
    let id = allocate_slot(t);

    let stack_bottom =
        frame_alloc::allocate_contiguous(KERNEL_STACK_SIZE / 4096).expect("task stack");
    let stack_top = stack_bottom + KERNEL_STACK_SIZE as u64;

    let frame_base = stack_top - 272;
    unsafe {
        core::ptr::write_bytes(frame_base as *mut u8, 0, 272);
        let slots = frame_base as *mut u64;
        *slots.add(0) = arg0; // x0
        *slots.add(30) = entry_addr; // x30 (LR)
        *slots.add(32) = entry_addr; // ELR_EL1
        *slots.add(33) = 0x0000_0005; // SPSR: EL1h, IRQs enabled
    }

    let ttbr0 = paging::user_ttbr0();
    t[id] = Task {
        sp: frame_base,
        user_stack_bottom: 0,
        user_stack_pages: 0,
        kernel_stack_bottom: stack_bottom,
        kernel_stack_pages: KERNEL_STACK_SIZE / 4096,
        kernel_stack_top: stack_top,
        ttbr0,
        state: State::Ready,
    };
    COUNT.store(task_count(), Ordering::SeqCst);

    serial_println!(
        "    task {} @ {:#x}, stack {:#x}..{:#x}",
        id,
        entry_addr,
        stack_bottom,
        stack_top
    );
    id
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
        if task.state != State::Free {
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
        core::arch::asm!("msr DAIFClr, #0x2", options(nomem, nostack));
        core::arch::asm!("wfi", options(nomem, nostack));
    }
}

pub fn unblock(task_id: usize) {
    let t = unsafe { &mut *tasks() };
    if task_id < MAX_TASKS && t[task_id].state == State::Blocked {
        t[task_id].state = State::Ready;
    }
}

pub fn terminate_current_with_reason(reason: u64) -> ! {
    let cur = CURRENT.load(Ordering::SeqCst);
    let t = unsafe { &mut *tasks() };

    if cur != 0 {
        let task = t[cur];
        t[cur] = EMPTY_TASK;
        queue_pending_free(task);
        crate::init_abi::task_exited(cur, reason);
        COUNT.store(task_count(), Ordering::SeqCst);
    }

    loop {
        super::interrupt_enable();
        super::halt();
    }
}

pub fn terminate_current() -> ! {
    terminate_current_with_reason(crate::init_abi::SERVICE_EXIT_FAULT)
}
