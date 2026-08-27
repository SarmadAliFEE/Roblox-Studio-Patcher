pub mod discord;
pub mod features;
pub mod mem;
pub mod platform;
pub mod reporter;
pub mod scan;
pub mod vm;

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

/// Directory the hook writes its log and crash reports into.
pub fn log_dir() -> std::path::PathBuf {
    if cfg!(target_os = "windows") {
        std::env::temp_dir()
    } else {
        std::path::PathBuf::from("/tmp")
    }
}

/// Path of the running log.
pub fn log_path() -> std::path::PathBuf {
    log_dir().join("studio_patcher_hook.txt")
}

/// Path crash reports are appended to.
pub fn crash_log_path() -> std::path::PathBuf {
    log_dir().join("studio_patcher_crash.txt")
}

pub(crate) fn log(message: &str) {
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(log_path()) {
        let _ = writeln!(file, "{message}");
    }
}

fn on_loaded() {
    let version: String = platform::studio_version().unwrap_or_else(|| "unknown".into());
    log(&format!("studio-hook loaded - studio {version}"));
    guard("reporter", reporter::init);
    guard("features", features::init);
    std::thread::spawn(|| {
        guard("install", || match vm::hook::install() {
            Ok(patched) => log(&format!("studio-hook: hooked {patched} vtable slot(s)")),
            Err(err) => log(&format!("studio-hook: install failed: {err:?}")),
        });
    });
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

#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
pub extern "system" fn RSPHookInit() {}
