//! Studio hook payload: injected into Roblox Studio and run inside its
//! process.
//!
//! Nothing here may unwind into the host. Every entry point the host can
//! reach - the load-time constructor and, later, the hooked vtable slots -
//! goes through `guard`, so a panic fails that one operation and leaves
//! Studio running.

pub mod mem;

use std::panic::{AssertUnwindSafe, catch_unwind};

/// Runs `body`, swallowing a panic instead of letting it cross the FFI
/// boundary into Studio. Returns `None` if `body` panicked.
///
/// Unwinding out of an `extern "C"` frame is undefined behaviour, and this
/// crate is loaded into a process that must survive our mistakes, so this
/// wraps every host-reachable entry point.
pub(crate) fn guard<T>(what: &str, body: impl FnOnce() -> T) -> Option<T> {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => Some(value),
        Err(_) => {
            log(&format!("panic caught at boundary: {what}"));
            None
        }
    }
}

/// Appends a line to the hook's log file.
///
/// Deliberately does its own open/append/close per line rather than
/// holding a handle: this is called from several of Studio's own threads,
/// and a log that survives a hard kill is worth more than a fast one.
pub(crate) fn log(message: &str) {
    use std::io::Write;
    let path = if cfg!(target_os = "windows") {
        std::env::temp_dir().join("studio_patcher_hook.txt")
    } else {
        std::path::PathBuf::from("/tmp/studio_patcher_hook.txt")
    };
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{message}");
    }
}

fn on_loaded() {
    log("studio-hook loaded");

    // Proves the memory layer is live and, more importantly, that an
    // unmapped address comes back as an error rather than taking Studio
    // down - the property the whole design rests on.
    let probe: mem::MemResult<u64> = mem::read(0x0000_7ffe_dead_0000);
    log(&format!("mem layer: unmapped probe -> {probe:?}"));
}

#[cfg(target_os = "macos")]
#[used]
#[unsafe(link_section = "__DATA,__mod_init_func")]
static LOAD_HOOK: extern "C" fn() = {
    extern "C" fn ctor() {
        guard("ctor", on_loaded);
    }
    ctor
};

#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
pub extern "system" fn DllMain(_module: *mut core::ffi::c_void, reason: u32, _reserved: *mut core::ffi::c_void) -> i32 {
    const DLL_PROCESS_ATTACH: u32 = 1;
    if reason == DLL_PROCESS_ATTACH {
        guard("DllMain", on_loaded);
    }
    1
}
