use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

use crate::Args;

/// One byte of a match pattern: a fixed value or a wildcard.
pub enum PatByte {
    Exact(u8),
    Wild,
}

/// Parses a space-separated hex byte pattern, "??" as wildcard if allowed.
pub fn parse_pattern(s: &str, allow_wild: bool) -> Result<Vec<PatByte>> {
    let mut out: Vec<PatByte> = vec![];
    for tok in s.split_whitespace() {
        if tok == "??" || tok == "?" {
            if !allow_wild {
                bail!("no wildcards in --patch, every byte needs a value");
            }
            out.push(PatByte::Wild);
        } else {
            out.push(PatByte::Exact(
                u8::from_str_radix(tok, 16).with_context(|| tok.to_string())?,
            ));
        }
    }
    Ok(out)
}

/// Finds every offset in `haystack` where `pattern` matches.
pub fn find_matches(haystack: &[u8], pattern: &[PatByte]) -> Vec<usize> {
    if pattern.is_empty() || haystack.len() < pattern.len() {
        return vec![];
    }
    let mut hits: Vec<usize> = vec![];
    for start in 0..=(haystack.len() - pattern.len()) {
        let ok: bool = pattern.iter().enumerate().all(|(i, p)| match p {
            PatByte::Wild => true,
            PatByte::Exact(b) => haystack[start + i] == *b,
        });
        if ok {
            hits.push(start);
        }
    }
    hits
}

/// Lists Roblox Studio installs under %LOCALAPPDATA%\Roblox\Versions, newest first.
#[cfg(target_os = "windows")]
pub fn discover_candidates() -> Result<Vec<PathBuf>> {
    let local = std::env::var_os("LOCALAPPDATA").context("no LOCALAPPDATA env var")?;
    let versions = PathBuf::from(local).join("Roblox").join("Versions");
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in fs::read_dir(&versions).context("no roblox install found, pass --binary")? {
        let exe = entry?.path().join("RobloxStudioBeta.exe");
        if !exe.exists() {
            continue;
        }
        let mtime = fs::metadata(&exe)?.modified()?;
        found.push((mtime, exe));
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    if found.is_empty() {
        bail!("no RobloxStudioBeta.exe under Roblox/Versions, pass --binary");
    }
    Ok(found.into_iter().map(|(_, path)| path).collect())
}

/// Lists Roblox Studio.app installs in the usual mac locations and via Spotlight.
#[cfg(not(target_os = "windows"))]
pub fn discover_candidates() -> Result<Vec<PathBuf>> {
    let mut candidates: Vec<PathBuf> = vec![
        PathBuf::from("/Applications/RobloxStudio.app"),
        PathBuf::from("/Applications/Roblox Studio.app"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join("Applications/RobloxStudio.app"));
    }
    let out: std::process::Output = Command::new("mdfind")
        .arg("kMDItemCFBundleIdentifier == 'com.roblox.RobloxStudioBrowser'")
        .output()?;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if !line.is_empty() {
            candidates.push(PathBuf::from(line));
        }
    }

    let mut found: Vec<PathBuf> = Vec::new();
    for candidate in candidates {
        if candidate.exists() && !found.contains(&candidate) {
            found.push(candidate);
        }
    }
    if found.is_empty() {
        bail!("couldn't find RobloxStudio.app, pass --binary");
    }
    Ok(found)
}

/// Resolves an .app bundle to its actual executable via CFBundleExecutable.
pub fn resolve_macho(path: &Path) -> Result<PathBuf> {
    if path.extension().and_then(|e| e.to_str()) != Some("app") {
        return Ok(path.to_path_buf());
    }
    let plist = path.join("Contents/Info.plist");
    let out: std::process::Output = Command::new("defaults")
        .args(["read", &plist.to_string_lossy(), "CFBundleExecutable"])
        .output()?;
    let name: String = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() {
        bail!("no CFBundleExecutable in {}", plist.display());
    }
    Ok(path.join("Contents/MacOS").join(name))
}

pub fn app_root(macho_path: &Path) -> Option<PathBuf> {
    macho_path
        .ancestors()
        .find(|p: &&Path| p.extension().and_then(|e: &std::ffi::OsStr| e.to_str()) == Some("app"))
        .map(Into::into)
}

pub fn backup(macho_path: &Path) -> Result<()> {
    let ts: u64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let bak: PathBuf = macho_path.with_extension(format!("bak-{ts}"));
    fs::copy(macho_path, &bak)?;
    println!("    {}", crate::term::dim(&format!("backup: {}", bak.display())));
    Ok(())
}

pub fn resign(macho_path: &Path) -> Result<()> {
    let target: PathBuf = app_root(macho_path).unwrap_or_else(|| macho_path.to_path_buf());
    println!("codesigning {} (adhoc)", target.display());
    let ok: bool = Command::new("codesign")
        .args(["--force", "--deep", "--sign", "-"])
        .arg(&target)
        .status()?
        .success();
    if !ok {
        bail!("codesign failed - binary is patched but won't launch till you resign it");
    }
    Ok(())
}

#[cfg(target_os = "windows")]
const STUDIO_PROCESS_NAMES: &[&str] = &["RobloxStudioBeta.exe", "RobloxCrashHandler.exe", "StudioMCP.exe"];
#[cfg(not(target_os = "windows"))]
const STUDIO_PROCESS_NAMES: &[&str] = &["RobloxStudio", "RobloxCrashHandler", "StudioMCP"];

