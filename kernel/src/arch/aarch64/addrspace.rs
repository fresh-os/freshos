/// Per-task address spaces (EL0 isolation spec, section 3).
///
/// Every task's top-level table copies the kernel's top-level entries, so the
/// kernel (RAM and devices, EL1-only) looks the same in every address space.
/// Slot 16, the 1 GiB window at 0x4_0000_0000, is private to the task: its
/// code, data and stack live there, mapped EL0-accessible and not-global, so
/// the TLB tags them with the task's ASID.
///
/// The kernel never dereferences a user virtual address. `copy_from_user` and
/// `copy_to_user` translate through the task's own table and copy through the
/// kernel's identity mapping of the frame, so PAN never has to be lifted.
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use freshos_abi::{Error, USER_BASE, USER_SIZE};

use crate::frame_alloc;

const VALID: u64 = 1 << 0;
const TABLE_OR_PAGE: u64 = 1 << 1;
const AP_EL0_RW: u64 = 0b01 << 6;
const AP_EL0_RO: u64 = 0b11 << 6;
const AP_MASK: u64 = 0b11 << 6;
const AF: u64 = 1 << 10;
const NG: u64 = 1 << 11;
const PXN: u64 = 1 << 53;
const UXN: u64 = 1 << 54;
const ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;
/// AttrIndx (bits 4:2) and shareability (bits 9:8): the memory type.
const MEMORY_TYPE_MASK: u64 = (0b111 << 2) | (0b11 << 8);

pub const PAGE: u64 = 4096;
pub const USER_L1_INDEX: usize = (USER_BASE >> 30) as usize;

static KERNEL_L1: AtomicU64 = AtomicU64::new(0);
static L1_ENTRIES: AtomicUsize = AtomicUsize::new(0);
static RAM_MEMORY_TYPE: AtomicU64 = AtomicU64::new(0);

// The names say what EL0 may do with the page; the shared prefix is the point.
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Perm {
    ReadOnly,
    ReadWrite,
    ReadExec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpaceError {
    OutOfMemory,
    OutsideWindow,
    AlreadyMapped,
}

/// Record the kernel's top-level table and the firmware's memory type for
/// RAM, sampled from the mapping of `ram_va`. Refuses geometries this module
/// doesn't handle, and a firmware map that already uses slot 16.
pub fn init(kernel_l1: u64, t0sz: u32, ram_va: u64) -> Result<(), &'static str> {
    // The walk must start at level 1 (T0SZ 25..=33), and the address space
    // must reach past 17 GiB for the user window (T0SZ <= 29).
    if !(25..=29).contains(&t0sz) {
        return Err("unsupported firmware page-table geometry");
    }
    if read_entry(kernel_l1, USER_L1_INDEX) & VALID != 0 {
        return Err("firmware map already uses the user window (slot 16)");
    }
    let leaf = kernel_leaf(kernel_l1, ram_va).ok_or("kernel RAM is not mapped")?;
    KERNEL_L1.store(kernel_l1, Ordering::Relaxed);
    L1_ENTRIES.store(1 << (34 - t0sz), Ordering::Relaxed);
    RAM_MEMORY_TYPE.store(leaf & MEMORY_TYPE_MASK, Ordering::Relaxed);
    Ok(())
}

/// Whether `[va, va + len)` lies entirely inside the user window.
pub fn in_window(va: u64, len: u64) -> bool {
    va >= USER_BASE && len <= USER_SIZE && va - USER_BASE <= USER_SIZE - len
}

pub struct AddressSpace {
    l1: u64,
    asid: u16,
    /// Every frame this space owns: page tables and data. Freed on drop.
    frames: Vec<u64>,
}

impl AddressSpace {
    pub fn new(asid: u16) -> Result<Self, SpaceError> {
        let mut space = AddressSpace { l1: 0, asid, frames: Vec::new() };
        space.l1 = space.alloc_zeroed()?;
        let kernel = KERNEL_L1.load(Ordering::Relaxed);
        for index in 0..L1_ENTRIES.load(Ordering::Relaxed) {
            if index != USER_L1_INDEX {
                write_entry(space.l1, index, read_entry(kernel, index));
            }
        }
        Ok(space)
    }

