/// The task registry: the kernel's first-hand record of every task and every
/// service by name (EL0 isolation spec, section 5). The kernel keeps the
/// facts; init keeps the policy. MCP, the flow view and the shell read this,
/// so none of them has to trust init's account.
use core::cell::UnsafeCell;

use freshos_abi::{ExitReason, NAME_LEN};

use crate::arch::IrqGuard;
use crate::arch::context::MAX_TASKS;

const MAX_SERVICES: usize = 32;

#[derive(Clone, Copy)]
pub struct Name {
    bytes: [u8; NAME_LEN],
    len: u8,
}

impl Name {
    pub const EMPTY: Name = Name { bytes: [0; NAME_LEN], len: 0 };

    pub fn new(name: &[u8]) -> Name {
        let len = name.len().min(NAME_LEN);
        let mut bytes = [0; NAME_LEN];
        bytes[..len].copy_from_slice(&name[..len]);
        Name { bytes, len: len as u8 }
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("?")
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[derive(Clone, Copy)]
pub struct ServiceRecord {
    pub name: Name,
    pub task: Option<u16>,
    pub starts: u64,
    pub exits: u64,
    pub last_exit: Option<ExitReason>,
}

struct Registry {
    task_names: [Name; MAX_TASKS],
    services: [Option<ServiceRecord>; MAX_SERVICES],
}

struct Cell(UnsafeCell<Registry>);
// SAFETY: every access holds an IrqGuard (one CPU, preemptible tasks).
unsafe impl Sync for Cell {}

static REGISTRY: Cell = Cell(UnsafeCell::new(Registry {
    task_names: [Name::EMPTY; MAX_TASKS],
    services: [None; MAX_SERVICES],
}));

fn with<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    let _irq = IrqGuard::mask();
    f(unsafe { &mut *REGISTRY.0.get() })
}

/// A task started as the service `name`.
pub fn on_spawn(task: usize, name: &[u8]) {
    let name = Name::new(name);
    with(|r| {
        if task < MAX_TASKS {
            r.task_names[task] = name;
        }
        let slot = r
            .services
            .iter()
            .position(|s| s.is_some_and(|s| s.name.as_str() == name.as_str()))
            .or_else(|| r.services.iter().position(Option::is_none));
        if let Some(index) = slot {
            let record = r.services[index].get_or_insert(ServiceRecord {
                name,
                task: None,
                starts: 0,
                exits: 0,
                last_exit: None,
            });
            record.task = Some(task as u16);
            record.starts += 1;
        }
    });
}

/// A task ended.
pub fn on_exit(task: usize, reason: ExitReason) {
    with(|r| {
        for record in r.services.iter_mut().flatten() {
            if record.task == Some(task as u16) {
                record.task = None;
                record.exits += 1;
                record.last_exit = Some(reason);
            }
        }
        if task < MAX_TASKS {
            r.task_names[task] = Name::EMPTY;
        }
    });
}

/// The name a task was started as (empty for tasks never named).
pub fn name(task: usize) -> Name {
    with(|r| r.task_names.get(task).copied().unwrap_or(Name::EMPTY))
}

/// A snapshot of every service record.
pub fn services() -> [Option<ServiceRecord>; MAX_SERVICES] {
    with(|r| r.services)
}
