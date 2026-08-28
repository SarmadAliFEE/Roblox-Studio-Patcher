use std::env;
use std::fs;

use studio_hook::scan::Pattern;
use studio_hook::vm::signatures as sig;

const DEFAULT_STUDIO: &str = "/Applications/RobloxStudio.app/Contents/MacOS/RobloxStudio";
const MH_MAGIC_64: u32 = 0xfeed_facf;
const LC_SEGMENT_64: u32 = 0x19;

struct Section {
    segment: String,
    name: String,
    addr: u64,
    offset: u32,
    size: u64,
}

fn read_u32(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().expect("4 bytes"))
}

fn read_u64(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().expect("8 bytes"))
}

fn cstr(data: &[u8]) -> String {
    let end = data.iter().position(|b| *b == 0).unwrap_or(data.len());
    String::from_utf8_lossy(&data[..end]).into_owned()
}

fn sections(data: &[u8]) -> Vec<Section> {
    assert_eq!(read_u32(data, 0), MH_MAGIC_64, "not a 64-bit little-endian mach-o");
    let ncmds = read_u32(data, 16) as usize;
    let mut out = Vec::new();
    let mut cursor = 32usize;
    for _ in 0..ncmds {
        let cmd = read_u32(data, cursor);
        let cmdsize = read_u32(data, cursor + 4) as usize;
        if cmd == LC_SEGMENT_64 {
            let segment = cstr(&data[cursor + 8..cursor + 24]);
            let nsects = read_u32(data, cursor + 64) as usize;
            let mut at = cursor + 72;
            for _ in 0..nsects {
                out.push(Section {
                    segment: segment.clone(),
                    name: cstr(&data[at..at + 16]),
                    addr: read_u64(data, at + 32),
                    size: read_u64(data, at + 40),
                    offset: read_u32(data, at + 48),
                });
                at += 80;
            }
        }
        cursor += cmdsize;
    }
    out
}

fn check(name: &str, spec: &str, text: &Section, bytes: &[u8]) -> bool {
    if spec.is_empty() {
        println!("  SKIP   {name:24} (no signature for this target)");
        return true;
    }
    let Ok(pattern) = Pattern::parse(spec) else {
        println!("  BAD    {name:24} unparseable signature");
        return false;
    };
    let report = |hits: &[usize]| -> String {
        hits.iter().take(3).map(|at| format!("{:#x}", text.addr as usize + at)).collect::<Vec<_>>().join(" ")
    };
    let strict = pattern.find_all(bytes);
    if strict.len() == 1 {
        println!("  OK     {name:24} {}", report(&strict));
        return true;
    }
    let relaxed = pattern.relaxed().map(|relaxed| relaxed.find_all(bytes)).unwrap_or_default();
    if strict.is_empty() && relaxed.len() == 1 {
        let at = relaxed[0];
        println!("  DRIFT  {name:24} {} - refresh to:", report(&relaxed));
        println!("           {}", pattern.render(&bytes[at..at + pattern.len()]));
        return true;
    }
    let kind = if strict.is_empty() { "MISSING" } else { "AMBIGUOUS" };
    println!("  FAIL   {name:24} {kind} (strict={} relaxed={})", strict.len(), relaxed.len());
    false
}

fn main() {
    let path = env::args().nth(1).unwrap_or_else(|| DEFAULT_STUDIO.to_owned());
    let data = fs::read(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
    let sections = sections(&data);
    let text = sections
        .iter()
        .find(|section| section.segment == "__TEXT" && section.name == "__text")
        .expect("no __TEXT,__text section");
    let start = text.offset as usize;
    let bytes = &data[start..start + text.size as usize];

    println!("{path}");
    println!("__text {:#x} ({} bytes)\n", text.addr, bytes.len());

    let mut ok = true;
    for (name, spec) in [
        ("STEP", sig::STEP),
        ("LUAU_LOAD_WRAPPER", sig::LUAU_LOAD_WRAPPER),
        ("CALL_DISPATCH", sig::CALL_DISPATCH),
        ("TASK_DEFER", sig::TASK_DEFER),
        ("LUA_NEWTHREAD", sig::LUA_NEWTHREAD),
        ("CAN_ACCESS_RESTRICTED", sig::CAN_ACCESS_RESTRICTED),
    ] {
        ok &= check(name, spec, text, bytes);
    }

    println!();
    for rtti in [sig::DATA_MODEL_RTTI, sig::SCRIPT_CONTEXT_RTTI, sig::WAITING_HYBRID_RTTI] {
        let mut needle = rtti.as_bytes().to_vec();
        needle.push(0);
        let found = memchr::memmem::find(&data, &needle).is_some();
        println!("  {:6} {rtti}", if found { "OK" } else { "FAIL" });
        ok &= found;
    }

    if !ok {
        std::process::exit(1);
    }
}