struct StudioProcess {
    pid: u32,
    path: PathBuf,
}

#[cfg(not(target_os = "windows"))]
fn studio_root(hint_path: &Path) -> Option<PathBuf> {
    app_root(hint_path)
}

#[cfg(target_os = "windows")]
fn studio_root(hint_path: &Path) -> Option<PathBuf> {
    hint_path
        .ancestors()
        .find(|p: &&Path| {
            p.file_name()
                .and_then(|n: &std::ffi::OsStr| n.to_str())
                .is_some_and(|n: &str| n.eq_ignore_ascii_case("Versions"))
        })
        .map(Into::into)
        .or_else(|| hint_path.parent().map(Into::into))
}

#[cfg(not(target_os = "windows"))]
fn find_studio_processes(root: Option<&Path>) -> Result<Vec<StudioProcess>> {
    let out: std::process::Output = Command::new("ps").args(["-Ao", "pid=,comm="]).output()?;
    let text: std::borrow::Cow<'_, str> = String::from_utf8_lossy(&out.stdout);
    let mut found: Vec<StudioProcess> = vec![];
    for line in text.lines() {
        let trimmed: &str = line.trim();
        let Some(sp) = trimmed.find(char::is_whitespace) else {
            continue;
        };
        let (pid_str, rest): (&str, &str) = trimmed.split_at(sp);
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        let path: PathBuf = PathBuf::from(rest.trim());
        let name: &str = path.file_name().and_then(|n: &std::ffi::OsStr| n.to_str()).unwrap_or("");
        let matched: bool = match root {
            Some(r) => path.starts_with(r),
            None => STUDIO_PROCESS_NAMES.contains(&name),
        };
        if matched {
            found.push(StudioProcess { pid, path });
        }
    }
    Ok(found)
}

#[cfg(not(target_os = "windows"))]
fn kill_pid(pid: u32) {
    let ok: bool = Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .map(|s: std::process::ExitStatus| s.success())
        .unwrap_or(false);
    if !ok {
        println!("warning: couldn't kill pid {pid} (may have already exited)");
    }
}

#[cfg(target_os = "windows")]
fn find_studio_processes(_root: Option<&Path>) -> Result<Vec<StudioProcess>> {
    let out: std::process::Output = Command::new("tasklist").args(["/NH", "/FO", "CSV"]).output()?;
    let text: std::borrow::Cow<'_, str> = String::from_utf8_lossy(&out.stdout);
    let mut found: Vec<StudioProcess> = vec![];
    for line in text.lines() {
        let fields: Vec<&str> = line.trim().split("\",\"").collect();
        let (Some(name_field), Some(pid_field)) = (fields.first(), fields.get(1)) else {
            continue;
        };
        let name: &str = name_field.trim().trim_matches('"');
        let Ok(pid) = pid_field.trim().trim_matches('"').parse::<u32>() else {
            continue;
        };
        if STUDIO_PROCESS_NAMES.iter().any(|n: &&str| n.eq_ignore_ascii_case(name)) {
            found.push(StudioProcess { pid, path: PathBuf::from(name) });
        }
    }
    Ok(found)
}

#[cfg(target_os = "windows")]
fn kill_pid(pid: u32) {
    let ok: bool = Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .status()
        .map(|s: std::process::ExitStatus| s.success())
        .unwrap_or(false);
    if !ok {
        println!("warning: couldn't kill pid {pid} (may have already exited)");
    }
}

pub fn kill_running_studio(hint_path: &Path, args: &Args) -> Result<()> {
    if args.dry_run || args.no_kill_studio {
        return Ok(());
    }
    let root: Option<PathBuf> = studio_root(hint_path);
    let running: Vec<StudioProcess> = find_studio_processes(root.as_deref())?;
    if running.is_empty() {
        return Ok(());
    }
    println!("roblox studio is running and needs to close before patching:");
    for p in &running {
        println!("  [{}] {}", p.pid, p.path.display());
    }
    if !crate::ask_yn("kill these processes now? (unsaved work in studio will be lost)") {
        bail!("studio is running - close it yourself, or pass --no-kill-studio, then try again");
    }
    for p in &running {
        kill_pid(p.pid);
    }
    println!("killed {} studio process(es)", running.len());
    Ok(())
}

pub fn is_pe(data: &[u8]) -> bool {
    data.len() > 0x40 && data[0..2] == *b"MZ"
}

struct PeSection {
    name: String,
    vaddr: u64,
    vsize: u64,
    fileoff: u64,
    filesize: u64,
}

