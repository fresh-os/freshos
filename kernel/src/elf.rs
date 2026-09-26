const ELF_MAGIC: &[u8; 4] = b"\x7FELF";
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const EM_AARCH64: u16 = 0xB7;
const PT_LOAD: u32 = 1;

// Every header field is untrusted: `off` may come from the file itself, so
// `off + N` must not overflow (a huge e_phoff used to panic here).
fn field<const N: usize>(bytes: &[u8], off: usize) -> Option<[u8; N]> {
    bytes.get(off..off.checked_add(N)?)?.try_into().ok()
}

fn read_u16(bytes: &[u8], off: usize) -> Option<u16> {
    field(bytes, off).map(u16::from_le_bytes)
}

fn read_u32(bytes: &[u8], off: usize) -> Option<u32> {
    field(bytes, off).map(u32::from_le_bytes)
}

fn read_u64(bytes: &[u8], off: usize) -> Option<u64> {
    field(bytes, off).map(u64::from_le_bytes)
}

fn align_down(value: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two());
    value & !(align - 1)
}

struct ImageLayout {
    entry: u64,
    phoff: usize,
    phentsize: usize,
    phnum: usize,
}

/// The fields of a 64-bit program header that the loader reads.
const PHDR_SIZE: usize = 56;

impl ImageLayout {
    /// Program header `idx`, as a slice that holds all of its fields, so
    /// reading them needs no further arithmetic on file-supplied offsets.
    fn program_header<'a>(&self, bytes: &'a [u8], idx: usize) -> Result<&'a [u8], &'static str> {
        let start = idx
            .checked_mul(self.phentsize)
            .and_then(|off| self.phoff.checked_add(off))
            .ok_or("program header offset overflow")?;
        let end = start.checked_add(PHDR_SIZE).ok_or("program header offset overflow")?;
        bytes.get(start..end).ok_or("truncated program header")
    }
}

fn parse_layout(bytes: &[u8]) -> Result<ImageLayout, &'static str> {
    if bytes.len() < 64 {
        return Err("ELF header too small");
    }
    if bytes.get(0..4) != Some(ELF_MAGIC) {
        return Err("bad ELF magic");
    }
    if bytes[4] != ELFCLASS64 || bytes[5] != ELFDATA2LSB {
        return Err("unsupported ELF class/data");
    }

    let e_type = read_u16(bytes, 16).ok_or("missing e_type")?;
    let e_machine = read_u16(bytes, 18).ok_or("missing e_machine")?;
    let e_entry = read_u64(bytes, 24).ok_or("missing e_entry")?;
    let e_phoff = read_u64(bytes, 32).ok_or("missing e_phoff")? as usize;
    let e_phentsize = read_u16(bytes, 54).ok_or("missing e_phentsize")? as usize;
    let e_phnum = read_u16(bytes, 56).ok_or("missing e_phnum")? as usize;

    if e_machine != EM_AARCH64 {
        return Err("unexpected ELF machine");
    }
    if e_type != ET_EXEC && e_type != ET_DYN {
        return Err("unsupported ELF type");
    }
    if e_phentsize < PHDR_SIZE {
        return Err("bad program header size");
    }

    let layout = ImageLayout {
        entry: e_entry,
        phoff: e_phoff,
        phentsize: e_phentsize,
        phnum: e_phnum,
    };
    let mut saw_load = false;

    for idx in 0..e_phnum {
        let ph = layout.program_header(bytes, idx)?;
        let p_type = read_u32(ph, 0).ok_or("truncated program header")?;
        if p_type != PT_LOAD {
            continue;
        }

        let p_offset = read_u64(ph, 8).ok_or("missing p_offset")? as usize;
        let p_vaddr = read_u64(ph, 16).ok_or("missing p_vaddr")?;
        let p_filesz = read_u64(ph, 32).ok_or("missing p_filesz")? as usize;
        let p_memsz = read_u64(ph, 40).ok_or("missing p_memsz")? as usize;
        read_u64(ph, 48).ok_or("missing p_align")?;

        if p_filesz > p_memsz {
            return Err("ELF filesz exceeds memsz");
        }
        if p_offset
            .checked_add(p_filesz)
            .filter(|end| *end <= bytes.len())
            .is_none()
        {
            return Err("ELF segment outside file");
        }

        saw_load = true;
        p_vaddr
            .checked_add(p_memsz as u64)
            .and_then(|end| end.checked_add(4095))
            .ok_or("ELF segment address overflow")?;
    }

    if !saw_load {
        return Err("ELF has no loadable segments");
    }

    Ok(layout)
}

