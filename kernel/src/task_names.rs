//! Kernel-owned task identity.
//!
//! A small `task id -> human name` registry so introspection (the message-flow
//! view) can label tasks by *who they are*, not by a guessed position in a
//! hardcoded array. Tasks self-register on entry, so a name follows the task
//! regardless of the order or id the scheduler assigns it — which is what made
//! the old position-indexed labels drift once `init` took a slot.

use core::cell::UnsafeCell;

/// Matches `arch::context::MAX_TASKS`. Lookups are bounds-checked, so a smaller
/// real table is harmless.
const MAX: usize = 16;

struct NamesCell(UnsafeCell<[&'static str; MAX]>);
// SAFETY: names are `&'static str` (Copy); writes are single-word stores from
// the registering task, reads are tolerant of races (worst case: a stale or
// empty label for one frame). No references escape.
unsafe impl Sync for NamesCell {}

static NAMES: NamesCell = NamesCell(UnsafeCell::new([""; MAX]));

/// Register a display name for the calling task.
pub fn register_current(name: &'static str) {
    let id = crate::arch::current_task();
    if id < MAX {
        unsafe { (*NAMES.0.get())[id] = name };
    }
}

/// The name registered for a task id, or `""` if none.
pub fn name(id: usize) -> &'static str {
    if id < MAX {
        unsafe { (*NAMES.0.get())[id] }
    } else {
        ""
    }
}
