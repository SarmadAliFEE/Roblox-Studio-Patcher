pub mod exec;
pub mod layout;
pub mod liveeval;
pub mod hook;
pub mod discovery;
pub mod resolve;
pub mod signatures;

use crate::mem;


const STAGE_ITER_BUDGET: usize = 12;
const INNER_SEARCH_BUDGET: usize = 16;
const VECTOR_MAX_ELEMENTS: usize = 4096;
const VECTOR_MAX_SPAN: usize = 0x40000;

pub fn object_matches_vtable(object: usize, vtable: usize) -> bool {
    mem::read_ptr(object).map(|found| found == vtable).unwrap_or(false)
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Cursor(usize);

impl Cursor {
    pub fn reset(&mut self) {
        self.0 = 0;
    }

    pub fn exhausted(&self) -> bool {
        self.0 == 0
    }
}

pub fn find_instance_by_vtable(
    root: usize,
    fields: usize,
    vtable: usize,
    cursor: &mut Cursor,
) -> Option<usize> {
    let start = cursor.0;
    let end = (start + STAGE_ITER_BUDGET).min(fields);
    let inner = fields.min(INNER_SEARCH_BUDGET);

    for index in start..end {
        let Ok(value) = mem::read_ptr(root + index * 8) else { continue };
        if object_matches_vtable(value, vtable) {
            cursor.reset();
            return Some(value);
        }
        for nested in 0..inner {
            let Ok(inner_value) = mem::read_ptr(value + nested * 8) else { continue };
            if object_matches_vtable(inner_value, vtable) {
                cursor.reset();
                return Some(inner_value);
            }
        }
    }

    cursor.0 = if end >= fields { 0 } else { end };
    None
}

pub fn find_instance_in_vector_fields(
    root: usize,
    fields: usize,
    vtable: usize,
    cursor: &mut Cursor,
) -> Option<usize> {
    let limit = fields.saturating_sub(1);
    let start = cursor.0;
    let end = (start + STAGE_ITER_BUDGET).min(limit);

    for index in start..end {
        let base = root + index * 8;
        let (Ok(begin), Ok(finish)) = (mem::read::<usize>(base), mem::read::<usize>(base + 8)) else {
            continue;
        };
        if begin < 0x1000 || finish < begin {
            continue;
        }
        let span = finish - begin;
        if span == 0 || span > VECTOR_MAX_SPAN {
            continue;
        }
        for stride in [8usize, 16] {
            if span % stride != 0 {
                continue;
            }
            let count = span / stride;
            if count > VECTOR_MAX_ELEMENTS {
                continue;
            }
            for element in 0..count {
                let Ok(value) = mem::read_ptr(begin + element * stride) else { continue };
                if object_matches_vtable(value, vtable) {
                    cursor.reset();
                    return Some(value);
                }
            }
        }
    }

    cursor.0 = if end >= limit { 0 } else { end };
    None
}

pub const SC_SCAN_SPAN: usize = 0x1200;

pub fn decode_vm_slot(storage: usize) -> Option<usize> {
    let low: u32 = mem::read(storage).ok()?;
    let high: u32 = mem::read(storage + 4).ok()?;
    let decoded = crate::scan::decode_self_xor_ptr(storage, low, high);
    mem::looks_like_pointer(decoded).then_some(decoded)
}

pub const MAX_VM_CANDIDATES: usize = 8;

/// Every VM main thread the ScriptContext holds, found by scanning it for self-xor
/// encoded `lua_State`s so the VM collection's field offsets never have to be
/// hardcoded. Being found inside the ScriptContext is what binds them to it. A
/// context carries one VM per class and only one of them owns the open place, so
/// callers try each in turn.
pub fn main_thread_candidates(script_context: usize, probe: &layout::LuaProbe) -> Vec<usize> {
    let mut found = Vec::new();
    for offset in (0..SC_SCAN_SPAN).step_by(4) {
        let Some(candidate) = decode_vm_slot(script_context + offset) else { continue };
        if probe.is_main_thread(candidate) && !found.contains(&candidate) {
            found.push(candidate);
            if found.len() >= MAX_VM_CANDIDATES {
                break;
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeMemory {
        buffer: Vec<u64>,
    }

    impl FakeMemory {
        fn new(words: usize) -> FakeMemory {
            FakeMemory { buffer: vec![0u64; words] }
        }

        fn addr_of(&self, word: usize) -> usize {
            &self.buffer[word] as *const u64 as usize
        }

        fn set_ptr(&mut self, word: usize, value: usize) {
            self.buffer[word] = value as u64;
        }

        fn set(&mut self, word: usize, value: u64) {
            self.buffer[word] = value;
        }
    }

    #[test]
    fn matches_an_object_by_its_vtable() {
        let mut memory = FakeMemory::new(4);
        let vtable = 0x1234_5000usize;
        memory.set_ptr(0, vtable);
        let object = memory.addr_of(0);
        assert!(object_matches_vtable(object, vtable));
        assert!(!object_matches_vtable(object, 0x9999_0000));
    }

    #[test]
    fn finds_a_direct_field_and_clears_the_cursor() {
        let mut memory = FakeMemory::new(8);
        let vtable = 0x4000_0000usize;
        memory.set_ptr(0, vtable);
        let target = memory.addr_of(0);
        memory.set_ptr(4, target);
        let root = memory.addr_of(4);

        let mut cursor = Cursor::default();
        assert_eq!(find_instance_by_vtable(root, 4, vtable, &mut cursor), Some(target));
        assert!(cursor.exhausted());
    }

    #[test]
    fn search_resumes_across_calls_instead_of_scanning_everything() {
        let fields = STAGE_ITER_BUDGET * 3;
        let memory = FakeMemory::new(fields + 1);
        let root = memory.addr_of(0);

        let mut cursor = Cursor::default();
        assert_eq!(find_instance_by_vtable(root, fields, 0xdead_0000, &mut cursor), None);
        assert!(!cursor.exhausted());

        let mut seen = 1;
        while !cursor.exhausted() {
            assert_eq!(find_instance_by_vtable(root, fields, 0xdead_0000, &mut cursor), None);
            seen += 1;
            assert!(seen < 10, "cursor must terminate");
        }
        assert_eq!(seen, 3);
    }

    #[test]
    fn vector_scan_finds_an_element_and_resumes() {
        let mut memory = FakeMemory::new(32);
        let vtable = 0x5555_0000usize;
        memory.set_ptr(0, vtable);
        let element = memory.addr_of(0);

        memory.set_ptr(8, element);
        let begin = memory.addr_of(8);
        memory.set_ptr(16, begin);
        memory.set_ptr(17, begin + 8);
        let root = memory.addr_of(16);

        let mut cursor = Cursor::default();
        assert_eq!(find_instance_in_vector_fields(root, 4, vtable, &mut cursor), Some(element));
    }
}