const PF_X: u32 = 1;
const PF_W: u32 = 2;

/// Load an ELF into `space` at its linked addresses, page by page, with
/// permissions from its flags. Refuses W+X segments, segments outside the
/// user window or overlapping the stack, and overlapping segments.
/// Returns the entry point.
pub fn load_into(
    bytes: &[u8],
    space: &mut crate::arch::addrspace::AddressSpace,
) -> Result<u64, &'static str> {
    use crate::arch::addrspace::{Perm, SpaceError, PAGE, in_window};
    use freshos_abi::{USER_BASE, USER_SIZE, USER_STACK_SIZE};

    let layout = parse_layout(bytes)?;
    // Everything the image uses must end below the stack's guard page.
    let image_limit = USER_BASE + USER_SIZE - USER_STACK_SIZE - PAGE;

    for idx in 0..layout.phnum {
        let ph = layout.program_header(bytes, idx)?;
        if read_u32(ph, 0).ok_or("truncated program header")? != PT_LOAD {
            continue;
        }
        let flags = read_u32(ph, 4).ok_or("missing p_flags")?;
        let offset = read_u64(ph, 8).ok_or("missing p_offset")? as usize;
        let vaddr = read_u64(ph, 16).ok_or("missing p_vaddr")?;
        let filesz = read_u64(ph, 32).ok_or("missing p_filesz")?;
        let memsz = read_u64(ph, 40).ok_or("missing p_memsz")?;

        if flags & PF_W != 0 && flags & PF_X != 0 {
            return Err("segment is writable and executable");
        }
        let end = vaddr.checked_add(memsz).ok_or("segment address overflow")?;
        if !in_window(vaddr, memsz) {
            return Err("segment outside the user window");
        }
        if end > image_limit {
            return Err("segment overlaps the stack");
        }
        let perm = if flags & PF_X != 0 {
            Perm::ReadExec
        } else if flags & PF_W != 0 {
            Perm::ReadWrite
        } else {
            Perm::ReadOnly
        };

        // parse_layout checked filesz <= memsz and offset + filesz <= the file
        // length for this same header, so neither sum below can overflow.
        let file_end = vaddr + filesz;
        let mut page = align_down(vaddr, PAGE);
        while page < end {
            let pa = space.map_new_page(page, perm).map_err(|e| match e {
                SpaceError::AlreadyMapped => "segments overlap a page",
                SpaceError::OutsideWindow => "segment outside the user window",
                SpaceError::OutOfMemory => "out of memory",
            })?;
            // Copy the part of the file image that falls in this page; the
            // rest of the page (including .bss) stays zero.
            let copy_start = page.max(vaddr);
            let copy_end = (page + PAGE).min(file_end);
            if copy_start < copy_end {
                let src = offset + (copy_start - vaddr) as usize;
                let n = (copy_end - copy_start) as usize;
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        bytes[src..src + n].as_ptr(),
                        (pa + (copy_start - page)) as *mut u8,
                        n,
                    );
                }
            }
            if perm == Perm::ReadExec {
                // The code was written through the kernel's alias of the
                // frame; make it visible to EL0's instruction fetch.
                crate::arch::paging::sync_icache(pa, PAGE);
            }
            page += PAGE;
        }
    }

    if !in_window(layout.entry, 4) {
        return Err("entry point outside the user window");
    }
    Ok(layout.entry)
}
