/// Preemptive round-robin scheduler with per-task page tables.
///
/// Each task has its own PML4 (page table root). The kernel is mapped
/// supervisor-only in all page tables. User tasks see only their own
/// code and stack pages. CR3 is switched on every context switch.
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::frame_alloc;
use crate::gdt;
use crate::paging;
use crate::pic;
use crate::serial::serial_println;

const MAX_TASKS: usize = 16;
const TASK_STACK_SIZE: usize = 4096 * 4; // 16 KiB user stack
const KERNEL_STACK_SIZE: usize = 4096 * 4; // 16 KiB kernel stack per task

// ---------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum State {
    Free,
    Ready,
    Running,
    Blocked,
}

#[derive(Clone, Copy)]
struct Task {
    rsp: u64,
    kernel_stack_top: u64,
    pml4_phys: u64, // CR3 value — this task's page table
    state: State,
}

const EMPTY_TASK: Task = Task {
    rsp: 0,
    kernel_stack_top: 0,
    pml4_phys: 0,
    state: State::Free,
};

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

struct TasksCell(UnsafeCell<[Task; MAX_TASKS]>);
unsafe impl Sync for TasksCell {}

static TASKS: TasksCell = TasksCell(UnsafeCell::new([EMPTY_TASK; MAX_TASKS]));
static CURRENT: AtomicUsize = AtomicUsize::new(0);
static COUNT: AtomicUsize = AtomicUsize::new(0);

fn tasks() -> *mut [Task; MAX_TASKS] {
    TASKS.0.get()
}

// ---------------------------------------------------------------------------
// Timer ISR stub
// ---------------------------------------------------------------------------

core::arch::global_asm!(
    ".global timer_isr_stub",
    "timer_isr_stub:",
    "    push rax",
    "    push rbx",
    "    push rcx",
    "    push rdx",
    "    push rsi",
    "    push rdi",
    "    push rbp",
    "    push r8",
    "    push r9",
    "    push r10",
    "    push r11",
    "    push r12",
    "    push r13",
    "    push r14",
    "    push r15",
    "",
    "    mov  rcx, rsp",
    "    sub  rsp, 32",
    "    call scheduler_tick",
    "    mov  rsp, rax",
    "",
    "    mov  al, 0x20",
    "    out  0x20, al",
    "",
    "    pop  r15",
    "    pop  r14",
    "    pop  r13",
    "    pop  r12",
    "    pop  r11",
    "    pop  r10",
    "    pop  r9",
    "    pop  r8",
    "    pop  rbp",
    "    pop  rdi",
    "    pop  rsi",
    "    pop  rdx",
    "    pop  rcx",
    "    pop  rbx",
    "    pop  rax",
    "",
    "    iretq",
);

unsafe extern "C" {
    fn timer_isr_stub();
}

// ---------------------------------------------------------------------------
// Scheduler tick
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
extern "C" fn scheduler_tick(stack_ptr: u64) -> u64 {
    let t = unsafe { &mut *tasks() };
    let cur = CURRENT.load(Ordering::SeqCst);
    let count = COUNT.load(Ordering::SeqCst);

    // Save outgoing task
    t[cur].rsp = stack_ptr;
    if t[cur].state == State::Running {
        t[cur].state = State::Ready;
    }

    // Round-robin
    let mut next = (cur + 1) % count;
    for _ in 0..count {
        if t[next].state == State::Ready {
            break;
        }
        next = (next + 1) % count;
    }

    t[next].state = State::Running;
    CURRENT.store(next, Ordering::SeqCst);

    // Switch page tables if changing tasks
    if next != cur && t[next].pml4_phys != 0 {
        unsafe {
            core::arch::asm!(
                "mov cr3, {}",
                in(reg) t[next].pml4_phys,
                options(nostack, preserves_flags),
            );
        }
    }

    // Update TSS.RSP0 for ring 3 → ring 0 transitions
    update_tss_rsp0(t[next].kernel_stack_top);

    // Update syscall entry kernel RSP
    crate::syscall::set_kernel_rsp(t[next].kernel_stack_top);

    t[next].rsp
}