    /// The TTBR0_EL1 value for this space: table address plus ASID in bits 63:48.
    pub fn ttbr0(&self) -> u64 {
        self.l1 | ((self.asid as u64) << 48)
    }

    /// Map a fresh, zeroed page at `va` with `perm`. Returns its physical address.
    pub fn map_new_page(&mut self, va: u64, perm: Perm) -> Result<u64, SpaceError> {
        if !va.is_multiple_of(PAGE) || !in_window(va, PAGE) {
            return Err(SpaceError::OutsideWindow);
        }
        let l3 = self.l3_table_for(va)?;
        let index = ((va >> 12) & 0x1FF) as usize;
        if read_entry(l3, index) & VALID != 0 {
            return Err(SpaceError::AlreadyMapped);
        }
        let pa = self.alloc_zeroed()?;
        let access = match perm {
            Perm::ReadOnly => AP_EL0_RO | PXN | UXN,
            Perm::ReadWrite => AP_EL0_RW | PXN | UXN,
            Perm::ReadExec => AP_EL0_RO | PXN,
        };
        let memory_type = RAM_MEMORY_TYPE.load(Ordering::Relaxed);
        write_entry(l3, index, pa | VALID | TABLE_OR_PAGE | AF | NG | memory_type | access);
        Ok(pa)
    }

    /// Make every mapping made so far visible to the table walker. Call once
    /// after a batch of `map_new_page` and before the space is first used.
    pub fn publish(&self) {
        unsafe { core::arch::asm!("dsb ishst", options(nostack)) };
    }

    /// The physical address behind `va`, if EL0 may read it (or write it, when
    /// `write`). This is the check every user pointer passes through.
    pub fn translate(&self, va: u64, write: bool) -> Option<u64> {
        if !in_window(va, 1) {
            return None;
        }
        let l2 = read_entry(self.l1, USER_L1_INDEX);
        if l2 & VALID == 0 {
            return None;
        }
        let l3 = read_entry(l2 & ADDR_MASK, ((va >> 21) & 0x1FF) as usize);
        if l3 & VALID == 0 {
            return None;
        }
        let pte = read_entry(l3 & ADDR_MASK, ((va >> 12) & 0x1FF) as usize);
        if pte & VALID == 0 {
            return None;
        }
        let ap = pte & AP_MASK;
        let allowed = if write { ap == AP_EL0_RW } else { ap == AP_EL0_RW || ap == AP_EL0_RO };
        allowed.then_some((pte & ADDR_MASK) | (va & (PAGE - 1)))
    }

    fn l3_table_for(&mut self, va: u64) -> Result<u64, SpaceError> {
        let l2 = self.child_table(self.l1, USER_L1_INDEX)?;
        self.child_table(l2, ((va >> 21) & 0x1FF) as usize)
    }

    fn child_table(&mut self, table: u64, index: usize) -> Result<u64, SpaceError> {
        let entry = read_entry(table, index);
        if entry & VALID != 0 {
            return Ok(entry & ADDR_MASK);
        }
        let child = self.alloc_zeroed()?;
        write_entry(table, index, child | VALID | TABLE_OR_PAGE);
        Ok(child)
    }

    fn alloc_zeroed(&mut self) -> Result<u64, SpaceError> {
        let frame = frame_alloc::allocate().ok_or(SpaceError::OutOfMemory)?;
        unsafe { core::ptr::write_bytes(frame as *mut u8, 0, PAGE as usize) };
        self.frames.push(frame);
        Ok(frame)
    }
}

impl Drop for AddressSpace {
    /// The caller must already have moved TTBR0 off this space (see
    /// `context::retire_current`): its tables are freed here.
    fn drop(&mut self) {
        let asid = (self.asid as u64) << 48;
        unsafe {
            core::arch::asm!("dsb ishst", "tlbi aside1is, {0}", "dsb ish", "isb", in(reg) asid, options(nostack));
        }
        for &frame in &self.frames {
            unsafe { frame_alloc::deallocate(frame) };
        }
    }
}

/// Check that every page of `[va, va + len)` is EL0-accessible (writable, if
/// `write`) before any byte is copied. Then no copy can fail halfway.
fn check_range(space: &AddressSpace, va: u64, len: u64, write: bool) -> Result<(), Error> {
    if !in_window(va, len) {
        return Err(Error::BadPointer);
    }
    if len == 0 {
        return Ok(());
    }
    let mut page = va & !(PAGE - 1);
    while page < va + len {
        space.translate(page, write).ok_or(Error::BadPointer)?;
        page += PAGE;
    }
    Ok(())
}

