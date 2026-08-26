use core::ffi::{c_char, c_void};
use std::sync::Once;

unsafe extern "C" {
    fn luau_compile(
        source: *const c_char,
        size: usize,
        options: *mut c_void,
        outsize: *mut usize,
    ) -> *mut c_char;
    fn free(ptr: *mut c_void);
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
    static mut LUAU_BOOL_FLAGS: *mut FValueBool;
}

fn enable_luau_flags() {
    let mut node = unsafe { LUAU_BOOL_FLAGS };
    while !node.is_null() {
        let name = unsafe { (*node).name };
        if !name.is_null() {
            let name = unsafe { core::ffi::CStr::from_ptr(name) };
            if name.to_bytes().starts_with(b"Luau") {
                unsafe { (*node).value = true };
            }
        }
        node = unsafe { (*node).next };
    }
}

#[derive(Debug)]
pub enum CompileError {
    Empty,
    Rejected(String),
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::Empty => write!(f, "compiler returned no bytecode"),
            CompileError::Rejected(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for CompileError {}

pub fn compile(source: &str) -> Result<Vec<u8>, CompileError> {
    static FLAGS: Once = Once::new();
    FLAGS.call_once(enable_luau_flags);

    let mut len = 0usize;
    let ptr = unsafe {
        luau_compile(source.as_ptr() as *const c_char, source.len(), core::ptr::null_mut(), &mut len)
    };
    if ptr.is_null() || len == 0 {
        if !ptr.is_null() {
            unsafe { free(ptr as *mut c_void) };
        }
        return Err(CompileError::Empty);
    }

    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
    let result = if bytes[0] == 0 {
        let message = core::ffi::CStr::from_bytes_until_nul(&bytes[1..])
            .ok()
            .and_then(|c| c.to_str().ok())
            .unwrap_or("unknown compile error")
            .to_owned();
        Err(CompileError::Rejected(message))
    } else {
        Ok(bytes.to_vec())
    };
    unsafe { free(ptr as *mut c_void) };
    result
}

#[cfg(test)]
mod tests {
    use super::compile;

    #[test]
    fn compiles_valid_source_to_studio_bytecode() {
        let bytecode = compile("return 1 + 1").expect("valid source compiles");
        assert_eq!(bytecode[0], 13, "must emit the bytecode version Studio accepts");
    }

    #[test]
    fn reports_a_syntax_error() {
        match compile("this is not lua ((") {
            Err(super::CompileError::Rejected(message)) => assert!(!message.is_empty()),
            other => panic!("expected a rejection, got {other:?}"),
        }
    }
}
