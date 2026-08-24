use memchr::memchr_iter;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    bytes: Vec<Option<u8>>,
    anchor: usize,
    anchor_byte: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternError {
    Empty,
    AllWildcards,
    BadToken,
}

impl Pattern {
    pub fn parse(spec: &str) -> Result<Pattern, PatternError> {
        let mut bytes = Vec::new();
        for token in spec.split_whitespace() {
            if token.starts_with('?') {
                bytes.push(None);
            } else {
                let value = u8::from_str_radix(token, 16).map_err(|_| PatternError::BadToken)?;
                bytes.push(Some(value));
            }
        }
        if bytes.is_empty() {
            return Err(PatternError::Empty);
        }
        let anchor = bytes
            .iter()
            .position(Option::is_some)
            .ok_or(PatternError::AllWildcards)?;
        let anchor_byte = bytes[anchor].expect("anchor position holds a concrete byte");
        Ok(Pattern { bytes, anchor, anchor_byte })
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
            .all(|(expected, actual)| match expected {
                Some(byte) => byte == actual,
                None => true,
            })
    }

    pub fn find_all(&self, haystack: &[u8]) -> Vec<usize> {
        let mut hits = Vec::new();
        if haystack.len() < self.bytes.len() {
            return hits;
        }
        let last_start = haystack.len() - self.bytes.len();
        for at in memchr_iter(self.anchor_byte, haystack) {
            if at < self.anchor {
                continue;
            }
            let start = at - self.anchor;
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

pub fn decode_arm64_bl(instruction: u32, instruction_addr: usize) -> Option<usize> {
    if instruction >> 26 != 0b100101 {
        return None;
    }
    let imm26 = instruction & 0x03ff_ffff;
    let offset = ((imm26 as i32) << 6) >> 6;
    let byte_offset = (offset as isize) * 4;
    Some((instruction_addr as isize + byte_offset) as usize)
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
