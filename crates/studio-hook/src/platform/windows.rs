use super::{PatchError, Segment};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{PAGE_READONLY, PAGE_READWRITE, VirtualProtect};

const IMAGE_DOS_SIGNATURE: u16 = 0x5a4d;
const IMAGE_NT_SIGNATURE: u32 = 0x0000_4550;

#[repr(C)]
struct ImageDosHeader {
    e_magic: u16,
    _pad: [u16; 29],
    e_lfanew: i32,
}

#[repr(C)]
struct ImageFileHeader {
    machine: u16,
    number_of_sections: u16,
    time_date_stamp: u32,
    pointer_to_symbol_table: u32,
    number_of_symbols: u32,
    size_of_optional_header: u16,
    characteristics: u16,
}

#[repr(C)]
struct ImageSectionHeader {
    name: [u8; 8],
    virtual_size: u32,
    virtual_address: u32,
    size_of_raw_data: u32,
    pointer_to_raw_data: u32,
    pointer_to_relocations: u32,
    pointer_to_linenumbers: u32,
    number_of_relocations: u16,
    number_of_linenumbers: u16,
    characteristics: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct Image {
    base: usize,
    pub slide: isize,
}

unsafe impl Send for Image {}
unsafe impl Sync for Image {}

pub fn find_main_image() -> Option<Image> {
    let base = unsafe { GetModuleHandleW(core::ptr::null()) } as usize;
    if base == 0 {
        return None;
    }
    let dos = base as *const ImageDosHeader;
    if unsafe { (*dos).e_magic } != IMAGE_DOS_SIGNATURE {
        return None;
    }
    let nt = base + unsafe { (*dos).e_lfanew } as usize;
    if unsafe { *(nt as *const u32) } != IMAGE_NT_SIGNATURE {
        return None;
    }
    Some(Image { base, slide: 0 })
}

impl Image {
    fn sections(&self) -> Vec<(String, Segment)> {
        let mut out = Vec::new();
        let dos = self.base as *const ImageDosHeader;
        let nt = self.base + unsafe { (*dos).e_lfanew } as usize;
        let file_header = (nt + 4) as *const ImageFileHeader;
        let count = unsafe { (*file_header).number_of_sections } as usize;
        let optional_size = unsafe { (*file_header).size_of_optional_header } as usize;
        let first = nt + 4 + core::mem::size_of::<ImageFileHeader>() + optional_size;

        for index in 0..count {
            let section =
                (first + index * core::mem::size_of::<ImageSectionHeader>()) as *const ImageSectionHeader;
            let raw_name = unsafe { (*section).name };
            let end = raw_name.iter().position(|b| *b == 0).unwrap_or(raw_name.len());
            let name = String::from_utf8_lossy(&raw_name[..end]).into_owned();
            let size = unsafe { (*section).virtual_size } as usize;
            if size == 0 {
                continue;
            }
            let start = self.base + unsafe { (*section).virtual_address } as usize;
            out.push((name, Segment { start, end: start + size }));
        }
        out
    }

    pub fn segments_with_prefix(&self, prefix: &str) -> Vec<Segment> {
        self.sections()
            .into_iter()
            .filter(|(name, _)| name.starts_with(prefix))
            .map(|(_, segment)| segment)
            .collect()
    }

    pub fn text_segments(&self) -> Vec<Segment> {
        self.segments_with_prefix(".text")
    }

    pub fn data_segments(&self) -> Vec<Segment> {
        let mut out = self.segments_with_prefix(".data");
        out.extend(self.segments_with_prefix(".rdata"));
        out
    }
}

fn scan_data_for(data: &[Segment], value: usize) -> Vec<usize> {
    let mut hits = Vec::new();
    for segment in data {
        let Some(bytes) = segment.as_slice() else { continue };
        hits.extend(crate::scan::find_aligned_usize(bytes, segment.start, value));
    }
    hits
}

fn scan_data_for_rva(data: &[Segment], value: u32) -> Vec<usize> {
    let mut hits = Vec::new();
    for segment in data {
        let Some(bytes) = segment.as_slice() else { continue };
        hits.extend(crate::scan::find_aligned_u32(bytes, segment.start, value));
    }
    hits
}

const COL_SIGNATURE_OFFSET: usize = 0;
const COL_OFFSET_OFFSET: usize = 4;
const COL_TYPE_DESCRIPTOR_OFFSET: usize = 12;
const COL_SELF_OFFSET: usize = 20;
const TYPE_DESCRIPTOR_NAME_OFFSET: usize = 16;

pub fn find_primary_vtable(rtti_name: &str, _text: &[Segment], data: &[Segment]) -> Option<usize> {
    let base = unsafe { GetModuleHandleW(core::ptr::null()) } as usize;
    if base == 0 {
        return None;
    }

    let mut needle = rtti_name.as_bytes().to_vec();
    needle.push(0);
    let name_addr = data.iter().find_map(|segment| {
        let bytes = segment.as_slice()?;
        crate::scan::find_bytes(bytes, &needle).map(|at| segment.start + at)
    })?;

    let type_descriptor = name_addr.checked_sub(TYPE_DESCRIPTOR_NAME_OFFSET)?;
    let type_descriptor_rva = (type_descriptor.checked_sub(base)?) as u32;

    for descriptor_ref in scan_data_for_rva(data, type_descriptor_rva) {
        let locator = descriptor_ref.checked_sub(COL_TYPE_DESCRIPTOR_OFFSET)?;
        let signature: u32 = crate::mem::read(locator + COL_SIGNATURE_OFFSET).ok()?;
        let offset: u32 = crate::mem::read(locator + COL_OFFSET_OFFSET).ok()?;
        let self_rva: u32 = crate::mem::read(locator + COL_SELF_OFFSET).ok()?;
        if signature != 1 || offset != 0 {
            continue;
        }
        if self_rva as usize != locator.wrapping_sub(base) {
            continue;
        }
        if let Some(vtable_ref) = scan_data_for(data, locator).into_iter().next() {
            return Some(vtable_ref + 8);
        }
    }
    None
}

pub fn vtable_slot_of(vtable: usize, func: usize, max_slots: usize) -> Option<usize> {
    (0..max_slots).find(|index| {
        crate::mem::read::<usize>(vtable + index * 8).map(|entry| entry == func).unwrap_or(false)
    })
}

pub fn patch_pointer(slot_addr: usize, value: usize) -> Result<usize, PatchError> {
    let previous: usize = crate::mem::read(slot_addr).map_err(|_| PatchError::Write)?;

    let mut old_protect = 0u32;
    let made_writable = unsafe {
        VirtualProtect(
            slot_addr as *const core::ffi::c_void,
            core::mem::size_of::<usize>(),
            PAGE_READWRITE,
            &mut old_protect,
        )
    };
    if made_writable == 0 {
        return Err(PatchError::Protect);
    }

    let wrote = crate::mem::write(slot_addr, value);

    let mut restored = 0u32;
    unsafe {
        VirtualProtect(
            slot_addr as *const core::ffi::c_void,
            core::mem::size_of::<usize>(),
            if old_protect == 0 { PAGE_READONLY } else { old_protect },
            &mut restored,
        );
    }

    wrote.map_err(|_| PatchError::Write)?;
    Ok(previous)
}
