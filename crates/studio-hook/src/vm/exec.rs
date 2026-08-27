use core::ffi::{c_char, c_void};

use crate::mem;
use luau_compile::{self as luau, CompileError};

pub const CLOSURE_IS_C: usize = 0x3;
pub const CLOSURE_PROTO: usize = 0x18;
pub const PROTO_CHILDREN: usize = 0x10;
pub const PROTO_CHILD_COUNT: usize = 0x8c;
pub const PROTO_CAPABILITY_OVERRIDE: usize = 0x60;
pub const EXTRA_SPACE_CAPABILITIES: usize = 0x40;
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

static ELEVATED_CAPABILITIES: u64 = u64::MAX;

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

pub fn elevate_proto_tree(proto: usize, depth: usize) {
    if proto == 0 || depth > MAX_PROTO_DEPTH {
        return;
    }
    let override_ptr = &ELEVATED_CAPABILITIES as *const u64 as usize;
    let _ = mem::write(proto + PROTO_CAPABILITY_OVERRIDE, override_ptr);

    let Ok(count) = mem::read::<i32>(proto + PROTO_CHILD_COUNT) else { return };
    if count <= 0 || count > MAX_PROTO_CHILDREN {
        return;
    }
    let Ok(children) = mem::read_ptr(proto + PROTO_CHILDREN) else { return };
    for index in 0..count as usize {
        let Ok(child) = mem::read_ptr(children + index * 8) else { continue };
        elevate_proto_tree(child, depth + 1);
    }
}

pub fn elevate_closure(closure: usize) {
    let Ok(is_c) = mem::read::<u8>(closure + CLOSURE_IS_C) else { return };
    if is_c != 0 {
        return;
    }
    let Ok(proto) = mem::read_ptr(closure + CLOSURE_PROTO) else { return };
    elevate_proto_tree(proto, 0);
}

pub fn elevate_thread(lua_state: usize) -> bool {
    let Ok(extra) = mem::read_ptr(lua_state + crate::vm::L_EXTRA_SPACE) else { return false };
    mem::write(extra + EXTRA_SPACE_CAPABILITIES, u64::MAX).is_ok()
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

const LUA_THREAD_TAG: u8 = 0x0a;

pub fn run(shared: usize, primitives: &Primitives, source: &str, chunk: &str) -> Result<Vec<Value>, ExecError> {
    if mem::read::<u8>(shared).unwrap_or(0) != LUA_THREAD_TAG {
        return Err(ExecError::NoThread);
    }

    let bytecode = luau::compile(source).map_err(ExecError::Compile)?;

    let shared_top_before = mem::read::<usize>(shared + crate::vm::L_TOP)
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

    let top = mem::read::<usize>(lua_state + crate::vm::L_TOP)
        .map_err(|_| ExecError::CorruptStack { top: 0, base: 0 })?;
    let base = top - TVALUE_SIZE;

    if let Ok(closure) = mem::read_ptr(base) {
        elevate_closure(closure);
    }
    elevate_thread(lua_state);
    if let Some(current) = primitives.security_context_current {
        elevate_security_context(current);
    }

    let call: CallFn = unsafe { core::mem::transmute(primitives.call) };
    let status = unsafe { call(lua_state as *mut c_void, 0, 0) };

    let top_after = mem::read::<usize>(lua_state + crate::vm::L_TOP)
        .map_err(|_| ExecError::CorruptStack { top: 0, base: 0 })?;
    let base_after = mem::read::<usize>(lua_state + crate::vm::L_BASE)
        .map_err(|_| ExecError::CorruptStack { top: top_after, base: 0 })?;

    let restore = |result| {
        if lua_state != shared {
            let _ = mem::write(shared + crate::vm::L_TOP, shared_top_before);
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