fn pe_sections(data: &[u8]) -> Result<(u64, Vec<PeSection>)> {
    if !is_pe(data) {
        bail!("not a PE file (no MZ magic)");
    }
    let e_lfanew: usize = u32::from_le_bytes(data[0x3c..0x40].try_into().unwrap()) as usize;
    if data[e_lfanew..e_lfanew + 4] != *b"PE\0\0" {
        bail!("bad PE signature");
    }
    let coff: usize = e_lfanew + 4;
    let nsections: usize = u16::from_le_bytes(data[coff + 2..coff + 4].try_into().unwrap()) as usize;
    let opt_hdr_size: usize = u16::from_le_bytes(data[coff + 16..coff + 18].try_into().unwrap()) as usize;
    let opt_off: usize = coff + 20;
    let magic: u16 = u16::from_le_bytes(data[opt_off..opt_off + 2].try_into().unwrap());
    if magic != 0x20b {
        bail!("only PE32+ (x64) is supported, got magic 0x{magic:x}");
    }
    let image_base: u64 = u64::from_le_bytes(data[opt_off + 24..opt_off + 32].try_into().unwrap());

    let sect_table: usize = opt_off + opt_hdr_size;
    let mut sections: Vec<PeSection> = vec![];
    for i in 0..nsections {
        let off: usize = sect_table + i * 40;
        let name: String = String::from_utf8_lossy(&data[off..off + 8]).trim_end_matches('\0').to_string();
        let vsize: u64 = u32::from_le_bytes(data[off + 8..off + 12].try_into().unwrap()) as u64;
        let vaddr: u64 = u32::from_le_bytes(data[off + 12..off + 16].try_into().unwrap()) as u64;
        let filesize: u64 = u32::from_le_bytes(data[off + 16..off + 20].try_into().unwrap()) as u64;
        let fileoff: u64 = u32::from_le_bytes(data[off + 20..off + 24].try_into().unwrap()) as u64;
        sections.push(PeSection { name, vaddr, vsize, fileoff, filesize });
    }
    Ok((image_base, sections))
}

fn pe_fileoff_to_va(sections: &[PeSection], image_base: u64, fileoff: u64) -> Option<u64> {
    sections
        .iter()
        .find(|s: &&PeSection| fileoff >= s.fileoff && fileoff < s.fileoff + s.filesize)
        .map(|s: &PeSection| image_base + s.vaddr + (fileoff - s.fileoff))
}

fn pe_va_to_fileoff(sections: &[PeSection], image_base: u64, va: u64) -> Option<u64> {
    let rva: u64 = va.checked_sub(image_base)?;
    sections
        .iter()
        .find(|s: &&PeSection| rva >= s.vaddr && rva < s.vaddr + s.vsize)
        .map(|s: &PeSection| s.fileoff + (rva - s.vaddr))
}

fn is_cmp_byte_rip(bytes: &[u8]) -> Option<i32> {
    if bytes.len() < 7 || bytes[0] != 0x80 || bytes[1] != 0x3D {
        return None;
    }
    Some(i32::from_le_bytes(bytes[2..6].try_into().unwrap()))
}

fn is_lea_rip(bytes: &[u8]) -> Option<(u8, i32)> {
    if bytes.len() < 7 {
        return None;
    }
    let rex: u8 = bytes[0];
    if rex & 0xFB != 0x48 {
        return None;
    }
    if bytes[1] != 0x8D {
        return None;
    }
    let modrm: u8 = bytes[2];
    if modrm & 0xC7 != 0x05 {
        return None;
    }
    let reg: u8 = ((modrm >> 3) & 0x7) | if rex & 0x4 != 0 { 0x8 } else { 0 };
    let disp: i32 = i32::from_le_bytes(bytes[3..7].try_into().unwrap());
    Some((reg, disp))
}

fn jmp_rel32_target(bytes: &[u8], pc: u64) -> Option<u64> {
    if bytes.len() < 5 || bytes[0] != 0xE9 {
        return None;
    }
    let disp: i32 = i32::from_le_bytes(bytes[1..5].try_into().unwrap());
    Some((pc as i64 + 5 + disp as i64) as u64)
}

fn discover_via_anchor_pe(data: &[u8], anchor: &str) -> Result<Vec<u64>> {
    let (image_base, sections) = pe_sections(data)?;
    let text: &PeSection = sections
        .iter()
        .find(|s: &&PeSection| s.name == ".text")
        .context("no .text section")?;
    let (text_start, text_end): (usize, usize) = (text.fileoff as usize, (text.fileoff + text.filesize) as usize);

    let needle: Vec<u8> = anchor.bytes().chain(std::iter::once(0)).collect();
    let pattern: Vec<PatByte> = needle.iter().map(|b: &u8| PatByte::Exact(*b)).collect();
    let str_offsets: Vec<usize> = find_matches(data, &pattern);
    if str_offsets.is_empty() {
        bail!("anchor string {anchor:?} not found in binary");
    }

    for &str_off in &str_offsets {
        let str_va: u64 = match pe_fileoff_to_va(&sections, image_base, str_off as u64) {
            Some(v) => v,
            None => continue,
        };

        let mut ref_sites: Vec<usize> = vec![];
        let mut i: usize = text_start;
        while i + 7 <= text_end {
            if let Some((_reg, disp)) = is_lea_rip(&data[i..(i + 7).min(data.len())]) {
                let next_va: u64 = pe_fileoff_to_va(&sections, image_base, (i + 7) as u64).unwrap_or(0);
                if (next_va as i64 + disp as i64) as u64 == str_va {
                    ref_sites.push(i);
                }
            }
            i += 1;
        }

        for &site in &ref_sites {
            let win_start: usize = site.saturating_sub(400);
            let mut j: usize = win_start;
            while j + 7 <= site {
                if let Some((_reg, disp)) = is_lea_rip(&data[j..j + 7]) {
                    let next_va: u64 = pe_fileoff_to_va(&sections, image_base, (j + 7) as u64).unwrap_or(0);
                    let candidate: u64 = (next_va as i64 + disp as i64) as u64;
                    if let Some(mut cand_off) = pe_va_to_fileoff(&sections, image_base, candidate) {
                        if (candidate >= image_base + text.vaddr) && (candidate < image_base + text.vaddr + text.vsize) {
                            for _hop in 0..4 {
                                let cur: usize = cand_off as usize;
                                if cur + 5 > data.len() {
                                    break;
                                }
                                if let Some(t) = jmp_rel32_target(&data[cur..cur + 5], pe_fileoff_to_va(&sections, image_base, cur as u64).unwrap_or(0)) {
                                    if let Some(o) = pe_va_to_fileoff(&sections, image_base, t) {
                                        cand_off = o;
                                        continue;
                                    }
                                }
                                break;
                            }
                            let cand_va: u64 = pe_fileoff_to_va(&sections, image_base, cand_off).unwrap_or(0);
                            let bound: usize = pe_function_end_va(data, &sections, image_base, cand_va)
                                .and_then(|end_va: u64| pe_va_to_fileoff(&sections, image_base, end_va))
                                .map(|o: u64| o as usize)
                                .unwrap_or((cand_off as usize + 64).min(text_end));
                            let addrs: Vec<u64> = cmp_byte_globals(data, &sections, image_base, cand_off as usize, bound);
                            if !addrs.is_empty() {
                                return Ok(addrs);
                            }
                        }
                    }
                }
                j += 1;
            }
        }
    }
    bail!("found the anchor string but couldn't trace a getter function near it")
}