fn update_tss_rsp0(rsp0: u64) {
    if rsp0 == 0 {
        return; // boot task uses original stack
    }
    use x86_64::VirtAddr;
    unsafe {
        let tss = &mut *core::ptr::addr_of_mut!(crate::gdt::TSS);
        tss.privilege_stack_table[0] = VirtAddr::new(rsp0);
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Register task 0 (boot/idle, ring 0, uses kernel page tables).
pub fn init(kernel_pml4: u64) {
    let t = unsafe { &mut *tasks() };
    t[0] = Task {
        rsp: 0,
        kernel_stack_top: 0,
        pml4_phys: kernel_pml4,
        state: State::Running,
    };
    COUNT.store(1, Ordering::SeqCst);
    CURRENT.store(0, Ordering::SeqCst);
}

/// Spawn a user-mode task (ring 3) with its own page tables.
///
/// `extra_regions` are additional `(addr, size)` pairs to map as USER
/// in the task's page table (e.g. the framebuffer).
pub fn spawn_user(entry: u64, extra_regions: &[(u64, u64)]) -> usize {
    let t = unsafe { &mut *tasks() };
    let id = COUNT.load(Ordering::SeqCst);
    assert!(id < MAX_TASKS, "too many tasks");

    // Allocate stacks
    let user_stack_bottom =
        frame_alloc::allocate_contiguous(TASK_STACK_SIZE / 4096).expect("user stack");
    let user_stack_top = user_stack_bottom + TASK_STACK_SIZE as u64;

    let kernel_stack_bottom =
        frame_alloc::allocate_contiguous(KERNEL_STACK_SIZE / 4096).expect("kernel stack");
    let kernel_stack_top = kernel_stack_bottom + KERNEL_STACK_SIZE as u64;

    // Determine which 2 MiB regions this task needs USER access to:
    //   1. The user stack
    //   2. The code/data region (where the task function and its data live)
    //
    // For code, we cover 8 MiB around the entry point. This is generous
    // but ensures the function, its callees, and static data (like the
    // scancode table) are all accessible.
    let code_base = entry & !(0x1F_FFFF); // round down to 2 MiB
    let mut regions = [(0u64, 0u64); 8];
    regions[0] = (user_stack_bottom, TASK_STACK_SIZE as u64);
    regions[1] = (code_base, 8 * 1024 * 1024); // 8 MiB of code/data
    let mut count = 2;
    for &r in extra_regions {
        if count < regions.len() && r.1 > 0 {
            regions[count] = r;
            count += 1;
        }
    }

    let pml4 = paging::create_user_page_table(&regions[..count]);

    // Seed the kernel stack with a fake interrupt frame → iretq enters ring 3
    unsafe {
        let p = kernel_stack_top as *mut u64;
        *p.offset(-1) = gdt::USER_DATA_SEL as u64;
        *p.offset(-2) = user_stack_top;
        *p.offset(-3) = 0x202; // RFLAGS: IF=1
        *p.offset(-4) = gdt::USER_CODE_SEL as u64;
        *p.offset(-5) = entry;
        for i in 6..=20 {
            *p.offset(-(i as isize)) = 0;
        }
    }

    let rsp = kernel_stack_top - 20 * 8;

    t[id] = Task {
        rsp,
        kernel_stack_top,
        pml4_phys: pml4,
        state: State::Ready,
    };
    COUNT.store(id + 1, Ordering::SeqCst);

    serial_println!(
        "    task {} @ {:#x}, stack {:#x}, pml4 {:#x}",
        id,
        entry,
        user_stack_bottom,
        pml4
    );
    id
}

/// Start the scheduler.
pub unsafe fn start() {
    unsafe {
        crate::idt::set_interrupt_handler(pic::TIMER_VECTOR, timer_isr_stub as *const () as u64);
    }
    init_pit(1000);
    pic::unmask(0);
    unsafe { core::arch::asm!("sti", options(nomem, nostack)) };

    serial_println!(
        "  Scheduler started (PIT @ 1000 Hz, {} tasks, per-task page tables)",
        COUNT.load(Ordering::SeqCst)
    );
}

pub fn task_count() -> usize {
    COUNT.load(Ordering::SeqCst)
}

pub fn current_task() -> usize {
    CURRENT.load(Ordering::SeqCst)
}

pub fn block_current() {
    let t = unsafe { &mut *tasks() };
    let cur = CURRENT.load(Ordering::SeqCst);
    t[cur].state = State::Blocked;
    unsafe { core::arch::asm!("sti; hlt", options(nomem, nostack)) };
}

pub fn unblock(task_id: usize) {
    let t = unsafe { &mut *tasks() };
    if task_id < MAX_TASKS && t[task_id].state == State::Blocked {
        t[task_id].state = State::Ready;
    }
}

// ---------------------------------------------------------------------------
// PIT
// ---------------------------------------------------------------------------

fn init_pit(hz: u32) {
    let divisor = 1_193_182u32 / hz;
    unsafe {
        core::arch::asm!("out dx, al", in("dx") 0x43u16, in("al") 0x36u8, options(nomem, nostack));
        core::arch::asm!("out dx, al", in("dx") 0x40u16, in("al") (divisor & 0xFF) as u8, options(nomem, nostack));
        core::arch::asm!("out dx, al", in("dx") 0x40u16, in("al") ((divisor >> 8) & 0xFF) as u8, options(nomem, nostack));
    }
}
