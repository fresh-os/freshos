/// Physical frame allocator — bitmap-based, covering the first 4 GiB.
///
/// Each bit represents a 4 KiB physical frame. Bit 0 = free, bit 1 = used.
/// All bits start as "used"; `init()` clears bits for conventional memory
/// regions reported by the UEFI memory map.
///
/// This is intentionally simple: a flat bitmap scanned linearly with a
/// next-free hint. Good enough for early boot. The real allocator will
/// come when we have a proper kernel heap.
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

pub const FRAME_SIZE: u64 = 4096;

const MAX_PHYS: u64 = 4 * 1024 * 1024 * 1024; // 4 GiB
const MAX_FRAMES: usize = (MAX_PHYS / FRAME_SIZE) as usize; // 1,048,576
const BITMAP_BYTES: usize = MAX_FRAMES / 8; // 131,072 = 128 KiB

// ---------------------------------------------------------------------------
// Bitmap storage (UnsafeCell avoids static-mut-refs lint in edition 2024)
// ---------------------------------------------------------------------------

struct BitmapCell(UnsafeCell<[u8; BITMAP_BYTES]>);
unsafe impl Sync for BitmapCell {}

static BITMAP: BitmapCell = BitmapCell(UnsafeCell::new([0xFF; BITMAP_BYTES]));
static FREE_COUNT: AtomicUsize = AtomicUsize::new(0);
static NEXT_HINT: AtomicUsize = AtomicUsize::new(0);

fn bm() -> *mut u8 {
    BITMAP.0.get().cast::<u8>()
}

// ---------------------------------------------------------------------------
// Region descriptor (shared with main.rs for boot info)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct MemRegion {
    pub start: u64,
    pub pages: u64,
    pub usable: bool,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Mark conventional memory regions as free. Call once at boot.
///
/// # Safety
/// Must be called exactly once, during single-threaded boot.
pub unsafe fn init(regions: &[MemRegion], count: usize) {
    for i in 0..count {
        let r = &regions[i];
        if !r.usable {
            continue;
        }

        let start = (r.start / FRAME_SIZE) as usize;
        let end = ((r.start + r.pages * FRAME_SIZE) / FRAME_SIZE) as usize;

        for frame in start..end.min(MAX_FRAMES) {
            if frame == 0 {
                continue; // never hand out frame 0
            }
            let byte = frame / 8;
            let bit = frame % 8;
            unsafe { *bm().add(byte) &= !(1u8 << bit) };
            FREE_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Allocate a single 4 KiB frame. Returns the physical address, or `None`.
pub fn allocate() -> Option<u64> {
    let hint = NEXT_HINT.load(Ordering::Relaxed);

    for offset in 0..BITMAP_BYTES {
        let idx = (hint + offset) % BITMAP_BYTES;
        let byte = unsafe { *bm().add(idx) };
        if byte == 0xFF {
            continue;
        }

        for bit in 0..8u8 {
            if byte & (1 << bit) == 0 {
                unsafe { *bm().add(idx) |= 1 << bit };
                FREE_COUNT.fetch_sub(1, Ordering::Relaxed);
                NEXT_HINT.store(idx, Ordering::Relaxed);
                let frame = idx * 8 + bit as usize;
                return Some(frame as u64 * FRAME_SIZE);
            }
        }
    }
    None
}

/// Return a frame to the free pool.
///
/// # Safety
/// Frame must have been returned by `allocate()` and must not be in use.
pub unsafe fn deallocate(phys: u64) {
    let frame = (phys / FRAME_SIZE) as usize;
    if frame == 0 || frame >= MAX_FRAMES {
        return;
    }
    let byte = frame / 8;
    let bit = frame % 8;
    unsafe { *bm().add(byte) &= !(1u8 << bit) };
    FREE_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Return `count` contiguous frames to the free pool.
///
/// # Safety
/// Range must have been returned by `allocate_contiguous()` and must not be in use.
pub unsafe fn deallocate_contiguous(base: u64, count: usize) {
    if count == 0 {
        return;
    }
    for offset in 0..count {
        unsafe { deallocate(base + offset as u64 * FRAME_SIZE) };
    }
}

/// Allocate `count` contiguous 4 KiB frames. Returns the base physical address.
pub fn allocate_contiguous(count: usize) -> Option<u64> {
    if count == 0 {
        return None;
    }
    let mut run_start: usize = 1; // skip frame 0
    let mut run_len: usize = 0;

    for frame in 1..MAX_FRAMES {
        let byte = frame / 8;
        let bit = frame % 8;
        let used = unsafe { *bm().add(byte) } & (1 << bit) != 0;

        if !used {
            if run_len == 0 {
                run_start = frame;
            }
            run_len += 1;
            if run_len == count {
                // Mark the entire run as used
                for f in run_start..run_start + count {
                    let b = f / 8;
                    let bi = f % 8;
                    unsafe { *bm().add(b) |= 1u8 << bi };
                }
                FREE_COUNT.fetch_sub(count, Ordering::Relaxed);
                return Some(run_start as u64 * FRAME_SIZE);
            }
        } else {
            run_len = 0;
        }
    }
    None
}

/// Allocate `count` contiguous 4 KiB frames whose base frame is aligned to
/// `align_count` frames. Returns the base physical address.
pub fn allocate_contiguous_aligned(count: usize, align_count: usize) -> Option<u64> {
    if count == 0 || align_count == 0 {
        return None;
    }

    let mut frame = align_count.max(1);
    while frame + count <= MAX_FRAMES {
        if frame % align_count != 0 {
            frame += align_count - (frame % align_count);
            continue;
        }

        let mut free = true;
        for candidate in frame..frame + count {
            let byte = candidate / 8;
            let bit = candidate % 8;
            let used = unsafe { *bm().add(byte) } & (1 << bit) != 0;
            if used {
                free = false;
                break;
            }
        }

        if free {
            for candidate in frame..frame + count {
                let byte = candidate / 8;
                let bit = candidate % 8;
                unsafe { *bm().add(byte) |= 1u8 << bit };
            }
            FREE_COUNT.fetch_sub(count, Ordering::Relaxed);
            return Some(frame as u64 * FRAME_SIZE);
        }

        frame += align_count;
    }

    None
}

pub fn free_count() -> usize {
    FREE_COUNT.load(Ordering::Relaxed)
}

pub fn free_mb() -> usize {
    free_count() * FRAME_SIZE as usize / (1024 * 1024)
}
