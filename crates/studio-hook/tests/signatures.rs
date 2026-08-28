use studio_hook::scan::Pattern;
use studio_hook::vm::signatures::STEP as STEP_SIGNATURE;

const STUDIO_BINARY: &str = "/Applications/RobloxStudio.app/Contents/MacOS/RobloxStudio";

const LC_SEGMENT_64: u32 = 0x19;
const MH_MAGIC_64: u32 = 0xfeed_facf;

struct TextSegment {
    file_range: std::ops::Range<usize>,
    vmaddr: u64,
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn read_u64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn text_segments(image: &[u8]) -> Vec<TextSegment> {
    assert_eq!(read_u32(image, 0), MH_MAGIC_64, "expected a thin arm64 Mach-O");
    let ncmds = read_u32(image, 16) as usize;
    let mut cursor = 32;
    let mut out = Vec::new();

    for _ in 0..ncmds {
        let cmd = read_u32(image, cursor);
        let cmdsize = read_u32(image, cursor + 4) as usize;
        if cmdsize == 0 {
            break;
        }
        if cmd == LC_SEGMENT_64 {
            let name_bytes = &image[cursor + 8..cursor + 24];
            let end = name_bytes.iter().position(|b| *b == 0).unwrap_or(16);
            if std::str::from_utf8(&name_bytes[..end]) == Ok("__TEXT") {
                let vmaddr = read_u64(image, cursor + 24);
                let fileoff = read_u64(image, cursor + 40) as usize;
                let filesize = read_u64(image, cursor + 48) as usize;
                if filesize > 0 {
                    out.push(TextSegment { file_range: fileoff..fileoff + filesize, vmaddr });
                }
            }
        }
        cursor += cmdsize;
    }
    out
}

fn find_in_text(image: &[u8], spec: &str) -> Vec<u64> {
    let pattern = Pattern::parse(spec).expect("signature parses");
    let mut hits = Vec::new();
    for segment in text_segments(image) {
        let slice = &image[segment.file_range.clone()];
        for at in pattern.find_all(slice) {
            hits.push(segment.vmaddr + at as u64);
        }
    }
    hits
}

#[test]
fn step_signature_matches_exactly_once() {
    let Ok(image) = std::fs::read(STUDIO_BINARY) else {
        eprintln!("skipping: {STUDIO_BINARY} not present");
        return;
    };
    let hits = find_in_text(&image, STEP_SIGNATURE);
    assert_eq!(
        hits.len(),
        1,
        "DataModelJob::step signature matched {} times (expected 1) - Studio likely updated, re-derive it",
        hits.len()
    );
    println!("DataModelJob::step @ {:#x}", hits[0]);
}
