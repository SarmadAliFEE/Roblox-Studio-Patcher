pub mod resolve;
pub mod signatures;

use crate::mem;

pub const L_STACK_LIMIT: usize = 0x50;
pub const L_TOP: usize = 0x58;
pub const L_GLOBAL: usize = 0x30;
pub const L_EXTRA_SPACE: usize = 0x78;
pub const GLOBAL_DEPTH: usize = 0x4980;
pub const EXTRA_SPACE_SHARED: usize = 0x18;
pub const SHARED_CONTEXT: usize = 0x18;
pub const DATAMODEL_GAME_STATE_TYPE: usize = 0x4f0;
pub const GAME_STATE_EDIT: i32 = 0;
pub const GAME_STATE_EMPTY: i32 = 3;

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

pub fn looks_like_lua_state(candidate: usize, expected_context: Option<usize>) -> bool {
    let (Ok(limit), Ok(top)) = (
        mem::read_ptr(candidate + L_STACK_LIMIT),
        mem::read_ptr(candidate + L_TOP),
    ) else {
        return false;
    };

    let Ok(limit_value) = mem::read::<usize>(limit) else { return false };
    let delta = limit_value as isize - top as isize;
    if !(-0x100000..=0x100000).contains(&delta) {
        return false;
    }

    let Ok(global) = mem::read_ptr(candidate + L_GLOBAL) else { return false };
    let Ok(depth) = mem::read::<i32>(global + GLOBAL_DEPTH) else { return false };
    if !(0..=10_000).contains(&depth) {
        return false;
    }

    let Some(context) = expected_context else { return true };
    let Ok(extra) = mem::read_ptr(candidate + L_EXTRA_SPACE) else { return false };
    let Ok(shared) = mem::read_ptr(extra + EXTRA_SPACE_SHARED) else { return false };
    let Ok(bound) = mem::read::<usize>(shared + SHARED_CONTEXT) else { return false };
    bound == context
}

pub fn find_lua_state_near(root: usize, fields: usize, cursor: &mut Cursor) -> Option<usize> {
    let start = cursor.0;
    let end = (start + STAGE_ITER_BUDGET).min(fields);
    let inner = fields.min(INNER_SEARCH_BUDGET);

    for index in start..end {
        let Ok(value) = mem::read_ptr(root + index * 8) else { continue };
        if looks_like_lua_state(value, Some(root)) {
            cursor.reset();
            return Some(value);
        }
        for nested in 0..inner {
            let Ok(inner_value) = mem::read_ptr(value + nested * 8) else { continue };
            if looks_like_lua_state(inner_value, Some(root)) {
                cursor.reset();
                return Some(inner_value);
            }
        }
    }

    cursor.0 = if end >= fields { 0 } else { end };
    None
}

pub fn game_state_type(data_model: usize) -> Option<i32> {
    mem::read::<i32>(data_model + DATAMODEL_GAME_STATE_TYPE).ok()
}

pub fn is_edit_data_model(data_model: usize) -> bool {
    game_state_type(data_model) == Some(GAME_STATE_EDIT)
}

pub fn is_play_test_state(state: i32) -> bool {
    state >= 0 && state != GAME_STATE_EDIT && state != GAME_STATE_EMPTY
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

    fn synthetic_lua_state(state: &mut FakeMemory, global: &mut FakeMemory, depth: i32) -> usize {
        let stack_word = 40;
        let top = state.addr_of(stack_word);
        state.set_ptr(stack_word, top);
        state.set_ptr(L_STACK_LIMIT / 8, state.addr_of(stack_word));
        state.set_ptr(L_TOP / 8, top);
        state.set_ptr(L_GLOBAL / 8, global.addr_of(0));
        global.set(GLOBAL_DEPTH / 8, depth as u32 as u64);
        state.addr_of(0)
    }

    #[test]
    fn accepts_a_structurally_plausible_lua_state() {
        let mut state = FakeMemory::new(64);
        let mut global = FakeMemory::new(GLOBAL_DEPTH / 8 + 8);
        let l = synthetic_lua_state(&mut state, &mut global, 3);
        assert!(looks_like_lua_state(l, None));
    }

    #[test]
    fn rejects_a_lua_state_with_an_absurd_call_depth() {
        let mut state = FakeMemory::new(64);
        let mut global = FakeMemory::new(GLOBAL_DEPTH / 8 + 8);
        let l = synthetic_lua_state(&mut state, &mut global, 999_999);
        assert!(!looks_like_lua_state(l, None));
    }

    #[test]
    fn rejects_a_lua_state_bound_to_another_script_context() {
        let mut memory = FakeMemory::new(64);
        memory.set_ptr(L_STACK_LIMIT / 8, 0);
        let l = memory.addr_of(0);
        assert!(!looks_like_lua_state(l, Some(0x1234)));
    }

    #[test]
    fn classifies_game_states() {
        assert!(!is_play_test_state(GAME_STATE_EDIT));
        assert!(!is_play_test_state(GAME_STATE_EMPTY));
        assert!(!is_play_test_state(-1));
        assert!(is_play_test_state(1));
        assert!(is_play_test_state(2));
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
