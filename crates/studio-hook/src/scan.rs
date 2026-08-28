use memchr::memchr_iter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MaskedByte {
    value: u8,
    mask: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    bytes: Vec<MaskedByte>,
    anchor: Option<(usize, u8)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternError {
    Empty,
    AllWildcards,
    BadToken,
}

const ARM64_INSTRUCTION: usize = 4;

/// Mask clearing the fields of an arm64 instruction that move between builds:
/// struct offsets in load/store/add immediates and branch displacements.
const fn arm64_volatile_mask(word: u32) -> u32 {
    const IMM26: u32 = 0x03ff_ffff;
    const IMM19: u32 = 0x7_ffff << 5;
    const IMM14: u32 = 0x3fff << 5;
    const IMM12: u32 = 0xfff << 10;
    const IMM7: u32 = 0x7f << 15;
    const ADR: u32 = (0x3 << 29) | (0x7_ffff << 5);

    let volatile = if (word >> 26) & 0x3f == 0b000101 || (word >> 26) & 0x3f == 0b100101 {
        IMM26
    } else if (word >> 24) & 0xff == 0b0101_0100 {
        IMM19
    } else if (word >> 25) & 0x3f == 0b011010 {
        IMM19
    } else if (word >> 25) & 0x3f == 0b011011 {
        IMM14
    } else if (word >> 24) & 0x1f == 0b1_0000 {
        ADR
    } else if (word >> 23) & 0x3f == 0b100010 {
        IMM12
    } else if (word >> 27) & 0x7 == 0b111 && (word >> 24) & 0x3 == 0b01 {
        IMM12
    } else if (word >> 27) & 0x7 == 0b101 && (word >> 25) & 0x1 == 0 {
        IMM7
    } else if (word >> 27) & 0x7 == 0b011 && (word >> 24) & 0x3 == 0b00 {
        IMM19
    } else {
        0
    };
    !volatile
}

impl Pattern {
    pub fn parse(spec: &str) -> Result<Pattern, PatternError> {
        let mut bytes = Vec::new();
        for token in spec.split_whitespace() {
            if token.starts_with('?') {
                bytes.push(MaskedByte { value: 0, mask: 0 });
            } else {
                let value = u8::from_str_radix(token, 16).map_err(|_| PatternError::BadToken)?;
                bytes.push(MaskedByte { value, mask: 0xff });
            }
        }
        Pattern::from_bytes(bytes)
    }

    fn from_bytes(bytes: Vec<MaskedByte>) -> Result<Pattern, PatternError> {
        if bytes.is_empty() {
            return Err(PatternError::Empty);
        }
        if bytes.iter().all(|byte| byte.mask == 0) {
            return Err(PatternError::AllWildcards);
        }
        let anchor = bytes
            .iter()
            .position(|byte| byte.mask == 0xff)
            .map(|at| (at, bytes[at].value));
        Ok(Pattern { bytes, anchor })
    }

    /// Same pattern with build-dependent instruction fields masked out, so a signature
    /// survives a Studio update that only moves struct offsets. `None` when nothing
    /// concrete would be left to anchor on.
    pub fn relaxed(&self) -> Option<Pattern> {
        if !cfg!(target_arch = "aarch64") {
            return None;
        }
        if self.bytes.len() % ARM64_INSTRUCTION != 0 {
            return None;
        }
        let mut bytes = self.bytes.clone();
        for group in bytes.chunks_exact_mut(ARM64_INSTRUCTION) {
            if group.iter().any(|byte| byte.mask != 0xff) {
                continue;
            }
            let word = u32::from_le_bytes([group[0].value, group[1].value, group[2].value, group[3].value]);
            let mask = arm64_volatile_mask(word).to_le_bytes();
            for (byte, mask) in group.iter_mut().zip(mask) {
                byte.mask = mask;
                byte.value &= mask;
            }
        }
        Pattern::from_bytes(bytes).ok()
    }

    /// Renders bytes found at a match as a signature string, keeping the wildcard
    /// positions of this pattern so the result can be pasted back into `signatures`.
    pub fn render(&self, found: &[u8]) -> String {
        let mut out = String::with_capacity(self.bytes.len() * 3);
        for (expected, actual) in self.bytes.iter().zip(found) {
            if expected.mask == 0 {
                out.push_str("?? ");
            } else {
                out.push_str(&format!("{actual:02x} "));
            }
        }
        out.truncate(out.trim_end().len());
        out
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    fn matches_at(&self, haystack: &[u8], start: usize) -> bool {
        if start + self.bytes.len() > haystack.len() {
            return false;
        }
        self.bytes
            .iter()
            .zip(&haystack[start..start + self.bytes.len()])
            .all(|(expected, actual)| actual & expected.mask == expected.value)
    }

    pub fn find_all(&self, haystack: &[u8]) -> Vec<usize> {
        let mut hits = Vec::new();
        if haystack.len() < self.bytes.len() {
            return hits;
        }
        let last_start = haystack.len() - self.bytes.len();
        let Some((anchor, anchor_byte)) = self.anchor else {
            hits.extend((0..=last_start).filter(|start| self.matches_at(haystack, *start)));
            return hits;
        };
        for at in memchr_iter(anchor_byte, haystack) {
            if at < anchor {
                continue;
            }
            let start = at - anchor;
            if start > last_start {
                break;
            }
            if self.matches_at(haystack, start) {
                hits.push(start);
            }
        }
        hits
    }

    pub fn find_one(&self, haystack: &[u8]) -> Option<usize> {
        let hits = self.find_all(haystack);
        match hits.len() {
            1 => Some(hits[0]),
            _ => None,
        }
    }
}

pub fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    memchr::memmem::find(haystack, needle)
}

pub fn find_aligned_usize(haystack: &[u8], base: usize, value: usize) -> Vec<usize> {
    let mut hits = Vec::new();
    let skew = base % 8;
    let start = if skew == 0 { 0 } else { 8 - skew };
    let wanted = value.to_ne_bytes();
    let mut at = start;
    while at + 8 <= haystack.len() {
        if haystack[at..at + 8] == wanted {
            hits.push(base + at);
        }
        at += 8;
    }
    hits
}

pub fn find_aligned_u32(haystack: &[u8], base: usize, value: u32) -> Vec<usize> {
    let mut hits = Vec::new();
    let skew = base % 4;
    let start = if skew == 0 { 0 } else { 4 - skew };
    let wanted = value.to_ne_bytes();
    let mut at = start;
    while at + 4 <= haystack.len() {
        if haystack[at..at + 4] == wanted {
            hits.push(base + at);
        }
        at += 4;
    }
    hits
}

pub fn decode_arm64_bl(instruction: u32, instruction_addr: usize) -> Option<usize> {
    if instruction >> 26 != 0b100101 {
        return None;
    }
    let imm26 = instruction & 0x03ff_ffff;
    let offset = ((imm26 as i32) << 6) >> 6;
    let byte_offset = (offset as isize) * 4;
    Some((instruction_addr as isize + byte_offset) as usize)
}

/// Byte offset encoded in an arm64 `LDR`/`STR` unsigned-immediate instruction,
/// already scaled by the access size. `None` when `word` is not that form.
pub const fn decode_arm64_load_offset(word: u32) -> Option<usize> {
    if (word >> 27) & 0x7 != 0b111 || (word >> 24) & 0x3 != 0b01 {
        return None;
    }
    let scale = (word >> 30) & 0x3;
    let imm12 = ((word >> 10) & 0xfff) as usize;
    Some(imm12 << scale)
}

/// Byte offset encoded in an x86-64 `mov r64, [reg+disp]` (`REX.W 8B /r`), the form
/// Studio's x86 builds use to load a struct field. `None` when `bytes` is not that form.
pub fn decode_x86_load_offset(bytes: [u8; 8]) -> Option<usize> {
    if bytes[0] & 0xf8 != 0x48 || bytes[1] != 0x8b {
        return None;
    }
    let modrm = bytes[2];
    let rm = modrm & 0x7;
    if rm == 0b100 || rm == 0b101 {
        return None;
    }
    match modrm >> 6 {
        0b01 => Some(bytes[3] as usize),
        0b10 => Some(u32::from_le_bytes([bytes[3], bytes[4], bytes[5], bytes[6]]) as usize),
        _ => None,
    }
}

pub fn decode_self_xor_ptr(storage_addr: usize, low: u32, high: u32) -> usize {
    let key = storage_addr as u32;
    let low = low ^ key;
    let high = high ^ key;
    ((high as u64) << 32 | low as u64) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_matches_with_wildcards() {
        let pattern = Pattern::parse("48 8B ?? ?? C3").expect("valid");
        assert_eq!(pattern.len(), 5);
        let haystack = [0x00, 0x48, 0x8B, 0xAA, 0xBB, 0xC3, 0x00];
        assert_eq!(pattern.find_all(&haystack), vec![1]);
    }

    #[test]
    fn leading_wildcards_do_not_break_the_anchor() {
        let pattern = Pattern::parse("?? ?? C3 90").expect("valid");
        let haystack = [0x11, 0x22, 0xC3, 0x90, 0xFF];
        assert_eq!(pattern.find_all(&haystack), vec![0]);
    }

    #[test]
    fn find_one_requires_uniqueness() {
        let pattern = Pattern::parse("90 90").expect("valid");
        let unique = [0x00, 0x90, 0x90, 0x00];
        assert_eq!(pattern.find_one(&unique), Some(1));
        let ambiguous = [0x90, 0x90, 0x00, 0x90, 0x90];
        assert_eq!(pattern.find_one(&ambiguous), None);
    }

    #[test]
    fn rejects_all_wildcard_patterns() {
        assert_eq!(Pattern::parse("?? ??"), Err(PatternError::AllWildcards));
        assert_eq!(Pattern::parse(""), Err(PatternError::Empty));
        assert_eq!(Pattern::parse("zz"), Err(PatternError::BadToken));
    }

    #[test]
    fn no_match_past_end_of_haystack() {
        let pattern = Pattern::parse("C3 90 90").expect("valid");
        let haystack = [0x00, 0x00, 0xC3, 0x90];
        assert!(pattern.find_all(&haystack).is_empty());
    }

    #[test]
    fn relaxed_ignores_a_struct_offset_that_moved() {
        let strict = Pattern::parse("08 01 1b 91").expect("valid");
        let moved = [0x08u8, 0xc1, 0x1b, 0x91];
        assert!(strict.find_all(&moved).is_empty());
        assert_eq!(strict.relaxed().expect("relaxable").find_all(&moved), vec![0]);
    }

    #[test]
    fn relaxed_keeps_opcode_and_registers_significant() {
        let relaxed = Pattern::parse("08 01 1b 91").expect("valid").relaxed().expect("relaxable");
        assert!(relaxed.find_all(&[0x09, 0x01, 0x1b, 0x91]).is_empty());
        assert!(relaxed.find_all(&[0x08, 0x01, 0x1b, 0xd1]).is_empty());
    }

    #[test]
    fn relaxed_ignores_load_pair_and_branch_displacements() {
        let ldp = Pattern::parse("19 23 45 a9").expect("valid").relaxed().expect("relaxable");
        assert_eq!(ldp.find_all(&[0x19, 0x23, 0x40, 0xa9]), vec![0]);
        let bl = Pattern::parse("bc fb ff 97").expect("valid").relaxed().expect("relaxable");
        assert_eq!(bl.find_all(&[0x11, 0x22, 0x33, 0x97]), vec![0]);
    }

    #[test]
    fn relaxed_leaves_register_moves_fully_significant() {
        let mov = Pattern::parse("f3 03 01 aa").expect("valid").relaxed().expect("relaxable");
        assert!(mov.find_all(&[0xf3, 0x03, 0x02, 0xaa]).is_empty());
    }

    #[test]
    fn relaxed_declines_patterns_that_are_not_instruction_sized() {
        assert!(Pattern::parse("48 8b 05").expect("valid").relaxed().is_none());
    }

    #[test]
    fn relaxed_preserves_explicit_wildcard_groups() {
        let pattern = Pattern::parse("?? ?? ?? ?? f3 03 01 aa").expect("valid");
        let relaxed = pattern.relaxed().expect("relaxable");
        assert_eq!(relaxed.find_all(&[0xde, 0xad, 0xbe, 0xef, 0xf3, 0x03, 0x01, 0xaa]), vec![0]);
    }

    #[test]
    fn finds_aligned_pointers_respecting_base_skew() {
        let value = 0x0000_7f11_2233_4455usize;
        let mut buf = vec![0u8; 32];
        buf[8..16].copy_from_slice(&value.to_ne_bytes());
        assert_eq!(find_aligned_usize(&buf, 0x1000, value), vec![0x1008]);
        assert!(find_aligned_usize(&buf, 0x1004, value).is_empty());
    }

    #[test]
    fn decodes_load_offsets_scaled_by_access_size() {
        assert_eq!(decode_arm64_load_offset(0xf9401668), Some(0x28));
        assert_eq!(decode_arm64_load_offset(0xf9400e68), Some(0x18));
        assert_eq!(decode_arm64_load_offset(0xf9401a68), Some(0x30));
        assert_eq!(decode_arm64_load_offset(0xf9402e68), Some(0x58));
    }

    #[test]
    fn rejects_instructions_that_are_not_unsigned_immediate_loads() {
        assert_eq!(decode_arm64_load_offset(0xaa0003f3), None);
        assert_eq!(decode_arm64_load_offset(0x9400001f), None);
        assert_eq!(decode_arm64_load_offset(0xa9402319), None);
    }

    #[test]
    fn decodes_arm64_bl_forward_and_backward() {
        assert_eq!(decode_arm64_bl(0x9400_0002, 0x1000), Some(0x1008));
        assert_eq!(decode_arm64_bl(0x97ff_ffff, 0x1000), Some(0x0ffc));
        assert_eq!(decode_arm64_bl(0xd503_201f, 0x1000), None);
    }

    #[test]
    fn self_xor_decode_round_trips() {
        let storage = 0x0000_0001_2345_6258usize;
        let real_ptr = 0x0000_7f12_3456_78a0usize;
        let key = storage as u32;
        let low = (real_ptr as u32) ^ key;
        let high = ((real_ptr >> 32) as u32) ^ key;
        assert_eq!(decode_self_xor_ptr(storage, low, high), real_ptr);
    }
}
