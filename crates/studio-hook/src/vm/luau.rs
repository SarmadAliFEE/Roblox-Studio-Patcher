use core::ffi::{c_char, c_void};

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

pub fn enable_luau_flags() -> usize {
    let mut enabled = 0;
    let mut node = unsafe { LUAU_BOOL_FLAGS };
    while !node.is_null() {
        let name = unsafe { (*node).name };
        if !name.is_null() {
            let name = unsafe { core::ffi::CStr::from_ptr(name) };
            if name.to_bytes().starts_with(b"Luau") {
                unsafe { (*node).value = true };
                enabled += 1;
            }
        }
        node = unsafe { (*node).next };
    }
    enabled
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    Empty,
    Rejected(String),
}

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
    pub fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.ptr as *const u8, self.len) }
    }

    pub fn len(&self) -> usize {
        self.len
    }

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

pub fn compile(source: &str) -> Result<Bytecode, CompileError> {
    let mut len = 0usize;
    let ptr = unsafe {
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

    let bytecode = Bytecode { ptr, len };
    let bytes = bytecode.as_slice();
    if bytes[0] == 0 {
        let message = core::ffi::CStr::from_bytes_until_nul(&bytes[1..])
            .ok()
            .and_then(|c| c.to_str().ok())
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
        let before = compile("return 1 + 1").expect("compiles").as_slice().to_vec();
        let enabled = enable_luau_flags();
        assert!(enabled > 0, "expected to find Luau* flags in the vendored compiler");
        let after = compile("return 1 + 1").expect("compiles").as_slice().to_vec();
        assert_eq!(after[0], 13);
        let _ = before;
    }

    #[test]
    fn compiles_valid_source_to_versioned_bytecode() {
        let bytecode = compile("return 1 + 1").expect("valid source compiles");
        assert!(!bytecode.is_empty());
        assert_eq!(bytecode.as_slice()[0], 13, "must emit the bytecode version Studio accepts");
    }

    #[test]
    fn reports_a_syntax_error_instead_of_emitting_bytecode() {
        let err = compile("this is not lua ((").unwrap_err();
        match err {
            CompileError::Rejected(message) => assert!(!message.is_empty()),
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    #[test]
    fn compiles_the_kind_of_script_the_hook_actually_runs() {
        let source = r#"
            local placeId = tostring(game.PlaceId)
            local ok, active = pcall(function()
                return game:GetService("StudioService").ActiveScript
            end)
            return placeId .. "\t" .. tostring(ok and active ~= nil)
        "#;
        let bytecode = compile(source).expect("poll script compiles");
        assert_eq!(bytecode.as_slice()[0], 13);
    }
}
