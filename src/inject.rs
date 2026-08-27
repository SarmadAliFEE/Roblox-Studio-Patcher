use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

const LC_SEGMENT_64: u32 = 0x19;
const LC_LOAD_DYLIB: u32 = 0xc;

fn earliest_section_offset(data: &[u8], sizeofcmds: u32) -> Option<u32> {
    let mut pos: usize = 32;
    let end: usize = 32 + sizeofcmds as usize;
    let mut earliest: Option<u32> = None;
    while pos < end {
        let cmd: u32 = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
        let cmdsize: u32 = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap());
        if cmd == LC_SEGMENT_64 {
            let nsects: u32 = u32::from_le_bytes(data[pos + 64..pos + 68].try_into().unwrap());
            let mut sect: usize = pos + 72;
            for _ in 0..nsects {
                let offset: u32 = u32::from_le_bytes(data[sect + 48..sect + 52].try_into().unwrap());
                if offset != 0 && earliest.is_none_or(|e: u32| offset < e) {
                    earliest = Some(offset);
                }
                sect += 80;
            }
        }
        pos += cmdsize as usize;
    }
    earliest
}

fn dylib_paths(data: &[u8], sizeofcmds: u32) -> Vec<String> {
    let mut pos: usize = 32;
    let end: usize = 32 + sizeofcmds as usize;
    let mut paths: Vec<String> = vec![];
    while pos < end {
        let cmd: u32 = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
        let cmdsize: u32 = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap());
        if cmd == LC_LOAD_DYLIB {
            let name_off: u32 = u32::from_le_bytes(data[pos + 8..pos + 12].try_into().unwrap());
            let start: usize = pos + name_off as usize;
            let cmd_end: usize = pos + cmdsize as usize;
            let str_end: usize = data[start..cmd_end]
                .iter()
                .position(|&b: &u8| b == 0)
                .map(|i: usize| start + i)
                .unwrap_or(cmd_end);
            paths.push(String::from_utf8_lossy(&data[start..str_end]).into_owned());
        }
        pos += cmdsize as usize;
    }
    paths
}

fn build_load_dylib_command(dylib_path: &str) -> Vec<u8> {
    let header_len: usize = 24;
    let raw_len: usize = header_len + dylib_path.len() + 1;
    let cmdsize: usize = raw_len.div_ceil(8) * 8;

    let mut cmd: Vec<u8> = Vec::with_capacity(cmdsize);
    cmd.extend_from_slice(&LC_LOAD_DYLIB.to_le_bytes());
    cmd.extend_from_slice(&(cmdsize as u32).to_le_bytes());
    cmd.extend_from_slice(&(header_len as u32).to_le_bytes());
    cmd.extend_from_slice(&0u32.to_le_bytes());
    cmd.extend_from_slice(&0u32.to_le_bytes());
    cmd.extend_from_slice(&0u32.to_le_bytes());
    cmd.extend_from_slice(dylib_path.as_bytes());
    cmd.resize(cmdsize, 0);
    cmd
}

fn inject_dylib_macho(macho_path: &Path, dylib_path: &str) -> Result<()> {
    let mut data: Vec<u8> = fs::read(macho_path)?;

    let ncmds: u32 = u32::from_le_bytes(data[16..20].try_into().unwrap());
    let sizeofcmds: u32 = u32::from_le_bytes(data[20..24].try_into().unwrap());

    if dylib_paths(&data, sizeofcmds).iter().any(|p: &String| p == dylib_path) {
        println!("{dylib_path} already loaded, nothing to do");
        return Ok(());
    }

    let cmd: Vec<u8> = build_load_dylib_command(dylib_path);
    let cmds_end: usize = 32 + sizeofcmds as usize;
    let earliest: usize = earliest_section_offset(&data, sizeofcmds)
        .context("couldn't find any segment sections")? as usize;
    let padding: usize = earliest - cmds_end;
    if padding < cmd.len() {
        bail!("not enough padding to inject a load command ({padding} bytes available, need {})", cmd.len());
    }

    data[cmds_end..cmds_end + cmd.len()].copy_from_slice(&cmd);
    data[16..20].copy_from_slice(&(ncmds + 1).to_le_bytes());
    data[20..24].copy_from_slice(&(sizeofcmds + cmd.len() as u32).to_le_bytes());

    fs::write(macho_path, &data)?;
    println!("injected {dylib_path}");
    Ok(())
}