fn pe_function_end_va(data: &[u8], sections: &[PeSection], image_base: u64, va: u64) -> Option<u64> {
    let pdata: &PeSection = sections.iter().find(|s: &&PeSection| s.name == ".pdata")?;
    let rva: u64 = va.checked_sub(image_base)?;
    let start: usize = pdata.fileoff as usize;
    let end: usize = (pdata.fileoff + pdata.filesize) as usize;
    let mut i: usize = start;
    while i + 12 <= end {
        let begin: u64 = u32::from_le_bytes(data[i..i + 4].try_into().unwrap()) as u64;
        let fend: u64 = u32::from_le_bytes(data[i + 4..i + 8].try_into().unwrap()) as u64;
        if rva >= begin && rva < fend {
            return Some(image_base + fend);
        }
        i += 12;
    }
    None
}

fn cmp_byte_globals(data: &[u8], sections: &[PeSection], image_base: u64, fstart: usize, fend: usize) -> Vec<u64> {
    let mut addrs: Vec<u64> = vec![];
    let mut i: usize = fstart;
    while i + 7 <= fend {
        if data[i] == 0xCC {
            break;
        }
        if let Some(disp) = is_cmp_byte_rip(&data[i..i + 7]) {
            if data[i + 6] == 0x00 {
                let next_va: u64 = pe_fileoff_to_va(sections, image_base, (i + 7) as u64).unwrap_or(0);
                addrs.push((next_va as i64 + disp as i64) as u64);
            }
        }
        i += 1;
    }
    addrs
}

fn scan_globals_pe(data: &[u8], globals: &[u64]) -> Result<Vec<usize>> {
    let (image_base, sections) = pe_sections(data)?;
    let text: &PeSection = sections.iter().find(|s: &&PeSection| s.name == ".text").context("no .text section")?;
    let (start, end): (usize, usize) = (text.fileoff as usize, (text.fileoff + text.filesize) as usize);

    let mut out: Vec<usize> = vec![];
    let mut i: usize = start;
    while i + 7 <= end {
        if let Some(disp) = is_cmp_byte_rip(&data[i..i + 7]) {
            if data[i + 6] == 0x00 {
                let next_va: u64 = pe_fileoff_to_va(&sections, image_base, (i + 7) as u64).unwrap_or(0);
                let target: u64 = (next_va as i64 + disp as i64) as u64;
                if globals.contains(&target) {
                    out.push(i + 6);
                }
            }
        }
        i += 1;
    }
    Ok(out)
}

fn text_bounds(data: &[u8]) -> Result<(u64, u64, u64)> {
    if data.len() < 32 || data[0..4] != [0xcf, 0xfa, 0xed, 0xfe] {
        bail!("bad macho magic, not arm64/x64 little endian?");
    }
    let ncmds: u32 = u32::from_le_bytes(data[16..20].try_into().unwrap());
    let mut off: usize = 32usize;
    for _ in 0..ncmds {
        let cmd: u32 = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
        let sz: usize = u32::from_le_bytes(data[off + 4..off + 8].try_into().unwrap()) as usize;
        if cmd == 0x19 && &data[off + 8..off + 14] == b"__TEXT" {
            let vmaddr: u64 = u64::from_le_bytes(data[off + 24..off + 32].try_into().unwrap());
            let vmsize: u64 = u64::from_le_bytes(data[off + 32..off + 40].try_into().unwrap());
            let fileoff: u64 = u64::from_le_bytes(data[off + 40..off + 48].try_into().unwrap());
            return Ok((vmaddr, fileoff, vmsize));
        }
        off += sz;
    }
    bail!("no __TEXT segment??")
}

