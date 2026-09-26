/// Per-task handle tables (EL0 isolation spec, section 4). A handle is a slot
/// index; the slot names a channel and what the task may do with it. Tasks
/// never see raw channel numbers.
use freshos_abi::{Error, Handle, MAX_HANDLES, Rights};

#[derive(Clone, Copy, Debug)]
pub struct Slot {
    pub channel: u32,
    pub rights: Rights,
}

#[derive(Clone, Copy)]
pub struct HandleTable {
    slots: [Option<Slot>; MAX_HANDLES],
}

impl HandleTable {
    pub const EMPTY: HandleTable = HandleTable { slots: [None; MAX_HANDLES] };

    /// The channel behind `handle`, if it carries `need`.
    pub fn lookup(&self, handle: Handle, need: Rights) -> Result<u32, Error> {
        let slot = self
            .slots
            .get(handle.0 as usize)
            .copied()
            .flatten()
            .ok_or(Error::NoSuchHandle)?;
        if slot.rights.contains(need) {
            Ok(slot.channel)
        } else {
            Err(Error::NoRight)
        }
    }

    pub fn insert(&mut self, slot: Slot) -> Result<Handle, Error> {
        let index = self.slots.iter().position(Option::is_none).ok_or(Error::TableFull)?;
        self.slots[index] = Some(slot);
        Ok(Handle(index as u32))
    }

    // SPAWN (EL0 isolation plan, Task 7) is the first caller.
    #[allow(dead_code)]
    pub fn get_mut(&mut self, handle: Handle) -> Option<&mut Slot> {
        self.slots.get_mut(handle.0 as usize).and_then(Option::as_mut)
    }

    /// Every occupied slot, as (slot index, slot).
    // SPAWN (EL0 isolation plan, Task 7) is the first caller.
    #[allow(dead_code)]
    pub fn slots(&self) -> impl Iterator<Item = (u32, Slot)> + '_ {
        self.slots.iter().enumerate().filter_map(|(i, s)| s.map(|s| (i as u32, s)))
    }
}
