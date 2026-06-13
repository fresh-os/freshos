/// Kernel heap allocator — linked-list with coalescing.
///
/// Backed by contiguous physical frames from the frame allocator.
/// Provides `#[global_allocator]` so `alloc::vec::Vec`, `alloc::string::String`,
/// `alloc::boxed::Box`, etc. work throughout the kernel.
///
/// Free blocks are kept in an address-ordered linked list. On dealloc,
/// adjacent blocks are merged to reduce fragmentation. On alloc, first-fit
/// is used.
///
/// Not the fastest allocator, but correct and simple. A slab allocator
/// or buddy system can replace it later when allocation pressure warrants.
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;

use crate::frame_alloc;
use crate::serial::serial_println;

const HEAP_SIZE: usize = 1024 * 1024; // 1 MiB
const HEAP_PAGES: usize = HEAP_SIZE / 4096;

// Minimum block size and alignment — must fit a FreeBlock (two usizes = 16 bytes).
const MIN_BLOCK: usize = 32;
const BLOCK_ALIGN: usize = 16;

// ---------------------------------------------------------------------------
// Free-list node (lives inside free memory blocks)
// ---------------------------------------------------------------------------

#[repr(C)]
struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

// ---------------------------------------------------------------------------
// Heap state
// ---------------------------------------------------------------------------

struct HeapInner {
    free_list: *mut FreeBlock,
    total: usize,
    used: usize,
}

struct LockedHeap(UnsafeCell<HeapInner>);
unsafe impl Sync for LockedHeap {}

#[global_allocator]
static HEAP: LockedHeap = LockedHeap(UnsafeCell::new(HeapInner {
    free_list: core::ptr::null_mut(),
    total: 0,
    used: 0,
}));

// ---------------------------------------------------------------------------
// Init
// ---------------------------------------------------------------------------

/// Allocate physical frames and initialise the kernel heap.
///
/// # Safety
/// Must be called once, after frame_alloc::init() and paging::init().
pub unsafe fn init() {
    let base = frame_alloc::allocate_contiguous(HEAP_PAGES).expect("heap frames");

    // The entire region is one big free block
    let node = base as *mut FreeBlock;
    unsafe {
        (*node).size = HEAP_SIZE;
        (*node).next = core::ptr::null_mut();
    }

    let inner = unsafe { &mut *HEAP.0.get() };
    inner.free_list = node;
    inner.total = HEAP_SIZE;
    inner.used = 0;

    serial_println!("  Heap: {} KiB at {:#x}", HEAP_SIZE / 1024, base,);
}

/// How many bytes are currently allocated.
pub fn used() -> usize {
    unsafe { (*HEAP.0.get()).used }
}

/// Total heap size.
pub fn total() -> usize {
    unsafe { (*HEAP.0.get()).total }
}

// ---------------------------------------------------------------------------
// GlobalAlloc implementation
// ---------------------------------------------------------------------------

unsafe impl GlobalAlloc for LockedHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let inner = unsafe { &mut *self.0.get() };
        let size = align_up(layout.size().max(MIN_BLOCK), BLOCK_ALIGN);

        // First-fit search
        let mut prev: *mut FreeBlock = core::ptr::null_mut();
        let mut curr = inner.free_list;

        while !curr.is_null() {
            let block_size = unsafe { (*curr).size };
            let block_next = unsafe { (*curr).next };

            if block_size >= size {
                let remaining = block_size - size;

                if remaining >= MIN_BLOCK {
                    // Split: new free block after the allocation
                    let new = (curr as usize + size) as *mut FreeBlock;
                    unsafe {
                        (*new).size = remaining;
                        (*new).next = block_next;
                    }
                    if prev.is_null() {
                        inner.free_list = new;
                    } else {
                        unsafe { (*prev).next = new };
                    }
                } else {
                    // Use the whole block
                    if prev.is_null() {
                        inner.free_list = block_next;
                    } else {
                        unsafe { (*prev).next = block_next };
                    }
                }

                inner.used += size;
                return curr as *mut u8;
            }

            prev = curr;
            curr = block_next;
        }

        core::ptr::null_mut() // out of memory
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let inner = unsafe { &mut *self.0.get() };
        let size = align_up(layout.size().max(MIN_BLOCK), BLOCK_ALIGN);

        let freed = ptr as *mut FreeBlock;
        unsafe {
            (*freed).size = size;
        }

        // Insert into free list sorted by address (enables coalescing)
        let mut prev: *mut FreeBlock = core::ptr::null_mut();
        let mut curr = inner.free_list;
        while !curr.is_null() && (curr as usize) < (freed as usize) {
            prev = curr;
            curr = unsafe { (*curr).next };
        }

        unsafe { (*freed).next = curr };
        if prev.is_null() {
            inner.free_list = freed;
        } else {
            unsafe { (*prev).next = freed };
        }

        // Coalesce with next block if adjacent
        if !curr.is_null() {
            let freed_end = freed as usize + unsafe { (*freed).size };
            if freed_end == curr as usize {
                unsafe {
                    (*freed).size += (*curr).size;
                    (*freed).next = (*curr).next;
                }
            }
        }

        // Coalesce with previous block if adjacent
        if !prev.is_null() {
            let prev_end = prev as usize + unsafe { (*prev).size };
            if prev_end == freed as usize {
                unsafe {
                    (*prev).size += (*freed).size;
                    (*prev).next = (*freed).next;
                }
            }
        }

        inner.used = inner.used.saturating_sub(size);
    }
}

// ---------------------------------------------------------------------------

const fn align_up(val: usize, align: usize) -> usize {
    (val + align - 1) & !(align - 1)
}