/// Export name a windows hook dll must expose; DllMain does the real init on load.
pub const PE_HOOK_ENTRY_SYMBOL: &str = "RSPHookInit";

const IMAGE_SCN_CNT_INITIALIZED_DATA: u32 = 0x00000040;
const IMAGE_SCN_MEM_READ: u32 = 0x40000000;
const IMAGE_SCN_MEM_WRITE: u32 = 0x80000000;
const PE32_PLUS_MAGIC: u16 = 0x20b;
const IMAGE_IMPORT_DESCRIPTOR_SIZE: usize = 20;

struct PeLayout {
    num_sections: usize,
    optional_header_offset: usize,
    section_table_offset: usize,
    section_alignment: u32,
    data_directory_offset: usize,
    num_rva_and_sizes: u32,
}

fn parse_pe_layout(data: &[u8]) -> Result<PeLayout> {
    if data.len() < 0x40 {
        bail!("too small to be a pe file");
    }
    let pe_offset: usize = u32::from_le_bytes(data[0x3c..0x40].try_into().unwrap()) as usize;
    if data.len() < pe_offset + 24 || &data[pe_offset..pe_offset + 4] != b"PE\0\0" {
        bail!("no PE signature at the e_lfanew offset, not a pe file");
    }

    let coff_offset: usize = pe_offset + 4;
    let num_sections: usize = u16::from_le_bytes(data[coff_offset + 2..coff_offset + 4].try_into().unwrap()) as usize;
    let size_of_optional_header: usize = u16::from_le_bytes(data[coff_offset + 16..coff_offset + 18].try_into().unwrap()) as usize;

    let optional_header_offset: usize = coff_offset + 20;
    if data.len() < optional_header_offset + size_of_optional_header {
        bail!("optional header runs off the end of the file");
    }
    let magic: u16 = u16::from_le_bytes(data[optional_header_offset..optional_header_offset + 2].try_into().unwrap());
    if magic != PE32_PLUS_MAGIC {
        bail!("only 64-bit (PE32+) executables are supported, got magic 0x{magic:x}");
    }

    let section_alignment: u32 = u32::from_le_bytes(data[optional_header_offset + 32..optional_header_offset + 36].try_into().unwrap());
    let num_rva_and_sizes: u32 = u32::from_le_bytes(data[optional_header_offset + 108..optional_header_offset + 112].try_into().unwrap());
    let data_directory_offset: usize = optional_header_offset + 112;

    let section_table_offset: usize = optional_header_offset + size_of_optional_header;

    Ok(PeLayout {
        num_sections,
        optional_header_offset,
        section_table_offset,
        section_alignment,
        data_directory_offset,
        num_rva_and_sizes,
    })
}

struct Section {
    header_offset: usize,
    virtual_size: u32,
    virtual_address: u32,
    size_of_raw_data: u32,
    pointer_to_raw_data: u32,
}

fn read_sections(data: &[u8], layout: &PeLayout) -> Vec<Section> {
    let mut out: Vec<Section> = vec![];
    for i in 0..layout.num_sections {
        let base: usize = layout.section_table_offset + i * 40;
        out.push(Section {
            header_offset: base,
            virtual_size: u32::from_le_bytes(data[base + 8..base + 12].try_into().unwrap()),
            virtual_address: u32::from_le_bytes(data[base + 12..base + 16].try_into().unwrap()),
            size_of_raw_data: u32::from_le_bytes(data[base + 16..base + 20].try_into().unwrap()),
            pointer_to_raw_data: u32::from_le_bytes(data[base + 20..base + 24].try_into().unwrap()),
        });
    }
    out
}

fn rva_to_offset(sections: &[Section], rva: u32) -> Option<usize> {
    for s in sections {
        if rva >= s.virtual_address && rva < s.virtual_address + s.size_of_raw_data.max(s.virtual_size) {
            return Some((s.pointer_to_raw_data + (rva - s.virtual_address)) as usize);
        }
    }
    None
}

