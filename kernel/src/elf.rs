use crate::frame_alloc;

pub struct LoadedImage {
    pub entry: u64,
    pub base: u64,
    pub size: usize,
}

const ELF_MAGIC: &[u8; 4] = b"\x7FELF";
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const EM_AARCH64: u16 = 0xB7;
const PT_LOAD: u32 = 1;

fn read_u16(bytes: &[u8], off: usize) -> Option<u16> {
    bytes
        .get(off..off + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
}

fn read_u32(bytes: &[u8], off: usize) -> Option<u32> {
    bytes
        .get(off..off + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn read_u64(bytes: &[u8], off: usize) -> Option<u64> {
    bytes
        .get(off..off + 8)
        .map(|s| u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
}

fn align_down(value: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two());
    value & !(align - 1)
}

fn align_up(value: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two());
    (value + align - 1) & !(align - 1)
}

struct ImageLayout {
    entry: u64,
    min_vaddr: u64,
    image_span: u64,
    max_align: u64,
    phoff: usize,
    phentsize: usize,
    phnum: usize,
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
    if e_phentsize < 56 {
        return Err("bad program header size");
    }

    let mut min_vaddr = u64::MAX;
    let mut max_vaddr = 0u64;
    let mut max_align = 4096u64;
    let mut saw_load = false;

    for idx in 0..e_phnum {
        let ph = e_phoff + idx * e_phentsize;
        let p_type = read_u32(bytes, ph).ok_or("truncated program header")?;
        if p_type != PT_LOAD {
            continue;
        }

        let p_offset = read_u64(bytes, ph + 8).ok_or("missing p_offset")? as usize;
        let p_vaddr = read_u64(bytes, ph + 16).ok_or("missing p_vaddr")?;
        let p_filesz = read_u64(bytes, ph + 32).ok_or("missing p_filesz")? as usize;
        let p_memsz = read_u64(bytes, ph + 40).ok_or("missing p_memsz")? as usize;
        let p_align = read_u64(bytes, ph + 48).ok_or("missing p_align")?.max(4096);

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
        min_vaddr = min_vaddr.min(align_down(p_vaddr, 4096));
        max_vaddr = max_vaddr.max(align_up(p_vaddr + p_memsz as u64, 4096));
        max_align = max_align.max(p_align);
    }

    if !saw_load {
        return Err("ELF has no loadable segments");
    }

    let image_span = max_vaddr
        .checked_sub(min_vaddr)
        .ok_or("ELF image span overflow")?;

    Ok(ImageLayout {
        entry: e_entry,
        min_vaddr,
        image_span,
        max_align,
        phoff: e_phoff,
        phentsize: e_phentsize,
        phnum: e_phnum,
    })
}

fn copy_segments(bytes: &[u8], layout: &ImageLayout, image_base: u64) -> Result<(), &'static str> {
    unsafe {
        core::ptr::write_bytes(image_base as *mut u8, 0, layout.image_span as usize);
    }

    let load_bias = image_base
        .checked_sub(layout.min_vaddr)
        .ok_or("ELF load bias underflow")?;

    for idx in 0..layout.phnum {
        let ph = layout.phoff + idx * layout.phentsize;
        let p_type = read_u32(bytes, ph).ok_or("truncated program header")?;
        if p_type != PT_LOAD {
            continue;
        }

        let p_offset = read_u64(bytes, ph + 8).ok_or("missing p_offset")? as usize;
        let p_vaddr = read_u64(bytes, ph + 16).ok_or("missing p_vaddr")?;
        let p_filesz = read_u64(bytes, ph + 32).ok_or("missing p_filesz")? as usize;
        let dest = load_bias
            .checked_add(p_vaddr)
            .ok_or("ELF destination overflow")?;

        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr().add(p_offset), dest as *mut u8, p_filesz);
        }
    }

    Ok(())
}

pub fn load_image(bytes: &[u8]) -> Result<LoadedImage, &'static str> {
    let layout = parse_layout(bytes)?;
    let alloc_bytes = layout
        .image_span
        .checked_add(layout.max_align)
        .ok_or("ELF allocation overflow")?;
    let alloc_pages = align_up(alloc_bytes, 4096) as usize / 4096;
    let raw_base = frame_alloc::allocate_contiguous(alloc_pages).ok_or("out of frames for ELF")?;
    let image_base = align_up(raw_base, layout.max_align);
    copy_segments(bytes, &layout, image_base)?;

    Ok(LoadedImage {
        entry: image_base
            .checked_sub(layout.min_vaddr)
            .ok_or("ELF load bias underflow")?
            .checked_add(layout.entry)
            .ok_or("ELF entry overflow")?,
        base: image_base,
        size: layout.image_span as usize,
    })
}

pub fn load_image_into(
    bytes: &[u8],
    image_base: u64,
    region_size: usize,
) -> Result<LoadedImage, &'static str> {
    let layout = parse_layout(bytes)?;
    if image_base & (layout.max_align - 1) != 0 {
        return Err("ELF target base misaligned");
    }
    if layout.image_span as usize > region_size {
        return Err("ELF image does not fit reserved region");
    }

    copy_segments(bytes, &layout, image_base)?;

    Ok(LoadedImage {
        entry: image_base
            .checked_sub(layout.min_vaddr)
            .ok_or("ELF load bias underflow")?
            .checked_add(layout.entry)
            .ok_or("ELF entry overflow")?,
        base: image_base,
        size: layout.image_span as usize,
    })
}