fn adrp(word: u32, pc: u64) -> Option<(u8, u64)> {
    if word & 0x9F000000 != 0x90000000 {
        return None;
    }
    let rd: u8 = (word & 0x1F) as u8;
    let lo: i64 = ((word >> 29) & 0x3) as i64;
    let hi: i64 = ((word >> 5) & 0x7FFFF) as i64;
    let mut imm: i64 = (hi << 2) | lo;
    if imm & (1 << 20) != 0 {
        imm -= 1 << 21;
    }
    Some((rd, ((pc as i64 & !0xFFF) + (imm << 12)) as u64))
}

fn add_imm(word: u32) -> Option<(u8, u8, u32)> {
    (word & 0x7FC00000 == 0x11000000).then(|| {
        (
            (word & 0x1F) as u8,
            ((word >> 5) & 0x1F) as u8,
            (word >> 10) & 0xFFF,
        )
    })
}

fn text_section_bounds(data: &[u8]) -> Result<(u64, u64)> {
    let ncmds: u32 = u32::from_le_bytes(data[16..20].try_into().unwrap());
    let mut off: usize = 32usize;
    for _ in 0..ncmds {
        let cmd: u32 = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
        let sz: usize = u32::from_le_bytes(data[off + 4..off + 8].try_into().unwrap()) as usize;
        if cmd == 0x19 && &data[off + 8..off + 14] == b"__TEXT" {
            let nsects: u32 = u32::from_le_bytes(data[off + 48..off + 52].try_into().unwrap());
            let mut sect_off: usize = off + 72;
            for _ in 0..nsects {
                if &data[sect_off..sect_off + 6] == b"__text" {
                    let addr: u64 =
                        u64::from_le_bytes(data[sect_off + 32..sect_off + 40].try_into().unwrap());
                    let size: u64 =
                        u64::from_le_bytes(data[sect_off + 40..sect_off + 48].try_into().unwrap());
                    return Ok((addr, addr + size));
                }
                sect_off += 80;
            }
        }
        off += sz;
    }
    bail!("no __text section??")
}

fn b_target(word: u32, pc: u64) -> Option<u64> {
    if word & 0xFC000000 != 0x14000000 {
        return None;
    }
    let mut imm: i64 = (word & 0x03FFFFFF) as i64;
    if imm & (1 << 25) != 0 {
        imm -= 1 << 26;
    }
    Some((pc as i64 + imm * 4) as u64)
}

fn and_ldrb_globals(data: &[u8], slide: i64, fstart: usize, fend: usize) -> Vec<u64> {
    let mut has_and: bool = false;
    let mut addrs: Vec<u64> = vec![];
    let mut i: usize = fstart;
    while i + 8 <= fend {
        let w1: u32 = u32::from_le_bytes(data[i..i + 4].try_into().unwrap());
        if is_and(w1) {
            has_and = true;
        }
        if let Some((rd, page)) = adrp(w1, (i as i64 + slide) as u64) {
            let w2: u32 = u32::from_le_bytes(data[i + 4..i + 8].try_into().unwrap());
            if let Some((_rt, rn, imm12)) = ldrb(w2) {
                if rn == rd {
                    addrs.push(page + imm12 as u64);
                }
            }
        }
        i += 4;
    }
    if has_and {
        addrs
    } else {
        vec![]
    }
}


fn macho_is_x86_64(data: &[u8]) -> bool {
    data.len() >= 8
        && data[0..4] == [0xcf, 0xfa, 0xed, 0xfe]
        && u32::from_le_bytes(data[4..8].try_into().unwrap()) == 0x0100_0007
}

fn run_globals_macho_x86(macho_path: &Path, data: &mut [u8], args: &Args) -> Result<()> {
    let (vmaddr, fileoff, vmsize) = text_bounds(data)?;
    let slide: i64 = vmaddr as i64 - fileoff as i64;
    let start: usize = fileoff as usize;
    let end: usize = ((fileoff + vmsize) as usize).min(data.len());

    let getter: usize = find_internal_permission_getter(data, start, end)
        .context("couldn't find hasInternalPermission getter - roblox may have changed it")?;
    let flag_a: u64 = ((getter + 10) as i64
        + slide
        + i32::from_le_bytes(data[getter + 6..getter + 10].try_into().unwrap()) as i64)
        as u64;
    let flag_b: u64 = ((getter + 16) as i64
        + slide
        + i32::from_le_bytes(data[getter + 12..getter + 16].try_into().unwrap()) as i64)
        as u64;
    let delta: i32 = (flag_a as i64 - flag_b as i64) as i32;

    let sites: Vec<usize> = flag_read_sites(data, start, end, slide, flag_b);
    if sites.is_empty() {
        bail!("found the getter but no flag reads to redirect - roblox may have changed this");
    }

    let getter_addr: u64 = getter as u64 + (vmaddr - fileoff);
    println!("hasInternalPermission at 0x{getter_addr:x} (flagA=0x{flag_a:x} flagB=0x{flag_b:x})");
    println!("{} flag read(s) redirected to the always-on flag", sites.len());

    if args.dry_run {
        println!("dry run");
        return Ok(());
    }
    kill_running_studio(macho_path, args)?;
    if !args.no_backup {
        backup(macho_path)?;
    }
    for &disp_off in &sites {
        let disp: i32 = i32::from_le_bytes(data[disp_off..disp_off + 4].try_into().unwrap());
        data[disp_off..disp_off + 4].copy_from_slice(&disp.wrapping_add(delta).to_le_bytes());
    }
    fs::write(macho_path, &data[..])?;
    println!("patched internal permission ({} site(s))", sites.len());
    if !args.no_resign {
        resign(macho_path)?;
    }
    Ok(())
}