fn align_up(value: u32, align: u32) -> u32 {
    if align == 0 {
        return value;
    }
    value.div_ceil(align) * align
}

fn dll_already_imported(data: &[u8], layout: &PeLayout, sections: &[Section], dll_name: &str) -> Result<bool> {
    if layout.num_rva_and_sizes < 2 {
        return Ok(false);
    }
    let dir: usize = layout.data_directory_offset + 8;
    let import_rva: u32 = u32::from_le_bytes(data[dir..dir + 4].try_into().unwrap());
    if import_rva == 0 {
        return Ok(false);
    }
    let Some(mut offset) = rva_to_offset(sections, import_rva) else {
        return Ok(false);
    };
    loop {
        if offset + IMAGE_IMPORT_DESCRIPTOR_SIZE > data.len() {
            break;
        }
        let entry: &[u8] = &data[offset..offset + IMAGE_IMPORT_DESCRIPTOR_SIZE];
        if entry.iter().all(|&b: &u8| b == 0) {
            break;
        }
        let name_rva: u32 = u32::from_le_bytes(entry[12..16].try_into().unwrap());
        if let Some(name_off) = rva_to_offset(sections, name_rva) {
            let end: usize = data[name_off..].iter().position(|&b: &u8| b == 0).map(|i: usize| name_off + i).unwrap_or(name_off);
            let name: &str = std::str::from_utf8(&data[name_off..end]).unwrap_or("");
            if name.eq_ignore_ascii_case(dll_name) {
                return Ok(true);
            }
        }
        offset += IMAGE_IMPORT_DESCRIPTOR_SIZE;
    }
    Ok(false)
}

