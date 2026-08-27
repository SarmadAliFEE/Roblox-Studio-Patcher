//! Bindings to the vendored Luau compiler, shared by the CLI and the hook.

use core::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicBool, Ordering};

unsafe extern "C" {
    unsafe fn luau_compile(
        source: *const c_char,
        size: usize,
        options: *mut c_void,
        outsize: *mut usize,
    ) -> *mut c_char;
    unsafe fn free(ptr: *mut c_void);
}

#[repr(C)]
struct FValueBool {
    value: bool,
    dynamic: bool,
    name: *const c_char,
    next: *mut FValueBool,
    version: u32,
}

unsafe extern "C" {
    #[link_name = "_ZN4Luau6FValueIbE4listE"]
    unsafe static mut LUAU_BOOL_FLAGS: *mut FValueBool;
}

/// Turns on every `Luau*` boolean feature flag in the vendored compiler.
///
/// Idempotent, and returns how many flags the walk found.
///
/// # Examples
/// ```
/// assert!(luau_compile::enable_luau_flags() > 0);
/// ```
pub fn enable_luau_flags() -> usize {
    let mut enabled: usize = 0;
    let mut node: *mut FValueBool = unsafe { LUAU_BOOL_FLAGS };
    while !node.is_null() {
        let name: *const i8 = unsafe { (*node).name };
        if !name.is_null() {
            let name: &std::ffi::CStr = unsafe { core::ffi::CStr::from_ptr(name) };
            if name.to_bytes().starts_with(b"Luau") {
                unsafe { (*node).value = true };
                enabled += 1;
            }
        }
        node = unsafe { (*node).next };
    }
    enabled
}

/// Why a compile did not produce bytecode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    Empty,
    Rejected(String),
}

impl core::fmt::Display for CompileError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CompileError::Empty => write!(f, "compiler returned no bytecode"),
            CompileError::Rejected(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for CompileError {}

/// Compiler output, freed on drop.
pub struct Bytecode {
    ptr: *mut c_char,
    len: usize,
}

impl core::fmt::Debug for Bytecode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Bytecode({} bytes, version {})", self.len, self.as_slice()[0])
    }
}

impl Bytecode {
    /// The compiled bytecode, starting with its version byte.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.ptr as *const u8, self.len) }
    }

    /// Length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when the compiler produced nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Drop for Bytecode {
    fn drop(&mut self) {
        unsafe { free(self.ptr as *mut c_void) };
    }
}

unsafe impl Send for Bytecode {}

/// Compiles Luau source to the bytecode version Studio accepts.
///
/// Feature flags are enabled once per process before the first compile.
///
/// # Errors
/// Returns [`CompileError::Rejected`] with the compiler message when `source`
/// does not parse, or [`CompileError::Empty`] when nothing is produced.
///
/// # Examples
/// ```
/// let bytecode = luau_compile::compile("return 1 + 1")?;
/// assert_eq!(bytecode.as_slice()[0], 13);
/// # Ok::<(), luau_compile::CompileError>(())
/// ```
pub fn compile(source: &str) -> Result<Bytecode, CompileError> {
    static FLAGS_READY: AtomicBool = AtomicBool::new(false);
    if !FLAGS_READY.load(Ordering::Relaxed) && enable_luau_flags() > 0 {
        FLAGS_READY.store(true, Ordering::Relaxed);
    }

    let mut len: usize = 0usize;
    let ptr: *mut i8 = unsafe {
        luau_compile(
            source.as_ptr() as *const c_char,
            source.len(),
            core::ptr::null_mut(),
            &mut len,
        )
    };
    if ptr.is_null() || len == 0 {
        if !ptr.is_null() {
            unsafe { free(ptr as *mut c_void) };
        }
        return Err(CompileError::Empty);
    }

    let bytecode: Bytecode = Bytecode { ptr, len };
    let bytes: &[u8] = bytecode.as_slice();
    if bytes[0] == 0 {
        let message: String = core::ffi::CStr::from_bytes_until_nul(&bytes[1..])
            .ok()
            .and_then(|c: &std::ffi::CStr| c.to_str().ok())
            .unwrap_or("unknown compile error")
            .to_owned();
        return Err(CompileError::Rejected(message));
    }
    Ok(bytecode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_list_is_walkable_and_flags_change_the_output() {
        let before: Vec<u8> = compile("return 1 + 1").expect("compiles").as_slice().to_vec();
        let enabled: usize = enable_luau_flags();
        assert!(enabled > 0, "expected to find Luau* flags in the vendored compiler");
        let after: Vec<u8> = compile("return 1 + 1").expect("compiles").as_slice().to_vec();
        assert_eq!(after[0], 13);
        let _ = before;
    }

    #[test]
    fn compiles_valid_source_to_versioned_bytecode() {
        enable_luau_flags();
        let bytecode: Bytecode = compile("return 1 + 1").expect("valid source compiles");
        assert!(!bytecode.is_empty());
        assert_eq!(bytecode.as_slice()[0], 13, "must emit the bytecode version Studio accepts");
    }

    #[test]
    fn reports_a_syntax_error_instead_of_emitting_bytecode() {
        let err: CompileError = compile("this is not lua ((").unwrap_err();
        match err {
            CompileError::Rejected(message) => assert!(!message.is_empty()),
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    #[test]
    fn compiles_the_kind_of_script_the_hook_actually_runs() {
        enable_luau_flags();
        let source: &str = r#"
            local placeId = tostring(game.PlaceId)
            local ok, active = pcall(function()
                return game:GetService("StudioService").ActiveScript
            end)
            return placeId .. "\t" .. tostring(ok and active ~= nil)
        "#;
        let bytecode: Bytecode = compile(source).expect("poll script compiles");
        assert_eq!(bytecode.as_slice()[0], 13);
    }
}
