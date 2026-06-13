/// Global Descriptor Table — kernel + user segments and TSS.
///
/// GDT layout (order matters for `syscall`/`sysret`):
///   0: null
///   1: kernel code  (0x08)  — ring 0
///   2: kernel data  (0x10)  — ring 0
///   3: user data    (0x1B)  — ring 3  (must precede user code for sysret)
///   4: user code    (0x23)  — ring 3
///   5-6: TSS        (0x28)  — 16-byte descriptor
///
/// The `syscall` instruction hardcodes the relationship between selectors:
///   STAR[47:32] = kernel CS base  (sysret adds 16 for user CS, 8 for user DS)
///   STAR[63:48] = user CS base minus 16
/// Our layout satisfies this: kernel_code=0x08, user_data=0x18, user_code=0x20.
///
/// # Safety
/// `init()` must be called exactly once, during single-threaded boot.
use x86_64::instructions::tables::load_tss;
use x86_64::registers::segmentation::{Segment, CS, DS, ES, SS};
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

// Segment selectors — index × 8, plus RPL for user segments.
pub const KERNEL_CODE_SEL: u16 = 0x08; // GDT[1], RPL 0
pub const KERNEL_DATA_SEL: u16 = 0x10; // GDT[2], RPL 0
pub const USER_DATA_SEL: u16 = 0x18 | 3; // GDT[3], RPL 3
pub const USER_CODE_SEL: u16 = 0x20 | 3; // GDT[4], RPL 3

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

const IST_STACK_SIZE: usize = 4096 * 5; // 20 KiB
const SYSCALL_STACK_SIZE: usize = 4096 * 4; // 16 KiB — per-CPU kernel stack for syscalls

static mut IST_STACK: [u8; IST_STACK_SIZE] = [0; IST_STACK_SIZE];
static mut SYSCALL_STACK: [u8; SYSCALL_STACK_SIZE] = [0; SYSCALL_STACK_SIZE];
pub(crate) static mut TSS: TaskStateSegment = TaskStateSegment::new();
static mut GDT: GlobalDescriptorTable = GlobalDescriptorTable::new();

/// Top of the kernel stack used by the syscall handler.
pub fn syscall_stack_top() -> u64 {
    unsafe {
        let bottom = core::ptr::addr_of!(SYSCALL_STACK) as *const u8;
        VirtAddr::from_ptr(bottom).as_u64() + SYSCALL_STACK_SIZE as u64
    }
}

/// Load our GDT, reload all segment registers, and activate the TSS.
///
/// # Safety
/// Must be called once during single-threaded boot.
pub unsafe fn init() {
    use core::ptr::addr_of;
    use core::ptr::addr_of_mut;

    // -- TSS: double-fault IST and privilege-level stacks --
    let ist_top = VirtAddr::from_ptr(addr_of!(IST_STACK) as *const u8) + IST_STACK_SIZE as u64;
    unsafe {
        (*addr_of_mut!(TSS)).interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = ist_top
    };

    // RSP0: the stack the CPU switches to when entering ring 0 from ring 3
    // via an interrupt. We'll update this per-task later; for now use the
    // syscall stack.
    let syscall_top = VirtAddr::new(syscall_stack_top());
    unsafe { (*addr_of_mut!(TSS)).privilege_stack_table[0] = syscall_top };

    // -- GDT entries: order is critical for syscall/sysret --
    let gdt = unsafe { &mut *addr_of_mut!(GDT) };
    let code_sel = gdt.append(Descriptor::kernel_code_segment()); // 0x08
    let data_sel = gdt.append(Descriptor::kernel_data_segment()); // 0x10
    let _user_data = gdt.append(Descriptor::user_data_segment()); // 0x18
    let _user_code = gdt.append(Descriptor::user_code_segment()); // 0x20
    let tss_sel = gdt.append(Descriptor::tss_segment(unsafe { &*addr_of!(TSS) }));

    gdt.load();

    unsafe {
        CS::set_reg(code_sel);
        DS::set_reg(data_sel);
        ES::set_reg(data_sel);
        SS::set_reg(data_sel);
        load_tss(tss_sel);
    }
}
