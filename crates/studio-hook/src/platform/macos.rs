use super::{PatchError, Segment};

const LC_SEGMENT_64: u32 = 0x19;
const MH_MAGIC_64: u32 = 0xfeed_facf;

#[repr(C)]
struct MachHeader64 {
    magic: u32,
    cputype: i32,
    cpusubtype: i32,
    filetype: u32,
    ncmds: u32,
    sizeofcmds: u32,
    flags: u32,
    reserved: u32,
}

#[repr(C)]
struct LoadCommand {
    cmd: u32,
    cmdsize: u32,
}

#[repr(C)]
struct SegmentCommand64 {
    cmd: u32,
    cmdsize: u32,
    segname: [u8; 16],
    vmaddr: u64,
    vmsize: u64,
    fileoff: u64,
    filesize: u64,
    maxprot: i32,
    initprot: i32,
    nsects: u32,
    flags: u32,
}

unsafe extern "C" {
    fn _dyld_image_count() -> u32;
    fn _dyld_get_image_name(index: u32) -> *const libc::c_char;
    fn _dyld_get_image_header(index: u32) -> *const MachHeader64;
    fn _dyld_get_image_vmaddr_slide(index: u32) -> isize;
}

#[derive(Debug, Clone, Copy)]
pub struct Image {
    header: *const MachHeader64,
    pub slide: isize,
}

unsafe impl Send for Image {}
unsafe impl Sync for Image {}

pub fn find_main_image() -> Option<Image> {
    let count = unsafe { _dyld_image_count() };
    for index in 0..count {
        let name_ptr = unsafe { _dyld_get_image_name(index) };
        if name_ptr.is_null() {
            continue;
        }
        let name = unsafe { core::ffi::CStr::from_ptr(name_ptr) };
        let Ok(name) = name.to_str() else { continue };
        if !name.contains("RobloxStudio") || name.ends_with(".dylib") {
            continue;
        }
        let header = unsafe { _dyld_get_image_header(index) };
        if header.is_null() || unsafe { (*header).magic } != MH_MAGIC_64 {
            continue;
        }
        return Some(Image {
            header,
            slide: unsafe { _dyld_get_image_vmaddr_slide(index) },
        });
    }
    None
}

impl Image {
    pub fn segments_with_prefix(&self, prefix: &str) -> Vec<Segment> {
        let mut out = Vec::new();
        let header = self.header;
        let ncmds = unsafe { (*header).ncmds };
        let mut cursor = unsafe { header.add(1) } as *const u8;

        for _ in 0..ncmds {
            let command = cursor as *const LoadCommand;
            let (cmd, cmdsize) = unsafe { ((*command).cmd, (*command).cmdsize) };
            if cmdsize == 0 {
                break;
            }
            if cmd == LC_SEGMENT_64 {
                let segment = cursor as *const SegmentCommand64;
                let raw_name = unsafe { (*segment).segname };
                let end = raw_name.iter().position(|b| *b == 0).unwrap_or(raw_name.len());
                let name = core::str::from_utf8(&raw_name[..end]).unwrap_or("");
                let vmsize = unsafe { (*segment).vmsize };
                if name.starts_with(prefix) && vmsize > 0 {
                    let start = (unsafe { (*segment).vmaddr } as isize + self.slide) as usize;
                    out.push(Segment { start, end: start + vmsize as usize });
                }
            }
            cursor = unsafe { cursor.add(cmdsize as usize) };
        }
        out
    }

    pub fn text_segments(&self) -> Vec<Segment> {
        self.segments_with_prefix("__TEXT")
    }

    pub fn data_segments(&self) -> Vec<Segment> {
        self.segments_with_prefix("__DATA")
    }
}

pub fn find_primary_vtable(rtti_name: &str, text: &[Segment], data: &[Segment]) -> Option<usize> {
    let mut needle = rtti_name.as_bytes().to_vec();
    needle.push(0);

    let name_addr = text.iter().find_map(|segment| {
        let bytes = segment.as_slice()?;
        crate::scan::find_bytes(bytes, &needle).map(|at| segment.start + at)
    })?;

    for name_ref in scan_data_for(data, name_addr) {
        let typeinfo = name_ref.checked_sub(8)?;
        for typeinfo_ref in scan_data_for(data, typeinfo) {
            let offset_to_top_addr = typeinfo_ref.checked_sub(8)?;
            let offset_to_top: isize = match crate::mem::read(offset_to_top_addr) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if offset_to_top != 0 {
                continue;
            }
            return Some(typeinfo_ref + 8);
        }
    }
    None
}

fn scan_data_for(data: &[Segment], value: usize) -> Vec<usize> {
    let mut hits = Vec::new();
    for segment in data {
        let Some(bytes) = segment.as_slice() else { continue };
        hits.extend(crate::scan::find_aligned_usize(bytes, segment.start, value));
    }
    hits
}

pub fn vtable_slot_of(vtable: usize, func: usize, max_slots: usize) -> Option<usize> {
    (0..max_slots).find(|index| {
        crate::mem::read::<usize>(vtable + index * 8).map(|entry| entry == func).unwrap_or(false)
    })
}

pub fn patch_pointer(slot_addr: usize, value: usize) -> Result<usize, PatchError> {
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let page = slot_addr & !(page_size - 1);
    let span = if slot_addr + core::mem::size_of::<usize>() > page + page_size {
        page_size * 2
    } else {
        page_size
    };

    let previous: usize = crate::mem::read(slot_addr).map_err(|_| PatchError::Write)?;

    let made_writable = unsafe {
        libc::mprotect(
            page as *mut libc::c_void,
            span,
            libc::PROT_READ | libc::PROT_WRITE,
        )
    };
    if made_writable != 0 {
        return Err(PatchError::Protect);
    }

    let wrote = crate::mem::write(slot_addr, value);

    unsafe {
        libc::mprotect(page as *mut libc::c_void, span, libc::PROT_READ);
    }

    wrote.map_err(|_| PatchError::Write)?;
    Ok(previous)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_helpers_behave() {
        let segment = Segment { start: 0x1000, end: 0x2000 };
        assert_eq!(segment.len(), 0x1000);
        assert!(segment.contains(0x1000));
        assert!(!segment.contains(0x2000));
        assert!(!segment.is_empty());
    }
}
