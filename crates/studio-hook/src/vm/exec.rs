use core::ffi::{c_char, c_void};
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::mem;
use luau_compile::{self as luau, CompileError};

use crate::vm::layout::CapabilityLayout;

pub const CONTEXT_CACHED_CAPABILITIES: usize = 0x28;
pub const CONTEXT_LAZY_FN: usize = 0x30;

const TVALUE_SIZE: usize = 0x10;
const TVALUE_TAG: usize = 0x0c;
const TSTRING_LEN: usize = 20;
const TSTRING_DATA: usize = 24;
const MAX_STRING: usize = 4096;
const MAX_RESULTS: usize = 64;
const MAX_PLAUSIBLE_RESULTS: usize = 256;
const MAX_PROTO_DEPTH: usize = 8;
const MAX_PROTO_CHILDREN: i32 = 64;
const LUA_FIELD_SCAN: usize = 0x80;
const BASE_UNKNOWN: usize = usize::MAX;


type LoadFn = unsafe extern "C" fn(*mut c_void, *const c_char, *const u8, usize, i32) -> i32;
type CallFn = unsafe extern "C" fn(*mut c_void, u64, i32) -> u64;
type ContextCurrentFn = unsafe extern "C" fn() -> *mut c_void;
type NewThreadFn = unsafe extern "C" fn(*mut c_void) -> *mut c_void;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Nil,
    Bool(bool),
    Number(f64),
    Int(i32),
    Str(String),
    Table,
    Function,
    UserData,
    Thread,
    Buffer,
    Unknown(i32),
    Unreadable,
}