// The syscall layer (EL0 isolation plan, Task 5) is the first caller.
#[allow(dead_code)]
pub fn copy_from_user(space: &AddressSpace, va: u64, out: &mut [u8]) -> Result<(), Error> {
    check_range(space, va, out.len() as u64, false)?;
    let mut done = 0;
    while done < out.len() {
        let at = va + done as u64;
        let pa = space.translate(at, false).ok_or(Error::BadPointer)?;
        let chunk = ((PAGE - at % PAGE) as usize).min(out.len() - done);
        unsafe {
            core::ptr::copy_nonoverlapping(pa as *const u8, out[done..].as_mut_ptr(), chunk);
        }
        done += chunk;
    }
    Ok(())
}

// The syscall layer (EL0 isolation plan, Task 5) is the first caller.
#[allow(dead_code)]
pub fn copy_to_user(space: &AddressSpace, va: u64, data: &[u8]) -> Result<(), Error> {
    check_range(space, va, data.len() as u64, true)?;
    let mut done = 0;
    while done < data.len() {
        let at = va + done as u64;
        let pa = space.translate(at, true).ok_or(Error::BadPointer)?;
        let chunk = ((PAGE - at % PAGE) as usize).min(data.len() - done);
        unsafe {
            core::ptr::copy_nonoverlapping(data[done..].as_ptr(), pa as *mut u8, chunk);
        }
        done += chunk;
    }
    Ok(())
}

/// Read a `T` from user memory. The address must be aligned for `T`.
// The syscall layer (EL0 isolation plan, Task 5) is the first caller.
#[allow(dead_code)]
pub fn read_user<T: Copy>(space: &AddressSpace, va: u64) -> Result<T, Error> {
    if !va.is_multiple_of(core::mem::align_of::<T>() as u64) {
        return Err(Error::BadPointer);
    }
    let mut value = core::mem::MaybeUninit::<T>::uninit();
    let bytes = unsafe {
        core::slice::from_raw_parts_mut(value.as_mut_ptr() as *mut u8, core::mem::size_of::<T>())
    };
    copy_from_user(space, va, bytes)?;
    Ok(unsafe { value.assume_init() })
}

/// Write a `T` to user memory. The address must be aligned for `T`.
// The syscall layer (EL0 isolation plan, Task 5) is the first caller.
#[allow(dead_code)]
pub fn write_user<T: Copy>(space: &AddressSpace, va: u64, value: &T) -> Result<(), Error> {
    if !va.is_multiple_of(core::mem::align_of::<T>() as u64) {
        return Err(Error::BadPointer);
    }
    let bytes = unsafe {
        core::slice::from_raw_parts(value as *const T as *const u8, core::mem::size_of::<T>())
    };
    copy_to_user(space, va, bytes)
}

/// Check that a `T` at `va` could be written, without writing it.
// The syscall layer (EL0 isolation plan, Task 5) is the first caller.
#[allow(dead_code)]
pub fn check_writable<T>(space: &AddressSpace, va: u64) -> Result<(), Error> {
    if !va.is_multiple_of(core::mem::align_of::<T>() as u64) {
        return Err(Error::BadPointer);
    }
    check_range(space, va, core::mem::size_of::<T>() as u64, true)
}

fn kernel_leaf(l1: u64, va: u64) -> Option<u64> {
    let mut table = l1;
    for (level, shift) in [(1u32, 30u32), (2, 21), (3, 12)] {
        let entry = read_entry(table, ((va >> shift) & 0x1FF) as usize);
        if entry & VALID == 0 {
            return None;
        }
        if level == 3 || entry & TABLE_OR_PAGE == 0 {
            return Some(entry);
        }
        table = entry & ADDR_MASK;
    }
    None
}

fn read_entry(table: u64, index: usize) -> u64 {
    unsafe { core::ptr::read_volatile((table as *const u64).add(index)) }
}

fn write_entry(table: u64, index: usize, value: u64) {
    unsafe { core::ptr::write_volatile((table as *mut u64).add(index), value) }
}