fn inject_dll_pe(exe_path: &Path, dll_path: &str) -> Result<()> {
    let mut data: Vec<u8> = fs::read(exe_path)?;
    let layout: PeLayout = parse_pe_layout(&data)?;
    let sections: Vec<Section> = read_sections(&data, &layout);

    let dll_name: &str = Path::new(dll_path)
        .file_name()
        .and_then(|n: &std::ffi::OsStr| n.to_str())
        .context("dll path has no file name")?;

    if dll_already_imported(&data, &layout, &sections, dll_name)? {
        println!("{dll_name} already imported, nothing to do");
        return Ok(());
    }

    if layout.num_rva_and_sizes < 2 {
        bail!("optional header has no import table data directory slot");
    }
    let import_dir_offset: usize = layout.data_directory_offset + 8;
    let orig_import_rva: u32 = u32::from_le_bytes(data[import_dir_offset..import_dir_offset + 4].try_into().unwrap());

    let mut orig_descriptors: Vec<u8> = vec![];
    if orig_import_rva != 0 {
        let mut offset: usize = rva_to_offset(&sections, orig_import_rva).context("import table rva doesn't map to any section")?;
        loop {
            let entry: &[u8] = &data[offset..offset + IMAGE_IMPORT_DESCRIPTOR_SIZE];
            let is_null: bool = entry.iter().all(|&b: &u8| b == 0);
            orig_descriptors.extend_from_slice(entry);
            if is_null {
                break;
            }
            offset += IMAGE_IMPORT_DESCRIPTOR_SIZE;
        }
        orig_descriptors.truncate(orig_descriptors.len() - IMAGE_IMPORT_DESCRIPTOR_SIZE);
    }
    let orig_count: usize = orig_descriptors.len() / IMAGE_IMPORT_DESCRIPTOR_SIZE;

    let by_file_end = |s: &Section| s.pointer_to_raw_data as u64 + s.size_of_raw_data as u64;
    let by_virtual_end = |s: &Section| s.virtual_address as u64 + s.virtual_size.max(s.size_of_raw_data) as u64;
    let ext_idx: usize = (0..sections.len())
        .max_by_key(|&i| by_file_end(&sections[i]))
        .context("pe file has no sections")?;
    if ext_idx != (0..sections.len()).max_by_key(|&i| by_virtual_end(&sections[i])).unwrap() {
        bail!("last section by file offset and by virtual address disagree, refusing to guess");
    }
    let ext = &sections[ext_idx];
    let section_va: u32 = ext.virtual_address;
    let old_virtual_size: u32 = ext.virtual_size;
    let ext_header_offset: usize = ext.header_offset;
    let ext_pointer_to_raw_data: u32 = ext.pointer_to_raw_data;

    let content_va: u32 = section_va + old_virtual_size;

    let descriptors_rva: u32 = content_va;
    let descriptors_len: usize = (orig_count + 2) * IMAGE_IMPORT_DESCRIPTOR_SIZE;

    let ilt_rva: u32 = descriptors_rva + descriptors_len as u32;
    let iat_rva: u32 = ilt_rva + 16;
    let import_by_name_rva: u32 = iat_rva + 16;
    let hint_name_len: usize = 2 + PE_HOOK_ENTRY_SYMBOL.len() + 1;
    let hint_name_padded_len: usize = hint_name_len.div_ceil(2) * 2;
    let dll_name_rva: u32 = import_by_name_rva + hint_name_padded_len as u32;
    let dll_name_bytes_len: usize = dll_name.len() + 1;

    let content_size: usize = (dll_name_rva as usize - content_va as usize) + dll_name_bytes_len;

    let mut section_bytes: Vec<u8> = Vec::with_capacity(content_size);
    section_bytes.extend_from_slice(&orig_descriptors);
    section_bytes.extend_from_slice(&ilt_rva.to_le_bytes());
    section_bytes.extend_from_slice(&0u32.to_le_bytes());
    section_bytes.extend_from_slice(&0u32.to_le_bytes());
    section_bytes.extend_from_slice(&dll_name_rva.to_le_bytes());
    section_bytes.extend_from_slice(&iat_rva.to_le_bytes());
    section_bytes.extend_from_slice(&[0u8; IMAGE_IMPORT_DESCRIPTOR_SIZE]);
    section_bytes.extend_from_slice(&(import_by_name_rva as u64).to_le_bytes());
    section_bytes.extend_from_slice(&0u64.to_le_bytes());
    section_bytes.extend_from_slice(&(import_by_name_rva as u64).to_le_bytes());
    section_bytes.extend_from_slice(&0u64.to_le_bytes());
    section_bytes.extend_from_slice(&0u16.to_le_bytes());
    section_bytes.extend_from_slice(PE_HOOK_ENTRY_SYMBOL.as_bytes());
    section_bytes.resize(section_bytes.len() + (hint_name_padded_len - hint_name_len) + 1, 0);
    section_bytes.extend_from_slice(dll_name.as_bytes());
    section_bytes.push(0);

    debug_assert_eq!(section_bytes.len(), content_size);

    let old_raw_size: u32 = ext.size_of_raw_data;
    let content_start_fileoff: usize = ext_pointer_to_raw_data as usize + old_virtual_size as usize;
    let pad_available: usize = old_raw_size.saturating_sub(old_virtual_size) as usize;

    let new_raw_size: u32;
    if content_size <= pad_available {
        data[content_start_fileoff..content_start_fileoff + content_size].copy_from_slice(&section_bytes);
        new_raw_size = old_raw_size;
    } else {
        let in_place: usize = pad_available;
        let overflow: &[u8] = &section_bytes[in_place..];
        data[content_start_fileoff..content_start_fileoff + in_place].copy_from_slice(&section_bytes[..in_place]);

        let append_at: usize = ext_pointer_to_raw_data as usize + old_raw_size as usize;
        let cert_dir_offset: usize = layout.data_directory_offset + 4 * 8;
        let cert_file_offset: u32 = if layout.num_rva_and_sizes > 4 {
            u32::from_le_bytes(data[cert_dir_offset..cert_dir_offset + 4].try_into().unwrap())
        } else {
            0
        };
        let cert_size: u32 = if layout.num_rva_and_sizes > 4 {
            u32::from_le_bytes(data[cert_dir_offset + 4..cert_dir_offset + 8].try_into().unwrap())
        } else {
            0
        };

        let mut new_data: Vec<u8>;
        if cert_file_offset != 0 && cert_size != 0 && cert_file_offset as usize == append_at {
            let cert_bytes: Vec<u8> = data[append_at..append_at + cert_size as usize].to_vec();
            new_data = data[..append_at].to_vec();
            new_data.extend_from_slice(overflow);
            while new_data.len() % 8 != 0 {
                new_data.push(0);
            }
            let new_cert_offset: u32 = new_data.len() as u32;
            new_data.extend_from_slice(&cert_bytes);
            new_data[cert_dir_offset..cert_dir_offset + 4].copy_from_slice(&new_cert_offset.to_le_bytes());
        } else {
            new_data = data[..append_at].to_vec();
            new_data.extend_from_slice(overflow);
        }
        data = new_data;

        new_raw_size = old_raw_size + overflow.len() as u32;
    }

    let new_virtual_size: u32 = old_virtual_size + content_size as u32;
    data[ext_header_offset + 8..ext_header_offset + 12].copy_from_slice(&new_virtual_size.to_le_bytes());
    data[ext_header_offset + 16..ext_header_offset + 20].copy_from_slice(&new_raw_size.to_le_bytes());
    let characteristics_offset: usize = ext_header_offset + 36;
    let characteristics: u32 = u32::from_le_bytes(data[characteristics_offset..characteristics_offset + 4].try_into().unwrap());
    data[characteristics_offset..characteristics_offset + 4]
        .copy_from_slice(&(characteristics | IMAGE_SCN_CNT_INITIALIZED_DATA | IMAGE_SCN_MEM_READ | IMAGE_SCN_MEM_WRITE).to_le_bytes());

    let new_size_of_image: u32 = align_up(section_va + new_virtual_size, layout.section_alignment);
    let size_of_image_offset: usize = layout.optional_header_offset + 56;
    data[size_of_image_offset..size_of_image_offset + 4].copy_from_slice(&new_size_of_image.to_le_bytes());

    data[import_dir_offset..import_dir_offset + 4].copy_from_slice(&descriptors_rva.to_le_bytes());
    data[import_dir_offset + 4..import_dir_offset + 8].copy_from_slice(&(descriptors_len as u32).to_le_bytes());

    fs::write(exe_path, &data)?;
    println!("injected {dll_name} (exporting {PE_HOOK_ENTRY_SYMBOL})");
    Ok(())
}

