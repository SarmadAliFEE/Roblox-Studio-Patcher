use crate::mem;
use crate::scan;

#[cfg(target_arch = "aarch64")]
const CALL_DISPATCH_GLOBAL_LOAD: usize = 36;
#[cfg(target_arch = "aarch64")]
const CALL_DISPATCH_TOP_LOAD: usize = 52;
#[cfg(all(not(target_arch = "aarch64"), target_os = "macos"))]
const CALL_DISPATCH_GLOBAL_LOAD: usize = 0x1c;
#[cfg(all(not(target_arch = "aarch64"), target_os = "macos"))]
const CALL_DISPATCH_TOP_LOAD: usize = 0x31;
#[cfg(all(not(target_arch = "aarch64"), target_os = "windows"))]
const CALL_DISPATCH_GLOBAL_LOAD: usize = 0x1c;
#[cfg(all(not(target_arch = "aarch64"), target_os = "windows"))]
const CALL_DISPATCH_TOP_LOAD: usize = 0x36;
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
    #[cfg(target_arch = "aarch64")]
    pub fn from_call_dispatch(call_dispatch: usize) -> Option<LuaProbe> {
        let global_word: u32 = mem::read(call_dispatch + CALL_DISPATCH_GLOBAL_LOAD).ok()?;
        let top_word: u32 = mem::read(call_dispatch + CALL_DISPATCH_TOP_LOAD).ok()?;
        Self::from_words(global_word, top_word)
    }

    /// x86 builds load the same two fields with `mov r64, [reg+disp]`, so the offsets are
    /// read out of the instruction stream the same way, only with a different encoding.
    #[cfg(not(target_arch = "aarch64"))]
    pub fn from_call_dispatch(call_dispatch: usize) -> Option<LuaProbe> {
        let mut global_bytes = [0u8; 8];
        let mut top_bytes = [0u8; 8];
        mem::read_bytes(call_dispatch + CALL_DISPATCH_GLOBAL_LOAD, &mut global_bytes).ok()?;
        mem::read_bytes(call_dispatch + CALL_DISPATCH_TOP_LOAD, &mut top_bytes).ok()?;
        Self::from_offsets(
            scan::decode_x86_load_offset(global_bytes)?,
            scan::decode_x86_load_offset(top_bytes)?,
        )
    }

    pub fn from_words(global_word: u32, top_word: u32) -> Option<LuaProbe> {
        Self::from_offsets(
            scan::decode_arm64_load_offset(global_word)?,
            scan::decode_arm64_load_offset(top_word)?,
        )
    }

    fn from_offsets(global: usize, top: usize) -> Option<LuaProbe> {
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

#[cfg(target_arch = "aarch64")]
const SPC_USERDATA_STORE: usize = 0;
#[cfg(target_arch = "aarch64")]
const SPC_CHILD_COUNT_LOAD: usize = 4;
#[cfg(target_arch = "aarch64")]
const SPC_CHILDREN_LOAD: usize = 44;
#[cfg(target_arch = "aarch64")]
const GTC_CAPABILITIES_LOAD: usize = 40;
#[cfg(not(target_arch = "aarch64"))]
const SPC_USERDATA_STORE: usize = 0;
#[cfg(not(target_arch = "aarch64"))]
const SPC_CHILD_COUNT_LOAD: usize = 0x33;
#[cfg(not(target_arch = "aarch64"))]
const SPC_CHILDREN_LOAD: usize = 0x20;
#[cfg(not(target_arch = "aarch64"))]
const GTC_CAPABILITIES_LOAD: usize = 0x13;

/// Struct offsets used when granting a loaded chunk full capabilities, recovered from the
/// engine's own `setProtoCapabilities` and `getThreadCapabilities` 
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityLayout {
    pub closure_is_c: usize,
    pub closure_proto: usize,
    pub proto_userdata: usize,
    pub proto_children: usize,
    pub proto_child_count: usize,
    pub extra_capabilities: usize,
}

impl CapabilityLayout {
    /// Reads every offset out of the engine's own capability functions.
    pub fn derive(set_proto_caps: Option<usize>, get_thread_caps: Option<usize>) -> Option<CapabilityLayout> {
        let spc = set_proto_caps?;
        let gtc = get_thread_caps?;
        Some(CapabilityLayout {
            closure_is_c: 0x3,
            closure_proto: 0x18,
            proto_userdata: decode_at(spc + SPC_USERDATA_STORE)?,
            proto_children: decode_at(spc + SPC_CHILDREN_LOAD)?,
            proto_child_count: decode_at(spc + SPC_CHILD_COUNT_LOAD)?,
            extra_capabilities: decode_at(gtc + GTC_CAPABILITIES_LOAD)?,
        })
    }
}

#[cfg(target_arch = "aarch64")]
fn decode_at(instruction_addr: usize) -> Option<usize> {
    let word: u32 = mem::read(instruction_addr).ok()?;
    let offset = scan::decode_arm64_load_offset(word)?;
    (offset != 0).then_some(offset)
}

#[cfg(not(target_arch = "aarch64"))]
fn decode_at(instruction_addr: usize) -> Option<usize> {
    let mut bytes = [0u8; 8];
    mem::read_bytes(instruction_addr, &mut bytes).ok()?;
    let offset = scan::decode_x86_load_offset(bytes)?;
    (offset != 0).then_some(offset)
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

    #[test]
    fn decodes_capability_offsets_from_the_engines_instructions() {
        assert_eq!(scan::decode_arm64_load_offset(0xf9001001), Some(0x20));
        assert_eq!(scan::decode_arm64_load_offset(0xb9409408), Some(0x94));
        assert_eq!(scan::decode_arm64_load_offset(0xf9402a88), Some(0x50));
        assert_eq!(scan::decode_arm64_load_offset(0xf9403916), Some(0x70));
    }

    #[test]
    fn caps_layout_is_refused_when_the_functions_are_unresolved() {
        assert!(CapabilityLayout::derive(None, None).is_none());
        assert!(CapabilityLayout::derive(Some(0), None).is_none());
        assert!(CapabilityLayout::derive(None, Some(0)).is_none());
    }

    #[test]
    fn decodes_the_x86_builds_field_loads() {
        assert_eq!(scan::decode_x86_load_offset([0x48, 0x8b, 0x43, 0x30, 0, 0, 0, 0]), Some(0x30));
        assert_eq!(scan::decode_x86_load_offset([0x48, 0x8b, 0x53, 0x58, 0, 0, 0, 0]), Some(0x58));
        assert_eq!(
            scan::decode_x86_load_offset([0x48, 0x8b, 0x80, 0x28, 0x05, 0x00, 0x00, 0]),
            Some(0x528)
        );
    }

    #[test]
    fn refuses_x86_bytes_that_are_not_a_field_load() {
        assert!(scan::decode_x86_load_offset([0x55, 0x48, 0x89, 0xe5, 0, 0, 0, 0]).is_none());
        assert!(scan::decode_x86_load_offset([0x48, 0x8b, 0x03, 0x30, 0, 0, 0, 0]).is_none());
        assert!(scan::decode_x86_load_offset([0x48, 0x8b, 0x44, 0x24, 0x50, 0, 0, 0]).is_none());
    }

    #[test]
    fn decodes_the_x86_capability_stores_and_movsxd() {
        assert_eq!(scan::decode_x86_load_offset([0x48, 0x89, 0x77, 0x60, 0, 0, 0, 0]), Some(0x60));
        assert_eq!(scan::decode_x86_load_offset([0x49, 0x8b, 0x46, 0x10, 0, 0, 0, 0]), Some(0x10));
        assert_eq!(
            scan::decode_x86_load_offset([0x49, 0x63, 0x86, 0x8c, 0x00, 0x00, 0x00, 0]),
            Some(0x8c)
        );
        assert_eq!(scan::decode_x86_load_offset([0x4c, 0x8b, 0x70, 0x40, 0, 0, 0, 0]), Some(0x40));
    }
}
