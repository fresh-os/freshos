/// Per-process page tables with kernel/user isolation.
///
/// The kernel identity-maps the first 4 GiB using 2 MiB huge pages,
/// supervisor-only. Each user task gets its own PML4 that shares the
/// kernel's page directories but grants USER access only to specific
/// 2 MiB regions (the task's code and stack). The scheduler switches
/// CR3 on every context switch.
///
/// What this means for ring 3 tasks:
///   - Can execute their own code (USER pages)
///   - Can read/write their own stack (USER pages)
///   - CANNOT access kernel data, other tasks' stacks, or page tables
///   - Any access outside USER pages triggers a page fault
///
/// Granularity is 2 MiB (huge page level). Fine-grained 4 KiB isolation
/// comes later when we split pages for specific regions.
use core::sync::atomic::{AtomicU64, Ordering};

use crate::frame_alloc;
use crate::serial::serial_println;

// x86-64 page table entry flags
const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const USER: u64 = 1 << 2;
const HUGE_PAGE: u64 = 1 << 7;

const TWO_MIB: u64 = 2 * 1024 * 1024;
const ONE_GIB: u64 = 1024 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Shared kernel page directory addresses (set during init, read by
// create_user_page_table to share supervisor-only GiB regions)
// ---------------------------------------------------------------------------

static KERNEL_PML4: AtomicU64 = AtomicU64::new(0);
static KERNEL_PD: [AtomicU64; 4] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

// ---------------------------------------------------------------------------
// Init — build the kernel's identity map (supervisor-only)
// ---------------------------------------------------------------------------

/// Create the kernel page tables and switch CR3. Returns the PML4 address.
///
/// # Safety
/// Frame allocator must be initialised. Call once during boot.
pub unsafe fn init() -> u64 {
    let pml4 = alloc_zeroed("kernel PML4");
    let pdpt = alloc_zeroed("kernel PDPT");

    // PML4[0] → PDPT (no USER — this is the kernel mapping)
    write_entry(pml4, 0, pdpt | PRESENT | WRITABLE);

    for gib in 0u64..4 {
        let pd = alloc_zeroed("kernel PD");
        write_entry(pdpt, gib as usize, pd | PRESENT | WRITABLE);
        KERNEL_PD[gib as usize].store(pd, Ordering::SeqCst);

        for idx in 0u64..512 {
            let phys = gib * ONE_GIB + idx * TWO_MIB;
            write_entry(pd, idx as usize, phys | PRESENT | WRITABLE | HUGE_PAGE);
            // No USER bit — ring 3 cannot access these pages
        }
    }

    KERNEL_PML4.store(pml4, Ordering::SeqCst);

    unsafe {
        core::arch::asm!("mov cr3, {}", in(reg) pml4, options(nostack, preserves_flags));
    }

    serial_println!("  Page tables loaded (4 GiB identity-mapped, supervisor-only)");
    pml4
}

/// The kernel's PML4 physical address (used by task 0 / idle).
pub fn kernel_pml4() -> u64 {
    KERNEL_PML4.load(Ordering::SeqCst)
}

/// Identity-map a 1 GiB MMIO window covering `addr`, supervisor-only.
/// Used for PCI BARs placed above the 4 GiB kernel identity map — notably
/// the virtio-GPU BAR, which q35 puts in the high MMIO hole.
///
/// # Safety
/// Frame allocator must be initialised and `init()` must have run.
pub unsafe fn map_mmio_1gib(addr: u64) {
    let pml4 = KERNEL_PML4.load(Ordering::SeqCst);
    assert!(pml4 != 0, "paging::init() must run first");

    let pml4_idx = ((addr >> 39) & 0x1FF) as usize;
    let pdpt_idx = ((addr >> 30) & 0x1FF) as usize;

    // Resolve or create the PDPT for this PML4 slot.
    let pdpt_entry = read_entry(pml4, pml4_idx);
    let pdpt = if pdpt_entry & PRESENT != 0 {
        pdpt_entry & 0x000F_FFFF_FFFF_F000
    } else {
        let new_pdpt = alloc_zeroed("mmio PDPT");
        write_entry(pml4, pml4_idx, new_pdpt | PRESENT | WRITABLE);
        new_pdpt
    };

    // Resolve or create the PD for this PDPT slot and fill it with 2 MiB
    // huge pages covering the full 1 GiB window.
    let pd_entry = read_entry(pdpt, pdpt_idx);
    let pd = if pd_entry & PRESENT != 0 {
        pd_entry & 0x000F_FFFF_FFFF_F000
    } else {
        let new_pd = alloc_zeroed("mmio PD");
        write_entry(pdpt, pdpt_idx, new_pd | PRESENT | WRITABLE);
        new_pd
    };

    let base = addr & !(ONE_GIB - 1);
    for idx in 0u64..512 {
        let phys = base + idx * TWO_MIB;
        write_entry(pd, idx as usize, phys | PRESENT | WRITABLE | HUGE_PAGE);
    }

    // Flush TLB so the new mapping is visible.
    unsafe {
        core::arch::asm!("mov {tmp}, cr3; mov cr3, {tmp}", tmp = out(reg) _, options(nostack, preserves_flags));
    }
}

fn read_entry(table: u64, idx: usize) -> u64 {
    unsafe { core::ptr::read_volatile((table as *const u64).add(idx)) }
}

// ---------------------------------------------------------------------------
// Per-task page tables
// ---------------------------------------------------------------------------

/// Create a user-mode page table that identity-maps the first 4 GiB but
/// only grants USER access to the specified regions. Returns the PML4
/// physical address.
///
/// Each region is `(start_address, size_in_bytes)`. Regions are rounded
/// outward to 2 MiB boundaries.
///
/// Pages outside the listed regions are supervisor-only — a ring 3 task
/// faults if it touches them.
pub fn create_user_page_table(regions: &[(u64, u64)]) -> u64 {
    // Build a bitmap of which 2-MiB pages need USER access
    // (4 GiB / 2 MiB = 2048 slots)
    let mut user_page = [false; 2048];
    for &(start, size) in regions {
        if size == 0 {
            continue;
        }
        let first = (start / TWO_MIB) as usize;
        let last = ((start + size - 1) / TWO_MIB) as usize;
        for i in first..=last.min(2047) {
            user_page[i] = true;
        }
    }

    let pml4 = alloc_zeroed("user PML4");
    let pdpt = alloc_zeroed("user PDPT");

    // PML4[0] needs USER so the CPU will traverse to user pages
    write_entry(pml4, 0, pdpt | PRESENT | WRITABLE | USER);

    for gib in 0usize..4 {
        let base = gib * 512; // offset into user_page[]
        let has_user = user_page[base..base + 512].iter().any(|&u| u);

        if !has_user {
            // No user pages in this GiB — share the kernel PD directly.
            // No USER bit on the PDPT entry, so ring 3 can't traverse here.
            let kpd = KERNEL_PD[gib].load(Ordering::SeqCst);
            write_entry(pdpt, gib, kpd | PRESENT | WRITABLE);
        } else {
            // Need a task-specific PD for this GiB region
            let pd = alloc_zeroed("user PD");
            write_entry(pdpt, gib, pd | PRESENT | WRITABLE | USER);

            for idx in 0usize..512 {
                let phys = (gib as u64) * ONE_GIB + (idx as u64) * TWO_MIB;
                let flags = if user_page[base + idx] {
                    PRESENT | WRITABLE | HUGE_PAGE | USER
                } else {
                    PRESENT | WRITABLE | HUGE_PAGE
                };
                write_entry(pd, idx, phys | flags);
            }
        }
    }

    // Mirror any supervisor-only MMIO mappings the kernel added above the
    // 4 GiB identity map. The BARs may live in any PML4 slot (the q35
    // virtio-GPU BAR sits at 0xC0_0000_0000, which is PML4 slot 1).
    // Syscalls run on the caller's CR3, so without this the kernel would
    // fault touching MMIO from a syscall.
    let kpml4_phys = KERNEL_PML4.load(Ordering::SeqCst);
    if kpml4_phys != 0 {
        // Mirror PDPT slots above the 4 GiB identity map within PML4[0].
        let kpdpt0 = read_entry(kpml4_phys, 0) & 0x000F_FFFF_FFFF_F000;
        for gib in 4usize..512 {
            let entry = read_entry(kpdpt0, gib);
            if entry & PRESENT != 0 {
                write_entry(pdpt, gib, entry);
            }
        }
        // Mirror entire PML4 slots beyond 0 (kernel-only MMIO above the
        // first 512 GiB).
        for slot in 1usize..512 {
            let entry = read_entry(kpml4_phys, slot);
            if entry & PRESENT != 0 {
                write_entry(pml4, slot, entry);
            }
        }
    }

    pml4
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn alloc_zeroed(name: &str) -> u64 {
    let phys = frame_alloc::allocate().unwrap_or_else(|| {
        serial_println!("FATAL: out of frames for {}", name);
        loop {
            unsafe { core::arch::asm!("cli; hlt") };
        }
    });
    unsafe { core::ptr::write_bytes(phys as *mut u8, 0, 4096) };
    phys
}

fn write_entry(table_phys: u64, index: usize, value: u64) {
    unsafe {
        let ptr = (table_phys as *mut u64).add(index);
        ptr.write_volatile(value);
    }
}
