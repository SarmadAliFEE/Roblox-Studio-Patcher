pub mod mem;
pub mod platform;
pub mod scan;

use std::panic::{AssertUnwindSafe, catch_unwind};

pub(crate) fn guard<T>(what: &str, body: impl FnOnce() -> T) -> Option<T> {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => Some(value),
        Err(_) => {
            log(&format!("panic caught at boundary: {what}"));
            None
        }
    }
}

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
    let Some(image) = platform::find_main_image() else { return };
    log(&format!("studio-hook loaded, image slide=0x{:x}", image.slide));
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
