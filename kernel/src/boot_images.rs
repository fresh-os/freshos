/// Boot images: every `*.ELF` in `\EFI\FreshOS\`, read into memory while
/// UEFI's file access still exists. The kernel requires only INIT.ELF;
/// everything else is found here by name when init asks (decision 0006).
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

use uefi::boot::{self, MemoryType};
use uefi::cstr16;
use uefi::proto::media::file::{File, FileAttribute, FileMode};

use crate::serial::serial_println;

const MAX_IMAGES: usize = 16;
const NAME_LEN: usize = 16;

#[derive(Clone, Copy)]
struct Image {
    name: [u8; NAME_LEN],
    name_len: usize,
    ptr: *const u8,
    len: usize,
}

const EMPTY: Image = Image { name: [0; NAME_LEN], name_len: 0, ptr: core::ptr::null(), len: 0 };

struct Table(UnsafeCell<[Image; MAX_IMAGES]>);
// SAFETY: written only by `load_all` during single-threaded boot, read-only after.
unsafe impl Sync for Table {}

static IMAGES: Table = Table(UnsafeCell::new([EMPTY; MAX_IMAGES]));
static COUNT: AtomicUsize = AtomicUsize::new(0);

/// Read every `*.ELF` in `\EFI\FreshOS\`. Call before `exit_boot_services`.
pub fn load_all() {
    let Ok(mut fs) = boot::get_image_file_system(boot::image_handle()) else {
        serial_println!("  Boot images: no file system");
        return;
    };
    let Ok(mut root) = fs.open_volume() else {
        serial_println!("  Boot images: volume won't open");
        return;
    };
    let Some(mut dir) = root
        .open(cstr16!("\\EFI\\FreshOS"), FileMode::Read, FileAttribute::empty())
        .ok()
        .and_then(|handle| handle.into_directory())
    else {
        serial_println!("  Boot images: \\EFI\\FreshOS missing");
        return;
    };

    let mut info_buf = [0u8; 512];
    while let Ok(Some(info)) = dir.read_entry(&mut info_buf) {
        if !info.is_regular_file() {
            continue;
        }
        let mut name = [0u8; NAME_LEN];
        let mut name_len = 0;
        let mut ascii = true;
        for ch in info.file_name().iter() {
            let c = u16::from(*ch);
            if c >= 0x80 || name_len == NAME_LEN {
                ascii = false;
                break;
            }
            name[name_len] = c as u8;
            name_len += 1;
        }
        let has_elf_suffix =
            name_len >= 4 && name[name_len - 4..name_len].eq_ignore_ascii_case(b".ELF");
        if !ascii || !has_elf_suffix {
            continue;
        }
        let len = info.file_size() as usize;
        let Some(file) = dir
            .open(info.file_name(), FileMode::Read, FileAttribute::empty())
            .ok()
            .and_then(|handle| handle.into_regular_file())
        else {
            continue;
        };
        let Some(ptr) = read_whole(file, len) else { continue };

        let index = COUNT.load(Ordering::Relaxed);
        if index == MAX_IMAGES {
            serial_println!("  Boot images: more than {} ELFs, ignoring the rest", MAX_IMAGES);
            break;
        }
        unsafe { (*IMAGES.0.get())[index] = Image { name, name_len, ptr, len } };
        COUNT.store(index + 1, Ordering::Relaxed);
        serial_println!(
            "  Boot image {} ({} bytes)",
            core::str::from_utf8(&name[..name_len]).unwrap_or("?"),
            len
        );
    }
}

fn read_whole(mut file: uefi::proto::media::file::RegularFile, len: usize) -> Option<*const u8> {
    let pool = boot::allocate_pool(MemoryType::LOADER_DATA, len.max(1)).ok()?;
    let buf = unsafe { core::slice::from_raw_parts_mut(pool.as_ptr(), len) };
    match file.read(buf) {
        Ok(read) if read == len => Some(pool.as_ptr()),
        _ => None,
    }
}

/// The image whose ESP file name is `name` (case-insensitive), e.g. "PONG.ELF".
pub fn find(name: &str) -> Option<&'static [u8]> {
    let images = unsafe { &*IMAGES.0.get() };
    images[..COUNT.load(Ordering::Relaxed)]
        .iter()
        .find(|image| image.name[..image.name_len].eq_ignore_ascii_case(name.as_bytes()))
        .map(|image| unsafe { core::slice::from_raw_parts(image.ptr, image.len) })
}
