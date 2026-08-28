use crate::mem;
use crate::scan;

const CALL_DISPATCH_GLOBAL_LOAD: usize = 36;
const CALL_DISPATCH_TOP_LOAD: usize = 52;
const LUA_THREAD_TAG: u8 = 0x0a;
const MAIN_THREAD_TO_GLOBAL: usize = 0x80;

/// `lua_State` field offsets recovered from the running Studio rather than hardcoded,
/// so a build that moves them stays supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LuaProbe {
    pub global: usize,
    pub top: usize,
}

impl LuaProbe {
    /// Decodes the offsets from `call_dispatch`, which loads `L->global` and `L->top`
    /// at fixed positions inside the matched signature.
    pub fn from_call_dispatch(call_dispatch: usize) -> Option<LuaProbe> {
        let global_word: u32 = mem::read(call_dispatch + CALL_DISPATCH_GLOBAL_LOAD).ok()?;
        let top_word: u32 = mem::read(call_dispatch + CALL_DISPATCH_TOP_LOAD).ok()?;
        Self::from_words(global_word, top_word)
    }

    pub fn from_words(global_word: u32, top_word: u32) -> Option<LuaProbe> {
        let global = scan::decode_arm64_load_offset(global_word)?;
        let top = scan::decode_arm64_load_offset(top_word)?;
        if global == top || global == 0 || top == 0 {
            return None;
        }
        Some(LuaProbe { global, top })
    }

    pub fn looks_like_lua_state(&self, candidate: usize) -> bool {
        if !mem::looks_like_pointer(candidate) {
            return false;
        }
        if mem::read::<u8>(candidate).unwrap_or(0) != LUA_THREAD_TAG {
            return false;
        }
        let Ok(global) = mem::read_ptr(candidate + self.global) else { return false };
        mem::looks_like_pointer(global)
    }

    /// True when `candidate` is a VM's main thread, which Luau allocates immediately
    /// before its `global_State`.
    pub fn is_main_thread(&self, candidate: usize) -> bool {
        if !self.looks_like_lua_state(candidate) {
            return false;
        }
        mem::read_ptr(candidate + self.global).map(|global| global == candidate + MAIN_THREAD_TO_GLOBAL).unwrap_or(false)
    }
}

const EXTRA_SPACE_SHARED: usize = 0x18;
const CONTEXT_SCAN: usize = 0x80;
const FIELD_SCAN: usize = 0x100;

/// Locates `L->userdata` by proving the chain `L -> extra -> shared -> script_context`
/// reaches the context this thread belongs to. Returning `None` means no write should
/// happen, which is why elevation is gated on it rather than on a hardcoded offset.
pub fn derive_extra_space(lua_state: usize, script_context: usize) -> Option<usize> {
    for offset in (0..FIELD_SCAN).step_by(8) {
        let Ok(extra) = mem::read_ptr(lua_state + offset) else { continue };
        let Ok(shared) = mem::read_ptr(extra + EXTRA_SPACE_SHARED) else { continue };
        let reaches = (0..CONTEXT_SCAN)
            .step_by(8)
            .any(|at| mem::read_ptr(shared + at) == Ok(script_context));
        if reaches {
            return Some(offset);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_offsets_this_studio_build_uses() {
        let probe = LuaProbe::from_words(0xf9401668, 0xf9400e68).expect("decodable");
        assert_eq!(probe, LuaProbe { global: 0x28, top: 0x18 });
    }

    #[test]
    fn decodes_the_offsets_the_previous_build_used() {
        let probe = LuaProbe::from_words(0xf9401a68, 0xf9402e68).expect("decodable");
        assert_eq!(probe, LuaProbe { global: 0x30, top: 0x58 });
    }

    #[test]
    fn refuses_instructions_that_are_not_loads() {
        assert!(LuaProbe::from_words(0xaa0003f3, 0xf9400e68).is_none());
        assert!(LuaProbe::from_words(0xf9401668, 0x9400001f).is_none());
    }

    #[test]
    fn refuses_when_both_offsets_collapse_to_the_same_field() {
        assert!(LuaProbe::from_words(0xf9401668, 0xf9401668).is_none());
    }
}