fn flag_read_sites(data: &[u8], start: usize, end: usize, slide: i64, flag: u64) -> Vec<usize> {
    let mut sites: Vec<usize> = vec![];
    let mut i: usize = start;
    while i + 8 <= end {
        let (prefix, has_imm, ok_reg): (usize, usize, bool) = match data[i] {
            0x0f if matches!(data.get(i + 1), Some(0xb6 | 0xb7)) => (2, 0, true),
            0x8a | 0x8b | 0x3a | 0x38 | 0x84 | 0x22 | 0x0a => (1, 0, true),
            0x80 => (1, 1, (data[i + 1] >> 3) & 7 == 7),
            0xf6 => (1, 1, matches!((data[i + 1] >> 3) & 7, 0 | 1)),
            _ => {
                i += 1;
                continue;
            }
        };
        let modrm: u8 = data[i + prefix];
        if ok_reg && modrm & 0xc7 == 0x05 {
            let disp_off: usize = i + prefix + 1;
            let disp: i32 = i32::from_le_bytes(data[disp_off..disp_off + 4].try_into().unwrap());
            let next: usize = disp_off + 4 + has_imm;
            if (next as i64 + slide + disp as i64) as u64 == flag {
                sites.push(disp_off);
                i = next;
                continue;
            }
        }
        i += 1;
    }
    sites
}