/// Injects a dylib (mach-o `LC_LOAD_DYLIB`) or dll (pe import table) into `target_path`.
pub fn inject_dylib(target_path: &Path, dylib_path: &str) -> Result<()> {
    let data: Vec<u8> = fs::read(target_path)?;
    if data.len() >= 4 && data[0..4] == [0xcf, 0xfa, 0xed, 0xfe] {
        return inject_dylib_macho(target_path, dylib_path);
    }
    if data.len() >= 2 && data[0..2] == *b"MZ" {
        return inject_dll_pe(target_path, dylib_path);
    }
    bail!("not an arm64 mach-o or a pe executable, can't inject");
}

/// ```ignore
/// let hooked = is_injected(target, "/Users/Shared/rbx-theme-set/studio_hook.dylib")?;
/// ```
pub fn is_injected(target: &Path, library: &str) -> Result<bool> {
    let data: Vec<u8> = fs::read(target)?;
    if data.len() >= 4 && data[0..4] == [0xcf, 0xfa, 0xed, 0xfe] {
        let sizeofcmds: u32 = u32::from_le_bytes(data[20..24].try_into().unwrap());
        return Ok(dylib_paths(&data, sizeofcmds).iter().any(|p: &String| p == library));
    }
    if data.len() >= 2 && data[0..2] == *b"MZ" {
        let layout: PeLayout = parse_pe_layout(&data)?;
        let sections: Vec<Section> = read_sections(&data, &layout);
        let name: &str = Path::new(library)
            .file_name()
            .and_then(|n: &std::ffi::OsStr| n.to_str())
            .unwrap_or(library);
        return dll_already_imported(&data, &layout, &sections, name);
    }
    bail!("not an arm64 mach-o or a pe executable")
}