impl core::fmt::Display for Value {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Value::Nil => write!(f, "nil"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Number(n) => write!(f, "{n}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Str(s) => write!(f, "{s}"),
            Value::Table => write!(f, "<table>"),
            Value::Function => write!(f, "<function>"),
            Value::UserData => write!(f, "<userdata>"),
            Value::Thread => write!(f, "<thread>"),
            Value::Buffer => write!(f, "<buffer>"),
            Value::Unknown(t) => write!(f, "<unknown {t}>"),
            Value::Unreadable => write!(f, "<unreadable>"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExecError {
    Compile(CompileError),
    LoadFailed(i32),
    Script(Value),
    NoThread,
    CorruptStack { top: usize, base: usize },
}

#[derive(Debug, Clone, Copy)]
pub struct Primitives {
    pub load: usize,
    pub call: usize,
    pub new_thread: Option<usize>,
    pub security_context_current: Option<usize>,
    pub lua_top: usize,
    pub caps: Option<CapabilityLayout>,
}

pub fn read_value(addr: usize) -> Value {
    let Ok(tag) = mem::read::<i32>(addr + TVALUE_TAG) else { return Value::Unreadable };
    match tag {
        0 => Value::Nil,
        1 => mem::read::<i32>(addr).map(|b| Value::Bool(b != 0)).unwrap_or(Value::Unreadable),
        3 => mem::read::<f64>(addr).map(Value::Number).unwrap_or(Value::Unreadable),
        4 => mem::read::<i32>(addr).map(Value::Int).unwrap_or(Value::Unreadable),
        6 => read_string(addr),
        7 => Value::Table,
        8 => Value::Function,
        9 => Value::UserData,
        10 => Value::Thread,
        11 => Value::Buffer,
        other => Value::Unknown(other),
    }
}

fn read_string(addr: usize) -> Value {
    let Ok(gc) = mem::read_ptr(addr) else { return Value::Unreadable };
    let Ok(len) = mem::read::<u32>(gc + TSTRING_LEN) else { return Value::Unreadable };
    let len = (len as usize).min(MAX_STRING);
    let mut bytes = vec![0u8; len];
    if mem::read_bytes(gc + TSTRING_DATA, &mut bytes).is_err() {
        return Value::Unreadable;
    }
    Value::Str(String::from_utf8_lossy(&bytes).into_owned())
}

/// Points a proto and its sub-protos at the full capability set by writing the address of
/// a static capability word into `proto->userdata`, the field the engine reads when
/// deriving a running frame's granted capabilities.
pub fn elevate_proto_tree(proto: usize, depth: usize, caps: &CapabilityLayout) {
    if proto == 0 || depth > MAX_PROTO_DEPTH {
        return;
    }
    let Ok(count) = mem::read::<i32>(proto + caps.proto_child_count) else { return };
    if !(0..=MAX_PROTO_CHILDREN).contains(&count) {
        return;
    }
    let grant = &PLUGIN_CAPABILITIES as *const u64 as usize;
    let _ = mem::write::<usize>(proto + caps.proto_userdata, grant);
    if count == 0 {
        return;
    }
    let Ok(children) = mem::read_ptr(proto + caps.proto_children) else { return };
    for index in 0..count as usize {
        let Ok(child) = mem::read_ptr(children + index * 8) else { continue };
        elevate_proto_tree(child, depth + 1, caps);
    }
}

pub fn elevate_closure(closure: usize, caps: &CapabilityLayout) {
    let Ok(is_c) = mem::read::<u8>(closure + caps.closure_is_c) else { return };
    if is_c != 0 {
        return;
    }
    let Ok(proto) = mem::read_ptr(closure + caps.closure_proto) else { return };
    elevate_proto_tree(proto, 0, caps);
}

pub fn elevate_security_context(current: usize) -> bool {
    let current: ContextCurrentFn = unsafe { core::mem::transmute(current) };
    let context = unsafe { current() } as usize;
    if context == 0 {
        return false;
    }
    let cached = mem::write(context + CONTEXT_CACHED_CAPABILITIES, u64::MAX).is_ok();
    let lazy = mem::write(context + CONTEXT_LAZY_FN, 0usize).is_ok();
    cached && lazy
}

static LUA_BASE_OFFSET: AtomicUsize = AtomicUsize::new(BASE_UNKNOWN);
static LUA_EXTRA_SPACE_OFFSET: AtomicUsize = AtomicUsize::new(BASE_UNKNOWN);
static CAPABILITY_REFUSALS: AtomicUsize = AtomicUsize::new(0);
const FULL_CAPABILITIES: u64 = 0xcc00_000f_ffff_ff3f;
static PLUGIN_CAPABILITIES: u64 = FULL_CAPABILITIES;

/// Records where `L->userdata` lives once a thread and its ScriptContext prove the
/// chain. Until this succeeds `elevate_thread` writes nothing.
pub fn calibrate_extra_space(lua_state: usize, script_context: usize) {
    if LUA_EXTRA_SPACE_OFFSET.load(Ordering::Relaxed) != BASE_UNKNOWN {
        return;
    }
    let Some(offset) = crate::vm::layout::derive_extra_space(lua_state, script_context) else {
        return;
    };
    LUA_EXTRA_SPACE_OFFSET.store(offset, Ordering::Relaxed);
    crate::log(&format!("layout: lua_State extra_space=+{offset:#x} (chain reaches script context)"));
}

/// Grants the thread every capability so the poll can reach Studio-only services.
/// The slot is only written when it does not currently hold something pointer-shaped:
/// a capability mask is far above the user address space, so this refuses to smash a
/// neighbouring object if the offset is ever wrong for a build.
pub fn elevate_thread(lua_state: usize, caps: &CapabilityLayout) -> bool {
    let offset = LUA_EXTRA_SPACE_OFFSET.load(Ordering::Relaxed);
    if offset == BASE_UNKNOWN {
        return false;
    }
    let Ok(extra) = mem::read_ptr(lua_state + offset) else { return false };
    let slot = extra + caps.extra_capabilities;
    let Ok(current) = mem::read::<u64>(slot) else { return false };
    if mem::looks_like_pointer(current as usize) {
        if CAPABILITY_REFUSALS.fetch_add(1, Ordering::Relaxed) == 0 {
            crate::log(&format!(
                "layout: capability slot at extra+{:#x} holds {current:#x}, refusing to write",
                caps.extra_capabilities
            ));
        }
        return false;
    }
    mem::write(slot, FULL_CAPABILITIES).is_ok()
}

/// Locates `L->base` by looking for the field holding `expected`, the stack slot the
/// freshly loaded closure sits at. Cached because the offset is fixed per Studio build.
fn base_offset(lua_state: usize, expected: usize, top_offset: usize) -> Option<usize> {
    let cached = LUA_BASE_OFFSET.load(Ordering::Relaxed);
    if cached != BASE_UNKNOWN {
        return Some(cached);
    }
    for offset in (0..LUA_FIELD_SCAN).step_by(8) {
        if offset == top_offset {
            continue;
        }
        if mem::read::<usize>(lua_state + offset) == Ok(expected) {
            LUA_BASE_OFFSET.store(offset, Ordering::Relaxed);
            crate::log(&format!("layout: lua_State base=+{offset:#x} (matched loaded closure slot)"));
            return Some(offset);
        }
    }
    None
}

const LUA_THREAD_TAG: u8 = 0x0a;

pub fn run(shared: usize, primitives: &Primitives, source: &str, chunk: &str) -> Result<Vec<Value>, ExecError> {
    if mem::read::<u8>(shared).unwrap_or(0) != LUA_THREAD_TAG {
        return Err(ExecError::NoThread);
    }

    let bytecode = luau::compile(source).map_err(ExecError::Compile)?;

    let shared_top_before = mem::read::<usize>(shared + primitives.lua_top)
        .map_err(|_| ExecError::CorruptStack { top: 0, base: 0 })?;

    let lua_state = match primitives.new_thread {
        Some(new_thread) => {
            let spawn: NewThreadFn = unsafe { core::mem::transmute(new_thread) };
            let fresh = unsafe { spawn(shared as *mut c_void) } as usize;
            if fresh == 0 {
                return Err(ExecError::NoThread);
            }
            fresh
        }
        None => shared,
    };

    if let Some(caps) = primitives.caps.as_ref() {
        elevate_thread(lua_state, caps);
    }
    if let Some(current) = primitives.security_context_current {
        elevate_security_context(current);
    }

    let name = std::ffi::CString::new(chunk).unwrap_or_else(|_| c"=chunk".to_owned());
    let load: LoadFn = unsafe { core::mem::transmute(primitives.load) };
    let status = unsafe {
        load(
            lua_state as *mut c_void,
            name.as_ptr(),
            bytecode.as_slice().as_ptr(),
            bytecode.len(),
            0,
        )
    };
    if status != 0 {
        return Err(ExecError::LoadFailed(status));
    }

    let top = mem::read::<usize>(lua_state + primitives.lua_top)
        .map_err(|_| ExecError::CorruptStack { top: 0, base: 0 })?;
    let base = top - TVALUE_SIZE;
    let base_field = base_offset(lua_state, base, primitives.lua_top);

    if let Some(caps) = primitives.caps.as_ref() {
        if let Ok(closure) = mem::read_ptr(base) {
            elevate_closure(closure, caps);
        }
        elevate_thread(lua_state, caps);
    }

    let call: CallFn = unsafe { core::mem::transmute(primitives.call) };
    let status = unsafe { call(lua_state as *mut c_void, 0, 0) };

    let top_after = mem::read::<usize>(lua_state + primitives.lua_top)
        .map_err(|_| ExecError::CorruptStack { top: 0, base: 0 })?;
    let base_after = base_field
        .and_then(|field| mem::read::<usize>(lua_state + field).ok())
        .unwrap_or(base);

    let restore = |result| {
        if lua_state != shared {
            let _ = mem::write(shared + primitives.lua_top, shared_top_before);
        }
        result
    };

    if top_after < base_after || (top_after - base_after) / TVALUE_SIZE > MAX_PLAUSIBLE_RESULTS {
        return restore(Err(ExecError::CorruptStack { top: top_after, base: base_after }));
    }

    if status != 0 {
        let error = read_value(top_after.saturating_sub(TVALUE_SIZE));
        return restore(Err(ExecError::Script(error)));
    }

    let mut values = Vec::new();
    let mut slot = base_after;
    while slot + TVALUE_SIZE <= top_after && values.len() < MAX_RESULTS {
        values.push(read_value(slot));
        slot += TVALUE_SIZE;
    }
    restore(Ok(values))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Arena(Vec<u64>);

    impl Arena {
        fn new(words: usize) -> Arena {
            Arena(vec![0u64; words])
        }
        fn addr(&self, word: usize) -> usize {
            &self.0[word] as *const u64 as usize
        }
        fn put(&mut self, word: usize, value: u64) {
            self.0[word] = value;
        }
    }

    fn write_tag(arena: &mut Arena, word: usize, tag: i32) {
        arena.put(word + 1, (tag as u32 as u64) << 32);
    }

    #[test]
    fn decodes_the_simple_tvalue_tags() {
        let mut arena = Arena::new(8);
        write_tag(&mut arena, 0, 0);
        assert_eq!(read_value(arena.addr(0)), Value::Nil);

        write_tag(&mut arena, 2, 1);
        arena.put(2, 1);
        assert_eq!(read_value(arena.addr(2)), Value::Bool(true));

        write_tag(&mut arena, 4, 4);
        arena.put(4, 42);
        assert_eq!(read_value(arena.addr(4)), Value::Int(42));
    }

    #[test]
    fn decodes_a_number() {
        let mut arena = Arena::new(4);
        write_tag(&mut arena, 0, 3);
        arena.put(0, 2.5f64.to_bits());
        assert_eq!(read_value(arena.addr(0)), Value::Number(2.5));
    }

    #[test]
    fn reports_unknown_tags_rather_than_guessing() {
        let mut arena = Arena::new(4);
        write_tag(&mut arena, 0, 99);
        assert_eq!(read_value(arena.addr(0)), Value::Unknown(99));
    }

    #[test]
    fn values_render_the_way_the_poll_script_expects() {
        assert_eq!(Value::Str("Tarot".into()).to_string(), "Tarot");
        assert_eq!(Value::Bool(false).to_string(), "false");
        assert_eq!(Value::Nil.to_string(), "nil");
    }
}