fn find_internal_permission_getter(data: &[u8], start: usize, end: usize) -> Option<usize> {
    let (vmaddr, fileoff, _) = text_bounds(data).ok()?;
    let slide: i64 = vmaddr as i64 - fileoff as i64;
    let granted: std::collections::HashSet<u64> = grant_set_flags(data, start, end, slide);

    let mut i: usize = start;
    while i + 16 <= end {
        if data[i] == 0x55
            && data[i + 1] == 0x48
            && data[i + 2] == 0x89
            && data[i + 3] == 0xe5
            && data[i + 4] == 0x8a
            && data[i + 5] == 0x05
            && data[i + 10] == 0x22
            && data[i + 11] == 0x05
        {
            let flag_b_disp: i32 =
                i32::from_le_bytes(data[i + 12..i + 16].try_into().unwrap());
            let flag_b: u64 = ((i + 16) as i64 + slide + flag_b_disp as i64) as u64;
            if granted.contains(&flag_b) {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn grant_set_flags(
    data: &[u8],
    start: usize,
    end: usize,
    slide: i64,
) -> std::collections::HashSet<u64> {
    let mut flags: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut i: usize = start;
    while i + 7 <= end {
        if data[i] == 0xc6 && data[i + 1] == 0x05 && data[i + 6] == 0x01 {
            let disp: i32 = i32::from_le_bytes(data[i + 2..i + 6].try_into().unwrap());
            flags.insert(((i + 7) as i64 + slide + disp as i64) as u64);
        }
        i += 1;
    }
    flags
}

fn discover_via_anchor(data: &[u8], anchor: &str) -> Result<Vec<u64>> {
    let (vmaddr, fileoff, vmsize) = text_bounds(data)?;
    let slide: i64 = vmaddr as i64 - fileoff as i64;
    let (text_lo, text_hi): (u64, u64) = text_section_bounds(data)?;

    let needle: Vec<u8> = anchor.bytes().collect();
    let pattern: Vec<PatByte> = needle.iter().map(|b: &u8| PatByte::Exact(*b)).collect();
    let str_offsets: Vec<usize> = find_matches(data, &pattern);
    if str_offsets.is_empty() {
        bail!("anchor string {anchor:?} not found in binary");
    }

    let (text_lo_off, text_hi_off): (usize, usize) = (
        ((text_lo as i64) - slide) as usize,
        ((text_hi as i64) - slide) as usize,
    );
    let mut starts: Vec<usize> = function_starts(data)?
        .into_iter()
        .map(|a: u64| ((a as i64) - slide) as usize)
        .filter(|&o: &usize| o >= text_lo_off && o < text_hi_off)
        .collect();
    starts.sort_unstable();
    starts.dedup();

    let (scan_start, scan_end) = (
        fileoff as usize,
        ((fileoff + vmsize) as usize).min(data.len()),
    );

    for &str_off in &str_offsets {
        let str_addr: u64 = (str_off as i64 + slide) as u64;

        let mut ref_sites: Vec<usize> = vec![];
        let mut i: usize = scan_start;
        while i + 8 <= scan_end {
            let w1: u32 = u32::from_le_bytes(data[i..i + 4].try_into().unwrap());
            if let Some((rd, page)) = adrp(w1, (i as i64 + slide) as u64) {
                let w2: u32 = u32::from_le_bytes(data[i + 4..i + 8].try_into().unwrap());
                if let Some((_rd2, rn, imm12)) = add_imm(w2) {
                    if rn == rd && page + imm12 as u64 == str_addr {
                        ref_sites.push(i);
                    }
                }
            }
            i += 4;
        }

        for &site in &ref_sites {
            let win_start: usize = site.saturating_sub(160);
            let win_end: usize = (site + 160).min(scan_end);
            let mut j: usize = win_start;
            while j + 8 <= win_end {
                let w1: u32 = u32::from_le_bytes(data[j..j + 4].try_into().unwrap());
                if let Some((rd, page)) = adrp(w1, (j as i64 + slide) as u64) {
                    let w2: u32 = u32::from_le_bytes(data[j + 4..j + 8].try_into().unwrap());
                    if let Some((_rd2, rn, imm12)) = add_imm(w2) {
                        if rn == rd {
                            let candidate: u64 = page + imm12 as u64;
                            if candidate >= text_lo && candidate < text_hi {
                                let mut cand_off: usize = ((candidate as i64) - slide) as usize;
                                for _hop in 0..4 {
                                    let Some(&fend) = starts.iter().find(|&&s| s > cand_off) else {
                                        break;
                                    };
                                    let bound: usize = fend.min(cand_off + 256);
                                    if bound - cand_off <= 8 {
                                        let w: u32 = u32::from_le_bytes(
                                            data[cand_off..cand_off + 4].try_into().unwrap(),
                                        );
                                        if let Some(t) =
                                            b_target(w, (cand_off as i64 + slide) as u64)
                                        {
                                            if t >= text_lo && t < text_hi {
                                                cand_off = ((t as i64) - slide) as usize;
                                                continue;
                                            }
                                        }
                                        break;
                                    }
                                    let addrs: Vec<u64> =
                                        and_ldrb_globals(data, slide, cand_off, bound);
                                    if !addrs.is_empty() {
                                        return Ok(addrs);
                                    }
                                    break;
                                }
                            }
                        }
                    }
                }
                j += 4;
            }
        }
    }
    bail!("found the anchor string but couldn't trace a getter function near it - roblox may have changed this pattern")
}

fn ldrb(word: u32) -> Option<(u8, u8, u32)> {
    (word & 0xFFC00000 == 0x39400000).then(|| {
        (
            (word & 0x1F) as u8,
            ((word >> 5) & 0x1F) as u8,
            (word >> 10) & 0xFFF,
        )
    })
}

fn mov_imm1(rd: u8) -> [u8; 4] {
    (0x52800020u32 | rd as u32).to_le_bytes()
}

fn is_and(word: u32) -> bool {
    let and_reg: bool = word & 0x7F200000 == 0x0A000000;
    let and_imm32: bool = word & 0xFF800000 == 0x12000000;
    let and_imm64: bool = word & 0xFF800000 == 0x92000000;
    and_reg || and_imm32 || and_imm64
}

fn uleb128(bytes: &[u8], i: &mut usize) -> u64 {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    loop {
        let byte: u8 = bytes[*i];
        *i += 1;
        result |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    result
}

fn function_starts(data: &[u8]) -> Result<Vec<u64>> {
    let (vmaddr, ..) = text_bounds(data)?;
    let ncmds: u32 = u32::from_le_bytes(data[16..20].try_into().unwrap());
    let mut off: usize = 32usize;
    for _ in 0..ncmds {
        let cmd: u32 = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
        let sz: usize = u32::from_le_bytes(data[off + 4..off + 8].try_into().unwrap()) as usize;
        if cmd == 0x26 {
            let dataoff: usize =
                u32::from_le_bytes(data[off + 8..off + 12].try_into().unwrap()) as usize;
            let datasize: usize =
                u32::from_le_bytes(data[off + 12..off + 16].try_into().unwrap()) as usize;
            let bytes: &[u8] = &data[dataoff..dataoff + datasize];
            let mut addrs: Vec<u64> = vec![];
            let mut addr: u64 = vmaddr;
            let mut i: usize = 0;
            while i < bytes.len() {
                let delta: u64 = uleb128(bytes, &mut i);
                if delta == 0 {
                    break;
                }
                addr += delta;
                addrs.push(addr);
            }
            return Ok(addrs);
        }
        off += sz;
    }
    bail!("no LC_FUNCTION_STARTS - can't auto-discover without it, pass --globals manually")
}

fn scan_globals(data: &[u8], globals: &[u64]) -> Result<Vec<(usize, [u8; 4])>> {
    let (vmaddr, fileoff, vmsize) = text_bounds(data)?;
    let slide: i64 = vmaddr as i64 - fileoff as i64;
    let (start, end) = (
        fileoff as usize,
        ((fileoff + vmsize) as usize).min(data.len()),
    );

    let mut out: Vec<(usize, [u8; 4])> = vec![];
    let mut i: usize = start;
    while i + 8 <= end {
        let w1: u32 = u32::from_le_bytes(data[i..i + 4].try_into().unwrap());
        if let Some((rd, page)) = adrp(w1, (i as i64 + slide) as u64) {
            let w2: u32 = u32::from_le_bytes(data[i + 4..i + 8].try_into().unwrap());
            if let Some((rt, rn, imm12)) = ldrb(w2) {
                if rn == rd && globals.contains(&(page + imm12 as u64)) {
                    out.push((i + 4, mov_imm1(rt)));
                }
            }
        }
        i += 4;
    }
    Ok(out)
}

pub fn run_discover(macho_path: &Path) -> Result<()> {
    let data: Vec<u8> = fs::read(macho_path)?;
    let addrs: Vec<u64> = if is_pe(&data) {
        discover_via_anchor_pe(&data, "HasInternalPermission")?
    } else {
        discover_via_anchor(&data, "HasInternalPermission")?
    };
    println!(
        "found it via the HasInternalPermission getter, {} global(s):",
        addrs.len()
    );
    for a in &addrs {
        println!("  0x{a:x}");
    }
    println!(
        "--globals {}",
        addrs
            .iter()
            .map(|a: &u64| format!("0x{a:x}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    Ok(())
}

pub fn run_globals(macho_path: &Path, args: &Args) -> Result<()> {
    let mut data: Vec<u8> = fs::read(macho_path)?;

    if is_pe(&data) {
        return run_globals_pe(macho_path, &mut data, args);
    }
    if macho_is_x86_64(&data) {
        return run_globals_macho_x86(macho_path, &mut data, args);
    }

    let globals: Vec<u64> = if args.globals.len() == 1 && args.globals[0] == "auto" {
        let found: Vec<u64> = discover_via_anchor(&data, "HasInternalPermission")?;
        println!("auto-discovered {} global(s): {:x?}", found.len(), found);
        found
    } else {
        args.globals
            .iter()
            .map(|s: &String| {
                u64::from_str_radix(s.trim().trim_start_matches("0x"), 16)
                    .with_context(|| s.clone())
            })
            .collect::<Result<Vec<_>>>()?
    };

    let patches: Vec<(usize, [u8; 4])> = scan_globals(&data, &globals)?;
    if patches.is_empty() {
        bail!("nothing found for {globals:x?}, wrong version or already patched");
    }
    println!("{} sites:", patches.len());
    for (off, new) in &patches {
        println!(
            "  0x{off:x} -> {}",
            new.iter()
                .map(|b| format!("{b:02X}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }

    if args.dry_run {
        println!("dry run");
        return Ok(());
    }
    kill_running_studio(macho_path, args)?;
    if !args.no_backup {
        backup(macho_path)?;
    }
    for (off, new) in &patches {
        data[*off..*off + 4].copy_from_slice(new);
    }
    fs::write(macho_path, &data)?;
    println!("patched {}", patches.len());
    if !args.no_resign {
        resign(macho_path)?;
    }
    Ok(())
}

fn run_globals_pe(exe_path: &Path, data: &mut Vec<u8>, args: &Args) -> Result<()> {
    let globals: Vec<u64> = if args.globals.len() == 1 && args.globals[0] == "auto" {
        let found: Vec<u64> = discover_via_anchor_pe(data, "HasInternalPermission")?;
        println!("auto-discovered {} global(s): {:x?}", found.len(), found);
        found
    } else {
        args.globals
            .iter()
            .map(|s: &String| {
                u64::from_str_radix(s.trim().trim_start_matches("0x"), 16)
                    .with_context(|| s.clone())
            })
            .collect::<Result<Vec<_>>>()?
    };

    let patches: Vec<usize> = scan_globals_pe(data, &globals)?;
    if patches.is_empty() {
        bail!("nothing found for {globals:x?}, wrong version or already patched");
    }
    println!("{} site(s):", patches.len());
    for off in &patches {
        println!("  0x{off:x} -> FF (was 00)");
    }

    if args.dry_run {
        println!("dry run");
        return Ok(());
    }
    kill_running_studio(exe_path, args)?;
    if !args.no_backup {
        backup(exe_path)?;
    }
    for &off in &patches {
        data[off] = 0xFF;
    }
    fs::write(exe_path, data)?;
    println!("patched {}", patches.len());
    Ok(())
}

pub fn run_sig(macho_path: &Path, args: &Args) -> Result<()> {
    let sig: Vec<PatByte> = parse_pattern(args.signature.as_deref().unwrap(), true)?;
    let patch: Vec<PatByte> = parse_pattern(args.patch.as_deref().unwrap(), false)?;
    if sig.len() != patch.len() {
        bail!(
            "sig is {} bytes, patch is {}, gotta match",
            sig.len(),
            patch.len()
        );
    }

    let mut data: Vec<u8> = fs::read(macho_path)?;
    let hits: Vec<usize> = find_matches(&data, &sig);
    match hits.len() {
        0 => bail!("signature not found - wrong binary/version?"),
        1 => println!("1 match @ 0x{:x}", hits[0]),
        n => {
            println!("{n} matches:");
            for (i, off) in hits.iter().enumerate() {
                println!("  [{i}] 0x{off:x}");
            }
            if args.occurrence >= n {
                bail!(
                    "--occurrence {} out of range ({n} matches)",
                    args.occurrence
                );
            }
        }
    }

    let offset: usize = hits[args.occurrence];
    if args.dry_run {
        println!("dry run");
        return Ok(());
    }
    kill_running_studio(macho_path, args)?;
    if !args.no_backup {
        backup(macho_path)?;
    }
    for (i, p) in patch.iter().enumerate() {
        if let PatByte::Exact(b) = p {
            data[offset + i] = *b;
        }
    }
    fs::write(macho_path, &data)?;
    println!("patched {} bytes @ 0x{:x}", patch.len(), offset);
    if !args.no_resign {
        resign(macho_path)?;
    }
    Ok(())
}
